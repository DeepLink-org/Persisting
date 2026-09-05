# Troubleshoot a Run

When a Run does not behave as expected, start with the Run Bundle instead of
repeating the command with stronger flags. The Bundle records the boundary that
was requested, the mechanisms that were installed, and the warnings that limit
what the Run can claim.

## Start with three read-only checks

Run these from the project that started the Agent:

```bash
pvisor status last
pvisor inspect last -- git status --short
pvisor review last
```

`status` tells you whether the Run is still active or stopped. `inspect` runs a
read-only command in the Run view, so it helps distinguish a staged change from
a change already present in the base project. `review` shows the durable Run
Bundle and its Evidence before any apply or drop decision.

## The Agent changed the project directly

Check whether the command used a stage:

```bash
pvisor run --stage ./runs/task-001 -- AGENT_COMMAND
```

Without a stage, the host executor may still provide safe-best-effort controls,
but there is no staged filesystem Effect to review and selectively apply. If a
staged Run was expected, inspect the recorded stage path and the executor
warnings before rerunning it.

## A requested capability was not enforced

Treat a requested option as intent, not evidence. Open the Run Bundle and look
for the effective capability record and its mechanism. Provider support varies
by operating system and executor; a cooperative network proxy, for example,
does not make every ambient connection impossible.

Continue with [Capabilities and evidence](../concepts/capabilities-and-evidence.md)
and [Execution environments](execution.md) before changing the command.

## The stage is empty or the wrong files appear

Check the command's working directory and stage path first:

```bash
pvisor inspect last -- pwd
pvisor inspect last -- git status --short
```

The Agent edits the Run-owned view. A command that writes outside that view may
be reported as an external Effect or may be unavailable to the executor. Keep
the stage path inside the intended project boundary and avoid comparing a
generated Run directory with the project root by filename alone.

## Capture or Dataset output is missing

Execution review and trajectory capture are separate decisions. First confirm
that the Run completed and its Bundle is readable. Then check the capture
configuration and the destination Dataset with pChronicle:

```bash
pchronicle ls ./trajectory-data
pchronicle analysis overview ./trajectory-data
```

If the Dataset is not present, read [Capture Agent trajectories](capture.md)
and verify the configured destination before starting another Run. A local Run
Bundle is not automatically a pChronicle Dataset.

## Before opening an issue

Include the pVisor version, operating system, executor, the exact command, and
the relevant `status` and `review` output. Remove credentials and private
workspace contents. The most useful report explains which capability was
requested, which mechanism the Bundle records, and where the observed result
differs.
