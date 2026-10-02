# 系统设计

Persisting 围绕 [pChronicle](../pchronicle/index.md) 构建 Agent 轨迹存储引擎。
Gateway 捕获模型流量，共享事件契约将记录交给存储，CLI 和 Web UI 提供 Dataset 浏览与分析入口。

```text
Gateway 捕获 ── EventRecord ── pChronicle 存储 ─┐
ATIF / ACTF / OpenAI Messages / Storyline Source ┴─> Dataset Snapshot
                                                       └─> 查询 / 交换 / Web UI
```

Canonical events 保存已记录的事实；Storyline 规范化和查询投影将受支持的 Source
呈现为统一视图。Snapshot 固定查询使用的 Source 版本，导入不会补出源数据中不存在的事实。

## 按问题继续阅读

- [组件职责与数据流](architecture.md)
- [设计原则](design-principles.md)
- [存储与查询实现](../pchronicle/design/index.md)
- [Gateway 捕获](../pchronicle/guides/serve-gateway.md)
- [项目工程说明](../project/engineering.md)
