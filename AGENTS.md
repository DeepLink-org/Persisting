# Persisting Agent Instructions

## Default project scope

Unless the user explicitly asks otherwise, treat the following subsystems as
out of scope for analysis, planning, implementation, refactoring, testing, and
documentation work:

- TTAS and tiered tensor memory
- Queue and its samplers
- Search
- the standalone `persisting-dlcapt` / `dlcapt` component

Do not modify these subsystems, expand their APIs, fix their tests, or include
their failures in the acceptance criteria for an unrelated task. Prefer
targeted build, lint, and test commands over workspace-wide commands when a
workspace-wide command would pull them into scope.

This exclusion covers, in particular:

- TTAS and tiered-memory code
- `persisting/queue/`, `persisting/sampler/`, and their tests and docs
- `persisting/search/`, `crates/persisting-pchronicle/src/search/`, Search CLI
  surfaces, and their tests and docs
- `crates/persisting-dlcapt/` and dlcapt-specific scripts, tests, features, and
  workflows

The default active scope is the Agent infrastructure centered on pVisor,
pPilot, pChronicle, Gateway, Control, OverlayFS, OverlayNet, and trajectory CLI
surfaces. `persisting-dlcapt` is a separate standalone component; excluding it
does not exclude Gateway trajectory capture or pChronicle capture storage.

Enter an excluded subsystem only when:

1. the user explicitly names that subsystem in the current task; or
2. an in-scope change cannot be completed without a minimal dependency-boundary
   adjustment there.

For the second case, keep the adjustment minimal and explain why it is
required. Do not use incidental cleanup as a reason to broaden scope.
