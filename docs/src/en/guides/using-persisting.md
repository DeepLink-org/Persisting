# Using Persisting

Choose the smallest workflow that answers the question in front of you. You do
not need to adopt every component at once.

## I need an Agent to change a project safely

Start with [pVisor](../pvisor/get-started.md): run one Agent in a staged
workspace, review the Run Bundle, and apply only a trusted path. Add network or
provider controls when the next Run needs them.

## I already have trajectory data

Start with [pChronicle](../pchronicle/get-started.md): open a Dataset, inspect a
summary, ask one bounded SQL question, and locate the evidence behind the
answer. Use exchange or serving guides only after the read-only path works.

## I need execution and history together

Use [pVisor capture](../pvisor/guides/capture.md) when lifecycle events from a
Run should become a pChronicle Source. The private Run Bundle remains a local
execution record; capture is an explicit handoff, not an implicit copy of every
artifact.

## A reliable operating habit

1. Start with one Run or one Dataset.
2. Record the exact command, path, and provider.
3. Review the result before applying, exporting, or sharing it.
4. Keep the Source and evidence location with any conclusion.
5. Move to automation only after the manual path is repeatable.

For implementation context, read the [design principles](../system-design/design-principles.md).
