# pVisor concepts

pVisor is built around an **Agent Run**, not a process, container, or virtual
machine. Read these concepts before comparing providers or interpreting a Run
Bundle.

!!! note "When this section helps"

    Come here after the first Run when a command succeeded but you need to know what
    was actually isolated, which changes are still staged, or why two execution
    providers make different guarantees.

Follow these articles in order:

1. [Run, Attempt, and Effect](run-model.md) defines one execution, its attempts,
   and the changes it produces.
2. [Capabilities and evidence](capabilities-and-evidence.md) explains how a
   request becomes an installed mechanism and a claim in the Run Bundle.
3. [What is an AgentVisor?](agentvisor.md) explains the product category and
   the boundary between an Agent and its runtime.

AgentVisor is a category definition, not a pVisor feature list. The Run and capability pages
define pVisor's stable user model. Platform mechanisms and current gaps belong
to [pVisor Design](../design/index.md).

After this section you should be able to read a Run Bundle without confusing a
requested capability with an enforced one. Continue to [practical guides](../guides/index.md)
when you are ready to make a provider or policy decision.
