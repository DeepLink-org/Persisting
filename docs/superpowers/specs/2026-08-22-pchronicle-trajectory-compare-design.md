# pChronicle 双轨迹比较与会话 Pinboard 设计

**日期：** 2026-08-22

**状态：** Approved

**范围：** `pchronicle-web` 桌面端轨迹浏览与比较体验

## 背景

pChronicle Web 已具备较完整的单轨迹可视化能力：Runs 浏览、Run Detail、
Chats/Steps 轨迹结构、turn evidence、运行指标、Analysis 和 Copilot。当前主要缺口
是无法在同一界面中比较同一任务、同一 root 下的两次运行。用户只能打开多个
页面并人工寻找对应 Chat、tool call 和指标差异。

本设计在现有轨迹可视化之上增加：

1. 会话内可 pin 多个轨迹的顶部暂存条；
2. 从当前轨迹与任一兼容 pinned 轨迹发起比较；
3. 类似常见代码 Diff 工具的同步左右对比工作区；
4. 完全由前端执行的确定性 Chats/Steps 对齐和内容 Diff。

## 目标

- 支持同一任务、同一 `root_session_id` 下两次运行的左右比较。
- 默认以现有 Chats 分组为比较主轴，并允许两侧同步切换到 Steps。
- 明确区分相同、变更、仅左侧和仅右侧内容。
- 对文本、tool input/output、指标和显式错误提供可解释的差异展示。
- 允许用户在持续浏览过程中 pin 多条候选轨迹，并与当前轨迹快速发起 Diff。
- 保持后端为通用只读数据服务；不把比较、对齐或差异判断下沉到后端。
- Compare URL 可刷新、复制和分享，不依赖 Pinboard 状态。

## 非目标

- 移动端或窄屏适配。
- 不同 root 或不同任务之间的比较。
- 语义相似度、向量检索或 LLM 驱动的自动对齐。
- Search 子系统及其 API。
- 后端 Compare 端点或后端差异模型。
- pinned 轨迹持久化、跨标签页同步或服务端收藏。
- 注释、Markdown 报告、Copilot 差异总结。
- 多于两条轨迹的同时可视化。

## 核心用户流程

### Pin 与持续浏览

1. 用户在 Runs 行或 Run Detail 页头点击 pin。
2. 页面标题区下方出现顶部横向 Pin 条。
3. 用户继续浏览其他轨迹；Pin 条在 Runs、Detail 和 Compare 之间保留。
4. 当前轨迹与某个 pinned 轨迹具有相同 root 时，该 chip 提供 `⇄ Diff` 动作。
5. 用户点击 chip 主体可打开 pinned 轨迹；点击 `⇄` 才发起比较。
6. 当前轨迹不能与自身比较。

### 从多个 pinned 轨迹发起比较

- Pin 条直接展示最多三个与当前 root 兼容、最近使用的 pinned 轨迹。
- 其余 pinned 轨迹进入“更多”菜单。
- 不同 root 的 pinned 轨迹仍被保留，但其 Diff 动作禁用并说明原因。
- 没有当前轨迹时，“更多”菜单允许从 pinned 列表中选择两个同 root 轨迹。
- pin 新轨迹不会替换旧项；同一完整轨迹身份只保留一份。
- 提供单项 unpin 和 `Clear all`。

### Compare 工作区

1. 用户进入独立 Compare 页面。
2. 页面不显示 Run paths，以保留完整横向空间。
3. 页头展示左右轨迹选择、状态、模型、时间、交换、返回和复制链接。
4. 较早运行默认在左，较新运行默认在右；用户可交换。
5. 指标条展示 status、Chats、Tools、Tokens、Latency P95 等 delta。
6. 默认进入 `Chats + Changes only`。
7. 用户通过共享切换器同步切换两侧 Chats/Steps。
8. 用户通过上一处/下一处动作巡检差异。
9. 展开任一对齐行时，左右证据同步展开。

## 信息架构

- 不新增常驻 Compare rail 入口。
- Runs、Detail 和 Compare 继续归属现有 Runs 一级导航。
- Compare 页头使用 `Runs / Compare` 层级信息。
- Compare 只通过 Pin 条、Detail 上下文动作或可分享 URL 进入。
- Analyze 页面隐藏 Pin 条，但不清除其会话状态。
- Pin 条为空时完全隐藏，不占据标题区空间。

## Pinboard 状态模型

`PinnedRunKey` 使用以下完整 coordinates 组成稳定身份：

- `dataset`
- `file`
- `run_id`
- `agent_id`
- `session_id`
- `root_session_id`

Pinboard 是 `App` 生命周期内的前端内存状态：

- SPA 页面导航期间保留；
- 页面刷新、标签页关闭或服务重启后清空；
- 不写 `localStorage`、session storage 或 pChronicle 仓库；
- 不跨标签页同步；
- 不进入 Compare URL。

Pin 条排序规则：

1. 与当前 root 兼容的轨迹优先；
2. 兼容项按最近 pin 或最近使用排序；
3. 其他 root 的轨迹进入“更多”菜单；
4. 当前轨迹已 pin 时显示实心 pin 状态。

## Compare 页面状态与 URL

Compare URL 保存：

- 左右两侧完整 run coordinates；
- `view=chats|steps`；
- `diff=changes|all`；
- 当前聚焦差异的稳定行标识。

行展开状态、已加载 turn detail 和 Pinboard 不进入 URL。

Compare route 必须使用显式左右前缀：

```text
?page=compare
&left_dataset=...
&left_file=...
&left_run_id=...
&left_agent_id=...
&left_session_id=...
&left_root_session_id=...
&right_dataset=...
&right_file=...
&right_run_id=...
&right_agent_id=...
&right_session_id=...
&right_root_session_id=...
&view=chats
&diff=changes
&focus=...
```

刷新 Compare URL 后，即使 Pinboard 已清空，左右轨迹仍可从 URL 恢复。

## 前端数据流

Diff 核心逻辑全部位于 `pchronicle-web`。后端不新增端点、不校验 root、
不执行对齐，也不返回差异判断。

1. 前端读取当前 `/api/query/tables` 的 `snapshot_id`。
2. 左右并行调用现有 `/api/explorer/run` 获取运行指标。
3. 左右通过现有 `/api/explorer/turns` 的 `offset/limit` 分页加载 summaries。
4. 加载完成后再次读取 catalog `snapshot_id`。
5. 如果前后 snapshot 不同，丢弃本次结果并自动重试一次；再次漂移则停止并提示用户刷新。
6. 前端复用 `group_chats`，执行确定性对齐并生成 Diff 行。
7. 展开行时，分别调用现有 `/api/explorer/turn` 并行加载完整证据。
8. 文本和 JSON Diff 在浏览器中计算并缓存于当前 Compare 会话。

Compare 页面不执行自动 catalog refresh。用户主动 Refresh 时，取消所有旧请求，
同时重新加载左右数据，并整体替换旧结果。禁止一侧显示新数据、另一侧保留旧数据。

## Chats 对齐规则

### 原则

- 复用现有 `group_chats` 结果。
- 不按 `Chat 1/2/3` 序号直接配对。
- 只自动配对高置信度项。
- 中低置信度或歧义项一律拆成“仅左侧 + 仅右侧”。
- 界面可以展示匹配依据，但不得把推测描述为事实。

### 确定性特征

每个 Chat 生成结构化 fingerprint：

- 规范化后的首个 user message；
- 有序 source/role 组成；
- 有序 tool name 序列；
- modality 集合；
- 相对序列位置。

规范化只执行首尾空白删除、连续空白折叠和换行统一；不删除标点、不改变大小写，
也不进行 embedding、LLM 推断或同义改写。

### 对齐过程

1. 使用两侧唯一出现的完全相同 user message 和非空 tool-name 序列建立硬锚点。
2. 按锚点把两侧序列切成局部窗口。
3. 局部窗口中，候选对必须满足以下至少一个条件：
   - 规范化 user message 完全相同；
   - 非空、有序 tool-name 序列完全相同。
4. 候选还必须具有兼容的 role 组成，并且在该窗口内是唯一的双向最佳匹配。
5. 不满足唯一性或结构约束的项不自动配对。
6. 配对结果记录 `AlignmentReason`，供诊断或界面提示。

对过大的未锚定窗口不运行全量二次复杂度匹配。窗口超过 200 个 Chat 时，
只执行唯一 exact-signature 配对，其余项保持未匹配。

## Steps 对齐规则

- Steps 只在已对齐的 Chat 对内部执行。
- 使用 role/source、tool name、tool_call_id（两侧稳定时）和相对顺序建立确定性配对。
- 不跨 Chat 移动 Step，也不执行全轨迹 Steps LCS。
- 未对齐 Chat 的所有 Steps 继承该 Chat 的新增或删除状态。
- 切回 Chats 时恢复之前的差异焦点和展开状态。

## Diff 语义与显示

### 行状态

- `equal`：结构和当前摘要层可比较事实相同；
- `modified`：已对齐，但内容或指标不同；
- `left_only`：仅 baseline 存在；
- `right_only`：仅 candidate 存在。

当数据被截断或证据未加载时，不得把未知部分标记为 `equal`。

### 折叠态

每行显示：

- Chat/Step 标识；
- role/source 组成；
- turn 或 sequence 范围；
- tool 数和 tool names；
- 显式错误数；
- token、latency 和已观测覆盖率；
- 对齐状态与可选匹配依据。

### 展开态

- 左右同时展开并共享高度。
- 普通消息执行确定性词级 Diff。
- tool call 先比较 tool name，再对 input JSON 执行字段级 Diff。
- tool output 为 JSON 时执行字段级 Diff，否则执行文本 Diff。
- JSON 差异使用稳定 path 表达；对象 key 顺序不构成差异，数组顺序构成差异。
- token、latency、TTFT、错误和时间戳作为独立事实行展示。
- 缺失测量显示 `unavailable`，不等同于零。
- 超长证据默认截断，可单独展开完整内容。

### 左右交换

交换左右后必须同时反转：

- baseline/candidate 标识；
- left-only/right-only 状态；
- 新增/删除颜色；
- 所有数值 delta 的方向；
- URL coordinates；
- 当前差异焦点的左右语义。

## 交互控制

- 共享 `Chats / Steps` segmented control；
- 共享 `All / Changes only` 控制；
- 上一处/下一处差异导航；
- 左右同步滚动；
- 配对行同步展开；
- 当前差异在中央 gutter 和两侧行同时高亮；
- 每侧标题固定，长列表滚动时保持 run 身份可见；
- 同 root 不满足时 Diff 动作直接禁用并说明原因。

## 组件边界

为避免继续扩大 `workspace.rs`，新增三个前端模块。

### `pinboard.rs`

- `PinnedRunKey`
- `PinnedRunsState`
- `PinBar`
- pin/unpin、去重、排序、兼容性和“更多”菜单

### `compare.rs`

- `CompareWorkspace`
- `CompareHeader`
- `MetricDiffStrip`
- `CompareToolbar`
- `AlignedTrajectoryView`
- `CompareRow`
- `EvidenceDiff`
- 数据加载、请求取消、URL 同步和懒加载缓存

### `diff.rs`

- 不依赖 Dioxus 的纯函数模块；
- Chat/Step fingerprint；
- anchor discovery 和局部对齐；
- `AlignmentReason` 与 Diff 行模型；
- 文本 token Diff；
- JSON path Diff；
- 指标 delta。

## 性能与资源预算

- 左右 summaries 分页并行加载。
- 初始安全预算为每侧最多 10,000 个 turn summaries 或约 16 MiB 序列化数据。
- 分页 loader 在反序列化前按原始 response text 的 UTF-8 字节数累计 16 MiB 预算。
- 达到任一预算即停止继续加载，并显示明确的部分覆盖提示。
- 未加载区域不参与 equal 判断。
- Chats 使用硬锚点分窗；局部二次复杂度窗口上限为 200。
- Steps 只在单个配对 Chat 内运行局部对齐。
- 可见 Diff 行超过 200 时启用列表虚拟化。
- turn detail 按需加载，并按完整 run key + turn id 做会话缓存。
- 每次加载分配递增 generation；切换 run、交换左右或刷新时，旧 generation 的响应
  必须被忽略，并在浏览器能力允许时主动 abort 请求。

## 错误与边界状态

- **root 不同：** Diff 禁用，并显示两侧 root。任一侧缺少 `root_session_id` 也视为
  不兼容，不能仅因为两侧都为空就允许比较。
- **一侧加载失败：** 保留成功侧身份和摘要；Retry 必须重新加载两侧并重新执行
  snapshot 检查，不能把新的失败侧数据与旧的成功侧数据直接组合。
- **轨迹消失：** 保留原 coordinates，显示不可用，不自动替换。
- **snapshot 漂移：** 自动整体重试一次；再次漂移后要求用户手动刷新。
- **预算截断：** 显示每侧已加载/总量和“部分比较”状态。
- **无可靠匹配：** 全部按新增/删除展示，并说明没有高置信度对齐。
- **turn detail 失败：** 错误限制在当前展开行，不破坏其余 Diff。
- **空比较：** 左右都无 Chat 时显示明确空状态，而不是空白列表。

## 可访问性

- pin、unpin、Diff、交换、过滤和差异导航均使用原生按钮。
- Pin chips 暴露完整轨迹 accessible name 和 pinned 状态。
- Compare 控件提供明确 label，不仅依赖图标或颜色。
- Diff 状态同时使用文字/符号和颜色。
- 对齐行可以键盘展开；焦点在 Changes only 过滤后移动到仍可见的对应项。
- 上一处/下一处差异可通过按钮和键盘快捷键操作。
- 左右列标题与行关系通过语义结构表达，屏幕阅读器能读出 baseline/candidate。

## 测试策略

### 纯函数单元测试

- `PinnedRunKey` 完整身份、去重和同 root 判断；
- 精确锚点、插入、删除、修改、重复 user message、重复 tool call、重试和歧义窗口；
- 中低置信度项不得自动配对；
- Steps 不得跨 Chat 对齐；
- 文本 token Diff；
- JSON 对象 key 无序、数组有序和嵌套 path Diff；
- 左右交换的状态和 delta 对称性。

### 组件测试

- 多个 pin、unpin、Clear all 和顶部溢出菜单；
- 不同 root 禁用 Diff；
- 当前轨迹不能与自身比较；
- Chats/Steps 同步切换；
- All/Changes only 与差异导航；
- 配对行同步展开和局部证据错误；
- 部分覆盖提示和未知项不得显示为 equal；
- Compare URL encode/decode 往返。

### 集成场景

1. pin 三条轨迹，跨 Runs/Detail 导航后仍存在；刷新后 pins 清空。
2. 当前轨迹与同 root pinned 轨迹进入 Compare。
3. 直接打开 Compare deep link，Pinboard 为空但比较可恢复。
4. 加载期间交换 run，旧请求结果不得覆盖新状态。
5. catalog snapshot 漂移时整体重试，不混用左右版本。
6. 超长轨迹达到预算后显示部分比较。
7. 一侧轨迹或单个 turn detail 不可用时保持其余工作区可操作。

后端路由和 Search 测试不属于本功能验收范围，因为本设计不修改它们。

## 分阶段交付

### 阶段 1：Pinboard 与 Compare Shell

- 多轨迹 pin/unpin；
- 顶部 Pin 条和“更多”菜单；
- 会话内状态；
- 当前轨迹与 pinned 轨迹发起 Diff；
- Compare route、页头、左右交换和 URL 恢复。

### 阶段 2：Chats Diff

- 左右 summaries 分页加载；
- catalog snapshot 漂移检测；
- 保守 Chat 对齐；
- 同步双栏、Changes only 和差异导航；
- 指标 delta、覆盖率和错误状态。

### 阶段 3：Evidence 与 Steps Diff

- 同步展开和 detail 懒加载；
- 文本词级 Diff；
- tool input/output JSON path Diff；
- Steps 模式；
- 缓存、虚拟化、键盘和完整可访问性验证。

## 已决策的替代方案

- 选择顶部横向 Pin 条，不采用底部暂存架或右侧抽屉。
- 允许 pin 多个轨迹，不限制为单一 baseline。
- pins 仅在当前页面会话中保留，不持久化。
- Compare 不进入左侧一级 rail。
- Compare 隐藏 Run paths，使用页头左右选择器。
- 默认同步双栏、Chats 和 Changes only。
- 只自动对齐高置信度项。
- Diff、root 校验和对齐完全在前端执行。
- 不新增后端 Compare 端点。

## 成功标准

- 用户可在连续浏览中维护多个临时比较候选。
- 从当前轨迹到同 root Diff 不需要复制任何 ID。
- 用户能在一个同步视图中定位结构、内容、tool 和指标差异。
- 所有自动配对均有确定性依据，歧义数据不会被错误合并。
- 数据不完整、测量缺失或 snapshot 漂移均被明确暴露。
- Compare deep link 可独立恢复和分享。
- 实现不修改后端 API，不进入 Search 或其他默认排除子系统。
