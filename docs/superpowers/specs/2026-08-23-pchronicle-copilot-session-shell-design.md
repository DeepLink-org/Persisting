# Copilot 会话壳设计

日期：2026-08-23
状态：已定，直接施工
范围：`pchronicle-web` Trajectory Copilot 停靠栏。不改 Copilot 工具、不让助手操作系统、不改 Analyze 的提问/写 SQL。

## 1. 决策

吸收 Langfuse Assistant 的**会话壳**：全局历史、新对话、顶栏动作、气泡与输入条。Copilot 仍是只读取证。切历史会切轨迹，并尽量保住当前页类型。

## 2. 存储

- 索引键 `pchronicle_copilot_index`：`{ sessions, active_id }`。
- 正文键 `pchronicle_copilot_session:{id}`：现有 `CopilotThread`（200KB 裁剪不变）。
- 每项 meta：`id`、完整 `RunSummary` 快照、标题（第一条用户消息截断）、`updated_at`。
- 索引最多 30 条，按更新时间淘汰；空草稿不进索引，直到发出第一条用户消息。
- 迁移：若存在旧键 `pchronicle_copilot:{run.query}` 且该 run 尚无会话，包成一条会话后删除旧键。

## 3. 交互

停靠栏不变。顶栏：会话标题、新对话、历史、设置、加宽、关闭。当前已是空草稿或未选中 run 时，新对话禁用。Copilot busy 时禁用新对话与点历史。

历史为面板内弹出列表（全局、新到旧）。点一项加载该会话，并用快照设置 `selected_run`。页类型：Analyze / 详情留下；Data / Runs 进详情。切到不同 run 时清空 `analysis_session_id`。面板按会话正文立刻显示，不等分析接口。分析失败走现有错误条，对话保留。

删当前会话：回落到同 run 最近一条，否则空草稿。

输入条：Enter 发送、Shift+Enter 换行。下方上下文行（当前轨迹 / 选中 step）。不做中断生成。

## 4. 测试

纯函数：迁移、空草稿不进索引、30 条淘汰、标题、新对话 no-op、页类型映射、删除回退。不测 LLM。
