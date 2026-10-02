# Why Persisting

Agent systems can produce useful work before they produce a useful record of
what happened. Persisting closes that gap.

## The problem

An Agent changes files, calls tools, reads data, and makes decisions across a
long-running session. A terminal transcript is too shallow to review safely;
an isolated sandbox without a durable record is hard to learn from; a raw event
log is difficult to query consistently.

Persisting focuses on durable Agent history:

- **pChronicle preserves the trajectory.** It normalizes supported Sources into
  queryable Datasets so teams can inspect, compare, and improve Runs later.

## The product promise

Every workflow should make three things easy to answer:

1. Which Agent, model, and tool calls were recorded?
2. What actually changed or happened?
3. Which evidence and history support the answer?

Answers remain tied to the recorded Sources and their versions. Missing
records remain a limit on what can be concluded.

## When Persisting fits

Use pChronicle when trajectory history should remain useful after a terminal
session ends.

If you only need a one-off script with no review or history requirement,
Persisting may be more infrastructure than the task needs.

## The design direction

Persisting is built around explicit data ownership, inspectable Sources,
versioned snapshots, and portable data. These principles guide the [system design](system-design/index.md)
and the current [roadmap](roadmap.md).
