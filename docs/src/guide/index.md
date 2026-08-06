# Choose a Capability

Start from the job you need to accomplish. The guides describe supported
workflows; architecture pages explain internal choices and experimental work.

## Choose by outcome

| You want to… | Start with |
|--------------|------------|
| Run one Agent with a reviewable workspace | [pVisor: run / review / checkpoint](../design/cli-pvisor.md) (design) |
| Orchestrate many Agent Runs with recovery | [pPilot: run / produce](../design/cli-ppilot.md) (design) |
| Query trajectory history with SQL | [pPilot: query / analysis](../design/cli-ppilot.md) (design) |
| Record agent LLM calls | [Capture](capture.md) |
| Control HTTP/HTTPS egress for proxy-aware Agent tools | [OverlayNet](overlaynet.md) |
| Store/retrieve parameters or KV cache by tensor subscript | [Tensor Memory](tensor-memory.md) |
| Stream events with persistence | [Queue](queue.md) |
| Index and search documents | [Search](search.md) |
| Plug in custom storage | [Custom Backends](custom-backends.md) |
| Reproduce a measurable result | [Examples](examples.md) |

Most guides pair with a runnable example under `examples/`. Each script clears
its `.work/` directory, runs the product commands in order, and prints the
generated files and reports directly. `just examples` runs them all.

## Capability maturity

| Capability | What it provides | Maturity |
|---|---|---|
| [pVisor](../design/cli-pvisor.md) | One Run's execution, control, and transactional workspace | Implemented |
| [pPilot](../design/cli-ppilot.md) | Batch orchestration, durable results, SQL analysis | Implemented |
| [pChronicle](../design/trajectory.md) | Canonical events, Storyline/ATIF, S3 storage | Implemented |
| [Capture](capture.md) | LLM traffic capture into Lance and Markdown views | Implemented |
| [OverlayNet](overlaynet.md) | Cooperative HTTP/HTTPS proxy policy and bandwidth control | Implemented |
| [Search](search.md) | Document indexing and vector/hybrid retrieval | Stable |
| [Queue](queue.md) | Persistent event stream and KV-style access | Stable |
| [Tensor Memory](tensor-memory.md) | Tensor subscript API and host/SSD block storage | Experimental |
| [Custom Backends](custom-backends.md) | Queue storage backend extension points | Reference |

## How the pieces relate

pVisor, pPilot, and pChronicle are the agent infrastructure: pVisor runs one
Run, pPilot schedules and recovers many, pChronicle keeps the canonical
history. Gateway, OverlayNet, Control, and OverlayFS are runtime drivers
assembled by pVisor. Tensor Memory, Queue, and Search are separate
capability-specific data systems — independently useful, not required by the
agent runtime. See [Architecture & Internals](../design/index.md) for the
implementation model and maturity notes.
