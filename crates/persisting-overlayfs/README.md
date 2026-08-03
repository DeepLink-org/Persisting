# persisting-overlayfs

Cross-platform **FUSE overlay** for pVisor staging (macOS via [macFUSE](https://macfuse.github.io/),
Linux via libfuse / fuse3).

This is pVisor's unprivileged, cross-platform overlay implementation. pVisor
links it as a library and owns the FUSE request thread in-process. It implements
overlay-style copy-up and portable `.wh.*` whiteouts that can be committed by
pVisor.

| Feature | Status |
|---------|--------|
| Ordered multi-`lowerdir` merge + directory or Jujutsu upper | yes |
| Multiple named OverlayFS forks in one Jujutsu store | yes |
| Unprivileged `.wh.*` whiteouts | yes |
| Opaque directories (`.wh..wh..opq` and overlay xattrs) | yes |
| Atomic copy-up through `workdir` | yes |
| Preserve mode, owner, timestamps, xattrs and POSIX node types | yes, subject to caller privileges |
| Whole merged-directory copy-up for lower-backed rename | yes |
| `RENAME_NOREPLACE`, exchange/swap, hard links and symlinks | yes |
| Stable hard-link inode identity and alias-preserving copy-up | yes |
| `get/set/list/remove xattr` | yes |
| Directory snapshots, `readdirplus`, `fsync`, `statfs`, `access` | yes |
| `fallocate`, `lseek`, `copy_file_range` | portable implementations |
| macOS Finder flags and extended times | yes |

Whiteouts match pVisor's `apply_overlay`, so review → apply works the same.

### Deliberate platform limits

Linux-only container features do not have a meaningful macOS equivalent:

- UID/GID namespace mapping is not emulated. Ownership changes use the caller's
  normal macOS permissions.
- Linux `metacopy`, `redirect_dir`, SELinux labeling and capability semantics
  are not synthesized. Ordinary xattrs and ACL-related xattrs are passed
  through when the host filesystem permits them.
- Hole punching and Linux-specific `fallocate` modes return `ENOTSUP`; mode
  zero guarantees the requested file length.
- Creating device nodes still requires host privileges. Unprivileged deletes
  always use portable `.wh.*` markers.
- Exchange is rollback-safe but implemented as three upper-layer renames,
  because macOS FUSE does not expose Linux `renameat2` atomically to the backing
  store.

Linux namespace mapping, `metacopy`, `redirect_dir`, SELinux labeling, and
capability semantics are outside the portable backend's current scope.

## Prerequisites

### macOS

1. Install macFUSE: <https://macfuse.github.io/> or `brew install --cask macfuse`
2. Apple Silicon: enable third-party kernel extensions (System Settings)
3. `pkg-config` able to find macFUSE (`brew install pkgconf`)

macFUSE 5 is supported through the workspace's patched `fuser` dependency.
The patch uses libfuse API 26 (the API 25 compatibility entry point returns
`EEXIST` on macFUSE 5), negotiates kernel ABI 7.19, and retains macFUSE's
extended rename request layout.

### Linux

FUSE3 development packages, for example `libfuse3-dev`.

## Build

Building pVisor also builds the embedded overlay library:

```bash
cargo build -p persisting-pvisor --release
```

The standalone CLI is optional and intended for diagnostics/manual mounts:

```bash
cargo build -p persisting-overlayfs --release
# → target/release/persisting-overlayfs
```

## CLI

```bash
# Directory-backed upper (pVisor default)
persisting-overlayfs -o lowerdir=/target,upperdir=/stage/upper,workdir=/stage/work /stage/merged

# Jujutsu-managed fork; another jjworkspace can share the same jjstore
persisting-overlayfs -o lowerdir=/target,jjstore=/shared/overlay.jj,jjworkspace=attempt-1 /stage/merged
```

The parser accepts repeated fuse-style option content including `allow_other`,
`allow_root`, `default_permissions`, `ro`/`rw`, and `fsname=...`. Escape a
literal lower-layer colon as `\:`. As with Linux overlayfs, the
leftmost entry in `lowerdir=top:next:bottom` has the highest priority.
`workdir=` is valid only for a directory upper. In directory mode, `upperdir`
and `workdir` must be separate directories on the same filesystem; layer,
work, upper, and mount paths may not overlap.

The Jujutsu backend gives every `jjworkspace` an independent directory upper
and Jujutsu working-copy commit. Named workspaces share the repository under
`jjstore`, including its Git objects and operation log. A clean writable
unmount snapshots the upper through `jj-lib`; read-only inspection never moves
the workspace head. Only one writable mount of a given workspace is allowed at
a time, while different workspaces can be mounted concurrently.

The live upper remains a normal POSIX directory, so random writes do not create
a Jujutsu commit per FUSE request. Jujutsu's Git object model preserves file
contents and executable bits in history; full uid/gid/xattr/hard-link recovery
from an old Jujutsu commit will require the planned OverlayFS metadata manifest.

The default macFUSE backend is the kernel VFS backend. `backend=fskit` is also
accepted on supported macFUSE versions, but FSKit mount points must be direct
children of `/Volumes`.

pVisor does not discover or launch an overlay binary. Its `OverlayMount` owns
the embedded `OverlaySession`, and teardown synchronously unmounts and joins
the FUSE request thread.
