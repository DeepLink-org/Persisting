# Installation

Persisting is distributed as a Python wheel containing both the Python package
and its matched CLI component set:

The public entry points documented here focus on controlled Agent execution and
durable trajectory history.

| Distribution | Contents | Use case |
|---|---|---|
| Host wheel | Python package plus the `pvisor`, `ppilot`, and `pchronicle` entry points | Python APIs, controlled Agent execution, Run orchestration, and trajectory data workflows |

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

Both installation commands install the matching `pvisor`, `ppilot`, and
`pchronicle` entry points into the Python environment's scripts directory.

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

The wheel bundles matched components. Use `pvisor` for one Run and its execution
environment, `ppilot` for bounded collections of Runs, and `pchronicle` for
Dataset catalog, SQL, analysis, exchange, and read-only serving.

`ppilot` is also installed from source through `just install-cli`.

### Cargo installation from source

```bash
git clone https://github.com/DeepLink-org/Persisting.git
cd Persisting
just install-cli
```

This alternative installs matching command-line components into the Cargo
binary directory without installing the Python package.

### Component overrides

Set `PERSISTING_PVISOR_BIN` to select an explicit pVisor binary.

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
pchronicle --help
```

## Dependencies

| Package | Version | Required | Purpose |
|---|---|---|---|
| `pulsing` | >=0.1.0 | Yes | Distributed actor runtime for the control plane |
| `lance` | >=0.9.0 | Optional (`[lance]`) | Lance columnar storage |
| `pyarrow` | >=14.0.0 | Optional (`[lance]`) | Apache Arrow |

## Next steps

- [Run your first Agent](pvisor/get-started.md) — complete the run, review, and selective-apply loop
- [Choose a workflow](overview.md) — start with pVisor or pChronicle
- [Explore pVisor](pvisor/index.md) or [pChronicle](pchronicle/index.md) by product
