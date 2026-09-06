# Installation

Persisting gives you two public command-line paths:

- `pvisor` runs an Agent inside a reviewable execution boundary.
- `pchronicle` opens, queries, exchanges, and serves trajectory Datasets.

The recommended install includes the Lance-backed Dataset features used by the
pChronicle walkthroughs.

## 1. Install the tools

```bash
pip install 'persisting[lance]'
```

Verify that the commands are available:

```bash
pvisor --version
pchronicle --help
```

The wheel installs matching versions of the Python package and the public CLI
entry points into the active Python environment. Use a virtual environment when
the project has other Python dependencies:

```bash
python3 -m venv .venv
source .venv/bin/activate
python -m pip install --upgrade pip
pip install 'persisting[lance]'
```

:::tip You can start with either product
You do not need a pVisor Run to explore pChronicle. If you want to run an Agent
first, continue with [Run your first Agent](pvisor/get-started.md). If you
already have trajectory data, continue with [Explore your first Dataset](pchronicle/get-started.md).
:::

## 2. Check platform requirements

The CLI supports macOS and Linux with Python 3.10 or newer. A normal host Run
works without a filesystem extension. On macOS, install macFUSE before using a
host-process `pvisor run --safe` workflow:

```bash
brew install --cask macfuse
```

Approve the macFUSE system extension when macOS asks. If the required mount
capability is unavailable, `--safe` fails closed rather than writing directly
to the project workspace. The libkrun VM executor does not require macFUSE.

## 3. Install from source when needed

Use the nightly wheel when you need the latest build published from `main`:

```bash
curl -fsSL https://raw.githubusercontent.com/DeepLink-org/Persisting/main/scripts/install-nightly.sh | bash
```

For local development, install the Python package from a checkout:

```bash
git clone https://github.com/DeepLink-org/Persisting.git
cd Persisting
pip install -e '.[lance]'
```

A source build of the CLI components is also available:

```bash
just install-cli
```

Use `PERSISTING_PVISOR_BIN` only when you deliberately need to test a specific
pVisor binary. Keep the Python package and CLI from the same revision when
debugging provider behavior.

## 4. Enable VM or OCI execution when needed

The default local workflow does not require Docker or Podman. To run an OCI
image through the VM executor, provide an image explicitly:

```bash
pvisor run --image ubuntu:latest -- COMMAND
```

`ubuntu:latest` is also the default VM image. `--image-store DIR` changes the
local content-addressed cache, `--overlayfs-target` selects the guest workspace,
and `--vm-rootfs DIR` points to a prepared Linux rootfs. Linux hosts use KVM;
Apple Silicon macOS hosts use HVF. Building the VM support from source on macOS
also requires Zig:

```bash
brew install zig
```

Treat these options as a separate platform step. First complete the staged host
workflow so that you have a baseline Run Bundle to compare against.

## 5. Choose the next step

- [Run your first Agent](pvisor/get-started.md) — stage, review, and selectively apply changes.
- [Explore your first Dataset](pchronicle/get-started.md) — query temporary data before preparing a source.
- [Choose a workflow](overview.md) — decide which product matches the work in front of you.
- [Execution environments](pvisor/guides/execution.md) — compare host, OCI, and VM boundaries.
