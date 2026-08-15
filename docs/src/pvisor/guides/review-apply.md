# Review and apply Agent changes

An Agent can work freely inside a staged workspace without receiving automatic
permission to modify the base project. After the Run, you decide which effects
cross that boundary.

## Run with a manual stage

The short form is:

```bash
pvisor run --safe codex
```

For explicit paths and commit behavior:

```bash
pvisor run \
  --overlayfs-base "$PWD" \
  --overlayfs-stage /tmp/my-agent-stage \
  --overlayfs-commit manual \
  -- codex
```

Use a separate stage for every concurrent Run.

## Review before accepting

```bash
pvisor review last
pvisor inspect last -- git status --short
```

`review` summarizes the Run Bundle, file effects, network evidence, and safety
warnings. `inspect` runs a read-only inspection command against the staged view.

## Apply only selected files

Selections can be path-based or pattern-based:

```bash
pvisor apply last --path src
pvisor apply last --include 'tests/**' --exclude 'tests/generated/**'
```

A filtered apply consumes only the selected dependency-closed batch. Opaque
directories and hard-link groups are not split when doing so would produce an
invalid result.

## Apply more than once

Applying a subset does not close the stage. Review what remains, then apply
another subset:

```bash
pvisor review last
pvisor apply last --path docs
pvisor review last
pvisor apply last --all
```

Successful batches are recorded in `apply-ledger.json`. You can stop at any
point and discard the remaining changes:

```bash
pvisor drop last
```

## Continue from an accepted point

Use a checkpoint before starting another line of work:

```bash
pvisor checkpoint last --name accepted-base
pvisor fork last --checkpoint accepted-base -- codex
```

The new Run receives its own identity and stage while preserving lineage to the
checkpoint.

## What this boundary does not cover

Selective apply governs staged filesystem effects. It does not undo a message,
payment, deployment, direct database write, or external API mutation. Those
effects require admission before execution, provider-specific containment when
available, and durable evidence afterward.

Next: [control network access](network.md) or [capture the Run](capture.md).
