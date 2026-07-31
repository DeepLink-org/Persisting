// SPDX-License-Identifier: GPL-2.0-or-later
//
// Xattr handling: namespace filtering, encoding/decoding, stat override, ACL inheritance.

use crate::datasource::StatOverrideMode;

// Internal xattr prefixes, hidden from userspace.
pub const XATTR_PREFIX: &str = "user.fuseoverlayfs.";
pub const OPAQUE_XATTR: &str = "user.fuseoverlayfs.opaque";

pub const XATTR_CONTAINERS_OVERRIDE_PREFIX: &str = "user.containers.override_";

pub const UNPRIVILEGED_XATTR_PREFIX: &str = "user.overlay.";
pub const UNPRIVILEGED_OPAQUE_XATTR: &str = "user.overlay.opaque";

pub const PRIVILEGED_XATTR_PREFIX: &str = "trusted.overlay.";
pub const PRIVILEGED_OPAQUE_XATTR: &str = "trusted.overlay.opaque";

pub const XATTR_SECURITY_PREFIX: &str = "security.";

pub const OPAQUE_WHITEOUT: &str = ".wh..wh..opq";

/// Check if a user can access (see) this xattr name.
/// Internal overlay xattrs are hidden.
pub fn can_access_xattr(name: &str, stat_override_mode: StatOverrideMode) -> bool {
    if name.starts_with(XATTR_PREFIX) {
        return false;
    }
    if name.starts_with(PRIVILEGED_XATTR_PREFIX) {
        return false;
    }
    if name.starts_with(UNPRIVILEGED_XATTR_PREFIX) {
        return false;
    }
    if stat_override_mode == StatOverrideMode::Containers && name.starts_with(XATTR_SECURITY_PREFIX)
    {
        return false;
    }
    true
}

/// Check if an xattr name is encoded with the containers override prefix.
pub fn is_encoded_xattr_name(name: &str, stat_override_mode: StatOverrideMode) -> bool {
    if !name.starts_with(XATTR_CONTAINERS_OVERRIDE_PREFIX) {
        return false;
    }
    let inner = &name[XATTR_CONTAINERS_OVERRIDE_PREFIX.len()..];
    !can_access_xattr(inner, stat_override_mode)
}

/// Decode an xattr name: strip containers override prefix if encoded,
/// return the name if accessible, or None if internal.
pub fn decode_xattr_name(name: &str, stat_override_mode: StatOverrideMode) -> Option<&str> {
    if is_encoded_xattr_name(name, stat_override_mode) {
        return Some(&name[XATTR_CONTAINERS_OVERRIDE_PREFIX.len()..]);
    }
    if can_access_xattr(name, stat_override_mode) {
        return Some(name);
    }
    None
}

/// Encode an xattr name for storage: if the name is an internal xattr
/// and we're in containers mode, prefix it with the containers override prefix.
/// Returns None if the name cannot be stored.
pub fn encode_xattr_name(name: &str, stat_override_mode: StatOverrideMode) -> Option<String> {
    if can_access_xattr(name, stat_override_mode) {
        return Some(name.to_string());
    }

    if stat_override_mode != StatOverrideMode::Containers {
        return None;
    }

    // xattr name max is 255 bytes
    if name.len() + XATTR_CONTAINERS_OVERRIDE_PREFIX.len() > 255 {
        return None;
    }

    Some(format!("{}{}", XATTR_CONTAINERS_OVERRIDE_PREFIX, name))
}

/// Filter an xattr list buffer: remove internal xattrs that should be hidden,
/// decode encoded container xattr names.
/// Returns the new length of the filtered buffer.
pub fn filter_xattr_list(buf: &[u8], stat_override_mode: StatOverrideMode) -> Vec<u8> {
    let mut result = Vec::new();

    // xattr list is null-separated names
    for name_bytes in buf.split(|&b| b == 0) {
        if name_bytes.is_empty() {
            continue;
        }
        let name = match std::str::from_utf8(name_bytes) {
            Ok(s) => s,
            Err(_) => continue,
        };

        if let Some(decoded) = decode_xattr_name(name, stat_override_mode) {
            result.extend_from_slice(decoded.as_bytes());
            result.push(0);
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_can_access_xattr() {
        assert!(!can_access_xattr(
            "user.fuseoverlayfs.origin",
            StatOverrideMode::None
        ));
        assert!(!can_access_xattr(
            "trusted.overlay.opaque",
            StatOverrideMode::None
        ));
        assert!(!can_access_xattr(
            "user.overlay.opaque",
            StatOverrideMode::None
        ));
        assert!(can_access_xattr("user.myattr", StatOverrideMode::None));
        assert!(can_access_xattr("security.selinux", StatOverrideMode::None));
        // In containers mode, security.* is hidden
        assert!(!can_access_xattr(
            "security.selinux",
            StatOverrideMode::Containers
        ));
    }

    #[test]
    fn test_decode_xattr_name() {
        // Normal accessible xattr
        assert_eq!(
            decode_xattr_name("user.myattr", StatOverrideMode::None),
            Some("user.myattr")
        );
        // Internal xattr, hidden
        assert_eq!(
            decode_xattr_name("user.fuseoverlayfs.origin", StatOverrideMode::None),
            None
        );
        // Encoded container xattr
        assert_eq!(
            decode_xattr_name(
                "user.containers.override_user.fuseoverlayfs.origin",
                StatOverrideMode::None
            ),
            Some("user.fuseoverlayfs.origin")
        );
    }

    #[test]
    fn test_encode_xattr_name() {
        // Accessible name returned as-is
        assert_eq!(
            encode_xattr_name("user.myattr", StatOverrideMode::None),
            Some("user.myattr".into())
        );
        // Internal name in non-containers mode, None
        assert_eq!(
            encode_xattr_name("user.fuseoverlayfs.origin", StatOverrideMode::None),
            None
        );
        // Internal name in containers mode, encoded
        assert_eq!(
            encode_xattr_name("user.fuseoverlayfs.origin", StatOverrideMode::Containers),
            Some("user.containers.override_user.fuseoverlayfs.origin".into())
        );
    }

    #[test]
    fn test_filter_xattr_list() {
        let input = b"user.myattr\0user.fuseoverlayfs.origin\0trusted.overlay.opaque\0user.other\0";
        let filtered = filter_xattr_list(input, StatOverrideMode::None);
        let names: Vec<&str> = filtered
            .split(|&b| b == 0)
            .filter(|s| !s.is_empty())
            .map(|s| std::str::from_utf8(s).unwrap())
            .collect();
        assert_eq!(names, vec!["user.myattr", "user.other"]);
    }
}
