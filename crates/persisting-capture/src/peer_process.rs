//! Resolve the OS process behind an inbound proxy TCP peer (local ephemeral port).

use std::net::SocketAddr;
use std::process::Command;

use anyhow::{Context, Result};

use crate::session_client::SessionClientMeta;

/// Look up the client process for a proxied connection's peer address.
pub(crate) fn resolve_peer_client(peer: SocketAddr) -> Result<Option<SessionClientMeta>> {
    #[cfg(unix)]
    {
        if let Some(pid) = lsof_client_pid_for_peer(peer)? {
            if let Some(command) = command_line_for_pid(pid)? {
                return Ok(Some(SessionClientMeta {
                    peer: peer.to_string(),
                    peer_port: peer.port(),
                    pid,
                    command,
                }));
            }
        }
        return Ok(None);
    }
    #[cfg(not(unix))]
    {
        let _ = peer;
        Ok(None)
    }
}

#[cfg(unix)]
fn lsof_client_pid_for_peer(peer: SocketAddr) -> Result<Option<u32>> {
    let spec = lsof_tcp_spec(peer);
    let output = Command::new("lsof")
        .args(["-nP", "-F", "pnc", "-i", &spec, "-sTCP:ESTABLISHED"])
        .output()
        .with_context(|| format!("lsof peer lookup for {peer}"))?;
    if !output.status.success() && output.stdout.is_empty() {
        return Ok(None);
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let entries = parse_lsof_pnc(&text);
    let self_pid = std::process::id();

    for entry in entries {
        if entry.pid == 0 || entry.pid == self_pid {
            continue;
        }
        if !entry
            .names
            .iter()
            .any(|name| is_client_outbound_socket(name, peer))
        {
            continue;
        }
        if let Some(cmd) = command_line_for_pid(entry.pid)? {
            if is_capture_proxy_command(&cmd) {
                continue;
            }
            return Ok(Some(entry.pid));
        }
    }
    Ok(None)
}

#[cfg(unix)]
struct LsofEntry {
    pid: u32,
    names: Vec<String>,
}

#[cfg(unix)]
fn parse_lsof_pnc(output: &str) -> Vec<LsofEntry> {
    let mut entries = Vec::new();
    let mut pid = 0u32;
    let mut names = Vec::new();

    for line in output.lines() {
        if line.is_empty() {
            continue;
        }
        let Some(tag) = line.as_bytes().first() else {
            continue;
        };
        let rest = &line[1..];
        match *tag {
            b'p' => {
                if pid != 0 {
                    entries.push(LsofEntry {
                        pid,
                        names: std::mem::take(&mut names),
                    });
                }
                pid = rest.trim().parse().unwrap_or(0);
            }
            b'n' => names.push(rest.to_string()),
            _ => {}
        }
    }
    if pid != 0 {
        entries.push(LsofEntry { pid, names });
    }
    entries
}

#[cfg(unix)]
fn is_client_outbound_socket(name: &str, peer: SocketAddr) -> bool {
    let local = format_socket_addr(peer);
    name.split_once("->")
        .is_some_and(|(lhs, _rhs)| lhs == local)
}

#[cfg(unix)]
fn format_socket_addr(addr: SocketAddr) -> String {
    match addr.ip() {
        std::net::IpAddr::V4(v4) => format!("{v4}:{}", addr.port()),
        std::net::IpAddr::V6(v6) => format!("[{v6}]:{}", addr.port()),
    }
}

#[cfg(unix)]
fn is_capture_proxy_command(cmd: &str) -> bool {
    cmd.contains("persisting capture run")
        || cmd.contains("persisting capture serve")
        || cmd.contains("persisting capture start")
}

#[cfg(unix)]
fn lsof_tcp_spec(peer: SocketAddr) -> String {
    match peer.ip() {
        std::net::IpAddr::V4(v4) => format!("TCP@{}:{}", v4, peer.port()),
        std::net::IpAddr::V6(v6) => format!("TCP@[{}]:{}", v6, peer.port()),
    }
}

#[cfg(unix)]
fn command_line_for_pid(pid: u32) -> Result<Option<String>> {
    #[cfg(target_os = "linux")]
    {
        let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok();
        if let Some(raw) = raw {
            let cmd = raw
                .split(|b| *b == 0)
                .filter(|part| !part.is_empty())
                .map(|part| String::from_utf8_lossy(part).into_owned())
                .collect::<Vec<_>>()
                .join(" ");
            if !cmd.is_empty() {
                return Ok(Some(cmd));
            }
        }
    }
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "args="])
        .output()
        .with_context(|| format!("ps args for pid {pid}"))?;
    if !output.status.success() {
        return Ok(None);
    }
    let cmd = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if cmd.is_empty() {
        Ok(None)
    } else {
        Ok(Some(cmd))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lsof_tcp_spec_ipv4() {
        let peer: SocketAddr = "127.0.0.1:54321".parse().unwrap();
        assert_eq!(lsof_tcp_spec(peer), "TCP@127.0.0.1:54321");
    }

    #[test]
    fn lsof_tcp_spec_ipv6() {
        let peer: SocketAddr = "[::1]:8080".parse().unwrap();
        assert_eq!(lsof_tcp_spec(peer), "TCP@[::1]:8080");
    }

    #[test]
    fn client_outbound_socket_matches_local_peer_port() {
        let peer: SocketAddr = "127.0.0.1:55522".parse().unwrap();
        assert!(is_client_outbound_socket(
            "127.0.0.1:55522->127.0.0.1:19081",
            peer
        ));
        assert!(!is_client_outbound_socket(
            "127.0.0.1:19081->127.0.0.1:55522",
            peer
        ));
    }

    #[test]
    fn parse_lsof_pnc_groups_names_by_pid() {
        let sample = "\
p100
n127.0.0.1:8080->127.0.0.1:55522
p200
n127.0.0.1:55522->127.0.0.1:8080
";
        let entries = parse_lsof_pnc(sample);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].pid, 100);
        assert_eq!(entries[1].pid, 200);
        assert_eq!(entries[1].names[0], "127.0.0.1:55522->127.0.0.1:8080");
    }

    #[test]
    fn skips_capture_proxy_command() {
        assert!(is_capture_proxy_command(
            "./target/debug/persisting capture run -o ./store -- claude"
        ));
        assert!(!is_capture_proxy_command("claude --model deepseek"));
    }
}
