# pChronicle client

Lightweight, versioned contracts and process client for `pchronicle control`.
It contains no Lance, Arrow, DataFusion, or object-store implementation.

Orchestrators depend on `ChronicleControl`; the production client starts a
standalone pChronicle process and communicates over an authenticated loopback
channel. `MemoryChronicleControl` is available for deterministic orchestration
tests that do not require crash persistence.
