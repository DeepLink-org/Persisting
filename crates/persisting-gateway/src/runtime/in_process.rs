//! In-process Gateway for one pVisor Attempt (no forked daemon).

use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::config::ProxyConfig;
use crate::runtime::service::CaptureDaemonState;
use crate::sink::CaptureEventSink;
use persisting_control::{ControlController, PolicyControlController};
use persisting_overlaynet::{InterceptionMetrics, InterceptionSnapshot};
use tokio::sync::oneshot;

pub struct InProcessCapture {
    shutdown_tx: Option<oneshot::Sender<()>>,
    join: Option<JoinHandle<Result<()>>>,
    pub listen: String,
    interception_metrics: InterceptionMetrics,
}

impl InProcessCapture {
    pub fn start(
        config: ProxyConfig,
        storage: PathBuf,
        sink: Arc<dyn CaptureEventSink>,
        stream_markdown: bool,
    ) -> Result<Self> {
        Self::start_with_control(
            config,
            storage,
            sink,
            stream_markdown,
            Arc::new(PolicyControlController),
        )
    }

    pub fn start_with_control(
        config: ProxyConfig,
        storage: PathBuf,
        sink: Arc<dyn CaptureEventSink>,
        stream_markdown: bool,
        controller: Arc<dyn ControlController>,
    ) -> Result<Self> {
        if let Some(state) = CaptureDaemonState::read(&storage)? {
            if state.is_running() {
                anyhow::bail!(
                    "Gateway already running (pid {}) for {}; \
                     stop it with `persisting gateway stop -o {}` first — in-process capture does not fork a daemon",
                    state.pid,
                    storage.display(),
                    storage.display(),
                );
            }
        }

        let listen = config.listen.clone();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let interception_metrics = InterceptionMetrics::default();
        let thread_metrics = interception_metrics.clone();

        let join = std::thread::Builder::new()
            .name("persisting-gateway".into())
            .spawn(move || {
                let rt = tokio::runtime::Runtime::new().context("tokio runtime")?;
                rt.block_on(crate::gateway::serve_with_runtime_control_and_metrics(
                    config,
                    storage,
                    sink,
                    stream_markdown,
                    crate::gateway::GatewayRuntimeControl {
                        controller,
                        interception_metrics: thread_metrics,
                    },
                    None,
                    async {
                        let _ = shutdown_rx.await;
                    },
                ))
            })
            .context("spawn in-process capture")?;

        wait_proxy_ready(&listen)?;

        Ok(Self {
            shutdown_tx: Some(shutdown_tx),
            join: Some(join),
            listen,
            interception_metrics,
        })
    }

    pub fn interception_snapshot(&self) -> InterceptionSnapshot {
        self.interception_metrics.snapshot()
    }

    pub fn shutdown(mut self) -> Result<()> {
        self.shutdown_inner()
    }

    fn shutdown_inner(&mut self) -> Result<()> {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        let Some(join) = self.join.take() else {
            return Ok(());
        };
        join.join()
            .map_err(|_| anyhow::anyhow!("in-process capture thread panicked"))??;
        Ok(())
    }
}

impl Drop for InProcessCapture {
    fn drop(&mut self) {
        if let Err(error) = self.shutdown_inner() {
            tracing::warn!(%error, "failed to stop in-process Gateway during drop");
        }
    }
}

fn wait_proxy_ready(listen: &str) -> Result<()> {
    for _ in 0..100 {
        if tcp_bound(listen) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    anyhow::bail!("capture proxy did not become ready on http://{listen}");
}

fn tcp_bound(listen: &str) -> bool {
    let addr = listen
        .strip_prefix("http://")
        .or_else(|| listen.strip_prefix("https://"))
        .unwrap_or(listen);
    TcpStream::connect(addr).is_ok()
}
