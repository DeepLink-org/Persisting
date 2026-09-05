# pVisor CLI design

The `pvisor` binary is a thin front end over Run, OverlayFS, OverlayNet, Gateway, and replay services. Every command resolves through the shared `RunConfig`/`RunSpec` model, keeping TOML, delegated containers, and host execution consistent.

`run` executes an Agent. Any first argument that is not a reserved command or help/version flag is rewritten as `pvisor run`, so `pvisor -- codex` equals `pvisor run -- codex`. `replay` is an independent trajectory workflow and does not implicitly enable Gateway or pChronicle capture. `env` manages named reusable stages (`create`, `start`, `stop`, `exec`, `shell`, `list`, `status`, `inspect`, `apply`, `drop`, `delete`). Lifecycle selectors accept a Run id, record directory, `run.json`, upper/merged path, or workspace.

`--spec` accepts TOML `RunConfig` or prepared JSON `RunSpec`. Explicit CLI scalars replace TOML scalars; repeated list options replace the complete list; the command after `--` replaces `run.command`. Executor selection can be inferred from `--container-image` or `--rootfs`.

`review`, `checkpoint`, `fork`, `apply`, and `drop` are separate transactional operations. Checkpoints are stopped-consistent; `apply` is dependency-closed and records batches in `apply-ledger.json`; live Runs refuse mutation. Reset operations create a new environment generation so stale metadata cannot overwrite a new stage.
