# pVisor

**pVisor runs an existing Agent command inside a controlled execution
environment.** It gives each Run its own workspace boundary, records the
controls that were actually installed, and lets you review filesystem changes
before they reach the project.

Within Persisting's model-state-to-Agent-history story, pVisor owns the
execution boundary and the reviewable record of one Run.

pVisor does not replace the Agent's reasoning loop. You can keep using Agent
CLIs, scripts, and frameworks you already have.

## Run, review, decide

From a project directory:

```bash
pvisor -- codex
pvisor review last
pvisor apply last --path src
```

With `--stage ./runs/task-001`, the Agent writes to a staged view of the project. After the Run,
you can apply all changes, accept selected paths in several batches, or discard
the stage:

```bash
pvisor apply last --all
# or
pvisor drop last
```

The exact filesystem and network boundary depends on the platform and chosen
executor. pVisor records the effective controls so that a Run is not described
as more isolated than it was.

## Choose a task

| I want to... | Start with |
| --- | --- |
| Complete one staged Agent Run | [Run your first Agent](get-started.md) |
| Review and selectively accept changes | [Review and apply](guides/review-apply.md) |
| Choose host, container, or VM execution | [Execution layouts](guides/execution.md) |
| Control network access | [Network policy](guides/network.md) |
| Capture trajectory data | [Capture trajectories](guides/capture.md) |
| Look up exact command syntax | [CLI reference](reference/cli.md) |

pVisor's local run-review-apply loop works on its own. pChronicle is optional:
use it when you want to retain and query trajectory Datasets after a Run.

## Keep reading

- [Run your first Agent](get-started.md)
- [Learn the pVisor concepts](concepts/index.md)
- [Follow practical guides](guides/index.md)
- [Inspect runtime and isolation design](design/index.md)
- [Explore trajectory history with pChronicle](../pchronicle/index.md)
