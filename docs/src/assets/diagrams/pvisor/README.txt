pVisor documentation diagrams

These SVG files are source assets, not generated build output. They are kept
inside the MkDocs docs_dir so the website and repository README can reference
the same versioned diagrams.

- agentvisor-architecture.svg: product ownership and the boundary between
  pVisor, pPilot, pChronicle, and execution providers.
- effect-promotion.svg: separation of Run-internal permission from promotion
  into the real environment.
- local-to-cluster.svg: stable AgentVisor semantics across the current local
  product and the target cluster control plane.

Keep status labels honest when the implementation changes. Solid current paths
and dashed product-gate paths must stay consistent with
docs/src/design/agentvisor.md.
