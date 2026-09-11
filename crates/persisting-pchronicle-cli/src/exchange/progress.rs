//! Pipeline-oriented CLI progress.
//!
//! Each pipeline stage owns one status line:
//! `listing  ok=12 skipped=2 empty=1 error=0  queue=3/64  1.2GiB  [listing] path.json`
//! The GiB column is attributed **source** bytes for that stage (not Lance/S3
//! on-disk size). Commit attributes bytes when a trajectory enters its write
//! batch so the column stays aligned with reading/parsing under backpressure.
//! Bracket status:
//! - `waiting` — stalled on upstream (no item yet)
//! - `pending→X` — blocked because downstream buffer `X` is full
//! - AIMD detail always follows the word `aimd` on fetch/commit

use super::super::*;
use std::io::Write;
use std::sync::Arc;

/// Stable id for a progress line / pipeline stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum StageId {
    Discover,
    Fetch,
    Parse,
    Commit,
    Delete,
}

impl StageId {
    pub(crate) fn noun(self) -> &'static str {
        match self {
            Self::Discover => "listing",
            Self::Fetch => "reading",
            Self::Parse => "parsing",
            Self::Commit => "commit",
            Self::Delete => "delete",
        }
    }

    pub(crate) fn verb(self) -> &'static str {
        match self {
            Self::Discover => "listing",
            Self::Fetch => "reading",
            Self::Parse => "parsing",
            Self::Commit => "committing",
            Self::Delete => "deleting",
        }
    }
}

#[derive(Debug, Clone, Default)]
struct StageState {
    /// Successfully processed items (files / trajectories).
    ok: u64,
    skipped: u64,
    empty: u64,
    error: u64,
    bytes: u64,
    /// Items accepted from the upstream stage (commit: trajectories ready).
    inbound: u64,
    /// Live inbound channel depth (pushed on enqueue, popped on dequeue).
    queue_depth: u64,
    /// Inbound channel capacity for `queue=depth/cap` display.
    queue_cap: Option<u64>,
    /// Optional known total (delete wipe, prelisted discover).
    total_items: Option<u64>,
    current: String,
    last_error: Option<String>,
    /// Refcount of workers blocked on downstream backpressure.
    flow_waiters: u32,
    /// Refcount of workers blocked waiting for an upstream item.
    upstream_waiters: u32,
    /// Human-readable downstream pending reason (e.g. `pending→parsing`).
    flow: Option<String>,
    /// Active object-store AIMD wait reason (`throttle` / `admit` / `backoff`), if any.
    aimd_event: Option<String>,
    /// Remaining AIMD wait from the latest gate tick (ms); drives live `cd=`.
    aimd_wait_ms: Option<u64>,
}

impl StageState {
    fn processed(&self) -> u64 {
        self.ok
            .saturating_add(self.skipped)
            .saturating_add(self.empty)
            .saturating_add(self.error)
    }

    fn format_line(&self, id: StageId, queue: &str, status: &str) -> String {
        let activity = if self.current.is_empty() {
            "-".into()
        } else {
            truncate_middle(&self.current, 72)
        };
        let bracket = stage_bracket(id, self.upstream_waiters > 0, self.flow_waiters > 0, status);
        format!(
            "{}\tok={} skipped={} empty={} error={}\tqueue={}\t{}\t[{bracket}] {}",
            id.noun(),
            self.ok,
            self.skipped,
            self.empty,
            self.error,
            queue,
            format_byte_count(self.bytes),
            activity,
        )
    }
}

/// Shared handle a running stage uses to report work / errors.
#[derive(Clone)]
pub(crate) struct StageHandle {
    id: StageId,
    state: Arc<std::sync::Mutex<StageState>>,
    painter: Arc<std::sync::Mutex<PipelinePainter>>,
}

impl StageHandle {
    #[allow(dead_code)]
    pub(crate) fn id(&self) -> StageId {
        self.id
    }

    pub(crate) fn set_current(&self, item: impl Into<String>) {
        if let Ok(mut state) = self.state.lock() {
            state.current = item.into();
        }
        let _ = self.repaint();
    }

    pub(crate) fn clear_current(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.current.clear();
        }
        let _ = self.repaint();
    }

    pub(crate) fn set_total_items(&self, total: u64) {
        if let Ok(mut state) = self.state.lock() {
            state.total_items = Some(total);
        }
        let _ = self.repaint();
    }

    pub(crate) fn set_queue_cap(&self, cap: u64) {
        if let Ok(mut state) = self.state.lock() {
            state.queue_cap = Some(cap);
        }
        let _ = self.repaint();
    }

    /// One item entered this stage's inbound channel.
    pub(crate) fn queue_push(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.queue_depth = state.queue_depth.saturating_add(1);
            if let Some(cap) = state.queue_cap {
                state.queue_depth = state.queue_depth.min(cap);
            }
        }
        let _ = self.repaint();
    }

    /// One item left this stage's inbound channel.
    pub(crate) fn queue_pop(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.queue_depth = state.queue_depth.saturating_sub(1);
        }
        let _ = self.repaint();
    }

    /// Set absolute queue depth (e.g. commit batch fill).
    pub(crate) fn set_queue(&self, queue: u64) {
        if let Ok(mut state) = self.state.lock() {
            state.queue_depth = match state.queue_cap {
                Some(cap) => queue.min(cap),
                None => queue,
            };
        }
        let _ = self.repaint();
    }

    /// Mark this stage blocked on downstream backpressure (`pending→…`).
    pub(crate) fn enter_flow_wait(&self, reason: impl Into<String>) {
        if let Ok(mut state) = self.state.lock() {
            state.flow_waiters = state.flow_waiters.saturating_add(1);
            state.flow = Some(reason.into());
        }
        let _ = self.repaint();
    }

    /// Clear one downstream-pending waiter; label drops when the last waiter leaves.
    pub(crate) fn leave_flow_wait(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.flow_waiters = state.flow_waiters.saturating_sub(1);
            if state.flow_waiters == 0 {
                state.flow = None;
            }
        }
        let _ = self.repaint();
    }

    /// Mark this stage blocked waiting for an upstream item.
    pub(crate) fn enter_upstream_wait(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.upstream_waiters = state.upstream_waiters.saturating_add(1);
        }
        let _ = self.repaint();
    }

    /// Clear one upstream-wait waiter.
    pub(crate) fn leave_upstream_wait(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.upstream_waiters = state.upstream_waiters.saturating_sub(1);
        }
        let _ = self.repaint();
    }

    /// Overlay AIMD reason + optional remaining wait; always repaints this stage.
    pub(crate) fn set_aimd_status(&self, event: Option<String>, wait_ms: Option<u64>) {
        if let Ok(mut state) = self.state.lock() {
            state.aimd_event = event;
            state.aimd_wait_ms = wait_ms;
        }
        let _ = self.repaint();
    }

    pub(crate) fn record(&self, items: u64, bytes: u64) {
        if let Ok(mut state) = self.state.lock() {
            state.ok = state.ok.saturating_add(items);
            state.bytes = state.bytes.saturating_add(bytes);
        }
        let _ = self.repaint();
    }

    pub(crate) fn record_skipped(&self, items: u64, bytes: u64) {
        if let Ok(mut state) = self.state.lock() {
            state.skipped = state.skipped.saturating_add(items);
            state.bytes = state.bytes.saturating_add(bytes);
        }
        let _ = self.repaint();
    }

    pub(crate) fn record_empty(&self, items: u64, bytes: u64) {
        if let Ok(mut state) = self.state.lock() {
            state.empty = state.empty.saturating_add(items);
            state.bytes = state.bytes.saturating_add(bytes);
        }
        let _ = self.repaint();
    }

    pub(crate) fn record_inbound(&self, items: u64) {
        if let Ok(mut state) = self.state.lock() {
            state.inbound = state.inbound.saturating_add(items);
        }
        let _ = self.repaint();
    }

    #[allow(dead_code)]
    pub(crate) fn set_items(&self, items: u64) {
        if let Ok(mut state) = self.state.lock() {
            state.ok = items;
        }
        let _ = self.repaint();
    }

    #[allow(dead_code)]
    pub(crate) fn record_bytes(&self, bytes: u64) {
        if let Ok(mut state) = self.state.lock() {
            state.bytes = state.bytes.saturating_add(bytes);
        }
        let _ = self.repaint();
    }

    /// Replace the size column with an absolute value (e.g. measured on-disk).
    pub(crate) fn set_bytes(&self, bytes: u64) {
        if let Ok(mut state) = self.state.lock() {
            state.bytes = bytes;
        }
        let _ = self.repaint();
    }

    pub(crate) fn record_error(&self, error: impl std::fmt::Display) {
        if let Ok(mut state) = self.state.lock() {
            state.last_error = Some(error.to_string());
            state.error = state.error.saturating_add(1);
        }
        let _ = self.repaint();
    }

    /// In-place activity override (e.g. index build note on the commit line).
    pub(crate) fn set_activity_override(&self, activity: &str) {
        if let Ok(mut painter) = self.painter.lock() {
            painter.activity_override = Some((self.id, activity.to_owned()));
            let _ = painter.paint();
        }
    }

    #[allow(dead_code)]
    pub(crate) fn clear_activity_override(&self) {
        if let Ok(mut painter) = self.painter.lock() {
            painter.activity_override = None;
            let _ = painter.paint();
        }
    }

    pub(crate) fn note_committed(&self, committed: u64, batch_bytes: u64) {
        if let Ok(mut state) = self.state.lock() {
            state.ok = committed;
            state.bytes = state.bytes.saturating_add(batch_bytes);
            state.current = format!("trajectories={committed}");
        }
        if let Ok(mut painter) = self.painter.lock() {
            painter.activity_override = None;
            if let Ok(state) = self.state.lock() {
                painter.stages.insert(self.id, state.clone());
            }
            if !painter.tty {
                let line = painter.line_for(self.id);
                painter.log_lines.push(line);
            }
            let _ = painter.paint();
        }
    }

    fn repaint(&self) -> Result<()> {
        let snapshot = self
            .state
            .lock()
            .map(|state| state.clone())
            .unwrap_or_default();
        if let Ok(mut painter) = self.painter.lock() {
            painter.stages.insert(self.id, snapshot);
            painter.paint()?;
        }
        Ok(())
    }
}

#[derive(Debug, Default)]
struct PipelinePainter {
    tty: bool,
    painted_lines: usize,
    /// When true, only the Delete stage line is shown (replace wipe).
    delete_mode: bool,
    stages: std::collections::HashMap<StageId, StageState>,
    order: Vec<StageId>,
    activity_override: Option<(StageId, String)>,
    log_lines: Vec<String>,
    last_paint: Option<std::time::Instant>,
}

impl PipelinePainter {
    fn stage_state(&self, id: StageId) -> StageState {
        self.stages.get(&id).cloned().unwrap_or_default()
    }

    /// Live inbound channel depth / capacity (not derived from ok counters).
    /// Depth is clamped to capacity so concurrent push/pop races never paint
    /// impossible values like `65/64`.
    fn queue_for(&self, id: StageId) -> String {
        let state = self.stage_state(id);
        match id {
            StageId::Discover | StageId::Delete => state
                .total_items
                .map(|total| {
                    let remaining = total.saturating_sub(state.processed());
                    format!("{remaining}/{total}")
                })
                .unwrap_or_else(|| "-".into()),
            StageId::Fetch | StageId::Parse | StageId::Commit => match state.queue_cap {
                Some(cap) => format!("{}/{}", state.queue_depth.min(cap), cap),
                None => format!("{}", state.queue_depth),
            },
        }
    }

    fn line_for(&self, id: StageId) -> String {
        let state = self.stage_state(id);
        let queue = self.queue_for(id);
        let status = enriched_status_label(id, &state);
        if let Some((override_id, activity)) = &self.activity_override
            && *override_id == id
        {
            let size = format_byte_count(state.bytes);
            let bracket = if status.is_empty() {
                "writing".to_owned()
            } else {
                format!("writing {status}")
            };
            return format!(
                "{}\tok={} skipped={} empty={} error={}\tqueue={queue}\t{size}\t[{bracket}] {}",
                id.noun(),
                state.ok,
                state.skipped,
                state.empty,
                state.error,
                truncate_middle(activity, 72),
            );
        }
        state.format_line(id, &queue, &status)
    }

    fn visible_ids(&self) -> Vec<StageId> {
        if self.delete_mode {
            vec![StageId::Delete]
        } else {
            self.order.clone()
        }
    }

    fn paint(&mut self) -> Result<()> {
        let lines: Vec<String> = self
            .visible_ids()
            .into_iter()
            .map(|id| self.line_for(id))
            .collect();
        if self.tty {
            let mut err = std::io::stderr();
            if self.painted_lines > 0 {
                write!(err, "\x1b[{}A", self.painted_lines)
                    .context("move pipeline progress cursor")?;
            }
            for line in &lines {
                write!(err, "\r\x1b[2K{line}\n").context("paint pipeline progress")?;
            }
            // Clear leftover lines if stage count shrank (e.g. leaving delete mode).
            for _ in lines.len()..self.painted_lines {
                write!(err, "\r\x1b[2K\n").context("clear stale progress line")?;
            }
            if lines.len() < self.painted_lines {
                write!(err, "\x1b[{}A", self.painted_lines - lines.len())
                    .context("rewind after clearing stale lines")?;
            }
            err.flush().context("flush pipeline progress")?;
            self.painted_lines = lines.len();
            self.last_paint = Some(std::time::Instant::now());
            return Ok(());
        }
        Ok(())
    }

    fn should_throttle(&self) -> bool {
        self.tty
            && self
                .last_paint
                .map(|at| at.elapsed() < std::time::Duration::from_millis(100))
                .unwrap_or(false)
    }

    fn finish_tty(&mut self) -> Result<()> {
        if self.tty && self.painted_lines > 0 {
            let mut err = std::io::stderr();
            writeln!(err).context("finish pipeline progress")?;
            err.flush().context("flush pipeline progress")?;
            self.painted_lines = 0;
        }
        Ok(())
    }
}

/// Multi-stage progress surface used by import (and reusable by export/sync).
pub(crate) struct CliProgress {
    painter: Arc<std::sync::Mutex<PipelinePainter>>,
    handles: std::collections::HashMap<StageId, StageHandle>,
    /// Index-build callbacks paint onto the commit stage.
    index_surface: Arc<std::sync::Mutex<IndexActivityBridge>>,
}

struct IndexActivityBridge {
    commit: Option<StageHandle>,
}

impl CliProgress {
    pub(crate) fn new(tty: bool) -> Self {
        let order = vec![
            StageId::Discover,
            StageId::Fetch,
            StageId::Parse,
            StageId::Commit,
        ];
        let painter = Arc::new(std::sync::Mutex::new(PipelinePainter {
            tty,
            painted_lines: 0,
            delete_mode: false,
            stages: std::collections::HashMap::new(),
            order: order.clone(),
            activity_override: None,
            log_lines: Vec::new(),
            last_paint: None,
        }));
        let mut handles = std::collections::HashMap::new();
        for id in order {
            let state = Arc::new(std::sync::Mutex::new(StageState::default()));
            if let Ok(mut painter) = painter.lock() {
                painter.stages.insert(id, StageState::default());
            }
            handles.insert(
                id,
                StageHandle {
                    id,
                    state,
                    painter: Arc::clone(&painter),
                },
            );
        }
        // Delete stage exists but is only shown in delete_mode.
        let delete_state = Arc::new(std::sync::Mutex::new(StageState::default()));
        handles.insert(
            StageId::Delete,
            StageHandle {
                id: StageId::Delete,
                state: delete_state,
                painter: Arc::clone(&painter),
            },
        );
        let index_surface = Arc::new(std::sync::Mutex::new(IndexActivityBridge {
            commit: handles.get(&StageId::Commit).cloned(),
        }));
        Self {
            painter,
            handles,
            index_surface,
        }
    }

    pub(crate) fn stage(&self, id: StageId) -> StageHandle {
        self.handles
            .get(&id)
            .cloned()
            .expect("stage registered in CliProgress::new")
    }

    pub(crate) fn attach_index_progress(
        &self,
    ) -> persisting_pchronicle::storage::IndexBuildProgressGuard {
        let bridge = Arc::clone(&self.index_surface);
        persisting_pchronicle::storage::install_index_build_progress(Arc::new(move |message| {
            if let Ok(bridge) = bridge.lock()
                && let Some(commit) = &bridge.commit
            {
                commit.set_activity_override(message);
            }
        }))
    }

    /// Mirror object-store AIMD / admit waits onto fetch (read) and commit (write).
    pub(crate) fn attach_object_store_throttle(
        &self,
    ) -> persisting_pchronicle::storage::ObjectStoreThrottleHookGuard {
        let fetch = self.stage(StageId::Fetch);
        let commit = self.stage(StageId::Commit);
        persisting_pchronicle::storage::install_object_store_throttle_hook(Arc::new(move |event| {
            let apply = |kind: persisting_pchronicle::storage::ObjectStoreIoKind,
                         reason: &str,
                         wait_ms: Option<u64>| {
                let (primary, sibling) = match kind {
                    persisting_pchronicle::storage::ObjectStoreIoKind::Read => (&fetch, &commit),
                    persisting_pchronicle::storage::ObjectStoreIoKind::Write => (&commit, &fetch),
                };
                let overlay = match reason {
                    "recover" | "ok" | "" => None,
                    other => Some(other.to_owned()),
                };
                primary.set_aimd_status(overlay, wait_ms);
                // Sibling line also re-reads the shared AIMD snapshot.
                let _ = sibling.repaint();
            };
            match event {
                persisting_pchronicle::storage::ObjectStoreThrottleEvent::Enter {
                    kind,
                    reason,
                    wait_ms,
                    ..
                }
                | persisting_pchronicle::storage::ObjectStoreThrottleEvent::Update {
                    kind,
                    reason,
                    wait_ms,
                    ..
                } => {
                    let wait = (wait_ms > 0).then_some(wait_ms);
                    apply(kind, reason, wait);
                }
                persisting_pchronicle::storage::ObjectStoreThrottleEvent::Leave { kind } => {
                    apply(kind, "", None);
                }
            }
        }))
    }

    #[allow(dead_code)]
    pub(crate) fn reset_import_counters(&mut self) {
        for id in [
            StageId::Discover,
            StageId::Fetch,
            StageId::Parse,
            StageId::Commit,
            StageId::Delete,
        ] {
            let handle = self.stage(id);
            if let Ok(mut state) = handle.state.lock() {
                *state = StageState::default();
            }
            let _ = handle.repaint();
        }
        if let Ok(mut painter) = self.painter.lock() {
            painter.delete_mode = false;
            painter.activity_override = None;
            for id in &painter.order.clone() {
                painter.stages.insert(*id, StageState::default());
            }
        }
    }

    pub(crate) fn set_discovered(&mut self, files: u64, bytes: u64) -> Result<()> {
        let discover = self.stage(StageId::Discover);
        if let Ok(mut state) = discover.state.lock() {
            state.ok = files;
            state.bytes = bytes;
            state.total_items = Some(files);
            state.current.clear();
        }
        for id in [StageId::Fetch, StageId::Parse] {
            let handle = self.stage(id);
            if let Ok(mut state) = handle.state.lock() {
                state.total_items = Some(files);
            }
        }
        discover.repaint()
    }

    pub(crate) fn note_discovered(&mut self, file: &str, bytes: u64) -> Result<()> {
        let discover = self.stage(StageId::Discover);
        discover.set_current(file);
        let throttle = self
            .painter
            .lock()
            .map(|p| p.should_throttle())
            .unwrap_or(false);
        if let Ok(mut state) = discover.state.lock() {
            state.ok = state.ok.saturating_add(1);
            state.bytes = state.bytes.saturating_add(bytes);
        }
        if throttle {
            return Ok(());
        }
        discover.repaint()
    }

    pub(crate) fn note_fetched(&mut self, file: &str, bytes: u64) -> Result<()> {
        let fetch = self.stage(StageId::Fetch);
        fetch.set_current(file);
        fetch.record(1, bytes);
        Ok(())
    }

    pub(crate) fn note_parsed(&mut self, file: &str, bytes: u64) -> Result<()> {
        let parse = self.stage(StageId::Parse);
        parse.set_current(file);
        parse.record(1, bytes);
        // Non-TTY: emit a dense completed line when a source finishes parse.
        if let Ok(mut painter) = self.painter.lock()
            && !painter.tty
        {
            let line = format!(
                "{}; {}; {}; {}",
                painter.line_for(StageId::Discover),
                painter.line_for(StageId::Fetch),
                painter.line_for(StageId::Parse),
                painter.line_for(StageId::Commit),
            );
            painter.log_lines.push(line);
        }
        Ok(())
    }

    pub(crate) fn note_deleted(&mut self, deleted: u64, total: u64, path: &str) -> Result<()> {
        if let Ok(mut painter) = self.painter.lock() {
            painter.delete_mode = true;
        }
        let delete = self.stage(StageId::Delete);
        delete.set_total_items(total);
        if let Ok(mut state) = delete.state.lock() {
            state.ok = deleted;
            state.current = path.to_owned();
        }
        if deleted == total {
            delete.repaint()?;
            if let Ok(mut painter) = self.painter.lock() {
                if !painter.tty {
                    let line = painter.line_for(StageId::Delete);
                    painter.log_lines.push(line);
                }
                painter.delete_mode = false;
            }
            return Ok(());
        }
        let throttle = self
            .painter
            .lock()
            .map(|p| p.should_throttle())
            .unwrap_or(false);
        if throttle && deleted > 1 && !deleted.is_multiple_of(64) {
            return Ok(());
        }
        delete.repaint()
    }

    #[allow(dead_code)]
    pub(crate) fn note_committed(&self, committed: u64, batch_bytes: u64) -> Result<()> {
        self.stage(StageId::Commit)
            .note_committed(committed, batch_bytes);
        Ok(())
    }

    pub(crate) fn finish(&mut self) -> Result<()> {
        if let Ok(mut painter) = self.painter.lock() {
            painter.activity_override = None;
            painter.finish_tty()?;
        }
        Ok(())
    }

    pub(crate) fn notice(&mut self, message: &str) -> Result<()> {
        self.finish()?;
        if let Ok(painter) = self.painter.lock() {
            if painter.tty {
                let mut err = std::io::stderr();
                writeln!(err, "{message}").context("write import notice")?;
                err.flush().context("flush import notice")?;
            } else {
                drop(painter);
                if let Ok(mut painter) = self.painter.lock() {
                    painter.log_lines.push(message.to_owned());
                }
            }
        }
        Ok(())
    }

    pub(crate) fn flush_log(self, out: &mut dyn Write) -> Result<()> {
        if let Ok(painter) = self.painter.lock() {
            for line in &painter.log_lines {
                writeln!(out, "{line}").context("flush import progress log")?;
            }
        }
        Ok(())
    }
}

pub(crate) fn format_byte_count(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    let value = bytes as f64;
    if value >= GIB {
        format!("{:.1}GiB", value / GIB)
    } else if value >= MIB {
        format!("{:.1}MiB", value / MIB)
    } else if value >= KIB {
        format!("{:.1}KiB", value / KIB)
    } else {
        format!("{bytes}B")
    }
}

pub(crate) fn truncate_middle(value: &str, max_chars: usize) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= max_chars {
        return value.to_owned();
    }
    if max_chars <= 3 {
        return chars.into_iter().take(max_chars).collect();
    }
    let head = (max_chars - 1) / 2;
    let tail = max_chars - 1 - head;
    let mut out: String = chars.iter().take(head).collect();
    out.push('…');
    out.extend(chars.iter().skip(chars.len() - tail));
    out
}

fn stage_bracket(
    id: StageId,
    waiting_upstream: bool,
    pending_downstream: bool,
    status: &str,
) -> String {
    let status = status.replace(',', " ").trim().to_owned();
    let verb = if waiting_upstream {
        "waiting".to_owned()
    } else if pending_downstream {
        // Prefer an explicit `pending→…` token already in status.
        if status
            .split_whitespace()
            .any(|part| part.starts_with("pending→"))
        {
            String::new()
        } else {
            "pending".to_owned()
        }
    } else {
        id.verb().to_owned()
    };

    match (verb.is_empty(), status.is_empty()) {
        (true, true) => id.verb().into(),
        (true, false) => status,
        (false, true) => verb,
        (false, false) => format!("{verb} {status}"),
    }
}

fn enriched_status_label(id: StageId, state: &StageState) -> String {
    let mut parts = Vec::new();
    if let Some(flow) = &state.flow {
        parts.push(flow.clone());
    }
    if matches!(id, StageId::Fetch | StageId::Commit) {
        let snap = persisting_pchronicle::storage::object_store_gate_snapshot();
        // Prefer the live tick's remaining wait when present so `cd=` moves
        // even if the paint lands between gate sleeps.
        let mut snap = snap;
        if let Some(wait_ms) = state.aimd_wait_ms {
            snap.cooldown_remaining_ms = wait_ms;
        }
        parts.push(
            persisting_pchronicle::storage::format_object_store_aimd_flow_label(
                &snap,
                state.aimd_event.as_deref(),
            ),
        );
    }
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_byte_count_uses_binary_units() {
        assert_eq!(format_byte_count(512), "512B");
        assert_eq!(format_byte_count(1536), "1.5KiB");
        assert_eq!(format_byte_count(2 * 1024 * 1024), "2.0MiB");
    }

    #[test]
    fn stage_line_matches_pipeline_shape() {
        let mut state = StageState {
            ok: 12,
            skipped: 2,
            empty: 1,
            error: 0,
            bytes: 1536,
            inbound: 0,
            queue_depth: 0,
            queue_cap: None,
            total_items: None,
            current: "a/long.json".into(),
            last_error: None,
            flow_waiters: 0,
            upstream_waiters: 0,
            flow: None,
            aimd_event: None,
            aimd_wait_ms: None,
        };
        let line = state.format_line(StageId::Discover, "-", "");
        assert!(
            line.starts_with("listing\tok=12 skipped=2 empty=1 error=0\tqueue=-\t1.5KiB\t"),
            "{line}"
        );
        assert!(!line.contains("flow="), "{line}");
        assert!(line.contains("[listing]"), "{line}");
        assert!(line.contains("a/long.json"), "{line}");

        state.error = 3;
        state.flow = Some("pending→parsing".into());
        state.flow_waiters = 1;
        let wait_line = state.format_line(StageId::Fetch, "4/64", "pending→parsing");
        assert!(wait_line.contains("error=3"), "{wait_line}");
        assert!(wait_line.contains("queue=4/64"), "{wait_line}");
        assert!(!wait_line.contains("flow="), "{wait_line}");
        assert!(wait_line.contains("[pending→parsing]"), "{wait_line}");

        state.flow_waiters = 0;
        state.flow = None;
        state.upstream_waiters = 1;
        let upstream_line = state.format_line(StageId::Fetch, "0/64", "aimd ok s=0/4 p=1/1");
        assert!(
            upstream_line.contains("[waiting aimd ok"),
            "{upstream_line}"
        );
    }

    #[test]
    fn queue_tracks_inbound_channel_depth() {
        let progress = CliProgress::new(false);
        let fetch = progress.stage(StageId::Fetch);
        let parse = progress.stage(StageId::Parse);
        let commit = progress.stage(StageId::Commit);
        fetch.set_queue_cap(64);
        parse.set_queue_cap(8);
        commit.set_queue_cap(4096);
        fetch.queue_push();
        fetch.queue_push();
        parse.queue_push();
        commit.set_queue(128);
        // Concurrent races must never paint above capacity.
        for _ in 0..62 {
            fetch.queue_push();
        }

        let painter = progress.painter.lock().unwrap();
        assert_eq!(painter.queue_for(StageId::Fetch), "64/64");
        assert_eq!(painter.queue_for(StageId::Parse), "1/8");
        assert_eq!(painter.queue_for(StageId::Commit), "128/4096");
    }

    #[test]
    fn bracket_embeds_wait_pending_and_aimd_status() {
        assert_eq!(
            stage_bracket(StageId::Parse, true, false, "aimd ok s=0/4 p=1/1"),
            "waiting aimd ok s=0/4 p=1/1"
        );
        assert_eq!(
            stage_bracket(
                StageId::Fetch,
                false,
                true,
                "pending→parsing aimd ok s=0/4 p=0/1"
            ),
            "pending→parsing aimd ok s=0/4 p=0/1"
        );
        assert_eq!(
            stage_bracket(StageId::Commit, false, false, "aimd ok s=0/4 p=1/1"),
            "committing aimd ok s=0/4 p=1/1"
        );
    }

    #[test]
    fn non_tty_progress_logs_parse_and_commit_lines() {
        let mut progress = CliProgress::new(false);
        progress.set_discovered(2, 300).unwrap();
        progress.note_discovered("a.json", 100).unwrap();
        progress.note_discovered("b.json", 200).unwrap();
        progress.note_fetched("a.json", 100).unwrap();
        progress.note_parsed("a.json", 100).unwrap();
        progress.note_fetched("b.json", 200).unwrap();
        progress.note_parsed("b.json", 200).unwrap();
        progress.note_committed(3, 0).unwrap();
        let mut out = Vec::new();
        progress.flush_log(&mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("listing\t"), "{text}");
        assert!(text.contains("reading\t"), "{text}");
        assert!(text.contains("parsing\t"), "{text}");
        assert!(text.contains("commit\t"), "{text}");
        assert!(!text.contains("status=fetching"), "{text}");
    }
}
