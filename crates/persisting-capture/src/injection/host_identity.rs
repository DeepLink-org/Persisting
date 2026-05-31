//! Stable one-way machine fingerprint for session client metadata.
//!
//! Stored as `machine_fp` on [`SessionClientMeta`](crate::storage::session_client::SessionClientMeta)
//! (32-char hex = first 128 bits of BLAKE3).

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
#[cfg(unix)]
use std::process::Command;

/// Inputs for [`fingerprint_host`]; exposed for deterministic unit tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostIdentityInput {
    pub hostname: String,
    pub username: String,
    /// IP used for identity (client peer when remote, else local primary IPv4).
    pub identity_ip: String,
    /// Linux `/etc/machine-id` when available — more stable than hostname alone.
    pub machine_id: Option<String>,
}

/// One-way machine fingerprint (`persisting-machine-fp/v1`).
pub fn fingerprint_host(input: &HostIdentityInput) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"persisting-machine-fp/v1\n");
    if let Some(id) = input
        .machine_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        hasher.update(b"machine_id=");
        hasher.update(id.as_bytes());
        hasher.update(b"\n");
    }
    hasher.update(b"hostname=");
    hasher.update(normalize_hostname(&input.hostname).as_bytes());
    hasher.update(b"\n");
    hasher.update(b"username=");
    hasher.update(normalize_username(&input.username).as_bytes());
    hasher.update(b"\n");
    hasher.update(b"ip=");
    hasher.update(input.identity_ip.trim().as_bytes());
    hasher.update(b"\n");
    hex16(&hasher.finalize())
}

/// Best-effort fingerprint for the machine / OS user behind a proxied TCP peer.
///
/// When `client_pid` is known, uses that process owner (`ps user=`); otherwise the
/// effective user of the capture proxy process.
pub fn machine_fingerprint_for_client(peer: SocketAddr, client_pid: Option<u32>) -> String {
    let username = client_pid
        .filter(|pid| *pid > 0)
        .and_then(username_for_pid)
        .unwrap_or_else(local_username);
    fingerprint_host(&HostIdentityInput {
        hostname: local_hostname(),
        username,
        identity_ip: identity_ip_for_peer(peer).to_string(),
        machine_id: read_linux_machine_id(),
    })
}

fn identity_ip_for_peer(peer: SocketAddr) -> IpAddr {
    if peer.ip().is_loopback() {
        local_primary_ipv4()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST))
    } else {
        peer.ip()
    }
}

fn normalize_hostname(host: &str) -> String {
    let h = host.trim().to_lowercase();
    h.strip_suffix(".local").unwrap_or(&h).to_string()
}

fn normalize_username(user: &str) -> String {
    user.trim().to_lowercase()
}

fn hex16(hash: &blake3::Hash) -> String {
    hash.as_bytes()[..16]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn local_hostname() -> String {
    hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into())
}

fn local_username() -> String {
    normalize_username(
        &std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_else(|_| "unknown".into()),
    )
}

#[cfg(unix)]
fn username_for_pid(pid: u32) -> Option<String> {
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "user="])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let user = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if user.is_empty() {
        None
    } else {
        Some(normalize_username(&user))
    }
}

#[cfg(not(unix))]
fn username_for_pid(_pid: u32) -> Option<String> {
    None
}

/// Primary outbound IPv4 (UDP connect trick); no packets sent on most OSes.
fn local_primary_ipv4() -> Option<Ipv4Addr> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    match socket.local_addr().ok()?.ip() {
        IpAddr::V4(v4) if !v4.is_loopback() => Some(v4),
        _ => None,
    }
}

fn read_linux_machine_id() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        for path in ["/etc/machine-id", "/var/lib/dbus/machine-id"] {
            if let Ok(text) = std::fs::read_to_string(path) {
                let id = text.trim().to_string();
                if !id.is_empty() {
                    return Some(id);
                }
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = ();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_input() -> HostIdentityInput {
        HostIdentityInput {
            hostname: "MacBook.local".into(),
            username: "alice".into(),
            identity_ip: "192.168.1.42".into(),
            machine_id: Some("abc-def".into()),
        }
    }

    #[test]
    fn fingerprint_is_stable_and_hex32() {
        let input = sample_input();
        let a = fingerprint_host(&input);
        let b = fingerprint_host(&input);
        assert_eq!(a, b);
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn hostname_normalization_strips_local_suffix() {
        let with = HostIdentityInput {
            hostname: "box.local".into(),
            username: "bob".into(),
            identity_ip: "10.0.0.1".into(),
            machine_id: None,
        };
        let without = HostIdentityInput {
            hostname: "box".into(),
            ..with.clone()
        };
        assert_eq!(fingerprint_host(&with), fingerprint_host(&without));
    }

    #[test]
    fn username_changes_fingerprint() {
        let base = HostIdentityInput {
            username: "alice".into(),
            ..sample_input()
        };
        let other = HostIdentityInput {
            username: "bob".into(),
            ..base.clone()
        };
        assert_ne!(fingerprint_host(&base), fingerprint_host(&other));
    }

    #[test]
    fn machine_id_changes_fingerprint() {
        let base = HostIdentityInput {
            machine_id: None,
            ..sample_input()
        };
        let with_id = HostIdentityInput {
            machine_id: Some("id-1".into()),
            ..base.clone()
        };
        assert_ne!(fingerprint_host(&base), fingerprint_host(&with_id));
    }

    #[test]
    fn loopback_peer_uses_localhost_ip_when_no_route() {
        let peer: SocketAddr = "127.0.0.1:12345".parse().unwrap();
        let fp = machine_fingerprint_for_client(peer, None);
        assert_eq!(fp.len(), 32);
    }
}
