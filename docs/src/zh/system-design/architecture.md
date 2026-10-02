# 系统架构

pChronicle 负责 Agent 轨迹历史的捕获、存储、查询与交换。本文定义引擎、入口和内部库
之间的职责；物理布局见 [pChronicle 设计](../pchronicle/design/index.md)。

## 组件职责

| 组件 | 职责 |
|---|---|
| `persisting-pchronicle` | 轨迹模型、存储、Source 发现、Dataset Snapshot、有界查询、格式转换与版本血缘 |
| `persisting-pchronicle-cli` | `pchronicle` 命令、本地只读 Warehouse API、可选 Control 服务、Gateway 配置与内嵌 Web 资源 |
| `pchronicle-web` | Dataset 浏览与查询界面 |
| `persisting-events` | 存储无关的 `EventRecord` 及可选的带版本 Control 客户端与协议 |
| `persisting-gateway` | 模型协议适配、转发、会话关联与轨迹捕获 |
| `persisting-overlaynet` | 代理传输、请求分类与已拦截流量的策略执行 |
| `persisting-agentctl` | 共享控制类型、策略状态转换与协作式客户端协议 |

## 捕获与导入

```text
Agent / SDK 请求
  → Gateway 协议处理与捕获
  → EventRecord
  → pChronicle canonical event 存储
  → Dataset Snapshot → 有界查询 → CLI / Web / 导出

受支持的文件与对象存储 Source
  → 发现并固定 Source 版本
  → Storyline 规范化与查询投影
  → Dataset Snapshot → 有界查询 → CLI / Web / 导出
```

Gateway 捕获经过它的流量，导入则直接读取受支持的外部格式。两条路径保留可用身份与
来源信息，不会补出源数据缺失的执行或隔离证据。

## 写入与读取边界

CLI 可以在进程内调用引擎。集成方也可以启动
`pchronicle serve --control 127.0.0.1:0 DATASET`，使用 `persisting-events` 中带认证、
带版本的本地协议。只有成功的 append ACK 才确认持久化；响应丢失意味着结果不确定，
调用方不能据此认定事件没有写入。

pChronicle 拥有物理 schema、writer fencing、manifest 发布与维护。Producer 提交逻辑
记录，不另行定义存储布局。详见 [RFC-0007](../rfcs/0007-events-contract-pchronicle-sidecar.md)
与[轨迹存储](../pchronicle/design/trajectory-storage.md)。

Warehouse HTTP API 与 Web UI 只读，单独启用的 Control 和 Gateway 捕获路径可以写入。
本地服务拒绝公共绑定地址，Control 协议面向可信本地进程。

## 事实、视图与版本

Canonical events 保留原始逻辑 payload。Storyline 为交换格式提供规范化模型，投影支持
分析而不替代原始事实。查询读取固定的 Snapshot，版本血缘标识派生输出。

捕获覆盖范围取决于 Source 中实际存在的记录。代理策略仅作用于到达代理的流量，不能
证明进程级网络隔离。详见 [Gateway 捕获](../pchronicle/guides/serve-gateway.md)和
[事实与投影](../pchronicle/concepts/facts-and-projections.md)。
