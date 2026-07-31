# fuse-overlayfs — Development Rules

## Critical: No crashes

This is a FUSE filesystem backing containers. A crash makes the filesystem unreachable and blocks the container permanently. **The process must never panic or abort.**

- All errors must be handled and reported via FUSE error replies, never via `unwrap()`, `expect()`, or `panic!()`.
- Use `reply.error(errno)` to report failures to the kernel.
- All file accesses on the data directories must be done in a safe manner, using `openat2()` or equivalent.

## No `unsafe` outside `src/sys/`

All `unsafe` code must live in `src/sys/` modules (`sys::fs`, `sys::io`, `sys::openat2`, `sys::xattr`, etc.). The rest of the codebase (`overlay.rs`, `copyup.rs`, `node.rs`, `layer.rs`, etc.) must be safe Rust only. If you need a new syscall or libc call, add a safe wrapper in the appropriate `src/sys/` module.

## No regressions

- All existing tests must pass before and after any change.
- Do not remove or weaken existing tests.
- New functionality should include tests.

## No warnings

The code must compile with zero warnings. Fix warnings immediately — do not suppress them with `#[allow(...)]` unless there is a documented reason.  Always run `cargo fmt` before committing.

## Build and test commands

```sh
# Build
CARGO_HOME=.cargo cargo build --release

# Run tests
CARGO_HOME=.cargo cargo test --release
```
