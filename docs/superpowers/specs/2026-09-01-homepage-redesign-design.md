# 首页重做设计（Homepage Redesign）

**日期**：2026-09-01
**范围**：`docs/overrides/home.html`、`docs/src/assets/stylesheets/home.css`
**背景**：对外发布前的最后一块入口层打磨。文档站其余部分已在 2026-08-30 的文档工程中完成。

## 1. 问题陈述

### 1.1 叙事问题：连续三节讲同一件事

现有首页第 2、3、4 节分别是：

| 节 | 标题 | 实际讲的内容 |
|---|---|---|
| 2 | One persistence story, two ways to start | 有两个产品，两个入口 |
| 3 | Two independent starting points | 有两个产品，可独立使用 |
| 4 | Use either. Connect both. | 有两个产品，可独立也可连通 |

三节都在陈述"两个产品、可独立使用"这一件事。贯通链路（执行 → 可查询历史）只在第 4 节的段落文字里被**断言**，从未被**展示**。

这与 2026-08-30 确定的宣发主角决策（链路主角：从执行到可查询历史的完整基础设施）不匹配：叙事重心仍落在"两个并列产品"上。

### 1.2 视觉问题

现有首页是纯文字 + 一张架构 SVG。没有产品实感：既没有终端执行的样子，也没有 pChronicle Web UI 的样子。对于 30 秒内形成判断的访客，缺少"这东西真的能跑"的证据。

### 1.3 技术问题：模板内联双语块

`home.html` 现有 192 行中，`{% if config.theme.language == "zh" %}` 内联重复约 20 次。后果：

- 结构与文案交织，模板难以阅读和修改；
- 中英文案分散在 20 处，容易单边修改导致孪生漂移（文档工程期间已发生过同类问题）。

## 2. 设计决策记录

| 决策点 | 选择 | 备选与否决理由 |
|---|---|---|
| 重做范围 | 视觉与叙事同时重做 | 单改其一无法解决 1.1 与 1.2 的耦合问题 |
| 视觉风格 | 现代基础设施风（Linear / Vercel / Supabase）：深色、渐变光效、产品截图为主 | 极简开发者工具风（uv/Ruff）——README 已采用该风格，首页需要更强的视觉承载；终端极客风——个性过强，与"基础设施"定位不符 |
| Hero 视觉 | 链路组合：终端（执行）+ 截图（可查询历史）双视觉 | 单终端——只讲一半；单截图——丢失执行侧；动态录屏——维护成本高 |
| 主题适配 | Hero 跟随明暗主题，两套各自调过的配方 | 常暗 Hero——视觉更震撼但浅色主题下断裂 |
| 数据带 | 纯能力数字，四格 | 性能数字——会过期，且 pVisor 侧无等价量化证据，会造成产品间不对称 |
| 动效 | 仅 hover，无滚动动效 | 滚动淡入——增加实现与无障碍成本，收益有限 |

## 3. 页面蓝图

自上而下七个区块，**每区块只讲一件事，零重复**。

### ① Hero — 双视觉贯通

- 品牌字（`Persisting`）、一句定位、说明文案、三个 CTA（沿用现有文案，已在 Task 8b 对齐链路叙事）
- 视觉主体：左侧风格化终端卡（`pvisor run --safe` → `review` → `apply`），右侧真实产品截图卡（pChronicle Runs 浏览器），中间一道渐变光带表达事实从执行流向历史
- 截图为浅色 UI，套浏览器窗口质感外框（圆角 + 投影），避免贴图感
- 移动端：两卡竖向堆叠，光带转为纵向

### ② 数据带 — 四格能力事实

紧贴 Hero 下方的窄条，作为定位主张的即时佐证。四项事实均已对代码与文档核实：

| 事实 | 来源 |
|---|---|
| 8 种轨迹格式 | `docs/src/pchronicle/reference/formats/index.md`：Events、Storyline、ACTF、ATIF、OpenAI Messages、Codex、Claude Code、AgenticMD |
| 2 种执行器（host 内核 / libkrun microVM） | `docs/src/pvisor/guides/execution.md` `--executor host\|vm` |
| 直接导入 Codex 与 Claude Code 本地会话 | `docs/src/pchronicle/guides/exchange.md`；decode-only |
| SQL 查询轨迹历史 | `docs/src/pchronicle/guides/discover-and-query.md` `pchronicle query --sql` |

两格偏 pVisor、两格偏 pChronicle，保持产品均衡。全部为静态能力事实，不随 nightly benchmark 变化，零维护。

**明确排除**：不放性能数字。理由有二——一是会过期（现有 `benchmark/pchronicle/bench.py` 的 marker 注入只写 README，扩展到首页需改 CI 产物路径）；二是 pVisor 侧无等价量化证据，放性能数字会使数据带单边倾斜到 pChronicle。

### ③ 贯通带 — 三步

横向三步，**用结构本身讲完链路故事**，替代旧版第 2、3、4 节：

| 步 | 含义 | 展示内容 |
|---|---|---|
| Run | 受治理执行 | `pvisor run --safe codex` |
| Capture | 事实成为记录 | 配置 capture 后运行事件进入 Dataset |
| Query | 历史可查询 | `pchronicle query --sql` |

配色从 teal（执行）渐变到蓝（历史），**颜色参与叙事**。

"两者可独立使用"降为本节末尾一行补充说明——保持事实准确（确实可独立使用），但不再是标题主张。

### ④ 纵深证据 — 两个产品面板

到此才引入产品分工，定位是"链路中的纵深"而非"竞争入口"：

- **pVisor 面板**：治理具体意味着什么——staged Effects、review / apply / drop、两种执行器。配 `pvisor review` 输出的终端卡（CSS 构造）
- **pChronicle 面板**：可查询具体意味着什么——SQL 查询、导入外部轨迹、Web UI。配 `analysis-sql.jpg` 截图

取代旧版的空洞能力卡片。截图分配不重复：Hero 用 `runs-browser.jpg`，本节用 `analysis-sql.jpg`。

### ⑤ 快速上手

统一的可复制代码块：装一次，然后两条路径任选。压缩自旧版第 3 节。安装命令统一为 `pip install persisting[lance]`（与 README、`installation.md` 一致）。

### ⑥ 继续阅读

四张链接卡。现有版本可用，保留结构，仅随新视觉系统调整样式。

### ⑦ 收束原则

保留并收紧现有文案。

### 关于现有架构图

旧版第 4 节的 `system-products.svg` 架构图**从首页移除**。理由：③ 贯通带已用页面结构本身表达了同一条链路，图与结构重复；该图在 `docs/src/overview.md` 中仍然保留，需要图解的读者可从 ⑥ 的"Choose a workflow"入口到达。资产文件不删除。

## 4. 视觉系统

### 4.1 主题自适应

Hero 在明暗两种配色下都成立，通过 CSS 变量分组覆盖实现，而非简单反色：

- **深色方案**（`[data-md-color-scheme="slate"]`）：沿用现有深蓝-teal 渐变
- **浅色方案**（`[data-md-color-scheme="default"]`）：近白底 + 极淡 teal/蓝径向光晕 + 深墨色文字

两套配方各自调校对比度，确保文字与 CTA 在两种方案下均满足可读性。

### 4.2 语义化配色

- **teal**（`#0f766e` / `#2dd4bf`）：执行侧（pVisor）
- **蓝**（`#3b82f6` 系）：历史侧（pChronicle）
- 贯通带三步的配色由 teal 渐变至蓝

这一语义编码是现有版本所没有的：颜色不只是装饰，而是链路叙事的一部分。

### 4.3 排版

沿用 Material 字体栈。Hero 主字沿用 `clamp()` 流体缩放。命令与代码使用等宽字体。

### 4.4 动效

仅保留 hover 反馈（卡片微抬、链接下划线、CTA 配色过渡）。不做滚动触发动效。所有过渡包在 `prefers-reduced-motion: reduce` 保护内。

## 5. 技术实现

### 5.1 模板：文案字典化

将内联双语块重构为顶部单一文案字典：

```jinja
{% set copy = {
  "en": {"hero_title": "...", "hero_positioning": "...", ...},
  "zh": {"hero_title": "...", "hero_positioning": "...", ...}
} %}
{% set t = copy["zh"] if config.theme.language == "zh" else copy["en"] %}
```

正文只引用 `t.hero_title` 这类键。收益：

- 模板从 192 行降至约 100 行纯结构，结构与文案解耦；
- 所有文案集中于一处，中英对照可一眼审阅，消除孪生漂移风险；
- 新增语言只需增加一个字典条目。

### 5.2 样式：设计令牌

`home.css` 重构为 `--ph-*` 前缀的设计令牌 + 配色方案覆盖块。保持单文件（预计 343 → 约 450 行）。

保留现有第 299–304 行的宽屏侧栏隐藏规则。

### 5.3 资产

复用现有资产，不新增二进制文件：

- `docs/src/assets/screenshots/pchronicle/runs-browser.jpg`（1280×720）
- 终端卡为纯 CSS + HTML 构造，非图片

## 6. 约束

- **双语同步**：中英文案必须在同一次修改中同步（字典结构天然保证这一点）
- **事实准确**：所有能力主张须对代码或文档核实；不得引入未经核实的数字
- **构建零警告**：`just docs-build`、`just docs-links`、`just docs-i18n` 三项均须通过
- **导航现状**：pPilot 已于 `f3ad8eeb` 移出导航，首页不得出现 pPilot 入口
- **不改动范围外文件**：仅 `home.html` 与 `home.css`；不动 `mkdocs.yml` 导航、不动其他文档页

## 7. 验收标准

1. 三处旧的重复叙事（"two ways to start" / "two independent starting points" / "use either, connect both"）不再并存；链路由结构展示而非文字断言
2. Hero 在明暗两种配色下均可读、无对比度断裂
3. 数据带四项事实与第 3 节 ② 表格中的来源一致
4. 模板无内联 `{% if config.theme.language %}` 重复块（字典分派除外）
5. 中英双语内容对等
6. 三项构建校验零警告
7. 移动端（≤620px）与平板（≤920px）断点下布局不破
