# Installation

This repository distributes the `pchronicle` command and embedded Web UI.

## Install

```bash
pip install persisting
pchronicle --version
pchronicle onboard
```

Platform wheels support Linux x86_64 and Apple Silicon macOS with Python 3.10+.
They install the Python package and `pchronicle` into the active environment.

## Nightly or source builds

```bash
curl -fsSL https://raw.githubusercontent.com/DeepLink-org/Persisting/main/scripts/install-nightly.sh | bash
```

From a checkout, `pip install -e .` builds pChronicle and its Web assets.
`just install-cli` installs the Rust CLI; building embedded Web assets requires
Dioxus CLI. See [Engineering notes](project/engineering.md).

Continue with [Explore your first Dataset](pchronicle/get-started.md) or
