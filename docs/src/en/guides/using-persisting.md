# Using Persisting

Choose the smallest workflow that answers the question in front of you. You do
not need to adopt every component at once.

## I already have trajectory data

Start with [pChronicle](../pchronicle/get-started.md): open a Dataset, inspect a
summary, ask one bounded SQL question, and locate the evidence behind the
answer. Use exchange or serving guides only after the read-only path works.

## I need to capture new model traffic

Use the [Gateway](../pchronicle/guides/serve-gateway.md) to forward requests
and persist canonical events. External execution components can submit events
through the pChronicle Control service.

## A reliable operating habit

1. Start with one Run or one Dataset.
2. Record the exact command, path, and provider.
3. Review the result before applying, exporting, or sharing it.
4. Keep the Source and evidence location with any conclusion.
5. Move to automation only after the manual path is repeatable.

For implementation context, read the [design principles](../system-design/design-principles.md).
