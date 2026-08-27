# Project

Persisting's public positioning spans model state and Agent history. This
section records delivery state, durable decisions, contributor workflows, and
systems outside the current pVisor → pChronicle product path.

## Architecture

- [System overview](../system-design/index.md)
- [End-to-end architecture](../system-design/architecture.md)
- [Local-to-fleet contracts](../system-design/local-to-fleet.md)
- [Security and evidence](../system-design/security-evidence.md)

## Build and release

- [Engineering notes](engineering.md)
- [Releasing Persisting](releasing.md)
- [Reproducible examples](examples.md)

## Decisions

- [RFC index](../rfcs/index.md)
- [Implementation status](engineering.md)

## Standalone data systems

Queue and its Python API remain independent of the Agent execution path:

- [Queue guide](../guide/queue.md)
- [Queue API](../api/queue.md)
- [Custom Queue backends](../guide/custom-backends.md)

Historical queue-era design notes are retained outside the published site in
the repository's `docs/archive/` directory.
