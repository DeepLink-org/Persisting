---
title: Start here
sidebar_label: Start here
---

# Start here

## Persistent infrastructure for the Agent era

Run Agents inside a reviewable execution boundary, then preserve accepted
decisions and resulting history as a queryable Dataset. Persisting turns Agent
work from an opaque process into a durable workflow you can inspect, approve,
and remember.

Its core value is simple:

- **Govern execution:** isolate a Run and record the capabilities and Evidence that actually applied.
- **Review change:** keep Effects staged until a person decides what enters the project.
- **Remember outcomes:** preserve selected execution facts and trajectories as traceable, queryable history.

Persisting gives you two independent product paths. Choose the one that matches the work in front of you:

- [Run an Agent safely with pVisor](pvisor/get-started.md): run the Agent in a staged workspace, inspect its changes, and write only what you approve into the project.
- [Explore durable history with pChronicle](pchronicle/get-started.md): open trajectory data, run a read-only query, and know which data and which source you are reading.
- [Choose a workflow](overview.md): decide which path to take, and how execution and history can optionally connect.

If you are evaluating the system, start with [Choose a workflow](overview.md), then follow the matching product walkthrough.

## What you will have after the first walkthrough

- **pVisor**: the Agent has stopped; its changes remain in a staging directory; you deliberately write them into the project or discard them. The project changes only when you choose to write.
- **pChronicle**: you have run a read-only query against trajectory data and know which data and which source you inspected.

You do not need both products to begin. Add the capture handoff only when you need to correlate one execution with durable trajectory history.

## Before you start

Install the CLI with the [installation guide](installation.md). Use pVisor when
you have a local project and an Agent command to run; use pChronicle when you
already have trajectory data or want to try its temporary onboarding Dataset.
