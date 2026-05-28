//! In-process capture proxy for `capture run` (no forked daemon).

use std::path::PathBuf;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result};
use persisting_capture::config::ProxyConfig;
use persisting_capture::proxy::serve_with_shutdown_and_ready;
use persisting_capture::runtime::service::CaptureDaemonState;
use persisting_capture::sink::CaptureSink;
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
                    "capture daemon already running (pid {}) for {}; \
                     run `persisting capture stop {}` first — \
                     `capture run` serves in-process and does not fork a daemon",
                    state.pid,
                    storage.display(),
                    storage.display()
                );
            }
        }

        let listen = config.listen.clone();
        let admin_listen = config.admin_listen.clone();
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

        wait_proxy_ready(&listen, &admin_listen)?;

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

fn wait_proxy_ready(listen: &str, admin_listen: &str) -> Result<()> {
    let admin_url = format!("http://{admin_listen}/admin/status");
    for _ in 0..100 {
        if admin_get_ok(&admin_url) {
            return Ok(());
        }
        if tcp_bound(listen) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    anyhow::bail!("capture proxy did not become ready on http://{listen}");
}

fn admin_get_ok(url: &str) -> bool {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(500))
        .build()
        .ok()
        .and_then(|c| c.get(url).send().ok())
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

fn tcp_bound(listen: &str) -> bool {
    std::net::TcpStream::connect(listen).is_ok()
}
