# Documentation Product Boundaries Design

## Purpose

Align Persisting's public documentation with the product and architecture
boundaries already present in the implementation and accepted RFCs. The
documentation should describe value, technical architecture, and current
technical behavior. It must not present market strategy, competitive analysis,
or an unimplemented roadmap as product fact.

The documentation must make two independent product domains clear:

- pVisor governs one Agent Run, its execution environment, capabilities,
  Effects, and Evidence. pPilot extends this domain to many Runs.
- pChronicle is the structured trajectory and Dataset data layer. It accepts
  native events and supported external representations, preserves source and
  lineage, and exposes normalized read and exchange surfaces.

The domains integrate through stable Run identity, events, artifacts, terminal
facts, lineage, and Evidence. Neither product requires the other to provide its
core standalone value.

## Problems in the Current Documentation

The low-level design documents mostly define ownership correctly, but the
top-level narrative presents a single mandatory sequence:

```text
pVisor execution -> pPilot scale -> pChronicle history
```

That sequence is one supported integration path, not the entire architecture.
It has three misleading consequences:

1. It makes pVisor appear to be the only entry point into Persisting.
2. It makes pChronicle appear to be only the historical tail of a pVisor Run,
   although pChronicle also discovers and normalizes external trajectory
   Sources and exchange formats.
3. It obscures pVisor's independent Run and Effect-governance product loop and
   pChronicle's independent Dataset and trajectory-data product loop.

The existing `execution-story.svg` reinforces the same linear dependency by
placing pChronicle as step four after pVisor and pPilot.

## Documentation Model

### Top-level value statement

The root README, site landing page, and overview will describe Persisting as
two connected capabilities:

- governed Agent execution with reviewable Effects and explicit Evidence;
- durable, structured trajectory Datasets with preserved origin, normalized
  views, lineage, query, and exchange.

The documentation may show the combined path from governed execution to a
queryable Dataset, but it must label that path as an integration rather than a
mandatory lifecycle.

### Stable ownership

The following ownership remains unchanged:

| Domain | Owns | Does not own |
| --- | --- | --- |
| pVisor | Run and Attempt lifecycle, admission, execution placement, capabilities, Effects, runtime Evidence | Dataset query, persistent interpretation, many-Run planning |
| pPilot | planning, bounded execution, leases, retry, recovery, and reconciliation for many Runs | Agent reasoning, provider enforcement, trajectory formats |
| pChronicle | Dataset and Source discovery, canonical events and terminal facts, normalized projections, revision lineage, query, and exchange | starting, scheduling, or controlling Runs |
| Runtime drivers | concrete filesystem, network, Gateway, AgentCtl, and execution mechanisms under pVisor | product-level Dataset semantics or logical Run ownership |

### Independent entry paths

The documentation will show both supported entry paths:

```text
Agent / task
  -> pVisor governed Run
  -> optional pPilot orchestration
  -> events, artifacts, terminal facts, Evidence
  -> pChronicle Dataset

External evaluator / Agent framework / exchange files
  -> importer, adapter, or Gateway-observed input
  -> pChronicle Source
  -> Catalog Snapshot and normalized Dataset views
```

Both paths converge on pChronicle's existing Dataset model. The documentation
will not claim a new global Dataset control plane or a new quality subsystem.

### Evidence and guarantee boundaries

Documentation will distinguish evidence by source:

| Source path | Supported claim | Explicit non-claim |
| --- | --- | --- |
| External file or imported Source | discovered content, pinned Source version, normalized representation, and recorded conversion lineage where implemented | completeness of an external task manifest or absence of unreported trajectories |
| Gateway capture | requests and responses observed and durably published through the configured Gateway path | absence of traffic that bypassed Gateway |
| pVisor Run | Run/Attempt identity, recorded terminal facts, installed mechanisms, observed Effects, and provider-specific Evidence | enforcement a selected provider did not supply |
| pPilot job | persisted task/Run mapping, retry and lease history, and terminal result behavior supported by its selected mode | physical exactly-once execution |

Claims must remain no stronger than current executable behavior, public types,
contract tests, and supported guides.

## Files and Changes

### Root and landing documentation

Review and update:

- `README.md`
- `docs/src/index.md`
- `docs/src/index.zh.md`
- `docs/overrides/home.html`
- `docs/src/overview.md`
- `docs/src/overview.zh.md`

Replace the mandatory four-step lifecycle with two independent product entry
points and one explicit integration path. Keep concise runnable examples for
both pVisor and pChronicle. Do not add strategy or roadmap prose.

### Product documentation

Review and update:

- `docs/src/pvisor/index.md`
- `docs/src/pvisor/index.zh.md`
- `crates/persisting-pvisor/README.md`
- `docs/src/pchronicle/index.md`
- `docs/src/pchronicle/index.zh.md`
- `docs/src/pchronicle/concepts/index.md`
- `docs/src/pchronicle/concepts/index.zh.md`
- `crates/persisting-pchronicle/README.md`
- `crates/persisting-ppilot/README.md`

pVisor pages must retain a complete standalone loop from Run admission through
Effect review and Run Bundle output. pChronicle pages must begin with Dataset
and trajectory-data value while retaining the current path-first identity,
Catalog Snapshot, canonical/projection, and read-only query semantics.

### System design

Review and update the English and Chinese system-design index and architecture
pages. Preserve the existing ownership tables and failure semantics while
showing external producers as first-class pChronicle inputs. Keep pPilot in the
pVisor many-Run domain.

Accepted RFCs record design decisions and will not be rewritten merely to
match new wording. They change only if a direct contradiction with current
implementation is found.

## Diagram Design

Architecture diagrams will use maintainable SVG under
`docs/src/assets/diagrams/`. Raster images will not be introduced for system
architecture.

### Cross-product diagram

Replace or substantially revise `persisting/execution-story.svg`. The new SVG
must show:

- pVisor and pChronicle as separate product domains rather than consecutive
  numbered stages;
- pPilot inside or adjacent to the pVisor many-Run path;
- external evaluators, Agent frameworks, and supported files as direct inputs
  to pChronicle;
- the explicit event/artifact/terminal-fact/Evidence handoff from pVisor to
  pChronicle;
- shared identities and contracts without implying that pChronicle controls
  execution;
- text alternatives through `<title>` and `<desc>`.

If the filename `execution-story.svg` becomes misleading after the redesign,
create a more precise SVG name and update all references rather than retaining
an inaccurate name solely for compatibility.

### Product diagrams

Review `persisting/pchronicle-product.svg` to ensure its producer list includes
external evaluators and Agent frameworks, and that its arrows distinguish
ingest/write paths from read surfaces. It must not imply that every Dataset is
created by Gateway or pVisor.

Review `pvisor/agentvisor-architecture.svg` to ensure pChronicle appears as an
optional durable data handoff, not an internal pVisor component. Preserve the
pVisor-specific architecture and provider placement detail.

Review other referenced SVGs for contradictory ownership or arrow direction.
Unrelated diagrams should not be restyled merely for visual consistency.

### SVG quality rules

- Keep a valid `viewBox`, descriptive `<title>` and `<desc>`, and readable text
  at documentation-column width.
- Use consistent color and typography with the existing diagram set.
- Avoid arrows crossing boxes or encoding an undocumented control dependency.
- Use solid arrows for data/control flow only when direction is meaningful;
  label important boundaries directly.
- Do not embed external fonts, scripts, raster images, or inaccessible text
  rendered only as paths.
- Ensure diagrams render on both the documentation background and GitHub's
  Markdown view.

## Validation

The documentation change will be checked with targeted validation:

1. Search for obsolete one-lifecycle and pVisor-only entry wording.
2. Check English and Chinese paired pages for semantic agreement.
3. Parse all changed SVG files as XML and inspect rendered output.
4. Run the repository's targeted documentation build and link checks.
5. Verify referenced CLI examples against current command documentation or
   executable help when commands change.
6. Review the final diff to ensure it contains value, architecture, and
   technical detail only—no market strategy or unimplemented feature claims.

## Scope Exclusions

This work will not modify or expand TTAS, tiered memory, Queue, samplers,
Search, or the standalone `persisting-dlcapt` component. It will not introduce
a Dataset quality engine, Trust Report, hosted control plane, evaluator,
scoring system, or new runtime API. It is a documentation consistency change;
implementation gaps discovered during review will be reported rather than
silently documented as completed behavior.
