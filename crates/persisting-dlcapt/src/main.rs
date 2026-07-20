use anyhow::{Context, Result};
use persisting_dlcapt::config::ProxyConfig;
use std::path::PathBuf;

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

    persisting_dlcapt::serve(config).await
}
