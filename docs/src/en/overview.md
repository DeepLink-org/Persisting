---
hide:
  - toc
---

# Get started

Capture, import, and query durable Agent history with pChronicle.

## 1. Installation

Install the command-line tools and confirm the pChronicle entry point:

```bash
pip install persisting
pchronicle --help
```

[Read the installation guide →](installation.md)

## 2. Recording and Analyzing Agent Trajectories

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
to record new model traffic.
