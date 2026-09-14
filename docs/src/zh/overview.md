---
hide:
  - toc
---

# 开始使用

沿着一条主线，从安装 CLI 到运行一个可以审查和查询的 Agent Run。

## 1. 安装

安装命令行工具，并确认两个产品入口可用：

```bash
pip install persisting
pvisor --help
pchronicle --help
```

macOS 使用 staged host workspace 前，需要安装 macFUSE：

```bash
brew install --cask macfuse
```

[阅读安装指南 →](installation.md)

## 2. 使用 pVisor 运行 Agent

在 staged workspace 中运行 Agent，检查实际发生的事情，只应用你信任的修改：

```bash
pvisor run --stage ./runs/task-001 -- codex
pvisor review last
pvisor apply last --path src
```

Agent 工作期间，基础项目保持不变。Run Bundle 会记录文件 Effect、实际控制机制、网络证据和警告。
继续阅读[运行第一个 Agent](pvisor/get-started.md)完成完整流程，再学习[选择性 apply](pvisor/guides/review-apply.md)。

**完成本节后：**你会得到一次经过审查的项目修改，并清楚哪些内容仍留在 stage 中。

## 3. 记录并分析 Agent 轨迹

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

**完成本节后：**你可以把一个答案连接到产生它的 Dataset 和 Source。需要连接两个产品时，继续阅读[pVisor 捕获指南](pvisor/guides/capture.md)。
