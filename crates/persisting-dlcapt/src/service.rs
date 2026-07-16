use crate::config::ProxyConfig;
use crate::proxy::{AppState, build_admin_router, build_public_router};
use anyhow::{Context, Result};
use std::net::SocketAddr;
use tracing::info;

pub async fn serve(config: ProxyConfig) -> Result<()> {
    let state = AppState::new(config);
    let public_addr: SocketAddr = state
        .listen_addr()
        .parse()
        .context("invalid listen address")?;
    let admin_addr: SocketAddr = state
        .admin_listen_addr()
        .parse()
        .context("invalid admin listen address")?;

    let public_listener = tokio::net::TcpListener::bind(public_addr)
        .await
        .context("failed binding public listener")?;
    let admin_listener = tokio::net::TcpListener::bind(admin_addr)
        .await
        .context("failed binding admin listener")?;

    info!("dlcapt public API listening on {public_addr}");
    info!("dlcapt admin API listening on {admin_addr}");

    tokio::try_join!(
        axum::serve(public_listener, build_public_router(state.clone())),
        axum::serve(admin_listener, build_admin_router(state)),
    )
    .context("proxy server exited with error")?;
    Ok(())
}
