# Run your first Agent

This walkthrough completes one useful cycle: install Persisting, run an Agent
inside a staged environment, inspect its effects, and selectively accept them.
It assumes macOS or Linux.

## 1. Install the CLI

The wheel installs the current Persisting command-line entry points together:

```bash
pip install persisting[lance]
```

To use the current nightly build instead:

```bash
curl -fsSL https://raw.githubusercontent.com/DeepLink-org/Persisting/main/scripts/install-nightly.sh | bash
```

On macOS, install macFUSE once before using a staged host workspace:

```bash
brew install --cask macfuse
```

Confirm the entry points:

```bash
pvisor --help
pchronicle --help
```

See [Installation](../installation.md) for source builds, VM support, platform
requirements, and component overrides.

## 2. Run one Agent

From a project directory:

```bash
pvisor -- codex
```

Replace `codex` with another Agent command if needed. `--stage ./runs/task-001` creates a
Run-owned stage for workspace writes and installs the supported platform
controls. It does not silently describe every platform as providing the same
isolation; the Run Bundle records filesystem, network, and other capability
evidence separately.

During the Run, the Agent edits its staged view. The base project is unchanged.

## 3. Review the effects

```bash
pvisor review last
pvisor inspect last -- git status --short
```

Review the file changes, network counters, effective controls, and warnings
before accepting anything.

## 4. Accept a subset

Apply one area first:

```bash
pvisor apply last --path src
```

The rest remains staged. Review again and select another batch:

```bash
pvisor review last
pvisor apply last --include 'tests/**' --exclude 'tests/generated/**'
```

Finish by accepting everything that remains, or discard it:

```bash
pvisor apply last --all
# or
pvisor drop last
```

This separation is the core local workflow: the Agent can operate without an
approval prompt for every edit, while the user controls which effects enter the
real project.

## 5. Choose where to continue

- [Understand the Persisting product overview](../overview.md)
- [Learn selective, repeatable apply](guides/review-apply.md)
- [Choose a host, container, or VM layout](guides/execution.md)
- [Control network access](guides/network.md)
- [Capture Agent trajectories](guides/capture.md)
- [Explore durable history](../pchronicle/get-started.md)
