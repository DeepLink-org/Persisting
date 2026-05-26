# `persisting trajectory` 设计说明

Trajectory 命令面向 **Agent 执行轨迹** 的写入、回放、统计与 **两层存储转换**，底层遵循 [轨迹存储模型](trajectory_storage.zh.md)（Lance canonical + TLV Markdown 物化）。

短名：**`traj`**。

---

## 1. 能力概览

| 操作 | 用户意图 |
|------|----------|
| **add** | 向某 agent 的某 session 批量追加事件（写 Lance；markdown 格式时自动 materialize） |
| **replay** | 按全局序号分页读取事件（默认 Lance） |
| **stats** | 查看 session 规模；可选逐轮树状摘要（含 subagent 分支） |
| **materialize** | Lance raw log → TLV Markdown（有损物化视图） |

---

## 2. 会话定位

轨迹按 **存储根 / agent / session** 组织。CLI 可：

- 显式指定 agent 与 session id；或
- 传入 session 目录，自动解析；或
- 在 storage 根下仅有一个 session 时自动选中

**Subagent** 通过 `--root-session-id` 指定 capture run 目录名，与主 session 共用 `{store}/{agent_id}/{run_id}/` 下的 sibling Lance / Markdown。详见 [轨迹存储 §7](trajectory_storage.zh.md)。

id 规则：单层路径段，不含路径分隔符，避免目录穿越。

```
persisting trajectory add          <STORAGE> [OPTIONS]
persisting trajectory replay       <STORAGE> [OPTIONS]
persisting trajectory stats        <STORAGE> [OPTIONS]
persisting trajectory materialize  <STORAGE> [OPTIONS]
```

---

## 3. 存储格式（`--storage-format`）

所有 append **只写 Lance**。`markdown` / `both` 在 append 后触发 **materialize**。

| 选项 | Append | Materialize |
|------|--------|-------------|
| **lance** | Lance only | 否 |
| **markdown** | Lance + import 批量 append md | 是（每批 append 后） |
| **both** | 同 markdown | 是 |
| **auto** | 空 session → Lance；不自动物化 | 否 |

**读取**（replay / stats）：有 Lance 时优先 Lance；否则读 Markdown 块。

Capture 使用 `-f md|bin`，不走 `--storage-format`；`-f md` 时 Proxy **CaptureEngine live upsert** Markdown（import 仍走批量 append），见 [capture 文档](cli_capture_command.zh.md)。

---

## 4. 输入格式（add）

| 格式 | 典型用途 |
|------|----------|
| TOML | 结构化批量记录（默认） |
| JSONL | 管道、日志导出 → Lance |
| Markdown | 导入 TLV 文档 → 解析后写入 Lance（`--storage-format markdown` 时再物化） |

stdin 为默认输入；格式无法从文件名推断时需显式 `--format`。

---

## 5. materialize（全量重写）

从 Lance **全量扫描**生成/覆盖**单个 storage_session_id** 对应的 Markdown（非 capture 热路径；日常 `-f md` 依赖 CaptureEngine live upsert；run 结束先看 `reconcile.json`，**不一致时再 materialize**）：

```bash
# 主 session（capture run 下 header session id）
persisting trajectory materialize store \
  --agent-id deepseek-proxy \
  --session-id fb47835b-e10d-4b29-abc3-68f4594ebce3 \
  --root-session-id run-20260524-161537-122998000

# subagent
persisting trajectory materialize store \
  --agent-id deepseek-proxy \
  --session-id agent-a2560e716f0b8b526 \
  --root-session-id run-20260524-161537-122998000
```

输出（stdout TOML）含：`markdown_path`、`lance_rows`、`markdown_blocks`、`skipped_events`。

适用：`-f bin` capture 后补做人读视图；或 live md 与 Lance 不一致时 **从 Lance 重建**（如并发 in-flight 丢块、历史版本路径错误）。

---

## 6. 输出约定

成功时 **stdout 为 TOML**，便于脚本解析：

- **add**：agent / session id、写入摘要（含 materialize 说明）
- **replay**：`records` 为 JSON 事件列表（Lance 为完整结构）
- **stats**：Lance 行数；`--detail` 时逐轮树状摘要
- **materialize**：物化路径与行/块统计

---

## 7. 与 Capture 的分工

| 组件 | 角色 |
|------|------|
| Capture | 生产轨迹（实时 proxy 或 import）→ **Lance**；`-f md` 时 CaptureEngine live upsert + 可选 materialize |
| Trajectory | 消费轨迹（replay、stats）；手动 add / materialize |

典型工作流：

```bash
persisting capture run -f md -o ./store -c proxy.yaml -- claude
persisting trajectory stats store --agent-id deepseek-proxy \
  --session-id fb47835b-e10d-4b29-abc3-68f4594ebce3 \
  --root-session-id run-20260524-161537-122998000
persisting trajectory replay store --agent-id deepseek-proxy \
  --session-id agent-a2560e716f0b8b526 \
  --root-session-id run-20260524-161537-122998000 \
  --storage-format lance --limit 20
```

---

## 8. 相关文档

- [轨迹存储模型](trajectory_storage.zh.md)
- [轨迹 Markdown 格式](trajectory_tlv_format.zh.md)
- [CLI 整体架构](cli_architecture.zh.md)
