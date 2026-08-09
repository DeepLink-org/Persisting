# Installation

Persisting is distributed as a Python wheel containing both the Python package
and its matched CLI component set:

| Distribution | Contents | Use case |
|---|---|---|
| Host wheel | Python package plus `persisting`, `pvisor`, and `ppilot` | Python APIs and the complete host CLI component set |

## Requirements

- Python 3.10+
- Pulsing, installed automatically as a dependency
- macOS or Linux for the CLI
- macOS: macFUSE 5 for `pvisor run --safe` staged workspaces

Install the macOS filesystem runtime once before the first safe Run:

```bash
brew install --cask macfuse
```

On Apple Silicon, enable the macFUSE system extension when macOS prompts for
approval. Plain non-staged host Runs remain available without macFUSE, but
`--safe` fails closed rather than writing directly to the project workspace.

## Python package

```bash
# Recommended: install with Lance support
pip install persisting[lance]

# Minimal install without Lance, for custom backends only
pip install persisting
```

Both installation commands install the matching `persisting`, `pvisor`, and `ppilot`
binaries into the Python environment's scripts directory.

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

The CLI bundled in the wheel is a matched component set. `persisting` delegates execution and
environment commands to `pvisor`, batch and query commands to `ppilot`, and
calls pChronicle directly for History and Eval.

### Cargo installation from source

```bash
git clone https://github.com/DeepLink-org/Persisting.git
cd Persisting
just install-cli
```

This alternative installs matching builds of `persisting`, `pvisor`, and
`ppilot` into the Cargo binary directory without installing the Python package.

### Component overrides

Set `PERSISTING_PVISOR_BIN` or `PERSISTING_PPILOT_BIN` to select an explicit
component binary.

## Container and KVM executors

`pvisor run --executor container|kvm` requires a compatible Linux pVisor for
the guest platform. Supply it explicitly through the executor's `pvisor_binary`
setting. Nightly releases do not publish a separate guest runtime. Host
execution does not need this additional artifact.

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
