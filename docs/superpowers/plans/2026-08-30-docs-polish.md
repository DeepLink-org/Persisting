# 文档体系打磨实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 将 Persisting 的 README 与文档体系打磨至顶级开源项目水平，移除令人困惑的文档，为对外发布与增长拉新做准备。

**Architecture:** 三层信息架构——顶层 README 负责快速吸引（uv/Ruff 风格），MkDocs 文档站负责分层深入（Overview → Get Started → Concepts → Guides → Design → Reference 契约），组件 README 面向贡献者。遗留重定向桩统一归档至 `docs/archive/legacy-nav/`，pPilot 三页孤立内容接入导航成为第三个产品小节。

**Tech Stack:** MkDocs Material + i18n 插件（en/zh 双语，`file.md` / `file.zh.md` 后缀约定）、just（`docs-build` / `docs-links`）、git mv 保留历史。

**Spec:** `docs/superpowers/specs/2026-08-30-docs-polish-design.md`（已获用户确认）

## Global Constraints

- **AGENTS.md 排除范围**：TTAS、Queue 及 sampler、Search、`persisting-dlcapt` 的文档**内容不修改、不翻译、不重构**。涉及文件：`docs/src/guide/queue.md(.zh.md)`、`docs/src/guide/custom-backends.md(.zh.md)`、`docs/src/api/index.md`、`docs/src/api/queue.md`、`crates/persisting-dlcapt/README.md`。这些文件仅允许随导航/构建健康做最小位移，本轮实际不动。
- **双语约定**：每个面向用户的页面必须有 `.zh.md` 中文版本；**例外**：`docs/src/rfcs/` 下除 `index.md` 外的 RFC 正文保持英文（ADR 惯例，用户已确认），Queue 排除项保持现状。
- **术语一致性**：中文译文遵循 `docs/mkdocs.yml` 的 `nav_translations` 与现有译文惯例（Run、Effect、Dataset、Capture 等术语保留英文）。
- **验证基线**：每个结构性改动后运行 `just docs-build && just docs-links`（strict），必须零警告通过。
- **README 自动化标记**：`README.md` 中的 `<!-- pchronicle-benchmark:start -->` / `<!-- pchronicle-benchmark:end -->` 块由 `benchmark/pchronicle/bench.py` 自动更新，**必须原样保留标记**。
- **重定向桩模板**：`template: redirect.html` 来自 Material 主题（`material/templates/redirect.html`），frontmatter 用 `location: <相对URL>`。
- **交付**：单 PR，每个 Task 一个 commit；commit message 遵循仓库现有风格（观察：`Refactor documentation to ...`、`feat: ...`）。
- **文档源事实顺序**（来自 docs/README.md）：`--help` 输出 > 用户指南/命令参考 > 架构页 > RFC > 研究笔记。命令示例必须与二进制 `--help` 一致。

---

### Task 1: 提交规格、归档遗留文档、修复 site_url

**Files:**
- Create: `docs/archive/legacy-nav/`（接收 38 个文件）
- Modify: `docs/mkdocs.yml`（site_url）
- Modify: `docs/archive/README.md`
- Delete: `docs/product/`（空目录）
- Move: `docs/pchronicle-design-review.md` → `docs/superpowers/reviews/2026-08-23-pchronicle-design-review.md`

- [ ] **Step 1: 提交规格与计划文档**

```bash
cd /Users/reiase/workspace/Persisting
git add docs/superpowers/specs/2026-08-30-docs-polish-design.md docs/superpowers/plans/2026-08-30-docs-polish.md
git commit -m "docs: add documentation polish spec and implementation plan"
```

- [ ] **Step 2: 归档重定向桩**

```bash
cd /Users/reiase/workspace/Persisting/docs
mkdir -p archive/legacy-nav
git mv src/design archive/legacy-nav/design
git mv src/dev archive/legacy-nav/dev
git mv src/quickstart.md archive/legacy-nav/quickstart.md
# guide/ 下仅移动重定向桩，保留 queue 与 custom-backends 内容页
mkdir -p archive/legacy-nav/guide
cd src/guide
git mv capture.md capture.zh.md examples.md examples.zh.md history.md history.zh.md \
  index.md index.zh.md orchestrate.md orchestrate.zh.md overlaynet.md overlaynet.zh.md \
  pvisor-execution.md pvisor-execution.zh.md review-apply.md review-apply.zh.md \
  ../../archive/legacy-nav/guide/
```

- [ ] **Step 3: 删除空目录、归位评审文档**

```bash
cd /Users/reiase/workspace/Persisting/docs
rm -f product/.DS_Store && rmdir product
git mv pchronicle-design-review.md superpowers/reviews/2026-08-23-pchronicle-design-review.md
```

- [ ] **Step 4: 修复 mkdocs.yml 的 site_url**

```yaml
# docs/mkdocs.yml 第 4 行
site_url: https://deeplink-org.github.io/Persisting/
```

- [ ] **Step 5: 更新 docs/archive/README.md**

在现有内容后追加：

```markdown

## legacy-nav/

Redirect stubs from the pre-restructure `/design/`, `/guide/`, `/dev/`, and
`/quickstart` URL space. The current navigation under `pvisor/`, `pchronicle/`,
`ppilot/`, `project/`, and `system-design/` has absorbed their targets, so the
stubs no longer serve external links. Archived 2026-08-30; excluded from the
MkDocs build because they live outside `src/`.
```

- [ ] **Step 6: 验证构建**

Run: `just docs-build && just docs-links`
Expected: 零警告通过；`docs/site/` 中不再生成 `design/`、`dev/`、`quickstart.html` 页面（`guide/` 仍生成 queue 与 custom-backends 两页）。

- [ ] **Step 7: Commit**

```bash
git add -A docs/
git commit -m "docs: archive legacy redirect stubs and fix mkdocs site_url"
```

---

### Task 2: pPilot 接入文档站导航

**Files:**
- Move: `docs/src/pvisor/guides/orchestrate.md(.zh.md)` → `docs/src/ppilot/guides/orchestrate.md(.zh.md)`
- Move: `docs/src/pvisor/design/orchestration.md` → `docs/src/ppilot/design/orchestration.md`
- Move: `docs/src/pvisor/reference/ppilot-cli.md` → `docs/src/ppilot/reference/cli.md`
- Create: `docs/src/ppilot/index.md`、`docs/src/ppilot/get-started.md`
- Modify: `docs/mkdocs.yml`（nav + nav_translations）

**Interfaces:**
- Produces: `ppilot/index.md`、`ppilot/get-started.md` 的英文定稿——Task 8 据此翻译中文版本。

- [ ] **Step 1: 移动文件**

```bash
cd /Users/reiase/workspace/Persisting/docs/src
mkdir -p ppilot/guides ppilot/design ppilot/reference
git mv pvisor/guides/orchestrate.md pvisor/guides/orchestrate.zh.md ppilot/guides/
git mv pvisor/design/orchestration.md ppilot/design/
git mv pvisor/reference/ppilot-cli.md ppilot/reference/cli.md
```

- [ ] **Step 2: 查找并修复入链**

Run: `rg -n "orchestrate|orchestration|ppilot-cli" docs/src --type md -g '!ppilot/**'`
对每一处指向旧路径的链接，改为新的 `ppilot/...` 相对路径。被移动文件内部的相对链接（如 `ppilot/reference/cli.md` 中的 `../../pchronicle/reference/cli.md`）深度不变，保持有效；逐一打开三个被移动文件确认链接仍然正确。

- [ ] **Step 3: 新写 `docs/src/ppilot/index.md`**

```markdown
# pPilot

**Durable Run production at scale.**

pPilot extends the Run model from one execution to a bounded collection of
tasks. It owns planning, bounded concurrency, leases and fencing decisions,
infrastructure retry and recovery, reconciliation, durable result publication,
and task-to-Run mapping.

It does not redefine the Agent runtime: each task remains an independent
[pVisor Run](../pvisor/concepts/run-model.md), executed by the standalone
`pvisor` binary.

| Command | Owns |
| --- | --- |
| `ppilot run` | execute a `plan()` / `execute(item)` workload with durable recovery |
| `ppilot produce` | create independent pVisor Runs from a streaming planner |

## Where to start

- [Get Started](get-started.md) — run your first parallel plan in five minutes
- [Orchestrate many Agent Runs](guides/orchestrate.md) — planning, workers, resume, and sinks
- [Orchestration design](design/orchestration.md) — leases, fencing, and recovery guarantees
- [pPilot CLI reference](reference/cli.md) — exact flags and exit behavior
```

- [ ] **Step 4: 新写 `docs/src/ppilot/get-started.md`**

```markdown
# Get Started with pPilot

This page runs the shortest verified pPilot loop: a streaming Python plan
executed by multiple workers, with terminal results written to a durable sink.

## Install

pPilot ships in the same component set as `pvisor` and `pchronicle`. From a
source checkout:

```bash
git clone https://github.com/DeepLink-org/Persisting.git
cd Persisting
just install-cli
ppilot --version
```

## Define the work

Create `plan.py`:

```python
def plan():
    for value in range(6):
        yield {"id": f"square-{value}", "value": value}


def execute(item):
    return {"square": item["value"] ** 2}
```

`plan()` yields work items with stable `id`s; `execute(item)` processes one
item. Stable identity lets an interrupted job resume without repeating
completed work.

## Run it

```bash
ppilot run plan.py --workers 2 --per-worker 2 --sink ./results --results ndjson
```

## Verify the durable result

```bash
cat ./results/ready.ndjson
```

Expected: six result records, one per task, with squares 0, 1, 4, 9, 16, 25
(sum 55). A scripted version of this loop lives in
[`examples/ppilot/01-run/`](https://github.com/DeepLink-org/Persisting/tree/main/examples/ppilot/01-run).

## Where to go next

- [Orchestrate many Agent Runs](guides/orchestrate.md) — resume, retries, and production sinks
- [pPilot CLI reference](reference/cli.md) — `run` and `produce` flags
```

（注：实施时先实际运行一次该示例确认输出格式与 `ready.ndjson` 路径准确——`just install-cli` 后在临时目录执行上述命令。）

- [ ] **Step 5: 更新 mkdocs.yml 导航**

在 `nav:` 的 pVisor 小节之后、pChronicle 之前插入：

```yaml
  - pPilot:
      - Overview: ppilot/index.md
      - Get Started: ppilot/get-started.md
      - Guides:
          - Orchestrate many Agent Runs: ppilot/guides/orchestrate.md
      - Design:
          - Orchestration architecture: ppilot/design/orchestration.md
      - Reference:
          - pPilot CLI: ppilot/reference/cli.md
```

在 `nav_translations` 的 zh 段追加（`Overview`/`Get Started`/`Guides`/`Design`/`Reference` 已有翻译，勿重复添加）：

```yaml
            Orchestrate many Agent Runs: 编排多个 Agent Run
            Orchestration architecture: 编排架构
            pPilot CLI: pPilot CLI
```

- [ ] **Step 6: 验证**

Run: `just docs-build && just docs-links`
Expected: 零警告；导航中出现 pPilot 小节；无指向旧路径的死链。若 strict 构建报告其他页面对旧路径的入链，修复链接；仅当某旧 URL 有外部收录风险时才在原位置留 redirect 桩（默认不留）。

- [ ] **Step 7: Commit**

```bash
git add -A docs/
git commit -m "docs: add pPilot section to site navigation"
```

---

### Task 3: 重写顶层 README（uv/Ruff 风格）

**Files:**
- Modify: `README.md`（整体重写）

- [ ] **Step 1: 核实 PyPI 发布状态**

Run: `curl -fsSL https://pypi.org/pypi/persisting/json | head -c 200`
Expected: 返回 JSON 则已发布，README 加 PyPI 徽章；404 则不加（避免死徽章），并在 Step 2 中跳过对应行。

- [ ] **Step 2: 写入新 README**

完整替换 `README.md` 为以下内容（若 Step 1 确认已发布 PyPI，在徽章行追加
`[![PyPI](https://img.shields.io/pypi/v/persisting.svg)](https://pypi.org/project/persisting/)`）：

```markdown
# Persisting

**Persistent infrastructure for the Agent era.**

[![CI](https://github.com/DeepLink-org/Persisting/actions/workflows/ci.yml/badge.svg)](https://github.com/DeepLink-org/Persisting/actions/workflows/ci.yml)
[![Documentation](https://img.shields.io/badge/docs-latest-blue)](https://deeplink-org.github.io/Persisting/)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

Persisting connects durable model state—parameters and KV caches—with durable
Agent history—trajectories and execution records. The current product provides
two commands:

- **`pvisor`** runs one Agent in a controlled environment and lets you review
  its effects before accepting them;
- **`pchronicle`** browses, queries, exchanges, and serves trajectory Datasets.

Each works on its own; together they preserve a path from execution to
queryable history.

![Current Persisting workflows and optional integration](docs/src/assets/diagrams/persisting/system-products.svg)

## Install

```bash
pip install persisting[lance]
pvisor --version
pchronicle --version
```

The rolling nightly build installs the same commands without a Rust toolchain:

```bash
curl -fsSL https://raw.githubusercontent.com/DeepLink-org/Persisting/main/scripts/install-nightly.sh | bash
```

See the [installation guide](https://deeplink-org.github.io/Persisting/installation/)
for platform requirements and executor setup.

## Run one Agent and review its changes

```bash
pvisor run --safe codex
pvisor review last
pvisor apply last --all   # or: pvisor drop last
```

`--safe` stages workspace changes; nothing enters your project tree before you
accept it. The exact boundary is platform-dependent and recorded with the
Run—consult the [execution guide](https://deeplink-org.github.io/Persisting/pvisor/guides/execution/)
before treating it as a security boundary.

## Query Agent trajectory history

```bash
pchronicle onboard
pchronicle onboard query
pchronicle agent codex ./trajectory-data --ask "Which tools fail most often?"
```

The onboarding flow creates a temporary example Dataset—no source checkout
required. `pchronicle import` accepts ATIF, ACTF, and OpenAI Messages;
`pchronicle serve` starts a loopback-only, read-only Dataset UI and API.

## Current maturity

| Capability | Status |
|---|---|
| pVisor host execution, review, checkpoints, and transactional workspace | Implemented |
| pChronicle local/S3 catalog, bounded SQL, analysis, find, import/export | Implemented |
| pChronicle loopback-only read API and embedded Web UI | Implemented |
| Gateway capture and cooperative proxy policy | Implemented |
| Container/libkrun executors and transparent network boundaries | Platform-dependent; see the pVisor and OverlayNet docs |
| Queue and document Search | Separate stable capabilities |
| Tensor Memory / TTAS | Experimental |

## Documentation

- [Choose a workflow](https://deeplink-org.github.io/Persisting/overview/) — pick the entry point that matches your task
- [Run your first Agent](https://deeplink-org.github.io/Persisting/pvisor/get-started/) — the run-review-apply loop
- [Explore durable history](https://deeplink-org.github.io/Persisting/pchronicle/get-started/) — browse and query a trajectory Dataset
- [Project architecture](https://deeplink-org.github.io/Persisting/system-design/) — ownership and delivery boundaries

Criterion.rs microbenchmarks and hyperfine lifecycle scenarios are compared
against `main` in CI; see the [benchmark contract](benchmark/pchronicle/README.md).

<!-- pchronicle-benchmark:start -->
No nightly benchmark has been published with the unified report format yet.
<!-- pchronicle-benchmark:end -->

## License

[Apache License 2.0](LICENSE). See [`NOTICE`](NOTICE) for third-party
attributions and separately licensed bundled components.
```

注意：benchmark 标记块原样保留（含当前占位文本）；`bench.py` 会更新其内容。

- [ ] **Step 3: 验证链接与命令**

- 逐一核对 README 中的命令与 `--help`：`pvisor run --help`、`pvisor review --help`、`pvisor apply --help`、`pchronicle onboard --help`、`pchronicle agent --help`；
- 确认 4 个文档站 URL 路径与 `docs/src/` 实际页面一致（`overview/`、`pvisor/get-started/`、`pchronicle/get-started/`、`system-design/`）。

- [ ] **Step 4: Commit**

```bash
git add README.md
git commit -m "docs: rewrite top-level README for first-time evaluators"
```

---

### Task 4: 文档站入口层打磨（中英同步）

**Files:**
- Modify: `docs/overrides/home.html`、`docs/src/overview.md(.zh.md)`、`docs/src/installation.md(.zh.md)`、`docs/src/pvisor/get-started.md(.zh.md)`、`docs/src/pchronicle/get-started.md(.zh.md)`

**Interfaces:**
- Consumes: Task 3 移除的 "Command ownership" 表（迁入 `overview.md`）。

- [ ] **Step 1: overview.md 吸收 Command ownership 表**

在 `overview.md` 的 "Current user workflows" 表格之后插入（中英两版同步）：

```markdown
## Command ownership

| Command | Primary responsibility |
|---|---|
| `pvisor` | One Run, environments, review, checkpoints, apply/drop |
| `ppilot` | Bounded collections of Runs: planning, concurrency, recovery, sinks |
| `pchronicle` | Dataset catalog, SQL, built-in analysis, find, import/export, read-only serving |
```

中文版（`overview.zh.md` 对应位置）：

```markdown
## 命令分工

| 命令 | 主要职责 |
|---|---|
| `pvisor` | 单个 Run、执行环境、审查、检查点、apply/drop |
| `ppilot` | 成组的 Run：规划、并发、恢复、结果汇聚 |
| `pchronicle` | Dataset 目录、SQL、内建分析、find、导入导出、只读服务 |
```

- [ ] **Step 2: home.html 首页检查**

以首次评估者视角审读 `docs/overrides/home.html`：hero 文案与 README 新定位保持一致；三个 CTA 按钮目标有效；四个版块（工作流 / 五分钟上手 / 整体关系 / 继续阅读）链接全部指向存活页面。仅在发现不一致时修改，不为改而改。

- [ ] **Step 3: installation.md 审计**

- 核对 `pip install persisting[lance]` 与 README 一致；
- 核对 macFUSE、libkrun、Zig 等平台要求仍然准确（对照 `justfile` 与 `crates/persisting-pvisor` 构建逻辑）；
- 确认页面未提及 `ppilot` 安装——在 "CLI component set" 一节补充一句：ppilot 经 `just install-cli` 从源码安装（中英同步）。

- [ ] **Step 4: 两个 get-started 审计**

`pvisor/get-started.md` 与 `pchronicle/get-started.md`：按 Get Started 契约（最短可验证成功循环、步骤可复制执行、每步有预期结果）逐行审读；实际执行其中的命令序列验证可用性；中英两版同步修订发现的问题。

- [ ] **Step 5: 验证 + Commit**

Run: `just docs-build && just docs-links`

```bash
git add -A docs/
git commit -m "docs: polish entry-layer pages for first-time evaluators"
```

---

### Task 5: pVisor 文档区审计

**Files:**
- Audit: `docs/src/pvisor/` 全部页面（index、get-started、concepts×4、guides×5、design×3、reference×2，及各自已有 `.zh.md`）

- [ ] **Step 1: 逐页应用文章类型契约**

对每一页核对（契约全文见 `docs/README.md` "Each article type has one job" 表）：

| 检查项 | 动作 |
|---|---|
| 页面是否只回答该类型该回答的问题 | 越界内容移至正确类型页面或删除 |
| 命令示例与 `--help` 输出一致 | 运行 `pvisor <cmd> --help` 逐一核对，修正漂移 |
| 有回链（owning concept/workflow）与前链（下一层） | 缺则补 |
| Design 页中 roadmap/target 内容有显式标注 | 缺则加 `!!! note "Target architecture"` admonition |
| 中英两版内容同步 | 英文改动同步进 `.zh.md`（本任务只改已有中文版的页面） |

pVisor 命令核对清单：`pvisor run`、`review`、`apply`、`drop`、`checkpoint`（以 `pvisor --help` 实际输出为准增减）。

- [ ] **Step 2: 验证 + Commit**

Run: `just docs-build && just docs-links`

```bash
git add -A docs/src/pvisor/
git commit -m "docs: audit pVisor section against article-type contract"
```

---

### Task 6: pChronicle 文档区审计

**Files:**
- Audit: `docs/src/pchronicle/` 全部页面（index、get-started、concepts×3、guides×6、design×5、reference×6，及各自已有 `.zh.md`）

- [ ] **Step 1: 逐页应用文章类型契约**

同 Task 5 的检查表。pChronicle 命令核对清单：`pchronicle onboard`、`import`、`export`、`agent`、`serve`、`find`、`query`（以 `pchronicle --help` 实际输出为准）。特别注意：

- `reference/cli.md` 与 `--help` 逐条对齐（这是参考页的核心职责）；
- `reference/query-model.md` 与 RFC-0012 的 find 语法一致性（以代码实现为准，RFC 仅历史参考）；
- `guides/serve-gateway.md` 中 Gateway 转发/改写/捕获描述与 `crates/persisting-gateway` 实际行为一致。

- [ ] **Step 2: 验证 + Commit**

Run: `just docs-build && just docs-links`

```bash
git add -A docs/src/pchronicle/
git commit -m "docs: audit pChronicle section against article-type contract"
```

---

### Task 7: Project、System Design 与 RFC 索引审计

**Files:**
- Audit: `docs/src/project/`（4 页）、`docs/src/system-design/`（4 页）、`docs/src/rfcs/index.md`

- [ ] **Step 1: 逐页审计**

- `project/`：engineering/releasing/examples 面向贡献者，核对命令（`just test`、`just docs-*`、发布流程）与 `justfile`、`.github/workflows/release.yml` 实际一致；
- `system-design/`：四页只回答"哪个产品拥有哪个跨产品对象/转移/失败"，发现与产品页重复的实现细节则改为链接；
- `rfcs/index.md`：确认 0001–0013 索引完整（注意无 0011）、状态标注准确；在页面说明 RFC 为历史决策记录、非命令参考（英文版已有则说明一致，中文版在 Task 8 补译时体现）。

- [ ] **Step 2: 验证 + Commit**

Run: `just docs-build && just docs-links`

```bash
git add -A docs/src/project/ docs/src/system-design/ docs/src/rfcs/index.md
git commit -m "docs: audit project, system-design, and RFC index pages"
```

---

### Task 8: 双语补译

**Files:**
- Create（12 个 `.zh.md`）:
  - `docs/src/pvisor/design/gateway.zh.md`
  - `docs/src/pvisor/design/isolation.zh.md`
  - `docs/src/pvisor/design/overlaynet.zh.md`
  - `docs/src/pvisor/reference/cli.zh.md`
  - `docs/src/ppilot/index.zh.md`
  - `docs/src/ppilot/get-started.zh.md`
  - `docs/src/ppilot/guides/orchestrate.zh.md`（已存在——从 pvisor 移动而来，核对链接即可，不重译）
  - `docs/src/ppilot/design/orchestration.zh.md`
  - `docs/src/ppilot/reference/cli.zh.md`
  - `docs/src/pchronicle/reference/agenticmd.zh.md`
  - `docs/src/project/engineering.zh.md`
  - `docs/src/project/releasing.zh.md`
  - `docs/src/rfcs/index.zh.md`

**Interfaces:**
- Consumes: Task 2 的 `ppilot/index.md`、`ppilot/get-started.md` 英文定稿；Task 5–7 审计后的英文定稿。

**反向对齐（Task 6 审计发现）**：以下 4 个页面的 `.md`（英文位）当前实为全中文内容，与 `.zh.md` 完全相同。本任务需将 `.md` **重写为地道英文**（以 `.zh.md` 为中文源，`.zh.md` 本身不动）：
- `docs/src/pchronicle/design/catalog.md`
- `docs/src/pchronicle/design/trajectory-storage.md`
- `docs/src/pchronicle/design/storyline-lance.md`
- `docs/src/pchronicle/reference/agenticmd.md`

- [ ] **Step 1: 翻译 pPilot 与 pVisor 页面**

翻译规范：
- 术语保留英文：Run、Effect、Dataset、Capture、Gateway、OverlayFS、OverlayNet、lease、fencing、sink；
- 代码块、命令、文件路径不翻译；
-  frontmatter 与相对链接保持与英文版一致（链接目标不本地化）；
- 参照 `pvisor/guides/orchestrate.zh.md` 的既有风格。

- [ ] **Step 2: 翻译 pchronicle/project/rfcs 页面**

`rfcs/index.zh.md` 中必须包含说明：RFC 正文保持英文，因为它们是历史决策快照，翻译会产生两个可能漂移的副本。

- [ ] **Step 3: 验证 + Commit**

Run: `just docs-build && just docs-links`，并抽查 3 个页面的中文渲染（`just docs-serve` 或构建后检查 `docs/site/zh/` 下对应 HTML 存在）。

```bash
git add -A docs/src/
git commit -m "docs: add Chinese translations for user-facing pages"
```

---

### Task 9: 组件 README 统一

**Files:**
- Modify（按模板套用，内容为精简对齐而非重写）:
  - crates（8 个）：`persisting-agentctl`、`persisting-gateway`、`persisting-overlayfs`、`persisting-overlaynet`、`persisting-pchronicle`、`persisting-pchronicle-cli`、`persisting-ppilot`、`persisting-pvisor` 的 `README.md`
  - examples（12 个）：`examples/README.md`、`examples/data/README.md`、`examples/pchronicle/README.md` + 6 个子示例、`examples/ppilot/README.md` + 2 个子示例、`examples/pvisor/README.md` + 4 个子示例
  - benchmark（5 个）：`benchmark/README.md`、`benchmark/gateway/README.md`、`benchmark/langfuse-pchronicle-review/README.md`、`benchmark/pchronicle/README.md`、`benchmark/pvisor/README.md`
  - tests（3 个）：`tests/regression/README.md`、`tests/regression/gateway-echo/README.md`、`tests/regression/gateway-fuzz/README.md`
  - web（1 个）：`pchronicle-web/README.md`
- 不动：`crates/persisting-dlcapt/README.md`（排除范围）、`crates/*/tests/fixtures/**/README.md`（fixture 数据说明）、`docs/`、`benchmark` 下非 README 文件

模板（按类型裁剪）：

```markdown
# <组件名>

**<一句话职责>.**

<边界段：拥有什么；不拥有什么（链向拥有者）。>

## <Use | Develop | Run>

<命令或步骤>

## Links

- <文档站对应页或相邻组件>
```

- [ ] **Step 1: crates README（8 个）**

逐一核对：一句话职责与 crate 实际边界一致（对照 `Cargo.toml` description 与代码）；构建/测试命令可执行（如 `cargo build -p persisting-pvisor --bin pvisor`）；链向文档站对应 Design 页。`persisting-ppilot/README.md` 已有较好的职责段，保留主体、补 Links 段。

- [ ] **Step 2: examples README（12 个）**

每个示例 README 核对：`run.sh` 可执行、预期输出描述与实际一致（抽查 `examples/ppilot/01-run/run.sh` 与 `examples/pvisor/01-filesystem-isolation/run.sh`）；顶部有一句话"问题：…可复现结论：…"（现有风格，保留并统一）。

- [ ] **Step 3: benchmark 与 tests README（8 个）**

核对复现命令与 `justfile` 配方一致（`just benchmark-pvisor`、`just benchmark-gateway` 等）；报告契约说明与 `benchmark/pchronicle/bench.py` 实际输出一致。

- [ ] **Step 4: pchronicle-web README**

核对开发命令（`npm`/`pnpm` 脚本）与 `pchronicle-web/package.json` 一致；补边界段（embedded Web UI 的前端源，构建产物由 `persisting-pchronicle` 嵌入——以代码实际为准）。

- [ ] **Step 5: Commit**

注意：工作区可能存在用户的未提交 WIP（如 `crates/persisting-pchronicle/src/`），**禁止 `git add -A crates/`**，只精确添加 README 文件：

```bash
git add crates/*/README.md crates/persisting-gateway/tests/README.md \
  examples/ benchmark/ tests/regression/ pchronicle-web/README.md
git status --short   # 确认暂存区只有 README 与 examples/benchmark/tests 文档改动
git commit -m "docs: standardize component READMEs on ownership template"
```

---

### Task 10: i18n 检查脚本与最终验收

**Files:**
- Create: `scripts/check-docs-i18n.py`

- [ ] **Step 1: 创建检查脚本**

```python
#!/usr/bin/env python3
"""Fail if any translatable docs page lacks a Chinese counterpart.

RFC bodies (historical decision records) and Queue-subsystem pages
(out of documentation scope per AGENTS.md) are intentionally English-only.
"""
from pathlib import Path
import sys

SRC = Path(__file__).resolve().parent.parent / "docs" / "src"

EN_ONLY = {
    "api/index.md",
    "api/queue.md",
    "guide/custom-backends.md",
    "guide/queue.md",
}

def is_translatable(rel: str) -> bool:
    if rel in EN_ONLY:
        return False
    if rel.startswith("rfcs/") and rel != "rfcs/index.md":
        return False
    return True

missing = []
for page in sorted(SRC.rglob("*.md")):
    if page.name.endswith(".zh.md"):
        continue
    rel = page.relative_to(SRC).as_posix()
    if not is_translatable(rel):
        continue
    if not page.with_name(page.name[:-3] + ".zh.md").exists():
        missing.append(rel)

if missing:
    print("Missing Chinese translations:")
    for rel in missing:
        print(f"  {rel}")
    sys.exit(1)
print("All translatable pages have Chinese counterparts.")
```

- [ ] **Step 2: 运行脚本**

Run: `python3 scripts/check-docs-i18n.py`
Expected: `All translatable pages have Chinese counterparts.`（若有遗漏，回到 Task 8 补齐）

- [ ] **Step 3: 最终验收清单**

依次执行并确认：
1. `just docs-build` — 零警告；
2. `just docs-links` — strict 通过；
3. `python3 scripts/check-docs-i18n.py` — 通过；
4. README 链接可达性：核对 4 个 Pages URL 与 `docs/src/` 页面一一对应；
5. 归档完整性：`docs/src/` 下不再存在 `design/`、`dev/`、`quickstart.md`；`git log --follow docs/archive/legacy-nav/design/index.md` 可见历史延续；
6. `git status` — 工作区干净，全部改动已提交。

- [ ] **Step 4: Commit**

```bash
git add scripts/check-docs-i18n.py
git commit -m "docs: add i18n coverage check for documentation site"
```

---

### Task 8b: 叙事对齐（宣发主角决策后的入口层修订）

**背景**：用户在执行期间确认了宣发主角为**链路**——"从执行到可查询历史的完整基础设施"。叙事原则（来自用户提供的分析）：pVisor 对外一句话定位收敛为"产生可持久化、可审查事实的执行器"；不并列多个旗号；强调 run → review/apply → capture → 可查询 Dataset 的贯通路径，同时保留"两者可独立使用"的灵活性说明。

**Files:**
- Modify: `README.md`、`docs/src/overview.md(.zh.md)`、`docs/overrides/home.html`

- [ ] **Step 1: README 叙事调整**

保持 uv/Ruff 式简洁，仅调整叙事层：
- 开篇定位段：从"two commands"并列改为链路叙事——pVisor 让 Agent 执行产生可审查的事实（staged Effects、执行记录），pChronicle 让这些事实成为可查询的历史；两者各自独立可用，连起来构成从执行到查询的完整路径；
- 两个快速上手小节保留，在 pChronicle 小节末尾或其后增加一行贯通提示（capture 配置后 pVisor Run 事件可进入 pChronicle Dataset，链向 `pvisor/guides/capture/`）；
- 不重排成熟度表、不动 benchmark 标记块。

- [ ] **Step 2: overview.md 叙事调整**

- "Optional integration" 一节升级表述：从"可选集成"改为"贯通路径"（the throughline），作为页面叙事收束而非附属说明；"两者独立可用"保留为灵活性说明而非开场主张；
- 中英同步。

- [ ] **Step 3: home.html hero 检查**

hero 已有 "One persistence story, two ways to start" 与 "Composable, not a mandatory pipeline" 版块——按链路主角口径审视："Composable, not a mandatory pipeline" 的措辞强调"非强制流水线"，与链路主角叙事张力过大时，调整为"独立可用，连通更强"类表述（保持事实准确：两者确实可独立使用）。中英同步。

- [ ] **Step 4: 验证 + Commit**

Run: `just docs-build && just docs-links`

```bash
git add README.md docs/src/overview.md docs/src/overview.zh.md docs/overrides/home.html
git commit -m "docs: align entry narrative on the execution-to-history throughline"
```

---

## Self-Review 记录

- **Spec 覆盖**：规格第 3–8 节分别映射到 Task 3、1/2/4、5–7、8、9、10；规格第 9 节实施顺序与 Task 1–10 顺序一致。无遗漏。
- **占位符扫描**：Task 5–7 的审计类步骤以检查表 + 命令清单为可执行内容（审计工作的产出取决于逐页读到的现状，无法预先给出定稿文案）；所有新建内容（README、ppilot 页面、脚本、nav YAML）均含完整文本。
- **类型一致性**：`ppilot/index.md`/`get-started.md` 在 Task 2 定义英文定稿，Task 8 消费同一文件名；`check-docs-i18n.py` 的排除清单与 Global Constraints 的排除范围一致。
