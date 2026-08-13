# Installation

Persisting is distributed as a Python wheel containing both the Python package
and its matched CLI component set:

| Distribution | Contents | Use case |
|---|---|---|
| Host wheel | Python package plus `pchronicle`, `pvisor`, and `ppilot` | Python APIs and the complete host CLI component set |

## Requirements

- Python 3.10+
- Pulsing, installed automatically as a dependency
- macOS or Linux for the CLI
- macOS: macFUSE 5 for host-process `pvisor run --safe` (not required by libkrun Runs)

Install the macOS filesystem runtime once before the first safe Run:

```bash
brew install --cask macfuse
```

On Apple Silicon, enable the macFUSE system extension when macOS prompts for
approval. Plain non-staged host Runs remain available without macFUSE, but
`--safe` fails closed rather than writing directly to the project workspace.
The libkrun OCI-image executor keeps its cached rootfs immutable through its
built-in Overlay virtio-fs backend and does not require macFUSE.

## Python package

```bash
# Recommended: install with Lance support
pip install persisting[lance]

# Minimal install without Lance, for custom backends only
pip install persisting
```

Both installation commands install the matching `pchronicle`, `pvisor`, and `ppilot`
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

## CLI component set

The wheel bundles a matched component set. Use `pvisor` for one Run and
environments, `ppilot` for batch orchestration, and `pchronicle` for Dataset
catalog, SQL, analysis, exchange, and read-only serving.

### Cargo installation from source

```bash
git clone https://github.com/DeepLink-org/Persisting.git
cd Persisting
just install-cli
```

This alternative installs matching builds of `pchronicle`, `pvisor`, and
`ppilot` into the Cargo binary directory without installing the Python package.

### Component overrides

Set `PERSISTING_PVISOR_BIN` or `PERSISTING_PPILOT_BIN` to select an explicit
component binary.

## Container and libkrun executors

Container execution still requires an explicitly configured compatible Linux
pVisor guest binary. The `vm` executor instead statically includes libkrun and
its Linux guest init in the host `pvisor` binary. Release wheels also install
libkrunfw beside `pvisor`. Source builds automatically download the pinned
official release into the user cache and verify its SHA-256; macOS compiles the
downloaded kernel bundle with `/usr/bin/cc`. `--vm-library-dir` may still point
at a system installation. Building pVisor from source on macOS also requires
Zig (`brew install zig`) to cross-compile libkrun's embedded Linux guest init.

Use `pvisor run --image ubuntu:latest -- COMMAND` to pull and run a public OCI
image without Docker or Podman. `ubuntu:latest` is also the default for
`--executor vm`; `--image-store DIR` overrides the local content-addressed
cache. `--overlayfs-target` selects the guest workspace path. `--vm-rootfs DIR`
remains available for an explicit prepared Linux rootfs. Linux hosts use KVM;
Apple Silicon macOS hosts use HVF through the same executor and configuration.

## Verify the installation

```python
import persisting
print(persisting.__version__)
```

```bash
pvisor --version
ppilot --help
pchronicle --help
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
