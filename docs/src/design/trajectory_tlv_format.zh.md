# 轨迹 Markdown 格式

会话 Markdown 是轨迹的**人读视图**：与 Lance 列存描述同一条时间线，但按对话块组织，便于直接打开、diff 与 code review。

默认文件名：**`0001.md`**（每个 session 一个文档）。旧版 **`trajectory.tlv.md`** 仍可读取，新写入统一用 `0001.md`。

---

## 1. 设计意图

| 目标 | 说明 |
|------|------|
| **可读** | 正文是纯 Markdown，元数据不污染可见内容 |
| **可解析** | 每块带结构化元数据，引擎可还原为逻辑事件 |
| **可 diff** | 适合 git 跟踪 Agent 行为变化 |
| **与列存对齐** | 块序号与 Lance 行序号对应同一会话时间线 |

---

## 2. 块模型（TLV）

每块由两部分组成：

```
[类型/长度 元数据]  ← HTML 注释中的 JSON，格式为 <!-- persisting:block:{speaker} {json} -->
[值 正文]           ← 裸 Markdown 消息体
```

其中 `{speaker}` 表示角色（`user` / `assistant` / `tool` 等），`{json}` 为结构化元数据。

元数据至少包含块类型与正文字节长度；常见扩展：轮次、角色、事件种类、模型名、token 统计、spawn 关联等。

正文**仅**放消息内容——用户提问、模型回复、工具块等。正文内不应出现与块边界相同的注释行，以免解析歧义。

### 2.1 完整示例

```markdown
---
format: "persisting:1.0"
block: "speaker json"
---

<!-- persisting:block:user {"type":"markdown","turn":1} -->
你好，请帮我查询一下昨天的销售数据

<!-- persisting:block:assistant {"type":"markdown","turn":1,"model":"claude-sonnet-4-6","tokens":156} -->
我来帮你查询昨天的销售数据。

<!-- persisting:block:tool {"type":"tool_call","turn":1,"name":"query_sales","id":"call_123"} -->
查询参数：{"date": "2026-05-23"}
```

YAML frontmatter 声明格式版本（`format: "persisting:1.0"`）与块布局约定，便于第三方工具识别。

更完整的实际示例见 [`examples/trajectory-tlv/demo-agent/demo-run-001/0001.md`](../../examples/trajectory-tlv/demo-agent/demo-run-001/0001.md)。

复杂场景（工具调用、subagent spawn）在元数据中携带关联字段与元数据；正文末尾可有显式引用注释，方便人读时跳转 subagent 目录。

---

## 3. 使用场景

| 场景 | 行为 |
|------|------|
| Capture `-f md` | 每轮对话实时追加块 |
| Trajectory add | 导入整份 Markdown 或双写时同步生成 |
| Trajectory replay | 从块序列还原为结构化记录列表 |

---

## 4. 与 Lance 的关系

| 维度 | Markdown | Lance |
|------|----------|-------|
| 主要读者 | 人 | 机器 |
| 单位 | 块 | 行 |
| 典型操作 | 打开文件、git diff | replay、stats、未来索引 |

两者由 [轨迹存储模型](trajectory_storage.zh.md) 统一编排，可选双写或择一。

---

## 5. 示例文件

- [`examples/trajectory-tlv/demo-agent/demo-run-001/0001.md`](../../examples/trajectory-tlv/demo-agent/demo-run-001/0001.md)

---

## 6. 相关文档

- [轨迹存储模型](trajectory_storage.zh.md)
- [内嵌 LLM 代理](llm_capture_proxy.zh.md) — subagent 目录与块级关联
- [`persisting capture` 命令](cli_capture_command.zh.md) — 产生本格式文件的主要入口
