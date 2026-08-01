# pPilot

**Durable Run Orchestrator.**

pPilot is a first-class Persisting component alongside pVisor and pChronicle:

- pPilot plans, schedules, resumes, and reconciles many Runs;
- pVisor owns execution and the lifecycle of each Run/Attempt;
- pChronicle owns canonical Run history and derived views.

pPilot consumes Run contracts and results. It does not own Agent protocol
adaptation, execution drivers, or trajectory storage formats.
