# Documentation Product Boundaries Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Align Persisting's public documentation and architecture diagrams with its two independent product domains while preserving current technical ownership and guarantees.

**Architecture:** Rewrite only the cross-product narrative and the product entry pages that currently imply a mandatory pVisor-to-pChronicle lifecycle. Keep pVisor's governed-execution loop and pChronicle's Dataset data model independently complete, then describe their integration through stable Run identity, events, artifacts, terminal facts, lineage, and Evidence. Redesign the affected architecture diagrams as accessible, maintainable SVG and validate both source semantics and rendered output.

**Tech Stack:** Markdown, MkDocs Material/Jinja templates, hand-authored SVG 1.1/XML, repository `just` documentation tasks, Poppler or available SVG renderer for visual inspection.

## Global Constraints

- Describe value, technical architecture, and current technical behavior only; do not add market strategy, competitive analysis, partnership strategy, or roadmap prose.
- Do not claim a Dataset quality engine, Trust Report, hosted control plane, evaluator, scoring system, or any other unimplemented capability.
- Preserve pVisor as an independent governed-execution product and pChronicle as an independent trajectory/Dataset data product.
- Preserve pPilot as the many-Run orchestration path associated with pVisor.
- Claims must be no stronger than executable help, public types, contract tests, supported guides, and accepted RFC ownership.
- Use maintainable SVG for architecture diagrams; do not introduce raster architecture assets, scripts, embedded fonts, or text converted to paths.
- Keep English and Chinese paired pages semantically aligned.
- Do not modify TTAS, tiered memory, Queue, samplers, Search, or the standalone `persisting-dlcapt` component.
- Preserve unrelated user changes and avoid unrelated documentation cleanup.

---

## File Map

| File group | Responsibility |
| --- | --- |
| `docs/src/assets/diagrams/persisting/system-products.svg` | New cross-product diagram showing independent pVisor and pChronicle entry paths and their integration |
| `docs/src/assets/diagrams/persisting/execution-story.svg` | Remove after all references move to the accurately named replacement |
| `docs/src/assets/diagrams/persisting/pchronicle-product.svg` | pChronicle-specific producer, Dataset, core, and read/exchange boundaries |
| `docs/src/assets/diagrams/pvisor/agentvisor-architecture.svg` | pVisor-specific control/containment architecture and optional durable handoff |
| `README.md`, `docs/src/index*.md`, `docs/overrides/home.html`, `docs/src/overview*.md` | Top-level value, entry paths, runnable examples, and navigation |
| `docs/src/pchronicle/index*.md`, `docs/src/pchronicle/concepts/index*.md`, `crates/persisting-pchronicle/README.md` | Dataset-first pChronicle value and current technical boundary |
| `docs/src/pvisor/index*.md`, `crates/persisting-pvisor/README.md`, `crates/persisting-ppilot/README.md` | Independent pVisor loop and precise pPilot relationship |
| `docs/src/system-design/index*.md`, `docs/src/system-design/architecture*.md` | Cross-product ownership, two ingress paths, and source-specific evidence limits |

### Task 1: Establish the Cross-Product SVG Contract

**Files:**
- Create: `docs/src/assets/diagrams/persisting/system-products.svg`
- Delete after reference migration: `docs/src/assets/diagrams/persisting/execution-story.svg`
- Reference: `docs/superpowers/specs/2026-08-16-documentation-product-boundaries-design.md`

**Interfaces:**
- Consumes: Current ownership in `docs/src/system-design/architecture.md` and accepted pChronicle ownership in `docs/src/rfcs/0003-pchronicle-ownership.md`.
- Produces: One reusable cross-product SVG referenced by the root README, landing page, overview, and system-design pages.

- [ ] **Step 1: Record the obsolete diagram references and semantic assertions**

Run:

```bash
rg -n 'execution-story\.svg|one lifecycle|One execution model|一个生命周期' README.md docs/src docs/overrides
```

Expected: references in the root README, landing template, overview pages, and system-design pages; the current SVG describes four numbered sequential stages.

- [ ] **Step 2: Create the new SVG with two independent domains**

Create `system-products.svg` with a `viewBox="0 0 1400 780"`, `role="img"`, `aria-labelledby="title desc"`, and visible groups containing exactly these concepts:

```text
Inputs
  Agent / task ---------------------> pVisor: governed execution
  Evaluator / framework / files ----> pChronicle: trajectory Dataset data

pVisor domain
  pPilot: plan · lease · retry · reconcile
  pVisor: Run · Attempt · capabilities · Effects · Evidence
  Host · Container · libkrun VM providers

Integration handoff
  events · artifacts · terminal facts · lineage · Evidence

pChronicle domain
  Sources · canonical facts · Catalog Snapshot
  normalized views · revisions · query · exchange

Shared contract
  stable Run identity where present; source identity remains visible
```

Use separate outer boxes for pVisor and pChronicle. The pVisor-to-pChronicle arrow must be one-way and labelled as a data/evidence handoff. The external-producer arrow must enter pChronicle directly. No arrow may point from pChronicle into pVisor's control path.

- [ ] **Step 3: Validate the SVG source**

Run:

```bash
python -c 'import xml.etree.ElementTree as E; p="docs/src/assets/diagrams/persisting/system-products.svg"; r=E.parse(p).getroot(); assert r.tag.endswith("svg"); assert r.get("viewBox"); assert r.get("role")=="img"; ids={e.get("id") for e in r.iter()}; assert {"title","desc"} <= ids'
```

Expected: exit 0.

- [ ] **Step 4: Render and inspect the SVG**

Use the first available renderer:

```bash
mkdir -p /tmp/persisting-doc-diagrams
rsvg-convert docs/src/assets/diagrams/persisting/system-products.svg -o /tmp/persisting-doc-diagrams/system-products.png
```

If `rsvg-convert` is unavailable, use:

```bash
inkscape docs/src/assets/diagrams/persisting/system-products.svg --export-filename=/tmp/persisting-doc-diagrams/system-products.png
```

Inspect the PNG at full size and a documentation-column width near 900 px. Confirm no clipped text, overlapping arrows, illegible labels, or false control dependency.

- [ ] **Step 5: Commit the diagram contract**

```bash
git add docs/src/assets/diagrams/persisting/system-products.svg
git commit -m "docs: add cross-product architecture diagram"
```

### Task 2: Rewrite the Top-Level Entry Paths

**Files:**
- Modify: `README.md`
- Modify: `docs/src/index.md`
- Modify: `docs/src/index.zh.md`
- Modify: `docs/overrides/home.html`
- Modify: `docs/src/overview.md`
- Modify: `docs/src/overview.zh.md`
- Delete: `docs/src/assets/diagrams/persisting/execution-story.svg`

**Interfaces:**
- Consumes: `docs/src/assets/diagrams/persisting/system-products.svg` from Task 1.
- Produces: Two equally valid top-level entry paths and updated references to the cross-product diagram.

- [ ] **Step 1: Replace the root README's single lifecycle**

Replace `The product follows one lifecycle` and its numbered sequence with:

```text
Persisting has two connected product domains:

- pVisor virtualizes and governs Agent execution. pPilot extends the same Run
  contract to many independent Runs.
- pChronicle turns native and external trajectory Sources into durable,
  queryable Datasets with preserved origin, normalized views, and lineage.

They integrate through stable Run identity, canonical events, artifacts,
terminal facts, and Evidence, but each product also has a standalone entry
path.
```

Keep one concise pVisor example and one concise pChronicle Dataset example. Keep Gateway, OverlayFS, OverlayNet, and Control described as pVisor runtime drivers.

- [ ] **Step 2: Update landing metadata and hero copy in both languages**

Set the English metadata and hero meaning to:

```text
Title: Persisting — governed Agent execution and durable trajectory Datasets
Description: Govern Agent Runs and turn native or external trajectories into durable, queryable Datasets with explicit identity, lineage, and Evidence.
```

Set the Chinese metadata and hero to the same meaning. Replace the single primary pVisor call to action with two peer actions:

```text
Run an Agent safely / 安全运行 Agent
Explore a Dataset / 查看轨迹 Dataset
```

Retain the product overview action. Do not add marketing comparisons or future promises.

- [ ] **Step 3: Replace the landing page's numbered four-stage grid**

Use two product-domain cards and one integration card:

```text
pVisor — governed execution
Run and Attempt identity; capabilities; staged Effects; provider-specific Evidence.

pChronicle — trajectory Datasets
Sources; canonical facts; Catalog Snapshots; normalized query and exchange views; revision lineage.

Integrated path
pPilot scales governed Runs; events, artifacts, terminal facts, lineage, and Evidence become pChronicle inputs.
```

Add a pChronicle Dataset quick-start block alongside the pVisor run/review/apply block. Change the diagram source to `system-products.svg`.

- [ ] **Step 4: Rewrite the English and Chinese overviews around two paths**

Use matching section order:

1. `Two product domains / 两个产品域`
2. `Govern Agent execution / 治理 Agent 执行`
3. `Build trajectory Datasets / 构建轨迹 Dataset`
4. `Use the integrated path / 使用集成路径`
5. `Guarantees remain source-specific / 保证取决于来源`

Keep current, runnable pVisor and pChronicle command examples. State that external Sources can enter pChronicle without pVisor, and that pVisor produces a useful Run Bundle without requiring pChronicle at runtime.

- [ ] **Step 5: Migrate references and remove the obsolete diagram**

Run:

```bash
rg -l 'execution-story\.svg' README.md docs/src docs/overrides
```

Update every listed reference to `system-products.svg`, adjust alt text to `Persisting product domains and integration`, then delete `execution-story.svg`.

- [ ] **Step 6: Check top-level wording**

Run:

```bash
rg -n 'one lifecycle|一个生命周期|organized around one stable object|只围绕一个稳定对象|execution-story\.svg' README.md docs/src/index.md docs/src/index.zh.md docs/src/overview.md docs/src/overview.zh.md docs/overrides/home.html docs/src/system-design
```

Expected: no matches.

- [ ] **Step 7: Commit the top-level rewrite**

```bash
git add README.md docs/src/index.md docs/src/index.zh.md docs/overrides/home.html docs/src/overview.md docs/src/overview.zh.md docs/src/system-design docs/src/assets/diagrams/persisting/system-products.svg docs/src/assets/diagrams/persisting/execution-story.svg
git commit -m "docs: expose independent product entry paths"
```

### Task 3: Make pChronicle Explicitly Dataset-First

**Files:**
- Modify: `docs/src/pchronicle/index.md`
- Modify: `docs/src/pchronicle/index.zh.md`
- Modify: `docs/src/pchronicle/concepts/index.md`
- Modify: `docs/src/pchronicle/concepts/index.zh.md`
- Modify: `docs/src/pchronicle/concepts/dataset-and-source.md`
- Modify: `docs/src/pchronicle/concepts/dataset-and-source.zh.md`
- Modify: `crates/persisting-pchronicle/README.md`
- Modify: `docs/src/assets/diagrams/persisting/pchronicle-product.svg`

**Interfaces:**
- Consumes: Existing `DatasetCatalogSnapshot`, Source-local identity, canonical/projection, query, import/export, and revision-lineage contracts.
- Produces: A Dataset-first pChronicle entry point without new runtime or quality claims.

- [ ] **Step 1: Rewrite the pChronicle opening contract in both languages**

Use this English meaning and a semantically identical Chinese version:

```text
pChronicle is Persisting's structured trajectory and Dataset data layer. It
discovers native and supported external Sources on local storage or S3,
preserves canonical facts and origin, exposes normalized Run views, and
supports bounded query, analysis, revision lineage, and format exchange.
```

Keep the explicit non-ownership statement: pChronicle does not execute or schedule Agents.

- [ ] **Step 2: Clarify Dataset and Source semantics**

Add to `dataset-and-source*.md`:

```text
A Dataset is a discovery, snapshot, query, and exchange boundary. It does not
claim that every expected external task produced a Source. pChronicle reports
the Sources it can discover and pin; it does not infer unreported trajectories.
```

Do not redefine Dataset as a global mutable control-plane object. Preserve `(dataset_uri, source_path, entity_kind, original_id)`.

- [ ] **Step 3: Align the concepts indexes and crate README**

Change `history`-only wording to `trajectory data` where the statement includes external Sources and Dataset operations. Retain `durable history` where the text specifically discusses canonical events or terminal facts. Preserve all current CLI examples and physical storage details.

- [ ] **Step 4: Redesign the pChronicle product SVG**

Keep the existing dark theme and accessible SVG metadata. Update the producer group to:

```text
External evaluators · Agent frameworks · exchange files
Gateway · pVisor · native writers
```

Show this flow:

```text
Producers -> Sources / Dataset URI -> pChronicle Core
pChronicle Core -> CLI query/analysis
pChronicle Core -> import/export and revisions
pChronicle Core -> read-only Warehouse API/Web
```

Label pChronicle Core with only implemented behaviors: discover, pin Snapshot, normalize, query, persist canonical events, and exchange. Do not add quality scoring or task completeness.

- [ ] **Step 5: Validate and render the pChronicle SVG**

Run the XML assertion from Task 1 against `pchronicle-product.svg`, render it to `/tmp/persisting-doc-diagrams/pchronicle-product.png`, and inspect it for clipping, readable producer labels, and correct arrow direction.

- [ ] **Step 6: Commit the pChronicle boundary update**

```bash
git add docs/src/pchronicle crates/persisting-pchronicle/README.md docs/src/assets/diagrams/persisting/pchronicle-product.svg
git commit -m "docs: define pchronicle as trajectory dataset core"
```

### Task 4: Preserve pVisor Independence and Align System Ownership

**Files:**
- Modify: `docs/src/pvisor/index.md`
- Modify: `docs/src/pvisor/index.zh.md`
- Modify: `crates/persisting-pvisor/README.md`
- Modify: `crates/persisting-ppilot/README.md`
- Modify: `docs/src/system-design/index.md`
- Modify: `docs/src/system-design/index.zh.md`
- Modify: `docs/src/system-design/architecture.md`
- Modify: `docs/src/system-design/architecture.zh.md`
- Modify if needed after review: `docs/src/assets/diagrams/pvisor/agentvisor-architecture.svg`

**Interfaces:**
- Consumes: Current Run/Attempt/Effect/Evidence ownership and the source-specific evidence table in the approved spec.
- Produces: Standalone pVisor loop plus precise cross-product handoffs and non-guarantees.

- [ ] **Step 1: Make the standalone pVisor result explicit**

In both pVisor index pages and the crate README, state that a pVisor Run completes with reviewable staged Effects and a private versioned Run Bundle. Describe pChronicle as the standard durable Dataset/history handoff, not a runtime prerequisite.

Preserve this ownership:

```text
RunSpec -> admission -> Attempt -> Effect review/apply/drop -> Run Bundle
```

- [ ] **Step 2: Correct pPilot ownership wording**

Review `crates/persisting-ppilot/README.md` against `docs/src/pvisor/design/orchestration.md` and public CLI behavior. Ensure pPilot owns planning, bounded execution, leases, retry/recovery, reconciliation, and task-to-Run mapping, while pChronicle owns durable canonical trajectory storage and Dataset operations. Describe any pChronicle control child as a storage/control implementation dependency, not transfer of orchestration ownership.

- [ ] **Step 3: Add both ingress paths to system design**

Update English and Chinese system-design pages to show:

```text
Agent goal -> pVisor / pPilot -> events + artifacts + terminal facts + Evidence
                                                   |
External Sources -> importer / adapter ------------+-> pChronicle Dataset
```

Retain the existing ownership and failure tables. Add the approved source-specific guarantees without claiming external manifest closure.

- [ ] **Step 4: Review the pVisor SVG for optional handoff semantics**

Inspect `agentvisor-architecture.svg`. If the right-side pChronicle box and arrow can be read as a mandatory internal component, change its label to `Optional durable handoff` and label the arrow `events · artifacts · terminal facts · Evidence`. Do not restructure pVisor's internal lifecycle, capabilities, Effects, checkpoint, Evidence, drivers, or provider placement.

- [ ] **Step 5: Validate paired ownership wording**

Run:

```bash
rg -n 'pChronicle owns durable leases|pChronicle.*schedule|pVisor.*Dataset query|must use pChronicle|必须使用 pChronicle' crates/persisting-ppilot/README.md crates/persisting-pvisor/README.md docs/src/pvisor docs/src/system-design
```

Expected: no incorrect ownership matches. Any legitimate non-ownership sentence may remain after manual inspection.

- [ ] **Step 6: Validate and render the pVisor SVG if changed**

Parse the SVG as XML, render it to `/tmp/persisting-doc-diagrams/agentvisor-architecture.png`, and inspect the pChronicle handoff arrow and all bottom provider labels for clipping.

- [ ] **Step 7: Commit product ownership alignment**

```bash
git add docs/src/pvisor docs/src/system-design crates/persisting-pvisor/README.md crates/persisting-ppilot/README.md docs/src/assets/diagrams/pvisor/agentvisor-architecture.svg
git commit -m "docs: clarify execution and dataset ownership"
```

### Task 5: Verify Documentation and Diagram Consistency

**Files:**
- Modify only when validation reveals an in-scope inconsistency: files changed in Tasks 1–4

**Interfaces:**
- Consumes: All documentation and SVG changes from Tasks 1–4.
- Produces: A link-clean, buildable, visually inspected documentation set with no obsolete narrative.

- [ ] **Step 1: Parse every architecture SVG**

Run:

```bash
python -c 'from pathlib import Path; import xml.etree.ElementTree as E; files=list(Path("docs/src/assets/diagrams").rglob("*.svg")); [E.parse(p) for p in files]; print(f"parsed {len(files)} SVG files")'
```

Expected: all SVG files parse successfully.

- [ ] **Step 2: Search for obsolete cross-product claims and asset references**

Run:

```bash
rg -n 'one lifecycle|一个生命周期|organized around one stable object|只围绕一个稳定对象|execution-story\.svg|pChronicle owns durable leases' README.md docs/src docs/overrides crates/persisting-pvisor/README.md crates/persisting-pchronicle/README.md crates/persisting-ppilot/README.md
```

Expected: no matches.

- [ ] **Step 3: Check that strategy language did not enter public docs**

Run:

```bash
rg -n -i 'market gap|competitive|competitor|partnership strategy|6.?12 month|roadmap|killer feature|市场空白|竞争对手|合作策略|杀手级|产品路线' README.md docs/src docs/overrides crates/persisting-pvisor/README.md crates/persisting-pchronicle/README.md crates/persisting-ppilot/README.md
```

Expected: no new matches in changed public documentation. Existing explicitly labelled roadmap material outside the changed files is out of scope and should be inspected rather than mechanically deleted.

- [ ] **Step 4: Run formatting and link validation**

Run:

```bash
git diff --check
just docs-links
```

Expected: both commands exit 0. `docs-links` performs the strict MkDocs build used by this repository.

- [ ] **Step 5: Inspect the built landing and product pages**

Run `just docs-build`, then inspect the generated English and Chinese landing, overview, pVisor, pChronicle, and system-design pages. Confirm:

- both pVisor and pChronicle are visible as independent entry paths;
- the new SVGs render at page width without clipping;
- English and Chinese navigation targets resolve;
- no page claims that external Sources prove absent or unreported trajectories;
- no page makes pChronicle a pVisor runtime requirement.

- [ ] **Step 6: Review the complete diff against the approved spec**

Run:

```bash
git diff --stat HEAD~4..HEAD
git diff HEAD~4..HEAD -- README.md docs/src docs/overrides crates/persisting-pvisor/README.md crates/persisting-pchronicle/README.md crates/persisting-ppilot/README.md
```

Confirm every changed paragraph describes current value, architecture, or technical detail. Remove any speculative feature claim or unrelated rewrite.

- [ ] **Step 7: Commit validation fixes if any**

If validation required edits:

```bash
git add README.md docs/src docs/overrides crates/persisting-pvisor/README.md crates/persisting-pchronicle/README.md crates/persisting-ppilot/README.md
git commit -m "docs: finish product boundary consistency checks"
```

If no edits were required, do not create an empty commit.
