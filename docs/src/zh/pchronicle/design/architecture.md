# pChronicle 架构

pChronicle 专注于 Agent 轨迹数据处理管线：

```text
ATIF / ACTF / OpenAI Messages / Codex / Claude Code
  → 解码并校验 Storyline
  → 直接写入 Storyline Lance
  → 固定 Snapshot 并执行 DataFusion 查询
  → CLI / Warehouse API / Web UI / 导出
```

AgenticMD 与 Storyline JSON 是 Storyline 的其他编码。Compact JSONL 保存任意
JSON 记录，不赋予轨迹语义。

## 职责

| 组件 | 职责 |
| --- | --- |
| `persisting-pchronicle` | 模型、编解码、数据源、Lance 存储、Snapshot 与查询 |
| `persisting-pchronicle-cli` | 命令、导入导出编排、loopback Warehouse HTTP、静态资源嵌入 |
| `pchronicle-web` | 浏览、分析、存储检查与 Assistant |

Dataset 是规范化的本地路径或对象存储 URI。pin 与挂载名最终解析成路径。
外部实体 ID 只在 Source 内有效，查询行保留来源路径。可选 Directory 负责认证
与授权路径解析，不保存轨迹数据。

## 读取

发现阶段固定 Source 成员和版本。Dataset 与 `_file_` 裁剪之后才延迟打开物理
Source。一次操作使用同一 Snapshot；独立 Source 不承诺全局事务时间。
规范化表包括 `sources`、`runs`、`steps`、`tool_calls` 与 `trajectories`。
Compact JSONL 暴露记录表。

CLI 与 Web 共享查询语义。SQL 有资源限制且只读，拒绝修改数据、网络函数和任意
文件系统函数。

## 写入

导入将支持的格式解码为 Storyline 后直接写入。Storyline store 在内容对象与
表变更持久化之后，通过 `CURRENT` 发布精确表版本。发布失败保留上一 Snapshot。
写入租约与 generation 校验防止并发替换静默丢失数据。维护按存储发布协议执行
compaction 和不可达内容回收。

## Warehouse

`pchronicle serve` 在 loopback 地址挂载命名路径。刷新先构建完整的新 Snapshot，
再切换读者。`--catalog-config` 启用 Directory 认证，并把数据操作交给限定授权
挂载与后端凭证的有界 worker。普通模式通过 loopback API 读取本地挂载。

参见 [Snapshot](catalog.md)、[轨迹存储](trajectory-storage.md)、
[Storyline Lance](storyline-lance.md) 与 [CLI 参考](../reference/cli.md)。
