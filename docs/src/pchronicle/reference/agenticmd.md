# AgenticMD 轨迹格式

AgenticMD 是 pChronicle 的人读与调试视图：普通 Markdown 正文可以附带机器定位的块头。
它不是存储协议或事实源；系统生成的文件使用 `{session_id}.md`，读取器对人工编辑、
缺失字段和未知扩展保持宽容。

## 1. 文档结构

```markdown
---
format: persisting
block: speaker+json+markdown
session_id: run-123
agent_id: coding
turn_count: 1
---

<!-- persisting:block:user {"type":"text","length":24,"source":"user","step_id":1,"call_id":"call-1"} -->
请检查这个仓库。

<!-- persisting:block:agent {"type":"text","length":27,"source":"agent","step_id":2,"call_id":"call-1"} -->
我先查看目录结构。
```

Frontmatter 是可选会话摘要；块头也是可选的调试元数据。系统写入时会记录正文 UTF-8
字节长度以支持 live upsert；读取时允许省略长度，并按下一块边界解析。没有块头的普通
Markdown 会作为一个 `system` 调试块读取。

## 2. 块头

```text
<!-- persisting:block:{speaker} {json} -->
```

新输出的 `source` 对齐 Storyline，通常为 `user`、`agent` 或 `system`。JSON 常用字段是：

| 字段 | 含义 |
|---|---|
| `source` | Storyline turn source |
| `step_id` | Storyline turn 顺序 |
| `call_id` | 模型调用身份，用于配对与 live upsert |
| `type`, `length` | 生成器的展示/定位提示，不构成业务 schema |

时间、模型、provider、token、工具和 subagent 引用可以作为扩展字段出现。消费者应忽略
未知字段。旧 `role`、`seq`、`session`、`agent` 作为读取别名保留；speaker 与 JSON 字段
不一致时不再拒绝整个文档。

## 3. Frontmatter

pChronicle 定义并序列化 frontmatter，常用字段包括：

- `format`、`block`；
- `session_id`、`agent_id`、`model_name`、`provider`；
- `started_at`、`duration`、`turn_count`；
- `total_tokens`、`estimated_cost_usd`；
- `subagents` 与可选 `client` 来源信息。

零值、未知值和整段 frontmatter 都可以省略。嵌套对象与未知字段会被保留，不建立独立
于 Storyline 的强制 frontmatter schema。

## 4. Live 更新

启用 live Markdown 时，Gateway 在写入 canonical Lance 的同时将可见对话投影为 AgenticMD：

1. user 块按 `call_id` 写入；
2. 流式 assistant 使用相同 `call_id` 原地更新；
3. 重写一个 assistant 块时必须保留其后的 user 块；
4. 内部探测、重复历史和不可见 thinking 不进入正文；
5. 图片等多模态内容用稳定占位符表示，不内嵌大体积 base64。

这些规则属于实时投影策略。AgenticMD 文件失败或缺失不改变 canonical append 结果。

## 5. 与 Lance 的关系

Lance events 负责保真、replay、stats 和结构化查询。内部 trajectory operation 可以从
Lance 重建 AgenticMD，但当前公共 `pchronicle` CLI 不提供 AgenticMD materialize 或 import
子命令。AgenticMD 不会自动 compact 或恢复 canonical event；公共交换使用
[`pchronicle import/export`](cli.md) 支持的格式。

## 6. 示例与实现

- Gateway 端到端定量示例：`examples/pvisor/04-gateway-llm-control/`
- Lance/ATIF 存储与分析示例：`examples/pchronicle/`
- 格式与视图实现：`crates/persisting-pchronicle/src/formats/`、`src/projection/`
- [pChronicle 轨迹存储](../design/trajectory-storage.md)
- [轨迹格式与交换边界](formats/index.md)
