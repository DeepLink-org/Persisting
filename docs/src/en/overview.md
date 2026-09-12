---
hide:
  - toc
---

# Get started

Follow one path from an installed CLI to an Agent run you can review and query.

## 1. Installation

Install the command-line tools and confirm both product entry points:

```bash
pip install persisting
pvisor --help
pchronicle --help
```

On macOS, install macFUSE before using a staged host workspace:

```bash
brew install --cask macfuse
```

[Read the installation guide →](installation.md)

## 2. Running an Agent with pVisor

Run an Agent in a staged workspace, inspect what actually happened, and apply
only the changes you trust:

```bash
pvisor run --stage ./runs/task-001 -- codex
pvisor review last
pvisor apply last --path src
```

The base project stays unchanged while the Agent works. The Run Bundle records
filesystem Effects, effective controls, network evidence, and warnings. Continue
with [Run your first Agent](pvisor/get-started.md) for the complete walkthrough,
then learn [selective apply](pvisor/guides/review-apply.md).

**At the end of this section:** you have a reviewed project change and a clear
record of what remains staged.

## 3. Recording and Analyzing Agent Trajectories

Once an Agent has run, use pChronicle to turn its trajectory into a Dataset you
can inspect and query. Start with a temporary example so the workflow is safe:

```bash
pchronicle onboard
```

The onboarding walks through listing data, checking a summary, and asking one
read-only SQL question. Then repeat the same path with your own data:

```bash
pchronicle onboard ./trajectory-data
pchronicle query ./trajectory-data \
  --sql 'SELECT session_id, COUNT(*) AS steps FROM dataset.steps GROUP BY session_id'
```

Continue with [Explore your first Dataset](pchronicle/get-started.md) to learn
Dataset health, evidence location, formats, exchange, and the read-only Web/API.

**At the end of this section:** you can connect an answer to the Dataset and
Source that produced it. If you need the two products together, continue with
[pVisor capture](pvisor/guides/capture.md).
