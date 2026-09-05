# pVisor concepts

pVisor is built around an **Agent Run**, not a process, container, or virtual
machine. Read these concepts before comparing providers or interpreting a Run
Bundle.

Follow these articles in order:

1. [What is an AgentVisor?](agentvisor.md) explains the product category and
   the boundary between an Agent and its runtime.
2. [Run, Attempt, and Effect](run-model.md) defines the stable objects that
   survive process and provider changes.
3. [Capabilities and evidence](capabilities-and-evidence.md) explains how a
   request becomes an installed mechanism and a claim in the Run Bundle.

The category article is implementation-neutral. The Run and capability pages
define pVisor's stable user model. Platform mechanisms and current gaps belong
to [pVisor Design](../design/index.md).
