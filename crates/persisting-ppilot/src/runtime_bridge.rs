//! Long-lived pPilot adapter for pVisor's AgentCtl v1 Control protocol.

use crate::agentctl::AgentCtlClient;
use anyhow::{bail, Context};
use persisting_agentctl::{AgentDirective, AgentState};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
struct BridgeState {
    agent_state: AgentState,
    accepting_work: bool,
    directive: AgentDirective,
    quiesce_deadline_unix_ms: Option<u64>,
    warnings: Vec<String>,
}

impl BridgeState {
    fn new(directive: AgentDirective) -> Self {
        let accepting_work = matches!(&directive, AgentDirective::Continue);
        let quiesce_deadline_unix_ms = match &directive {
            AgentDirective::Quiesce {
                deadline_unix_ms, ..
            } => *deadline_unix_ms,
            AgentDirective::Continue | AgentDirective::Shutdown { .. } => None,
        };
        Self {
            agent_state: AgentState::Active,
            accepting_work,
            directive,
            quiesce_deadline_unix_ms,
            warnings: Vec::new(),
        }
    }
}

struct BridgeInner {
    sync: Mutex<()>,
    client: Mutex<AgentCtlClient>,
    state: Mutex<BridgeState>,
    cancellation: CancellationToken,
    changed: Notify,
}

/// One Run-scoped pPilot client that continuously exchanges state and directives.
pub struct PilotRuntimeBridge {
    inner: Arc<BridgeInner>,
    stop: CancellationToken,
    sync_task: Option<JoinHandle<()>>,
}

impl PilotRuntimeBridge {
    /// Connect the client and start periodic state synchronization.
    pub fn start(
        mut client: AgentCtlClient,
        cancellation: CancellationToken,
    ) -> anyhow::Result<Self> {
        let directive = client.connect().context("connect pPilot AgentCtl")?;
        if let AgentDirective::Shutdown { reason } = &directive {
            bail!(
                "pVisor requested shutdown during AgentCtl handshake{}",
                reason
                    .as_deref()
                    .map(|reason| format!(": {reason}"))
                    .unwrap_or_default()
            );
        }
        let interval = Duration::from_millis(client.sync_interval_ms().unwrap_or(1_000).max(20));
        let inner = Arc::new(BridgeInner {
            sync: Mutex::new(()),
            client: Mutex::new(client),
            state: Mutex::new(BridgeState::new(directive)),
            cancellation,
            changed: Notify::new(),
        });
        sync_once(&inner)?;
        let stop = CancellationToken::new();
        let loop_stop = stop.clone();
        let loop_inner = Arc::clone(&inner);
        let sync_task = tokio::spawn(async move {
            let mut ticker =
                tokio::time::interval_at(tokio::time::Instant::now() + interval, interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    _ = loop_stop.cancelled() => break,
                    _ = ticker.tick() => {
                        if let Err(error) = sync_once(&loop_inner) {
                            push_warning(&loop_inner, format!("AgentCtl sync failed: {error:#}"));
                        }
                    }
                }
            }
        });

        Ok(Self {
            inner,
            stop,
            sync_task: Some(sync_task),
        })
    }

    /// Mark the client active if pVisor currently permits work.
    pub fn set_active(&self) -> anyhow::Result<()> {
        let mut state = lock(&self.inner.state);
        if !state.accepting_work {
            bail!("pVisor is not accepting Agent work");
        }
        state.agent_state = AgentState::Active;
        self.inner.changed.notify_waiters();
        Ok(())
    }

    /// Mark the client idle, or quiesced when a checkpoint is already pending.
    pub fn set_idle(&self) {
        let mut state = lock(&self.inner.state);
        state.agent_state = match &state.directive {
            AgentDirective::Quiesce { checkpoint_id, .. } => AgentState::Quiesced {
                checkpoint_id: checkpoint_id.clone(),
            },
            AgentDirective::Continue | AgentDirective::Shutdown { .. } => AgentState::Idle,
        };
        self.inner.changed.notify_waiters();
    }

    /// Return the most recently observed pVisor directive.
    pub fn directive(&self) -> AgentDirective {
        lock(&self.inner.state).directive.clone()
    }

    /// Enter an idle safe point and wait for a pending checkpoint to release it.
    pub async fn finish(mut self) -> Vec<String> {
        self.set_idle();
        if let Err(error) = sync_once(&self.inner) {
            push_warning(&self.inner, format!("final AgentCtl sync failed: {error:#}"));
        }

        loop {
            let wait_until = {
                let state = lock(&self.inner.state);
                if !matches!(state.agent_state, AgentState::Quiesced { .. }) {
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
            if let Err(error) = sync_once(&self.inner) {
                push_warning(&self.inner, format!("AgentCtl sync failed: {error:#}"));
            }
        }

        self.stop.cancel();
        if let Some(sync_task) = self.sync_task.take() {
            let _ = sync_task.await;
        }
        lock(&self.inner.state).warnings.clone()
    }

    /// Return the bridge's small diagnostic state.
    pub fn snapshot(&self) -> BTreeMap<String, serde_json::Value> {
        let state = lock(&self.inner.state);
        BTreeMap::from([
            ("state".into(), serde_json::json!(state.agent_state)),
            ("directive".into(), serde_json::json!(state.directive)),
        ])
    }
}

impl Drop for PilotRuntimeBridge {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

fn sync_once(inner: &Arc<BridgeInner>) -> anyhow::Result<()> {
    // The ticker and lifecycle calls may request synchronization concurrently.
    // Serialize the complete snapshot/exchange/apply sequence so an older
    // response can never overwrite a newer directive.
    let _sync = lock(&inner.sync);
    let agent_state = lock(&inner.state).agent_state.clone();
    let directive = lock(&inner.client).sync(agent_state)?;
    let immediate_sync = {
        let mut state = lock(&inner.state);
        apply_directive(&mut state, directive)
    };
    if matches!(lock(&inner.state).directive, AgentDirective::Shutdown { .. }) {
        inner.cancellation.cancel();
    }

    if immediate_sync {
        let agent_state = lock(&inner.state).agent_state.clone();
        let directive = lock(&inner.client).sync(agent_state)?;
        let mut state = lock(&inner.state);
        apply_directive(&mut state, directive);
        if matches!(state.directive, AgentDirective::Shutdown { .. }) {
            inner.cancellation.cancel();
        }
    }
    inner.changed.notify_waiters();
    Ok(())
}

/// Apply a directive and return whether the new quiesced state needs immediate Sync.
fn apply_directive(state: &mut BridgeState, directive: AgentDirective) -> bool {
    let immediate_sync = match &directive {
        AgentDirective::Continue => {
            state.accepting_work = true;
            state.quiesce_deadline_unix_ms = None;
            if matches!(state.agent_state, AgentState::Quiesced { .. }) {
                state.agent_state = AgentState::Idle;
            }
            false
        }
        AgentDirective::Shutdown { .. } => {
            state.accepting_work = false;
            false
        }
        AgentDirective::Quiesce {
            checkpoint_id,
            deadline_unix_ms,
        } => {
            state.accepting_work = false;
            state.quiesce_deadline_unix_ms = *deadline_unix_ms;
            let reached_safe_point = matches!(state.agent_state, AgentState::Idle)
                || matches!(
                    &state.agent_state,
                    AgentState::Quiesced {
                        checkpoint_id: current
                    } if current != checkpoint_id
                );
            if reached_safe_point {
                state.agent_state = AgentState::Quiesced {
                    checkpoint_id: checkpoint_id.clone(),
                };
                true
            } else {
                false
            }
        }
    };
    state.directive = directive;
    immediate_sync
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_client_becomes_quiesced_for_the_requested_checkpoint() {
        let mut state = BridgeState::new(AgentDirective::Continue);
        state.agent_state = AgentState::Idle;

        assert!(apply_directive(
            &mut state,
            AgentDirective::Quiesce {
                checkpoint_id: "cp".into(),
                deadline_unix_ms: None,
            },
        ));
        assert_eq!(
            state.agent_state,
            AgentState::Quiesced {
                checkpoint_id: "cp".into()
            }
        );
        assert!(!state.accepting_work);
    }

    #[test]
    fn active_client_drains_before_reporting_quiesced() {
        let mut state = BridgeState::new(AgentDirective::Continue);

        assert!(!apply_directive(
            &mut state,
            AgentDirective::Quiesce {
                checkpoint_id: "cp".into(),
                deadline_unix_ms: Some(10),
            },
        ));
        assert_eq!(state.agent_state, AgentState::Active);
        assert!(!state.accepting_work);
    }

    #[test]
    fn continue_releases_a_quiesced_client_to_idle() {
        let mut state = BridgeState::new(AgentDirective::Quiesce {
            checkpoint_id: "cp".into(),
            deadline_unix_ms: None,
        });
        state.agent_state = AgentState::Quiesced {
            checkpoint_id: "cp".into(),
        };

        assert!(!apply_directive(&mut state, AgentDirective::Continue));
        assert_eq!(state.agent_state, AgentState::Idle);
        assert!(state.accepting_work);
    }

    #[test]
    fn quiesced_client_rebinds_to_a_back_to_back_checkpoint() {
        let mut state = BridgeState::new(AgentDirective::Quiesce {
            checkpoint_id: "cp-1".into(),
            deadline_unix_ms: None,
        });
        state.agent_state = AgentState::Quiesced {
            checkpoint_id: "cp-1".into(),
        };

        assert!(apply_directive(
            &mut state,
            AgentDirective::Quiesce {
                checkpoint_id: "cp-2".into(),
                deadline_unix_ms: None,
            },
        ));
        assert_eq!(
            state.agent_state,
            AgentState::Quiesced {
                checkpoint_id: "cp-2".into()
            }
        );
    }

    #[test]
    fn shutdown_disables_work_admission() {
        let mut state = BridgeState::new(AgentDirective::Continue);

        assert!(!apply_directive(
            &mut state,
            AgentDirective::Shutdown {
                reason: Some("done".into()),
            },
        ));
        assert!(!state.accepting_work);
    }
}
