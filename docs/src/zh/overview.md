---
hide:
  - toc
---

# 开始使用

使用 pChronicle 捕获、导入并查询持久 Agent 历史。

## 1. 安装

安装命令行工具，并确认 pChronicle 入口可用：

```bash
pip install persisting
pchronicle --help
```

[阅读安装指南 →](installation.md)

## 2. 记录并分析 Agent 轨迹

Agent 运行后，使用 pChronicle 把轨迹变成可以检查和查询的 Dataset。先用临时示例，安全地熟悉流程：

```bash
pchronicle onboard
```

引导会带你列出数据、查看汇总，并提出一个只读 SQL 问题。然后用自己的数据重走同一条路径：

```bash
pchronicle onboard ./trajectory-data
pchronicle query ./trajectory-data \
  --sql 'SELECT session_id, COUNT(*) AS steps FROM dataset.steps GROUP BY session_id'
```

继续阅读[探索第一个 Dataset](pchronicle/get-started.md)，学习 Dataset 健康检查、证据定位、格式、导入导出和只读 Web/API。

**完成本节后：**你可以把一个答案连接到产生它的 Dataset 和 Source。需要记录新的模型请求时，使用[Gateway 捕获](pchronicle/guides/serve-gateway.md)。
