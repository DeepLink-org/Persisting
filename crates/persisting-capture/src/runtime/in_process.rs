//! In-process capture proxy for one Attempt / `traj capture` (no forked daemon).

use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::config::ProxyConfig;
use crate::proxy::serve_with_shutdown_and_ready;
use crate::runtime::service::CaptureDaemonState;
use crate::sink::CaptureSink;
use tokio::sync::oneshot;

pub struct InProcessCapture {
    shutdown_tx: Option<oneshot::Sender<()>>,
    join: JoinHandle<Result<()>>,
    pub listen: String,
}

impl InProcessCapture {
    pub fn start(
        config: ProxyConfig,
        storage: PathBuf,
        sink: Arc<dyn CaptureSink>,
        stream_markdown: bool,
    ) -> Result<Self> {
        if let Some(state) = CaptureDaemonState::read(&storage)? {
            if state.is_running() {
                anyhow::bail!(
                    "traj proxy already running (pid {}) for {}; \
                     stop it first — in-process capture does not fork a daemon",
                    state.pid,
                    storage.display(),
                );
            }
        }

        let listen = config.listen.clone();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();

        let join = std::thread::Builder::new()
            .name("persisting-capture".into())
            .spawn(move || {
                let rt = tokio::runtime::Runtime::new().context("tokio runtime")?;
                rt.block_on(serve_with_shutdown_and_ready(
                    config,
                    storage,
                    sink,
                    stream_markdown,
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
            join,
            listen,
        })
    }

    pub fn shutdown(mut self) -> Result<()> {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        self.join
            .join()
            .map_err(|_| anyhow::anyhow!("in-process capture thread panicked"))??;
        Ok(())
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
