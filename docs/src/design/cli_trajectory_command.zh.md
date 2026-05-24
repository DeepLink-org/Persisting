# `persisting trajectory` 设计说明

Trajectory 命令面向 **Agent 执行轨迹** 的写入、回放与统计，底层遵循 [轨迹存储模型](trajectory_storage.zh.md)（Lance 列存 + 会话 Markdown）。

短名：**`traj`**。

---

## 1. 能力概览

| 操作 | 用户意图 |
|------|----------|
| **add** | 向某 agent 的某 session 批量追加事件 |
| **replay** | 按全局序号分页读取事件 |
| **stats** | 查看 session 规模；可选逐轮树状摘要（含 subagent 分支） |

---

## 2. 会话定位

轨迹按 **存储根 / agent / session** 组织。CLI 可：

- 显式指定 agent 与 session id；或
- 传入 session 目录，自动解析；或
- 在 storage 根下仅有一个 session 时自动选中

**Subagent** 通过「父 session + 子 session」嵌套路径访问，与 capture 目录拓扑一致。

id 规则：单层路径段，不含路径分隔符，避免目录穿越。

所有 trajectory 子命令的首个位置参数均为 **`<STORAGE>`**——轨迹 store 根目录路径。

```
persisting trajectory add     <STORAGE> [OPTIONS]
persisting trajectory replay  <STORAGE> [OPTIONS]
persisting trajectory stats   <STORAGE> [OPTIONS]
```

---

## 3. 存储选择

| 选项 | 含义 |
|------|------|
| Lance | 仅列存 |
| Markdown | 仅会话文档 |
| both | 双写 |
| auto | 按已有数据或输入类型推断 |

CLI 通过 `--storage-format` 选项选择存储后端（默认 auto）。

Capture 通过 `-f md|bin` 直接选定格式，不走 auto 双写逻辑。

---

## 4. 输入格式（add）

| 格式 | 典型用途 |
|------|----------|
| TOML | 结构化批量记录（默认） |
| JSONL | 管道、日志导出 |
| Markdown | 导入整份会话文档（含 legacy `.tlv.md`） |

stdin 为默认输入；格式无法从文件名推断时需显式指定。

---

## 5. 输出约定

成功时 **stdout 为 TOML**，便于脚本解析：

- **add**：返回实际使用的 agent / session id 及写入摘要
- **replay**：`records` 为事件列表（Markdown 回放偏对话视图，Lance 回放为完整结构）
- **stats**：行数/块数、数据集状态；`--detail` 时改为人类可读的逐轮树

---

## 6. 与 Capture 的分工

| 组件 | 角色 |
|------|------|
| Capture | 生产轨迹（实时或 import） |
| Trajectory | 消费轨迹（回放、统计、手动追加） |

通常工作流：`capture run …` 产生 store → `trajectory replay` / `stats` 审查。

---

## 7. 相关文档

- [轨迹存储模型](trajectory_storage.zh.md)
- [轨迹 Markdown 格式](trajectory_tlv_format.zh.md)
- [CLI 整体架构](cli_architecture.zh.md)
