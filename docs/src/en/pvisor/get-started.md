# Run your first Agent

This path takes you from an empty project to a reviewed change. Each step leaves
you with a useful checkpoint, so you can stop before adding more power.

!!! tip "The pVisor loop"

    **Run → review → choose → continue.** The Agent works in a staged view;
    your project changes only when you apply an Effect.

## Before you start

You need macOS or Linux, a project directory, and an Agent command such as
`codex`. Install the CLI and confirm both product entry points:

```bash
pip install persisting
pvisor --help
pchronicle --help
```

On macOS, install macFUSE before using a staged host workspace:

```bash
brew install --cask macfuse
```

See [Installation](../installation.md) for source builds, VM support, and
platform requirements.

## 1. Run one Agent in a stage

From the project directory, start with one explicit stage:

```bash
pvisor run --stage ./runs/task-001 -- codex
```

Replace `codex` with your Agent command. The Agent edits the staged view while
the base project stays unchanged. When the command finishes, you have a Run
Bundle to inspect.

!!! success "Checkpoint: the base is still safe"

    Check `git status` in the base project. The Agent's edits should not appear
    there until you apply them.

## 2. Review what actually happened

Start with the summary, then inspect the staged view:

```bash
pvisor review last
pvisor inspect last -- git status --short
```

Review file Effects, effective controls, network evidence, and warnings before
deciding what crosses the boundary. A successful command does not mean every
requested capability was available; the Run Bundle records the mechanisms that
actually applied.

## 3. Apply one small, trusted change

Apply a path first. Everything else remains staged:

```bash
pvisor apply last --path src
pvisor review last
```

You can apply another dependency-closed selection later:

```bash
pvisor apply last --include 'tests/**' --exclude 'tests/generated/**'
```

Finish with `pvisor apply last --all`, or discard the remaining Effects with
`pvisor drop last`.

!!! success "Checkpoint: you control the boundary"

    The accepted batch is in the real project. The remaining batch is still
    reviewable and can be applied, inspected, or dropped independently.

## 4. Choose the next layer

Only add the control you need for the next Run:

- [Apply changes repeatedly and keep checkpoints](guides/review-apply.md)
- [Choose a host, OCI, or VM execution environment](guides/execution.md)
- [Control network access](guides/network.md)
- [Capture the Run as pChronicle history](guides/capture.md)
- [Replay or compare a sandbox](guides/sandbox-replay.md)

For the complementary history workflow, continue with [Explore your first
Dataset](../pchronicle/get-started.md).
