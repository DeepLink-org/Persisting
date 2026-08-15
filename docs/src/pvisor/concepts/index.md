# pVisor concepts

pVisor is built around an **Agent Run**, not a process, container, or virtual
machine. Read these concepts before comparing providers or interpreting a Run
Bundle.

| Question | Concept article |
| --- | --- |
| What infrastructure category virtualizes Agent execution? | [What is an AgentVisor?](agentvisor.md) |
| What survives process and provider changes? | [Run, Attempt, and Effect](run-model.md) |
| How is authority requested and enforcement reported? | [Capabilities and evidence](capabilities-and-evidence.md) |

The category article is implementation-neutral. The Run and capability pages
define pVisor's stable user model. Platform mechanisms and current gaps belong
to [pVisor Design](../design/index.md).
