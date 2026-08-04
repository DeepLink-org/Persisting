# Installation

Persisting is distributed as three installable parts. Choose the parts needed
for your workflow:

| Distribution | Contents | Use case |
|---|---|---|
| Python package | `persisting` wheel | Python APIs for Tensor Memory, Queue, and Search |
| Unified CLI | `persisting`, `pvisor`, `ppilot`, and `libpersisting_engine` | `execute`, `env`, `batch`, `query`, `history`, `eval`, and `gateway` commands |
| Guest runtime | Static Linux `pvisor` for `linux-amd64` or `linux-arm64` | Injection into Container and KVM executors |

## Requirements

- Python 3.10+
- Pulsing, installed automatically as a dependency
- macOS or Linux for the CLI; guest runtimes are static Linux binaries

## Python package

```bash
# Recommended: install with Lance support
pip install persisting[lance]

# Minimal install without Lance, for custom backends only
pip install persisting
```

### Nightly wheel

The rolling [`nightly`](https://github.com/DeepLink-org/Persisting/releases/tag/nightly)
release is updated daily and on pushes to `main`:

```bash
curl -fsSL https://raw.githubusercontent.com/DeepLink-org/Persisting/main/scripts/install-nightly.sh | bash
```

### From source

```bash
git clone https://github.com/DeepLink-org/Persisting.git
cd Persisting
pip install -e ".[lance]"
```

## Unified CLI

The CLI is a matched component set. `persisting` delegates execution and
environment commands to `pvisor`, batch and query commands to `ppilot`, and
dynamically loads `libpersisting_engine` for Search, History, and Eval.

### From source (recommended for development)

```bash
git clone https://github.com/DeepLink-org/Persisting.git
cd Persisting
just install-cli
```

This installs matching builds of `persisting`, `pvisor`, `ppilot`, and the
engine library into the Cargo binary directory.

### Nightly binaries (no Rust toolchain required)

```bash
curl -fsSL https://raw.githubusercontent.com/DeepLink-org/Persisting/main/scripts/install-cli-nightly.sh | bash
```

The installer writes the component set to `~/.persisting/cli/bin`, or under
`PERSISTING_CLI_ROOT` when set, and prints the line needed to add it to `PATH`.
Every release archive has a matching `.sha256` checksum.

### Component overrides

Set `PERSISTING_PVISOR_BIN`, `PERSISTING_PPILOT_BIN`, or
`PERSISTING_ENGINE_LIB` to select an explicit binary or engine library.

## Guest runtime for Container and KVM executors

`pvisor run --executor container|kvm` needs a static Linux pVisor matching the
guest platform:

```bash
# linux-amd64 is the default; repeat with --platform linux-arm64 when needed
curl -fsSL https://raw.githubusercontent.com/DeepLink-org/Persisting/main/scripts/install-guest-runtimes.sh | bash -s -- --platform linux-amd64
```

The runtime is installed at
`~/.persisting/runtimes/<version>/<platform>/pvisor`, or at
`$PERSISTING_PVISOR_RUNTIME_DIR/<platform>/pvisor` when the override is set.
pVisor discovers it automatically. Host execution remains available without a
guest runtime; only Container and KVM execution require one.

`just install-cli-nightly` and `just install-guest-runtimes` wrap the same
installer scripts.

## Verify the installation

```python
import persisting
print(persisting.__version__)
```

```bash
pvisor --version
ppilot --help
```

## Dependencies

| Package | Version | Required | Purpose |
|---|---|---|---|
| `pulsing` | >=0.1.0 | Yes | Distributed actor runtime for the control plane |
| `lance` | >=0.9.0 | Optional (`[lance]`) | Lance columnar storage |
| `pyarrow` | >=14.0.0 | Optional (`[lance]`) | Apache Arrow |

## Next steps

- [Quick Start](quickstart.md) — run an Agent workflow in five minutes
- [Choose a Capability](guide/index.md) — find the workflow for your goal
- [Architecture & Internals](design/index.md) — understand the implementation
