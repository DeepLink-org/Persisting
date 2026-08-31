# 文档体系打磨设计：发布级 README 与文档站

> 日期：2026-08-30
> 背景：项目即将对外发布并启动增长拉新，需要将 README 与文档打磨至顶级开源项目水平，并移除令人困惑的文档。
> 状态：设计已与用户逐节确认（2026-08-30）。

## 1. 目标与成功标准

**目标读者**（用户确认）：混合受众——README 负责快速吸引首次到访者，文档站负责分层深入。

**成功标准**：

1. 首次到访者 30 秒内理解项目价值主张，5 分钟内跑通任意一条工作流；
2. 文档站不存在重复、过时、孤立（不在导航中却被构建）的页面；
3. 所有对外用户页面中英双语对齐（RFC 除外，见第 6 节）；
4. 40+ 个组件 README 风格统一、职责清晰；
5. `just docs-build` 与 `just docs-links`（strict）零警告通过。

**范围**（用户确认"全面打磨"）：顶层 README、整个 MkDocs 文档站、全部组件级 README。

**范围约束**（遵循 AGENTS.md）：TTAS、Queue 及其 sampler、Search、`persisting-dlcapt` 四个子系统的文档**内容不修改、不翻译、不重构**；仅当导航结构调整不可避免时做最小位移。Queue 页面（`guide/queue.md`、`api/queue.md`、`api/index.md`、`guide/custom-backends.md`）维持 "Standalone systems" 导航现状。

**交付方式**（用户确认）：单一 PR，内部按"清理 → README → 文档站 → 翻译 → 组件 README"拆多个 commit 便于评审。

## 2. 三层信息架构

确立"每层一个职责"的契约，消除职责混叠：

| 层 | 职责 | 读者 |
|---|---|---|
| 顶层 `README.md` | 30 秒传达"是什么、为什么需要、怎么跑起来" | 首次到访者 |
| 文档站（MkDocs Material） | 分层深入：入门 → 指南 → 概念 → 参考 → 设计 | 评估者/使用者 |
| 组件 README（crates/examples/benchmark/tests） | 组件边界与开发说明 | 贡献者/工程师 |

原则：

- README 不重复文档站内容，只保留价值主张、最快成功路径、链接；
- 文档站是唯一权威来源，遵循 `docs/README.md` 已定义的文章类型契约（Overview → Get Started → Concepts → Guides → Design → Reference）；
- 组件 README 不写用户级教程，教程一律链向文档站。

## 3. 顶层 README 重写（uv/Ruff 风格）

新结构（从上到下）：

1. **徽章行**：CI（`.github/workflows/ci.yml`）、文档站、License；若 `persisting` 已发布 PyPI 则加版本徽章（实施时核实，未发布则不加，避免死徽章）；
2. **一句话价值主张**：保留 "Persistent Infrastructure for the Agent Era" 定位；
3. **定位段**：现有 "model state + Agent history" 叙事压缩至 3 行以内；
4. **架构图**：保留现有 `docs/src/assets/diagrams/persisting/system-products.svg`；
5. **Quickstart**：安装（pip + nightly 二选一）+ pvisor 三行（run/review/apply）+ pchronicle 三行（onboard/query/agent），全部可直接复制执行；
6. **两条工作流**：各一句话描述 + 链向文档站对应 Get Started；
7. **成熟度表**：保留现有表格，仅微调措辞；
8. **文档入口**：精简为 3–4 个关键链接（文档站首页、安装、两条工作流的 Get Started）；
9. **License**：保留。

删减：现有 "Choose a workflow" 的细节段落（文档站已有更完整版本）；"pChronicle performance" benchmark 段压缩为一行链接指向 `benchmark/pchronicle/README.md`；"Command ownership" 表移入文档站 `src/overview.md`（它本身就是两条工作流的对比页），README 不保留。

风格基准（用户确认）：uv / Ruff——极简、徽章、一句话价值主张、快速上手。纯文字与结构先行，不新增截图/GIF（用户确认素材后续再补）。

## 4. 文档站清理与归档

### 4.1 归档（git mv 至 `docs/archive/legacy-nav/`，从构建排除）

以下均为重定向桩（frontmatter `template: redirect.html`），新结构已承接其 URL 语义，桩本身无对外价值：

- `src/design/`：16 篇英文 + 3 篇中文重定向桩（`agent-infrastructure`、`agentvisor`、`architecture`、`cli-pchronicle`、`cli-ppilot`、`cli-pvisor`、`dataset-catalog`、`gateway`、`overlaynet`、`pchronicle-product`、`ppilot`、`pvisor-isolation`、`storyline-lance`、`trajectory-format`、`trajectory`、`index`，其中仅 `agentvisor`、`architecture`、`index` 有 `.zh.md`）；
- `src/guide/` 中的重定向桩：8 篇英文 + 8 篇中文（`capture`、`examples`、`history`、`index`、`orchestrate`、`overlaynet`、`pvisor-execution`、`review-apply` 及各自 `.zh.md`）；
- `src/dev/`：`index.md`、`releasing.md`；
- `src/quickstart.md`。

合计 38 个文件；归档后文档站剩余约 71 个英文页。

实施方式：`docs/archive/legacy-nav/` 位于 `docs_dir`（`src/`）之外，移动后天然从构建排除；`docs/archive/README.md` 增补一段说明归档来源与原因。

### 4.2 删除与归位

- 删除 `docs/product/`（空目录，仅含 `.DS_Store`）；
- `docs/pchronicle-design-review.md`（内部深度评审，中文）→ 移至 `docs/superpowers/reviews/2026-08-23-pchronicle-design-impl-review.md` 旁归位（文件名保持日期前缀惯例）。

### 4.3 pPilot 孤立页接入导航

`pvisor/guides/orchestrate.md`、`pvisor/design/orchestration.md`、`pvisor/reference/ppilot-cli.md` 是实质内容，且 pPilot 属 AGENTS.md 核心范围（pVisor/pPilot/pChronicle/Gateway/Control/OverlayFS/OverlayNet）。处理：在 `mkdocs.yml` 导航新增 **pPilot** 顶层小节：

- Overview：新写一页 `ppilot/index.md`（职责：多 Run 编排；与 pVisor 的分工：pVisor 管一个 Run，pPilot 管一组 Run）；
- Get Started：基于 `examples/ppilot/01-run` 新写最短成功循环；
- Guides：`orchestrate.md` 移入 `ppilot/guides/orchestrate.md`；
- Design：`orchestration.md` 移入 `ppilot/design/orchestration.md`；
- Reference：`ppilot-cli.md` 移入 `ppilot/reference/cli.md`。

文件物理移动至 `src/ppilot/`，与 pvisor/pchronicle 结构对齐。原位置是否留重定向桩以链接检查为准：`mkdocs build --strict` 若发现其他页面存在指向原位置的入链则留桩（沿用现有 redirect.html 惯例），否则不留——与第 4.1 节"无价值桩一律归档"的原则保持一致。中文导航翻译同步加入 `nav_translations`。

### 4.4 附带修复

- `mkdocs.yml` 的 `site_url` 当前误填为 `https://github.com/DeepLink-org/Persisting`，改为 `https://deeplink-org.github.io/Persisting/`（与 README 中文档链接一致）。

## 5. 文档站内容打磨标准

以 `docs/README.md` 的文章类型契约为验收标准，逐页审计归档后保留的约 71 个英文页：

| 类型 | 验收要点 |
|---|---|
| Overview | 讲清产品拥有什么、从哪里开始；不讲实现细节 |
| Get Started | 最短可验证成功循环；步骤可复制执行 |
| Concepts | 稳定对象与心智模型；不混入操作步骤 |
| Guides | 一个用户目标全流程，含决策点与验证方法 |
| Design | 机制、保证与已知缺口；roadmap 内容显式标注 |
| Reference | 与二进制 `--help` 输出逐条对齐 |

通用要求：每页有回链（owning concept/workflow）与前链（下一层）；跨产品链接只出现在真实交接点（pVisor Run 输出 → pChronicle 历史；pPilot 编排 → pVisor 执行）；命令示例全部对照 `--help` 核实。

重点重写入口四页：`index.md`（经 `overrides/home.html` 渲染，同步检查 hero 文案）、`overview.md`、`installation.md`、两个产品的 `get-started.md`，以首次评估者视角组织。

## 6. 双语对齐

- **补译**（10 个现有用户页面 + 新增 pPilot 页面）：`pvisor/design/gateway.md`、`pvisor/design/isolation.md`、`pvisor/design/overlaynet.md`、`pvisor/reference/cli.md`、`pchronicle/reference/agenticmd.md`、`project/engineering.md`、`project/releasing.md`、`rfcs/index.md`，以及第 4.3 节移动后的 `ppilot/design/orchestration.md`、`ppilot/reference/cli.md` 与新增的 `ppilot/index.md`、Get Started；
- **重写/改写的页面**：中英双版同步产出；
- **RFC 保持英文单语**（用户已确认）：RFC 是历史决策快照（ADR 性质），翻译会产生两个可能漂移的副本；在 `rfcs/index.md`（需补中文）中向中文读者说明这一点；
- **Queue 相关页不补译**（AGENTS.md 排除范围）：`api/queue.md`；
- 新增检查脚本 `scripts/check-docs-i18n.py`（或并入现有脚本）：扫描 `docs/src/`，列出应有 `.zh.md` 而缺失的页面（排除 RFC、归档、Queue 排除项），供 CI 选用。

## 7. 组件 README 统一模板

模板（按组件类型裁剪）：

1. **一句话职责**；
2. **边界**：拥有什么 / 不拥有什么（链向拥有者）；
3. **使用或开发入口**：命令或构建方式；
4. **链接**：文档站对应页。

分类套用：

- **crates/**（10 个，如 `persisting-pvisor`、`persisting-pchronicle`、`persisting-gateway` 等）：偏边界与开发说明；`persisting-dlcapt` 不动（排除范围）；
- **examples/**（约 12 个）：偏运行步骤，现有基础较好，以对齐为主；
- **benchmark/ 与 tests/**（约 10 个）：偏复现方法与报告契约说明。

原则：精简对齐，不新增教程内容；与文档站重复的内容改为链接。

## 8. 验证与验收

1. `just docs-build` 零警告；
2. `just docs-links`（`mkdocs build --strict`）通过；
3. README 中所有链接可达（文档站 URL 与 Pages 实际路径一致）；
4. README 与文档站中的命令示例与对应二进制 `--help` 输出一致（抽查 pvisor/pchronicle/ppilot 各核心命令）；
5. i18n 检查脚本通过：应译页面均有 `.zh.md`；
6. 归档后导航无死链：strict 构建即覆盖此项；
7. `just test` 不受影响（本次不改代码，但文档中示例若被测试引用需核对）。

## 9. 实施顺序（单 PR 多 commit）

1. **清理**：归档重定向桩、删除空目录、归位评审文档、修 `site_url`；
2. **pPilot 接入**：移动三页 + 新写 Overview/Get Started + 导航与翻译配置；
3. **README 重写**；
4. **文档站入口层重写**：index/overview/installation/两个 get-started（中英同步）；
5. **文档站全量审计**：按第 5 节标准逐页修订；
6. **双语补译**：第 6 节清单；
7. **组件 README**：按第 7 节模板套用；
8. **验证**：第 8 节全部验收项。

## 10. 已确认决策记录

| 决策点 | 用户选择 |
|---|---|
| 目标读者 | 混合受众：README 快速吸引，文档站分层深入 |
| 工作范围 | 全面打磨：README + 文档站 + 全部组件 README |
| 旧文档处置 | 统一归档至 `docs/archive/`，从构建排除 |
| 中文要求 | 完全双语对齐；RFC 保持英文（后续确认） |
| 风格标杆 | uv / Ruff |
| 视觉素材 | 纯文字与结构先行，素材后续再补 |
| 推进方式 | 一次性大爆炸：单 PR 交付 |
