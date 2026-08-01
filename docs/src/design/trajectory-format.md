# AgenticMD 轨迹格式

AgenticMD 是 pChronicle 的人读轨迹表示：普通 Markdown 正文加可机器定位的块头。
文件统一使用 `{session_id}.md`，不读取编号文件或 `.tlv.md` 历史格式。

## 1. 文档结构

```markdown
---
format: persisting:1.0
block: speaker+json+markdown
session: run-123
agent: coding
turns: 1
---

<!-- persisting:block:user {"v":1,"seq":1,"call_id":"call-1"} -->
请检查这个仓库。

<!-- persisting:block:assistant {"v":1,"seq":2,"call_id":"call-1"} -->
我先查看目录结构。
```

Frontmatter 是会话摘要；每个对话块由一行注释头和紧随其后的 Markdown 正文组成。
正文长度和结构由解析器验证，不依赖标题或空行猜测边界。

## 2. 块头

```text
<!-- persisting:block:{speaker} {json} -->
```

`speaker` 当前为 `user` 或 `assistant`。JSON 的稳定核心字段是：

| 字段 | 含义 |
|---|---|
| `v` | 块 schema 版本 |
| `seq` | Story 内顺序 |
| `call_id` | 模型调用身份，用于配对与 live upsert |

时间、模型、provider、token、工具和 subagent 引用可以作为扩展字段出现。消费者应忽略
未知字段，不应把正文中的相似 HTML 注释误认为块头。

## 3. Frontmatter

pChronicle 定义并序列化 frontmatter，常用字段包括：

- `format`、`block`；
- `session`、`agent`、`model`、`provider`；
- `started`、`duration`、`turns`；
- `total_tokens`、`estimated_cost_usd`；
- `subagents` 与可选 `client` 来源信息。

零值和未知值可以省略。Gateway 可以更新会话 rollup，但不能定义另一套 frontmatter
schema。

## 4. Live 更新

Markdown 模式下，Gateway 将可见对话投影为 AgenticMD：

1. user 块按 `call_id` 写入；
2. 流式 assistant 使用相同 `call_id` 原地更新；
3. 重写一个 assistant 块时必须保留其后的 user 块；
4. 内部探测、重复历史和不可见 thinking 不进入正文；
5. 图片等多模态内容用稳定占位符表示，不内嵌大体积 base64。

这些规则属于实时投影策略。文档解析、索引、写入和分页 replay 由 pChronicle 提供。

## 5. 与 Lance 的关系

AgenticMD 强调可读性，Lance events 强调保真和结构化查询。`history materialize` 可以从
Lance 重建 AgenticMD；反向导入只能恢复 Markdown 中实际存在的信息，不能恢复被投影
过滤掉的原始协议字段。

## 6. 示例与实现

- 静态示例：`examples/trajectory-agenticmd/`
- 端到端示例：`examples/capture-walkthrough/`
- 格式与存储实现：`crates/persisting-pchronicle/src/formats/`、`src/store/`
- [pChronicle 轨迹存储](trajectory.md)
