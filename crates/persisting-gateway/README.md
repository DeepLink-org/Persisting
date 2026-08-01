# persisting-gateway

`persisting-gateway` is pVisor's built-in Agent protocol driver. It implements
`persisting-overlaynet::OverlaySink` and owns the application-level path from
LLM HTTP exchanges to trajectory events:

- recognize and adapt supported Agent/LLM protocols;
- select and forward to upstream providers;
- correlate run, session, story, and call identities;
- capture canonical `pChronicle` events;
- coordinate WAL and live human-readable projections.

The crate does not own the proxy data plane or the canonical trajectory storage
format. `persisting-overlaynet` owns proxy transport, access enforcement, and
generic sink dispatch. `persisting-pchronicle` owns schemas, persistence,
reading, replay, conversion, and derived views.

Capture remains the name of the user-facing capability and `traj capture`
command. Gateway is an internal pVisor driver and a reusable crate, not a peer
of pVisor, pPilot, or pChronicle in the top-level product architecture.
