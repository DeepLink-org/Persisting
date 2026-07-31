// SPDX-License-Identifier: GPL-2.0-or-later
//
// UID/GID mapping: bidirectional translation between host and container IDs.

use std::fs;

/// A single mapping entry: maps [host..host+len) <-> [to..to+len).
#[derive(Debug, Clone)]
pub struct IdMapping {
    pub host: u32,
    pub to: u32,
    pub len: u32,
}

/// Overflow UID and GID read from /proc/sys/kernel/overflow{u,g}id.
#[derive(Debug, Clone, Copy)]
pub struct OverflowIds {
    pub uid: u32,
    pub gid: u32,
}

impl OverflowIds {
    pub fn read() -> Self {
        let uid = read_proc_id("/proc/sys/kernel/overflowuid").unwrap_or(65534);
        let gid = read_proc_id("/proc/sys/kernel/overflowgid").unwrap_or(65534);
        OverflowIds { uid, gid }
    }
}

fn read_proc_id(path: &str) -> Option<u32> {
    let s = fs::read_to_string(path).ok()?;
    s.trim().parse().ok()
}

/// Parse a mapping string like "0:1000:1:1:1001:1" into a list of IdMapping.
/// Format: colon-separated triples of host:to:len.
pub fn parse_mappings(s: &str) -> Result<Vec<IdMapping>, String> {
    let parts: Vec<&str> = s.split(':').filter(|p| !p.is_empty()).collect();
    if parts.len() % 3 != 0 {
        return Err(format!("invalid mapping specified: {}", s));
    }

    let mut mappings = Vec::new();
    for chunk in parts.chunks(3) {
        let host: u32 = chunk[0]
            .parse()
            .map_err(|_| format!("invalid mapping specified: {}", s))?;
        let to: u32 = chunk[1]
            .parse()
            .map_err(|_| format!("invalid mapping specified: {}", s))?;
        let len: u32 = chunk[2]
            .parse()
            .map_err(|_| format!("invalid mapping specified: {}", s))?;
        mappings.push(IdMapping { host, to, len });
    }

    Ok(mappings)
}

/// Look up an ID through the mapping table.
///
/// - `direct=true`: host→container (used when reading from disk to present to FUSE)
/// - `direct=false`: container→host (used when writing from FUSE to disk)
pub fn find_mapping(
    id: u32,
    mappings: &[IdMapping],
    direct: bool,
    squash_to_root: bool,
    squash_to_id: Option<u32>,
    overflow_id: u32,
) -> u32 {
    // squash_to_uid/gid takes precedence over squash_to_root
    if direct {
        if let Some(squash_id) = squash_to_id {
            return squash_id;
        }
        if squash_to_root {
            return 0;
        }
    }

    if mappings.is_empty() {
        return id;
    }

    for m in mappings {
        if direct {
            if id >= m.host && id - m.host < m.len {
                return m.to + (id - m.host);
            }
        } else if id >= m.to && id - m.to < m.len {
            return m.host + (id - m.to);
        }
    }

    overflow_id
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_mappings() {
        let m = parse_mappings("0:1000:1").unwrap();
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].host, 0);
        assert_eq!(m[0].to, 1000);
        assert_eq!(m[0].len, 1);
    }

    #[test]
    fn test_parse_mappings_multiple() {
        let m = parse_mappings("0:1000:1:1:1001:65535").unwrap();
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn test_parse_mappings_leading_colon() {
        let m = parse_mappings(":0:100:10000").unwrap();
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].host, 0);
        assert_eq!(m[0].to, 100);
        assert_eq!(m[0].len, 10000);
    }

    #[test]
    fn test_parse_mappings_invalid() {
        assert!(parse_mappings("0:1000").is_err());
        assert!(parse_mappings("abc:1000:1").is_err());
    }

    #[test]
    fn test_find_mapping_direct() {
        let m = vec![IdMapping {
            host: 1000,
            to: 0,
            len: 1,
        }];
        // host 1000 -> container 0
        assert_eq!(find_mapping(1000, &m, true, false, None, 65534), 0);
        // host 1001 -> overflow
        assert_eq!(find_mapping(1001, &m, true, false, None, 65534), 65534);
    }

    #[test]
    fn test_find_mapping_reverse() {
        let m = vec![IdMapping {
            host: 1000,
            to: 0,
            len: 1,
        }];
        // container 0 -> host 1000
        assert_eq!(find_mapping(0, &m, false, false, None, 65534), 1000);
    }

    #[test]
    fn test_squash_to_root() {
        let m = vec![IdMapping {
            host: 1000,
            to: 0,
            len: 1,
        }];
        assert_eq!(find_mapping(500, &m, true, true, None, 65534), 0);
    }

    #[test]
    fn test_squash_to_uid() {
        let m = vec![IdMapping {
            host: 1000,
            to: 0,
            len: 1,
        }];
        assert_eq!(find_mapping(500, &m, true, false, Some(42), 65534), 42);
        // squash_to_uid takes precedence over squash_to_root
        assert_eq!(find_mapping(500, &m, true, true, Some(42), 65534), 42);
    }
}
