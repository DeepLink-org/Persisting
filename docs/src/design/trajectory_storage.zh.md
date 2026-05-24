# 轨迹存储模型

Agent 轨迹在 Persisting 中同时服务两类读者：**机器**（回放、统计、检索）与**人**（阅读、diff、审查）。两种物理形态——**Lance 列存**与会话 **Markdown**——由同一存储抽象统一管理，上层不必关心具体落盘格式。

---

## 1. 设计目标

| 目标 | 说明 |
|------|------|
| **双视图** | Lance 适合顺序回放与分析；Markdown 适合人读与版本对比 |
| **统一语义** | 无论哪种后端，一条轨迹都是带全局序号的**事件序列** |
| **可并存** | 同一会话可同时写 Lance 与 Markdown，或按已有数据自动选择 |
| **与捕获解耦** | 捕获层只产出逻辑记录；存储层决定如何持久化 |

---

## 2. 分层

```
数据源（代理流量 / IDE 日志 / 历史导出）
        │
        ▼ 归一化为逻辑轨迹事件
    捕获与对话层
        │
        ▼ 带序号的事件批次
    轨迹引擎（路径解析、追加、回放、统计）
        │
        ▼ 存储抽象
   ┌────┴────┐
   │         │
 Lance    会话 Markdown
（列存）   （块式文档）
```

**捕获层**负责 HTTP 代理、会话路由、对话块渲染。  
**引擎层**负责会话定位、格式选择与读写编排。  
**存储抽象**定义追加、回放、统计、检查是否存在、格式声明与路径显示等操作，由 Lance 与 Markdown 各自实现。

---

## 3. 会话命名空间

轨迹按 **存储根 → Agent → Session** 三级组织：

| 层级 | 含义 |
|------|------|
| 存储根 | 用户指定的输出目录（一次 capture run 或长期 store） |
| Agent | 项目或 agent 标识，同一 agent 下可有多个 session |
| Session | 一次 run 或逻辑会话；subagent 可嵌套在父 session 下 |

**主 session** 与 **subagent session** 分目录存放：并行 subagent 各自独立，主 session 保留 spawn 关系与汇总对话。

嵌套规则、路由策略见 [内嵌 LLM 代理与 Capture](llm_capture_proxy.zh.md)。

---

## 4. 两种物理形态

### 4.1 Lance（机器视图）

- 每个 session 对应一个 Lance 数据集
- 每行一条逻辑事件，带单调递增序号
- 适合：`replay`、`stats`、后续索引与检索

### 4.2 Markdown（人读视图）

- 每个 session 一个会话文档（默认 `0001.md`）
- 每块 = 元数据注释 + 裸对话正文（TLV 结构）
- 适合：直接打开阅读、git diff、人工 review

两种形态描述**同一条时间线**，只是编码不同。详见 [轨迹 Markdown 格式](trajectory_tlv_format.zh.md)。

---

## 5. 存储策略

| 策略 | 行为 |
|------|------|
| 仅 Lance | 只写列存，适合纯分析管线 |
| 仅 Markdown | 只写会话文档，适合以人读为主的 capture |
| 双写 | 同时维护两种视图 |
| 自动 | 若 session 已有数据则沿用；新 session 可按输入类型推断 |

**读取优先级**：双写或自动模式下，回放与统计默认以 Lance 为准（若存在）；纯 Markdown session 则从块序列还原。

---

## 6. 逻辑事件

不论后端，一条轨迹事件通常包含：

| 维度 | 说明 |
|------|------|
| 序号 | 会话内全局单调，保证 replay 顺序 |
| 来源 | 代理捕获、IDE 导入、网关导出等 |
| 类型 | 用户消息、模型回复、LLM 请求/响应、spawn 关联等 |
| 时间 | 事件发生时刻 |
| 载荷 | 原始 JSON 或渲染后的正文 |

Capture 实时写入时可直接追加 Markdown 块；批量导入或 CLI 追加则经引擎统一编排。

---

## 7. 与其它能力的关系

- **Capture**：轨迹的主要生产者；代理路径实时写入，import 路径事后合并
- **Trajectory CLI**：追加、回放、统计的用户入口
- **Search**（演进）：未来可在 Lance 轨迹上建索引，与 Agent Search 打通

---

## 8. 相关文档

- [轨迹 Markdown 格式](trajectory_tlv_format.zh.md) — 块结构与元数据约定
- [内嵌 LLM 代理与 Capture](llm_capture_proxy.zh.md) — 捕获、路由、subagent 拓扑
- [`persisting capture`](cli_capture_command.zh.md) — 捕获命令与使用模式
- [`persisting trajectory`](cli_trajectory_command.zh.md) — 回放与统计
