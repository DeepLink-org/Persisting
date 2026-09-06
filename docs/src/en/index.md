---
title: Start here
sidebar_label: Start here
---

# Start here

Persisting gives you two independent product paths. Choose the one that matches the work in front of you:

- [Run an Agent safely with pVisor](pvisor/get-started.md): stage workspace changes, inspect the Run Bundle, and apply only what you approve.
- [Explore durable history with pChronicle](pchronicle/get-started.md): open a Dataset, run a read-only query, and follow Source lineage.
- [Understand the product boundary](overview.md): see how execution and history connect without becoming one opaque system.

If you are evaluating the system, start with [Choose a workflow](overview.md), then follow the product walkthrough that matches your data.

## What you will have after the first walkthrough

- A **pVisor** walkthrough ends with a stopped Run, a readable Run Bundle, and
  a deliberate apply or drop decision. Your project is changed only when you
  choose to apply the staged Effect.
- A **pChronicle** walkthrough ends with a read-only Dataset query and a clear
  distinction between the Dataset, its Source, and the Snapshot being read.

You do not need both products to begin. Add the capture handoff only when you
need to correlate execution evidence with durable trajectory history.

## Before you start

Install the CLI with the [installation guide](installation.md). Use pVisor when
you have a local project and an Agent command to run; use pChronicle when you
already have trajectory data or want to try its temporary onboarding Dataset.
