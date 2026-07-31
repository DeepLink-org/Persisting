# persisting-fs-overlay

This crate **vendors / incorporates** source from
[containers/fuse-overlayfs](https://github.com/containers/fuse-overlayfs)
(upstream license: **GPL-2.0-or-later**; see `COPYING`).

The Persisting workspace as a whole is distributed under **GPL-3.0-or-later**
(see the repository root `LICENSE` and `NOTICE`). Upstream’s “or later” grant
allows this tree to be used under GPL-3.0 terms together with the rest of
Persisting.

Binary name remains `fuse-overlayfs` for CLI compatibility.

## Build

Requires Rust ≥ 1.85 and FUSE3 development headers.

```bash
cargo build -p persisting-fs-overlay --release
# → target/release/fuse-overlayfs
```

Not part of workspace `default-members` (needs FUSE devel); always pass
`-p persisting-fs-overlay`.

## Sync from upstream

```bash
./scripts/sync-fuse-overlayfs.sh
# optional ref: ./scripts/sync-fuse-overlayfs.sh <tag-or-commit>
```

Pin files:

- `UPSTREAM.REVISION`
- `UPSTREAM.REMOTE`
- `UPSTREAM.LOG`
