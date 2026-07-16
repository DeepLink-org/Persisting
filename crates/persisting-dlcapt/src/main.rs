use anyhow::{Context, Result};
use persisting_dlcapt::config::ProxyConfig;
use persisting_dlcapt::proxy::{AppState, build_admin_router, build_public_router};
use std::net::SocketAddr;
use std::path::PathBuf;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "dlcapt=info,tower_http=info".into()),
        )
        .init();

    let config_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "config/proxy.toml".to_string());
    let config_path = PathBuf::from(config_path);
    let config = ProxyConfig::load(&config_path)
        .with_context(|| format!("failed loading config from {}", config_path.display()))?;

    let state = AppState::new(config);
    let public_router = build_public_router(state.clone());
    let admin_router = build_admin_router(state.clone());

    let public_addr: SocketAddr = state
        .listen_addr()
        .parse()
        .with_context(|| "invalid listen address".to_string())?;
    let admin_addr: SocketAddr = state
        .admin_listen_addr()
        .parse()
        .with_context(|| "invalid admin_listen address".to_string())?;

    info!("dlcapt public API listening on {public_addr}");
    info!("dlcapt admin API listening on {admin_addr}");

    let public_listener = tokio::net::TcpListener::bind(public_addr)
        .await
        .with_context(|| "failed binding public listener".to_string())?;
    let admin_listener = tokio::net::TcpListener::bind(admin_addr)
        .await
        .with_context(|| "failed binding admin listener".to_string())?;

    let public_server = axum::serve(public_listener, public_router);
    let admin_server = axum::serve(admin_listener, admin_router);

    tokio::try_join!(public_server, admin_server)
        .with_context(|| "proxy server exited with error".to_string())?;
    Ok(())
}
