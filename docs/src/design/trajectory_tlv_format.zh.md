# 轨迹 Markdown 格式

会话 Markdown 是轨迹的**物化人读视图**：从 Lance raw event log **materialize** 得到，按对话块组织，便于直接打开、diff 与 code review。

> **注意**：Capture 运行时**不再**直接向 Markdown 追加块。`-f md` 时先写 Lance，结束后再 materialize 生成本格式文件。

默认文件名：**`{session_id}.md`**。兼容读取 legacy **`0001.md`**、**`trajectory.tlv.md`**。

---

## 1. 设计意图

| 目标 | 说明 |
|------|------|
| **可读** | 正文是纯 Markdown，元数据藏在 HTML 注释 |
| **可解析** | 每块带 JSON 元数据，可 compact 回 Lance |
| **可 diff** | 适合 git 跟踪 Agent 行为变化 |
| **有损物化** | 相对 Lance 省略内部流量与 lifecycle；见 [存储模型 §1](trajectory_storage.zh.md) |

---

## 2. 块模型（TLV）

每块由两部分组成：

```
[类型/长度 元数据]  ← <!-- persisting:block:{speaker} {json} -->
[值 正文]           ← 裸 Markdown 消息体
```

`{speaker}`：`user` / `assistant` / `tool` 等。`{json}` 含 `kind`、`seq`、`turn`、`model`、token 统计、spawn 关联等。

正文**仅**放可见消息内容；不应含与块边界相同的注释行。

### 2.1 完整示例

```markdown
---
format: "persisting:1.0"
block: "speaker json"
---

<!-- persisting:block:user {"kind":"llm.request","seq":0,"turn":1} -->
你好，请帮我查询一下昨天的销售数据

<!-- persisting:block:assistant {"kind":"llm.response","seq":1,"turn":1,"model":"deepseek-chat"} -->
我来帮你查询昨天的销售数据。
```

YAML frontmatter 声明 `format: "persisting:1.0"`。完整示例见 [`examples/trajectory-tlv/demo-agent/demo-run-001/0001.md`](../../examples/trajectory-tlv/demo-agent/demo-run-001/0001.md)。

---

## 3. 使用场景

| 场景 | 行为 |
|------|------|
| Capture `-f md` | Lance 批量写入 + **流式 append** Markdown 块（可 `tail -f`） |
| Capture `-f bin` | 仅 Lance |
| `trajectory materialize` | 全量扫描 Lance，**重写** Markdown（离线修复/补全） |
| `trajectory add` + markdown 存储 | append Lance 后自动 materialize |
| `trajectory add` + 输入为 `.md` | 解析 TLV → compact/append 到 Lance |
| `trajectory replay` | 有 Lance 时读 Lance；纯 md session 从块还原 |

---

## 4. 与 Lance 的关系

| 维度 | Markdown（物化） | Lance（canonical） |
|------|------------------|-------------------|
| 角色 | 人读视图 | 全量 event log |
| 单位 | 块 | 行 |
| 写入 | materialize / compact 导入 | 唯一 append 目标 |
| 数据完整性 | 有损（过滤内部事件） | 无损 |
| 典型操作 | 打开、git diff | replay、stats、Search |

双向转换由 `persisting-capture::trajectory_convert` 与引擎 `trajectory::convert` 实现。详见 [轨迹存储模型](trajectory_storage.zh.md)。

---

## 5. Compact 回 Lance

从 Markdown 导入 Lance 时：

- 每块 → 一条 `CaptureRecord`（`llm.request` / `llm.response` 等）
- 块头字段额外写入 `payload._tlv`，保留 TLV 元数据
- 完整 HTTP body 若未出现在块中，则由正文近似重建

因此 **Markdown → Lance 可压缩存储更多结构化字段**，但无法恢复 materialize 时已丢弃的内部事件。

---

## 6. 相关文档

- [轨迹存储模型](trajectory_storage.zh.md)
- [内嵌 LLM 代理](llm_capture_proxy.zh.md)
- [`persisting capture` 命令](cli_capture_command.zh.md)
- [`persisting trajectory` 命令](cli_trajectory_command.zh.md)
