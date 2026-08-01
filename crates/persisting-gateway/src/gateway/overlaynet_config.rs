//! Adapt capture configuration to overlaynet's egress policy.

pub use persisting_overlaynet::policy::*;

use crate::config::ProxyConfig;

impl PolicyConfig for ProxyConfig {
    fn listen(&self) -> &str {
        &self.listen
    }

    fn network(&self) -> &crate::config::NetworkConfig {
        &self.network
    }

    fn trusted_upstream_hosts(&self) -> Vec<String> {
        upstream_hosts_from_models(self)
    }
}

fn upstream_hosts_from_models(config: &ProxyConfig) -> Vec<String> {
    let mut hosts = Vec::new();
    for route in &config.models {
        for url in [&route.upstream, &route.upstream_anthropic]
            .into_iter()
            .flatten()
        {
            if let Ok(parsed) = url::Url::parse(url) {
                if let Some(host) = parsed.host_str() {
                    let normalized = normalize_host(host);
                    if !normalized.is_empty() && !hosts.iter().any(|item| item == &normalized) {
                        hosts.push(normalized);
                    }
                }
            }
        }
    }
    hosts
}
