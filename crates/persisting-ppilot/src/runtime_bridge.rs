//! Long-lived pPilot adapter for pVisor's semantic Agent ABI.

use crate::agent_abi::AgentCtlClient;
use anyhow::{bail, Context};
use persisting_agentctl::{
    AgentCheckpointQuiesced, AgentDirective, AgentEffectBegin, AgentEffectComplete,
    AgentEffectOutcome, AgentLifecycleState, AgentProcessRegistration,
};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
struct BridgeState {
    lifecycle: AgentLifecycleState,
    accepting_effects: bool,
    directive: AgentDirective,
    directive_seq: u64,
    quiesced_checkpoint_id: Option<String>,
    quiesce_deadline_unix_ms: Option<u64>,
    open_effects: BTreeSet<String>,
    warnings: Vec<String>,
}

impl BridgeState {
    fn new(directive: AgentDirective, directive_seq: u64) -> Self {
        let accepting_effects = matches!(&directive, AgentDirective::Continue);
        Self {
            lifecycle: AgentLifecycleState::Starting,
            accepting_effects,
            directive,
            directive_seq,
            quiesced_checkpoint_id: None,
            quiesce_deadline_unix_ms: None,
            open_effects: BTreeSet::new(),
            warnings: Vec::new(),
        }
    }
}

struct BridgeInner {
    client: Mutex<AgentCtlClient>,
    state: Mutex<BridgeState>,
    cancellation: CancellationToken,
    changed: Notify,
}

/// One Run-scoped pPilot client that continuously observes pVisor directives.
///
/// The bridge stops admitting new semantic effects as soon as it observes a
/// quiesce directive. It acknowledges the checkpoint only at an idle safe point
/// with an empty local/pVisor effect journal.
pub struct PilotRuntimeBridge {
    inner: Arc<BridgeInner>,
    stop: CancellationToken,
    heartbeat: Option<JoinHandle<()>>,
}

impl PilotRuntimeBridge {
    pub fn start(
        mut client: AgentCtlClient,
        registration: AgentProcessRegistration,
        cancellation: CancellationToken,
    ) -> anyhow::Result<Self> {
        let welcome = client.connect().context("connect pPilot Agent ABI")?;
        if let AgentDirective::Shutdown { reason } = &welcome.directive {
            bail!(
                "pVisor requested shutdown during Agent ABI handshake{}",
                reason
                    .as_deref()
                    .map(|reason| format!(": {reason}"))
                    .unwrap_or_default()
            );
        }
        client
            .register_process(registration)
            .context("register pPilot process")?;

        let inner = Arc::new(BridgeInner {
            client: Mutex::new(client),
            state: Mutex::new(BridgeState::new(welcome.directive, welcome.directive_seq)),
            cancellation,
            changed: Notify::new(),
        });
        let stop = CancellationToken::new();
        let loop_stop = stop.clone();
        let loop_inner = Arc::clone(&inner);
        let interval = Duration::from_millis(welcome.heartbeat_interval_ms.max(20));
        let heartbeat = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    _ = loop_stop.cancelled() => break,
                    _ = ticker.tick() => {
                        if let Err(error) = heartbeat_once(&loop_inner) {
                            push_warning(&loop_inner, format!("Agent ABI heartbeat failed: {error:#}"));
                        }
                    }
                }
            }
        });

        let bridge = Self {
            inner,
            stop,
            heartbeat: Some(heartbeat),
        };
        bridge.set_lifecycle(AgentLifecycleState::Running);
        heartbeat_once(&bridge.inner)?;
        Ok(bridge)
    }

    pub fn set_lifecycle(&self, lifecycle: AgentLifecycleState) {
        lock(&self.inner.state).lifecycle = lifecycle;
        self.inner.changed.notify_waiters();
    }

    pub fn begin_effect(
        &self,
        effect_id: impl Into<String>,
        kind: impl Into<String>,
        request_digest: impl Into<String>,
        idempotency_key: Option<String>,
    ) -> anyhow::Result<u64> {
        let effect_id = effect_id.into();
        let mut state = lock(&self.inner.state);
        if !state.accepting_effects {
            bail!("pVisor is quiescing; refusing new effect {effect_id}");
        }
        if state.open_effects.contains(&effect_id) {
            bail!("effect {effect_id} is already open");
        }
        let sequence = lock(&self.inner.client).begin_effect(AgentEffectBegin {
            effect_id: effect_id.clone(),
            kind: kind.into(),
            request_digest: request_digest.into(),
            idempotency_key,
        })?;
        state.open_effects.insert(effect_id);
        Ok(sequence)
    }

    pub fn complete_effect(
        &self,
        effect_id: &str,
        outcome: AgentEffectOutcome,
    ) -> anyhow::Result<()> {
        let mut state = lock(&self.inner.state);
        if !state.open_effects.contains(effect_id) {
            bail!("effect {effect_id} is not open");
        }
        lock(&self.inner.client).complete_effect(AgentEffectComplete {
            effect_id: effect_id.to_owned(),
            outcome,
        })?;
        state.open_effects.remove(effect_id);
        self.inner.changed.notify_waiters();
        Ok(())
    }

    pub fn open_effects(&self) -> BTreeSet<String> {
        lock(&self.inner.state).open_effects.clone()
    }

    pub fn directive(&self) -> AgentDirective {
        lock(&self.inner.state).directive.clone()
    }

    /// Enter an idle safe point, service a pending checkpoint, then stop the
    /// heartbeat only after pVisor publishes Continue (or its deadline passes).
    pub async fn finish(mut self) -> Vec<String> {
        self.set_lifecycle(AgentLifecycleState::Idle);
        if let Err(error) = heartbeat_once(&self.inner) {
            push_warning(
                &self.inner,
                format!("final Agent ABI heartbeat failed: {error:#}"),
            );
        }

        loop {
            let wait_until = {
                let state = lock(&self.inner.state);
                if state.quiesced_checkpoint_id.is_none() {
                    break;
                }
                state.quiesce_deadline_unix_ms
            };
            let notified = self.inner.changed.notified();
            if let Some(deadline) = wait_until {
                let now = unix_now_ms();
                if now >= deadline.saturating_add(1_000) {
                    push_warning(
                        &self.inner,
                        "checkpoint Continue was not observed before its deadline".into(),
                    );
                    break;
                }
                let remaining = Duration::from_millis(deadline.saturating_add(1_000) - now);
                let _ = tokio::time::timeout(remaining, notified).await;
            } else {
                let _ = tokio::time::timeout(Duration::from_secs(5), notified).await;
            }
            if let Err(error) = heartbeat_once(&self.inner) {
                push_warning(
                    &self.inner,
                    format!("Agent ABI heartbeat failed: {error:#}"),
                );
            }
        }

        self.stop.cancel();
        if let Some(heartbeat) = self.heartbeat.take() {
            let _ = heartbeat.await;
        }
        lock(&self.inner.state).warnings.clone()
    }

    pub fn snapshot(&self) -> BTreeMap<String, serde_json::Value> {
        let state = lock(&self.inner.state);
        BTreeMap::from([
            ("lifecycle".into(), serde_json::json!(state.lifecycle)),
            ("directive".into(), serde_json::json!(state.directive)),
            (
                "directive_seq".into(),
                serde_json::json!(state.directive_seq),
            ),
            ("open_effects".into(), serde_json::json!(state.open_effects)),
        ])
    }
}

impl Drop for PilotRuntimeBridge {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

fn heartbeat_once(inner: &Arc<BridgeInner>) -> anyhow::Result<()> {
    let lifecycle = lock(&inner.state).lifecycle;
    let ack = lock(&inner.client).heartbeat(lifecycle)?;

    let checkpoint = {
        let mut state = lock(&inner.state);
        state.directive_seq = ack.directive_seq;
        state.directive = ack.directive.clone();
        match ack.directive {
            AgentDirective::Continue => {
                state.accepting_effects = true;
                state.quiesce_deadline_unix_ms = None;
                if state.quiesced_checkpoint_id.take().is_some() {
                    state.lifecycle = AgentLifecycleState::Idle;
                }
                None
            }
            AgentDirective::Shutdown { .. } => {
                state.accepting_effects = false;
                state.lifecycle = AgentLifecycleState::Stopping;
                inner.cancellation.cancel();
                None
            }
            AgentDirective::Quiesce {
                checkpoint_id,
                deadline_unix_ms,
            } => {
                state.accepting_effects = false;
                state.quiesce_deadline_unix_ms = deadline_unix_ms;
                let at_safe_point = matches!(
                    state.lifecycle,
                    AgentLifecycleState::Idle | AgentLifecycleState::Quiesced
                ) && state.open_effects.is_empty();
                if at_safe_point && state.quiesced_checkpoint_id.as_deref() != Some(&checkpoint_id)
                {
                    Some(AgentCheckpointQuiesced {
                        checkpoint_id,
                        directive_seq: ack.directive_seq,
                    })
                } else {
                    None
                }
            }
        }
    };

    if let Some(checkpoint) = checkpoint {
        lock(&inner.client).checkpoint_quiesced(checkpoint.clone())?;
        let mut state = lock(&inner.state);
        state.lifecycle = AgentLifecycleState::Quiesced;
        state.quiesced_checkpoint_id = Some(checkpoint.checkpoint_id);
    }
    inner.changed.notify_waiters();
    Ok(())
}

fn unix_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

fn push_warning(inner: &Arc<BridgeInner>, warning: String) {
    lock(&inner.state).warnings.push(warning);
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
