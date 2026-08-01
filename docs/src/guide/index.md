# Choose a Capability

Start from the job you need to accomplish. The guides describe supported user
workflows; architecture pages explain internal choices and experimental work.

## Choose by outcome

| You want to… | Start with |
|--------------|------------|
| Store/retrieve parameters or KV cache by tensor subscript | [Tensor Memory](tensor-memory.md) |
| Record agent LLM calls | [Capture](capture.md) |
| Stream events with persistence | [Queue](queue.md) |
| Index and search documents | [Search](search.md) |
| Plug in custom storage | [Custom Backends](custom-backends.md) |

## Capability maturity

| Capability | What it provides | Maturity |
|---|---|---|
| [Capture](capture.md) | Capture LLM traffic into Lance and Markdown views | Stable |
| [Search](search.md) | Document indexing and vector/hybrid retrieval | Stable |
| [Queue](queue.md) | Persistent event stream and KV-style access | Stable |
| [Tensor Memory](tensor-memory.md) | Tensor subscript API and host/SSD block storage | Experimental |
| [Custom Backends](custom-backends.md) | Queue storage backend extension points | Reference |

## How the pieces relate

Capture, Search, and Queue are independently useful tools. pPilot is currently
an internal orchestration library, not a supported CLI workflow. Tensor
Memory is an experimental storage substrate with TTAS addressing; it is not a
required dependency for the stable tools. See [Architecture & Internals](../design/index.md)
when you need the implementation model or roadmap.
