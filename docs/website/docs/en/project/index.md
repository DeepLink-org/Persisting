# Project

Persisting's public positioning spans model state and Agent history. This
section records delivery state, durable decisions, contributor workflows, and
systems outside the current pVisor and pChronicle product path.

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
- [Contributor workflows](engineering.md)

## Standalone data systems

Queue and its Python API remain independent of the Agent execution path:

- Queue guide
- Queue API remains outside the default product documentation
- Custom Queue backends remain outside the default product documentation

Historical queue-era design notes are retained outside the published site in
the repository's `docs/archive/` directory.
