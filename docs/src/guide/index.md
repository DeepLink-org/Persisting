# Choose a Capability

Start from the job you need to accomplish. The guides describe supported
workflows; architecture pages explain internal choices and experimental work.

## Choose by outcome

| You want to… | Start with |
|--------------|------------|
| Run one Agent with a reviewable workspace | [pVisor: run / review / checkpoint](../design/cli-pvisor.md) (design) |
| Orchestrate many Agent Runs with recovery | [pPilot: run / produce](../design/cli-ppilot.md) (design) |
| Browse, query, import, or export trajectory Datasets | [pChronicle command reference](../design/cli-pchronicle.md) |
| Query or analyze trajectory Datasets | [pChronicle CLI](../design/cli-pchronicle.md) |
| Record agent LLM calls | [Capture](capture.md) |
| Control HTTP/HTTPS egress for proxy-aware Agent tools | [OverlayNet](overlaynet.md) |
| Stream events with persistence | [Queue](queue.md) |
| Plug in custom storage | [Custom Backends](custom-backends.md) |
| Reproduce a measurable result | [Examples](examples.md) |

Most guides pair with a runnable example under `examples/`. Each script clears
its `.work/` directory, runs the product commands in order, and prints the
generated files and reports directly. `just examples` runs them all.

## Capability maturity

| Capability | What it provides | Maturity |
|---|---|---|
| [pVisor](../design/cli-pvisor.md) | One Run's execution, control, and transactional workspace | Implemented |
| [pPilot](../design/cli-ppilot.md) | Batch orchestration, durable results, and Run production | Implemented |
| [pChronicle](../design/cli-pchronicle.md) | Dataset catalog, bounded SQL/analysis, exchange, local read-only UI | Implemented |
| [Capture](capture.md) | LLM traffic capture into Lance and Markdown views | Implemented |
| [OverlayNet](overlaynet.md) | Cooperative HTTP/HTTPS proxy policy and bandwidth control | Implemented |
| [Queue](queue.md) | Persistent event stream and KV-style access | Stable |
| [Custom Backends](custom-backends.md) | Queue storage backend extension points | Reference |

## How the pieces relate

pVisor, pPilot, and pChronicle are the agent infrastructure: pVisor runs one
Run, pPilot schedules and recovers many, pChronicle keeps the canonical
history. Gateway, OverlayNet, Control, and OverlayFS are runtime drivers
assembled by pVisor. Queue is a separate capability-specific data system that
is independently useful and not required by the agent runtime. See
[Architecture & Internals](../design/index.md) for the implementation model and
maturity notes.
