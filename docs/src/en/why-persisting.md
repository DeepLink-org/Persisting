# Why Persisting

Agent systems can produce useful work before they produce a useful record of
what happened. Persisting closes that gap.

## The problem

An Agent changes files, calls tools, reads data, and makes decisions across a
long-running session. A terminal transcript is too shallow to review safely;
an isolated sandbox without a durable record is hard to learn from; a raw event
log is difficult to query consistently.

Persisting treats execution and history as two related but independent jobs:

- **pVisor governs the Run.** It gives an Agent a staged workspace, records the
  controls that were actually active, and lets a person review Effects before
  applying them.
- **pChronicle preserves the trajectory.** It normalizes supported Sources into
  queryable Datasets so teams can inspect, compare, and improve Runs later.

## The product promise

Every workflow should make three things easy to answer:

1. What was the Agent allowed to do?
2. What actually changed or happened?
3. Which evidence and history support the answer?

Persisting does not claim that a successful command proves a perfect boundary.
It records the mechanisms, limitations, Effects, and evidence that were
actually available.

## When Persisting fits

Use Persisting when an Agent can change a real project, when a Run needs human
review before merge, or when trajectory history should remain useful after the
terminal session ends. Start with pVisor for controlled execution, pChronicle
for existing history, or connect both when the question crosses the execution
and history boundaries.

If you only need a one-off script with no review or history requirement,
Persisting may be more infrastructure than the task needs.

## The design direction

Persisting is built around explicit boundaries, inspectable evidence, reversible
writes, and portable data. These principles guide the [system design](system-design/index.md)
and the current [roadmap](roadmap.md).
