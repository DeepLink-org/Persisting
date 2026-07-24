//! Harbor-aligned egress policy for forward-proxy traffic.
//!
//! Modes: `public` | `no-network` | `allowlist`.
//! Matching: exact hostname, leading `*.suffix`, IPv4/IPv6 literal, CIDR.
//! Entries are not URLs, ports, or paths.

use std::net::IpAddr;
use std::str::FromStr;

use axum::http::StatusCode;
use ipnet::IpNet;

use crate::config::{NetworkConfig, NetworkMode, ProxyConfig};

/// Compiled policy used at request time.
#[derive(Debug, Clone)]
pub struct NetworkPolicy {
    pub mode: NetworkMode,
    /// Normalized allowlist entries (config + model upstream hosts when allowlist).
    pub allowed: Vec<AllowedEntry>,
    /// Host part of `listen` (always bypassed with other loopbacks).
    pub listen_host: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AllowedEntry {
    Exact(String),
    WildcardSuffix(String),
    Ip(IpAddr),
    Cidr(IpNet),
}

impl NetworkPolicy {
    pub fn from_config(cfg: &ProxyConfig) -> anyhow::Result<Self> {
        let listen_host = host_from_listen(&cfg.listen);
        let mut raw = cfg.network.allowed_hosts.clone();
        if cfg.network.mode == NetworkMode::Allowlist {
            for host in upstream_hosts_from_models(cfg) {
                if !raw.iter().any(|h| normalize_host(h) == host) {
                    raw.push(host);
                }
            }
        }
        let mut allowed = Vec::with_capacity(raw.len());
        for entry in &raw {
            allowed.push(parse_allowed_entry(entry)?);
        }
        Ok(Self {
            mode: cfg.network.mode,
            allowed,
            listen_host,
        })
    }

    pub fn mode_str(&self) -> &'static str {
        match self.mode {
            NetworkMode::Public => "public",
            NetworkMode::NoNetwork => "no-network",
            NetworkMode::Allowlist => "allowlist",
        }
    }
}

pub fn validate_network_config(network: &NetworkConfig) -> anyhow::Result<()> {
    for entry in &network.allowed_hosts {
        parse_allowed_entry(entry)?;
    }
    Ok(())
}

/// Normalize host for comparison: trim, lowercase, strip trailing dot.
pub fn normalize_host(host: &str) -> String {
    host.trim()
        .trim_matches(|c| c == '[' || c == ']')
        .to_ascii_lowercase()
        .trim_end_matches('.')
        .to_string()
}

/// Parse `listen` (`127.0.0.1:19081` or `http://127.0.0.1:19081`) into host.
pub fn host_from_listen(listen: &str) -> String {
    let s = listen.trim();
    let without_scheme = s
        .strip_prefix("http://")
        .or_else(|| s.strip_prefix("https://"))
        .unwrap_or(s);
    host_from_authority(without_scheme.trim_end_matches('/'))
}

/// Extract host from `host:port`, `[ipv6]:port`, or bare host.
pub fn host_from_authority(authority: &str) -> String {
    let authority = authority.trim();
    if let Some(rest) = authority.strip_prefix('[') {
        if let Some(end) = rest.find(']') {
            return normalize_host(&rest[..end]);
        }
    }
    if let Some((host, port)) = authority.rsplit_once(':') {
        if !host.is_empty() && port.chars().all(|c| c.is_ascii_digit()) {
            return normalize_host(host);
        }
    }
    normalize_host(authority)
}

pub fn parse_allowed_entry(raw: &str) -> anyhow::Result<AllowedEntry> {
    let entry = raw.trim();
    if entry.is_empty() {
        anyhow::bail!("allowed_hosts entry must not be empty");
    }
    if entry.contains("://") || entry.contains(']') || entry.contains('[') {
        anyhow::bail!(
            "allowed_hosts entry `{entry}` must be a hostname, `*.suffix`, IP, or CIDR \
             (not a URL or bracketed IPv6)"
        );
    }
    // Path-like (but allow CIDR `a.b.c.d/nn`).
    if entry.contains('/') && IpNet::from_str(entry).is_err() {
        anyhow::bail!(
            "allowed_hosts entry `{entry}` must be a hostname, `*.suffix`, IP, or CIDR \
             (not a URL path)"
        );
    }
    // Port in entry is forbidden (Harbor semantics).
    if let Some((host_part, maybe_port)) = entry.rsplit_once(':') {
        if !host_part.is_empty()
            && maybe_port.chars().all(|c| c.is_ascii_digit())
            && !entry.contains('/')
            && host_part.parse::<IpAddr>().is_err()
            && !host_part.contains(':')
        {
            // hostname:port — reject
            anyhow::bail!(
                "allowed_hosts entry `{entry}` must not include a port (got hostname:port)"
            );
        }
    }

    if let Some(suffix) = entry.strip_prefix("*.") {
        let suffix = normalize_host(suffix);
        if suffix.is_empty() || suffix.contains('*') {
            anyhow::bail!("invalid wildcard allowed_hosts entry `{entry}`");
        }
        if suffix.parse::<IpAddr>().is_ok() {
            anyhow::bail!("wildcard allowed_hosts cannot wrap an IP (`{entry}`)");
        }
        return Ok(AllowedEntry::WildcardSuffix(suffix));
    }

    if entry.contains('*') {
        anyhow::bail!(
            "allowed_hosts entry `{entry}`: only leading `*.suffix` wildcards are supported"
        );
    }

    if let Ok(cidr) = IpNet::from_str(entry) {
        // Prefer CIDR form when a prefix length is present.
        if entry.contains('/') {
            return Ok(AllowedEntry::Cidr(cidr));
        }
        // `IpNet::from_str("1.1.1.1")` succeeds as /32 — treat as literal IP.
        return Ok(AllowedEntry::Ip(cidr.addr()));
    }
    if let Ok(ip) = IpAddr::from_str(entry) {
        return Ok(AllowedEntry::Ip(ip));
    }

    let host = normalize_host(entry);
    if host.is_empty() || host.contains(':') {
        anyhow::bail!("invalid allowed_hosts hostname `{entry}`");
    }
    Ok(AllowedEntry::Exact(host))
}

pub fn host_matches(host: &str, allowed: &[AllowedEntry]) -> bool {
    let host = normalize_host(host);
    if host.is_empty() {
        return false;
    }
    let host_ip = IpAddr::from_str(&host).ok();
    for entry in allowed {
        match entry {
            AllowedEntry::Exact(h) => {
                if host == *h {
                    return true;
                }
            }
            AllowedEntry::WildcardSuffix(suffix) => {
                // `*.example.com` matches subdomains only, not apex `example.com`.
                if host.ends_with(suffix)
                    && host.len() > suffix.len()
                    && host.as_bytes()[host.len() - suffix.len() - 1] == b'.'
                {
                    return true;
                }
            }
            AllowedEntry::Ip(ip) => {
                if host_ip == Some(*ip) {
                    return true;
                }
            }
            AllowedEntry::Cidr(net) => {
                if let Some(ip) = host_ip {
                    if net.contains(&ip) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

pub fn is_loopback_host(host: &str, listen_host: &str) -> bool {
    let h = normalize_host(host);
    if h == "localhost" || h == "127.0.0.1" || h == "::1" || h == "0:0:0:0:0:0:0:1" {
        return true;
    }
    if !listen_host.is_empty() && h == normalize_host(listen_host) {
        return true;
    }
    if let Ok(ip) = IpAddr::from_str(&h) {
        return ip.is_loopback();
    }
    false
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DenyReason {
    NoNetwork,
    AllowlistEmpty,
    NotInAllowlist,
}

impl DenyReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NoNetwork => "no-network",
            Self::AllowlistEmpty => "allowlist-empty",
            Self::NotInAllowlist => "not-in-allowlist",
        }
    }
}

/// Check whether forward-proxy egress to `host` is allowed.
pub fn assert_egress_allowed(policy: &NetworkPolicy, host: &str) -> Result<(), DenyReason> {
    if is_loopback_host(host, &policy.listen_host) {
        return Ok(());
    }
    match policy.mode {
        NetworkMode::Public => Ok(()),
        NetworkMode::NoNetwork => Err(DenyReason::NoNetwork),
        NetworkMode::Allowlist => {
            if policy.allowed.is_empty() {
                return Err(DenyReason::AllowlistEmpty);
            }
            if host_matches(host, &policy.allowed) {
                Ok(())
            } else {
                Err(DenyReason::NotInAllowlist)
            }
        }
    }
}

pub fn forbidden_response(host: &str, reason: &DenyReason) -> (StatusCode, String) {
    (
        StatusCode::FORBIDDEN,
        format!(
            "persisting-proxy: egress to `{host}` denied ({})",
            reason.as_str()
        ),
    )
}

fn upstream_hosts_from_models(cfg: &ProxyConfig) -> Vec<String> {
    let mut out = Vec::new();
    for route in &cfg.models {
        for url in [&route.upstream, &route.upstream_anthropic]
            .into_iter()
            .flatten()
        {
            if let Ok(parsed) = url::Url::parse(url) {
                if let Some(host) = parsed.host_str() {
                    let n = normalize_host(host);
                    if !n.is_empty() && !out.iter().any(|h| h == &n) {
                        out.push(n);
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{CaptureLevel, ModelRoute, NetworkConfig, NetworkMode, ProxyConfig};

    fn cfg(mode: NetworkMode, allowed: &[&str], upstream: Option<&str>) -> ProxyConfig {
        ProxyConfig {
            listen: "127.0.0.1:19081".into(),
            admin_listen: "127.0.0.1:9876".into(),
            agent_id: "default".into(),
            session_header: "x-persisting-session-id".into(),
            capture_level: CaptureLevel::Dialogue,
            debug: false,
            network: NetworkConfig {
                mode,
                allowed_hosts: allowed.iter().map(|s| (*s).to_string()).collect(),
            },
            models: vec![ModelRoute {
                name: "*".into(),
                provider: None,
                upstream: upstream.map(str::to_string),
                upstream_anthropic: None,
                path_prefix: None,
                api_key_env: None,
                api_key: None,
                forward: None,
            }],
        }
    }

    #[test]
    fn normalize_and_authority_host() {
        assert_eq!(normalize_host(" Example.COM. "), "example.com");
        assert_eq!(host_from_authority("api.openai.com:443"), "api.openai.com");
        assert_eq!(host_from_authority("[::1]:443"), "::1");
        assert_eq!(host_from_listen("http://127.0.0.1:19081"), "127.0.0.1");
    }

    #[test]
    fn exact_and_wildcard_match() {
        let entries = vec![
            parse_allowed_entry("example.com").unwrap(),
            parse_allowed_entry("*.example.com").unwrap(),
        ];
        assert!(host_matches("example.com", &entries));
        assert!(!host_matches("www.example.com", &entries[0..1]));
        assert!(host_matches("www.example.com", &entries));
        assert!(host_matches("a.b.example.com", &entries));
        assert!(!host_matches("example.org", &entries));
        // wildcard does not include apex alone when only wildcard present
        let wild = vec![parse_allowed_entry("*.example.com").unwrap()];
        assert!(!host_matches("example.com", &wild));
    }

    #[test]
    fn ip_and_cidr_match() {
        let entries = vec![
            parse_allowed_entry("1.1.1.1").unwrap(),
            parse_allowed_entry("10.0.0.0/8").unwrap(),
        ];
        assert!(host_matches("1.1.1.1", &entries));
        assert!(host_matches("10.1.2.3", &entries));
        assert!(!host_matches("8.8.8.8", &entries));
    }

    #[test]
    fn rejects_url_port_path_entries() {
        assert!(parse_allowed_entry("https://example.com").is_err());
        assert!(parse_allowed_entry("example.com/path").is_err());
        assert!(parse_allowed_entry("example.com:443").is_err());
        assert!(parse_allowed_entry("[::1]").is_err());
        assert!(parse_allowed_entry("*example.com").is_err());
    }

    #[test]
    fn public_allows_all() {
        let p = NetworkPolicy::from_config(&cfg(NetworkMode::Public, &[], None)).unwrap();
        assert!(assert_egress_allowed(&p, "evil.example").is_ok());
    }

    #[test]
    fn no_network_denies_non_loopback() {
        let p = NetworkPolicy::from_config(&cfg(NetworkMode::NoNetwork, &[], None)).unwrap();
        assert_eq!(
            assert_egress_allowed(&p, "pypi.org"),
            Err(DenyReason::NoNetwork)
        );
        assert!(assert_egress_allowed(&p, "127.0.0.1").is_ok());
        assert!(assert_egress_allowed(&p, "localhost").is_ok());
    }

    #[test]
    fn allowlist_empty_denies() {
        let p = NetworkPolicy::from_config(&cfg(NetworkMode::Allowlist, &[], None)).unwrap();
        // upstream None → empty effective list
        assert_eq!(
            assert_egress_allowed(&p, "pypi.org"),
            Err(DenyReason::AllowlistEmpty)
        );
    }

    #[test]
    fn allowlist_merges_upstream_host() {
        let p = NetworkPolicy::from_config(&cfg(
            NetworkMode::Allowlist,
            &["pypi.org"],
            Some("https://api.openai.com/v1"),
        ))
        .unwrap();
        assert!(assert_egress_allowed(&p, "pypi.org").is_ok());
        assert!(assert_egress_allowed(&p, "api.openai.com").is_ok());
        assert_eq!(
            assert_egress_allowed(&p, "github.com"),
            Err(DenyReason::NotInAllowlist)
        );
    }

    #[test]
    fn toml_network_section_loads() {
        let cfg = ProxyConfig::from_toml_str(
            r#"
listen = "127.0.0.1:1"

[network]
mode = "allowlist"
allowed_hosts = ["pypi.org", "*.googleapis.com"]

[[models]]
name = "*"
upstream = "http://example.com/v1"
"#,
        )
        .unwrap();
        assert_eq!(cfg.network.mode, NetworkMode::Allowlist);
        assert_eq!(cfg.network.allowed_hosts.len(), 2);
        assert!(NetworkPolicy::from_config(&cfg).is_ok());
    }

    #[test]
    fn toml_rejects_bad_allowed_host() {
        let err = ProxyConfig::from_toml_str(
            r#"
listen = "127.0.0.1:1"

[network]
mode = "allowlist"
allowed_hosts = ["https://bad.example"]

[[models]]
name = "*"
upstream = "http://example.com/v1"
"#,
        );
        assert!(err.is_err());
    }

    #[test]
    fn matching_is_case_insensitive() {
        let entries = vec![parse_allowed_entry("Example.COM").unwrap()];
        assert!(host_matches("example.com", &entries));
        assert!(host_matches("EXAMPLE.COM", &entries));
    }

    #[test]
    fn ipv6_literal_and_cidr() {
        let entries = vec![
            parse_allowed_entry("2001:db8::1").unwrap(),
            parse_allowed_entry("fe80::/10").unwrap(),
        ];
        assert!(host_matches("2001:db8::1", &entries));
        assert!(host_matches("fe80::abcd", &entries));
        assert!(!host_matches("2001:db8::2", &entries));
    }

    #[test]
    fn listen_host_is_always_bypassed() {
        let mut c = cfg(NetworkMode::NoNetwork, &[], None);
        c.listen = "10.0.0.5:19081".into();
        let p = NetworkPolicy::from_config(&c).unwrap();
        assert!(assert_egress_allowed(&p, "10.0.0.5").is_ok());
        assert_eq!(
            assert_egress_allowed(&p, "10.0.0.6"),
            Err(DenyReason::NoNetwork)
        );
    }

    #[test]
    fn merges_upstream_anthropic_host() {
        let mut c = cfg(
            NetworkMode::Allowlist,
            &[],
            Some("https://api.openai.com/v1"),
        );
        c.models[0].upstream_anthropic = Some("https://api.anthropic.com/v1".into());
        let p = NetworkPolicy::from_config(&c).unwrap();
        assert!(assert_egress_allowed(&p, "api.openai.com").is_ok());
        assert!(assert_egress_allowed(&p, "api.anthropic.com").is_ok());
        assert_eq!(
            assert_egress_allowed(&p, "evil.example"),
            Err(DenyReason::NotInAllowlist)
        );
    }

    #[test]
    fn allowlist_only_upstream_is_not_empty_deny() {
        // empty allowed_hosts but upstream present → effective list non-empty
        let p = NetworkPolicy::from_config(&cfg(
            NetworkMode::Allowlist,
            &[],
            Some("https://api.deepseek.com/v1"),
        ))
        .unwrap();
        assert!(assert_egress_allowed(&p, "api.deepseek.com").is_ok());
        assert_eq!(
            assert_egress_allowed(&p, "pypi.org"),
            Err(DenyReason::NotInAllowlist)
        );
    }

    #[test]
    fn forbidden_response_mentions_host_and_reason() {
        let (status, body) = forbidden_response("github.com", &DenyReason::NotInAllowlist);
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(body.contains("github.com"));
        assert!(body.contains("not-in-allowlist"));
    }

    #[test]
    fn example_allowlist_toml_loads_multiline_hosts() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/llm-proxy/allowlist.toml");
        let cfg = ProxyConfig::from_toml_file(&path).unwrap();
        assert_eq!(cfg.network.mode, NetworkMode::Allowlist);
        assert!(cfg.network.allowed_hosts.len() >= 3);
        assert!(cfg.network.allowed_hosts.iter().any(|h| h == "pypi.org"));
        let policy = NetworkPolicy::from_config(&cfg).unwrap();
        assert!(assert_egress_allowed(&policy, "pypi.org").is_ok());
        // upstream host merged from deepseek.toml sibling
        assert!(assert_egress_allowed(&policy, "api.deepseek.com").is_ok());
        assert_eq!(
            assert_egress_allowed(&policy, "evil.example"),
            Err(DenyReason::NotInAllowlist)
        );
    }

    #[test]
    fn toml_multiline_allowed_hosts_one_per_line() {
        let cfg = ProxyConfig::from_toml_str(
            r#"
listen = "127.0.0.1:1"

[network]
mode = "allowlist"
allowed_hosts = [
    "pypi.org",
    "files.pythonhosted.org",
    "*.example.com",
]

[[models]]
name = "*"
upstream = "http://127.0.0.1:9/v1"
"#,
        )
        .unwrap();
        assert_eq!(cfg.network.allowed_hosts.len(), 3);
        let p = NetworkPolicy::from_config(&cfg).unwrap();
        assert!(assert_egress_allowed(&p, "a.example.com").is_ok());
        assert!(assert_egress_allowed(&p, "example.com").is_err());
    }
}
