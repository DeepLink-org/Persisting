//! In-place stderr stage progress for import/export.
//!
//! Each stage occupies one line in wget style:
//! `export loading [=======================> ] 709/710`
//! `=` is finished work, `>` is the current head, and spaces are remaining
//! work. Unbounded stages keep the same brackets and walk `- \ | /` across
//! the bar until they settle on a solid `=` fill.

use std::future::Future;
use std::io::Write;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tokio::time::{interval, MissedTickBehavior};

const SPINNER: &[char] = &['-', '\\', '|', '/'];
const BAR_WIDTH: usize = 23;
const PULSE_EVERY: Duration = Duration::from_millis(80);
const SI_SUFFIX: &[&str] = &["", "K", "M", "G", "T"];
const SI_SCALE: &[f64] = &[
    1.0,
    1_000.0,
    1_000_000.0,
    1_000_000_000.0,
    1_000_000_000_000.0,
];

fn si_unit_index(n: usize) -> usize {
    let mut index = 0usize;
    let mut value = n as f64;
    while index + 1 < SI_SUFFIX.len() && value >= 1_000.0 {
        value /= 1_000.0;
        index += 1;
    }
    if index + 1 < SI_SUFFIX.len() && value >= 999.95 {
        index += 1;
    }
    index
}

fn compact_number(n: usize, unit: usize) -> String {
    if unit == 0 {
        return n.to_string();
    }
    let value = n as f64 / SI_SCALE[unit];
    let text = if value >= 10.0 {
        format!("{value:.0}")
    } else {
        format!("{value:.1}")
    };
    let text = text.trim_end_matches('0').trim_end_matches('.');
    format!("{}{}", text, SI_SUFFIX[unit])
}

fn compact_pair(current: usize, total: usize) -> String {
    let unit = si_unit_index(total.max(current));
    format!(
        "{}/{}",
        compact_number(current, unit),
        compact_number(total, unit)
    )
}

pub(crate) struct StageProgress<'a> {
    stderr: &'a mut dyn Write,
    stage: Option<String>,
    current: Option<usize>,
    total: Option<usize>,
    frame: usize,
    painted: usize,
    last_paint: Option<Instant>,
}

impl<'a> StageProgress<'a> {
    pub(crate) fn new(stderr: &'a mut dyn Write) -> Self {
        Self {
            stderr,
            stage: None,
            current: None,
            total: None,
            frame: 0,
            painted: 0,
            last_paint: None,
        }
    }

    pub(crate) fn begin(&mut self, stage: impl Into<String>, total: Option<usize>) -> Result<()> {
        self.finish()?;
        self.stage = Some(stage.into());
        self.current = total.map(|_| 0);
        self.total = total;
        self.frame = 0;
        self.last_paint = None;
        self.paint(false)
    }

    pub(crate) fn set(&mut self, current: usize) -> Result<()> {
        let before = self.filled_cells();
        self.current = Some(current);
        let after = self.filled_cells();
        let last = self.total == Some(current);
        let due = self
            .last_paint
            .is_none_or(|last_paint| last_paint.elapsed() >= PULSE_EVERY);
        if last || due || current == 1 || after != before {
            self.paint(false)?;
        }
        Ok(())
    }

    pub(crate) fn pulse(&mut self) -> Result<()> {
        self.paint(false)
    }

    pub(crate) fn finish(&mut self) -> Result<()> {
        if self.stage.is_none() {
            return Ok(());
        }
        if let (Some(total), current) = (self.total, self.current) {
            self.current = Some(current.unwrap_or(total).max(total));
        }
        self.paint(true)?;
        self.stage = None;
        self.current = None;
        self.total = None;
        self.painted = 0;
        self.last_paint = None;
        Ok(())
    }

    pub(crate) async fn spin_while<F, T>(&mut self, fut: F) -> Result<T>
    where
        F: Future<Output = Result<T>>,
    {
        let mut fut = Box::pin(fut);
        let mut ticks = interval(PULSE_EVERY);
        ticks.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                biased;
                result = &mut fut => return result,
                _ = ticks.tick() => self.pulse()?,
            }
        }
    }

    pub(crate) async fn spin_blocking<T: Send + 'static>(
        &mut self,
        work: impl FnOnce() -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let handle = tokio::task::spawn_blocking(work);
        self.spin_while(async move { handle.await.map_err(anyhow::Error::from)? })
            .await
    }

    fn filled_cells(&self) -> usize {
        let Some(total) = self.total.filter(|total| *total > 0) else {
            return 0;
        };
        let current = self.current.unwrap_or(0).min(total);
        current * BAR_WIDTH / total
    }

    fn bar(&self, done: bool) -> String {
        if done {
            return "=".repeat(BAR_WIDTH);
        }
        if self.total.is_some() {
            let filled = self.filled_cells().min(BAR_WIDTH.saturating_sub(1));
            format!(
                "{}>{}",
                "=".repeat(filled),
                " ".repeat(BAR_WIDTH - filled - 1)
            )
        } else {
            let spin = SPINNER[self.frame % SPINNER.len()];
            let pos = self.frame % BAR_WIDTH;
            let mut cells = vec![' '; BAR_WIDTH];
            cells[pos] = spin;
            cells.into_iter().collect()
        }
    }

    fn paint(&mut self, done: bool) -> Result<()> {
        let Some(stage) = self.stage.clone() else {
            return Ok(());
        };
        let bar = self.bar(done);
        if !done {
            self.frame += 1;
        }
        let detail = match (self.current, self.total) {
            (Some(current), Some(total)) => format!(" {}", compact_pair(current, total)),
            (Some(current), None) => {
                format!(" {}", compact_number(current, si_unit_index(current)))
            }
            _ => String::new(),
        };
        let line = format!("{stage} [{bar}]{detail}");
        let width = line.chars().count();
        let pad = self.painted.saturating_sub(width);
        write!(self.stderr, "\r{line}{}", " ".repeat(pad))
            .context("write pChronicle stage progress")?;
        if done {
            write!(self.stderr, "\r{line}\n").context("finish pChronicle stage progress")?;
        }
        self.stderr
            .flush()
            .context("flush pChronicle stage progress")?;
        self.painted = if done { 0 } else { width };
        self.last_paint = Some(Instant::now());
        Ok(())
    }
}

impl Drop for StageProgress<'_> {
    fn drop(&mut self) {
        if self.stage.is_some() {
            let _ = writeln!(self.stderr);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn in_progress_bar(current: usize, total: usize) -> String {
        let filled = (current * BAR_WIDTH / total).min(BAR_WIDTH.saturating_sub(1));
        format!(
            "{}>{}",
            "=".repeat(filled),
            " ".repeat(BAR_WIDTH - filled - 1)
        )
    }

    #[test]
    fn stage_progress_spins_then_settles_on_equals() {
        let mut stderr = Vec::new();
        {
            let mut progress = StageProgress::new(&mut stderr);
            progress.begin("export encoding", None).unwrap();
            progress.pulse().unwrap();
            progress.finish().unwrap();
        }
        let text = String::from_utf8(stderr).unwrap();
        assert!(text.contains('\r'), "{text:?}");
        assert!(
            text.contains("export encoding [") && text.contains(']'),
            "{text:?}"
        );
        assert!(
            text.contains('-') || text.contains('\\') || text.contains('|') || text.contains('/'),
            "{text:?}"
        );
        assert_eq!(
            last_frame(&text),
            format!("export encoding [{}]", "=".repeat(BAR_WIDTH))
        );
    }

    #[test]
    fn stage_progress_uses_fixed_width_arrow_bar() {
        let mut stderr = Vec::new();
        {
            let mut progress = StageProgress::new(&mut stderr);
            progress.begin("export loading", Some(2)).unwrap();
            progress.set(1).unwrap();
            progress.set(2).unwrap();
            progress.finish().unwrap();
        }
        let text = String::from_utf8(stderr).unwrap();
        assert!(
            text.contains(&format!("export loading [{}] 1/2", in_progress_bar(1, 2))),
            "{text:?}"
        );
        assert_eq!(
            last_frame(&text),
            format!("export loading [{}] 2/2", "=".repeat(BAR_WIDTH))
        );
    }

    #[test]
    fn stage_progress_keeps_fixed_width_for_large_totals() {
        let mut stderr = Vec::new();
        {
            let mut progress = StageProgress::new(&mut stderr);
            progress.begin("export writing", Some(1_000)).unwrap();
            progress.set(500).unwrap();
            progress.finish().unwrap();
        }
        let text = String::from_utf8(stderr).unwrap();
        assert!(
            text.contains(&format!(
                "export writing [{}] {}",
                in_progress_bar(500, 1_000),
                compact_pair(500, 1_000)
            )),
            "{text:?}"
        );
        assert_eq!(
            last_frame(&text),
            format!(
                "export writing [{}] {}",
                "=".repeat(BAR_WIDTH),
                compact_pair(1_000, 1_000)
            )
        );
    }

    #[test]
    fn compact_pair_keeps_small_counts() {
        assert_eq!(compact_pair(1, 2), "1/2");
        assert_eq!(compact_pair(28, 28), "28/28");
        assert_eq!(compact_pair(999, 999), "999/999");
    }

    #[test]
    fn compact_pair_prefers_larger_si_units() {
        assert_eq!(compact_pair(500, 1_000), "0.5K/1K");
        assert_eq!(compact_pair(2_400_000, 2_400_000), "2.4M/2.4M");
        assert_eq!(compact_pair(1_484_415_223, 1_484_415_223), "1.5G/1.5G");
        assert_eq!(compact_pair(999_950, 999_950), "1M/1M");
    }

    fn last_frame(text: &str) -> &str {
        text.rsplit('\r').next().unwrap_or(text).trim_end()
    }
}
