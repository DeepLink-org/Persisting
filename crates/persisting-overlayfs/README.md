# persisting-overlayfs

**Cross-platform FUSE overlay for pVisor staging (macFUSE / libfuse).**

Owns the unprivileged, in-process FUSE overlay: ordered multi-`lowerdir` merge,
directory or Jujutsu upper, portable `.wh.*` whiteouts, and the optional
standalone `persisting-overlayfs` diagnostic CLI.

Does not own review, apply, drop, or Run lifecycle.
[`persisting-pvisor`](../persisting-pvisor/README.md) links this crate as a
library, owns the FUSE request thread, and commits whiteouts through
`apply_overlay`. Portable, FUSE-neutral overlay mechanics live in
`persisting-overlay-core` (used by libkrun virtio-fs without a host FUSE
mount).

Whiteouts match pVisor's `apply_overlay`, so review → apply works the same
across host FUSE and virtio-fs.

Linux-only container features are out of scope: UID/GID namespace mapping,
`metacopy`, `redirect_dir`, SELinux labeling, and capability semantics are not
emulated.

## Develop

### Prerequisites

macOS: install [macFUSE](https://macfuse.github.io/) (`brew install --cask macfuse`),
enable third-party kernel extensions on Apple Silicon, and ensure `pkg-config`
can find macFUSE (`brew install pkgconf`). macFUSE 5 is supported through the
workspace's patched `fuser` dependency.

Linux: FUSE3 development packages, for example `libfuse3-dev`.

### Build and test

```bash
cargo build -p persisting-pvisor --bin pvisor --release
just test persisting-overlayfs
```

pVisor embeds the overlay library; it does not discover or launch an overlay
binary. The standalone CLI is optional and intended for diagnostics or manual
mounts:

```bash
cargo build -p persisting-overlayfs --release
# → target/release/persisting-overlayfs
```

## Links

- [Isolation architecture](../../docs/src/pvisor/design/isolation.md)
- [Review and apply effects](../../docs/src/pvisor/guides/review-apply.md)
- [`persisting-pvisor`](../persisting-pvisor/README.md)
