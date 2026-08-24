# pChronicle

**pChronicle 用于浏览、查询、交换和服务 Agent 轨迹 Dataset。** 它既可以读取
Persisting 产生的轨迹，也可以直接读取受支持的外部格式；使用 pChronicle 不要求先运行
pVisor。

在 Persisting“从模型状态到 Agent 历史”的主线中，pChronicle 是持久、可查询的 Agent 历史层。

## 你只需要面对 Dataset

**Dataset 是 pChronicle 操作的统一数据对象。** 它是一组可以被浏览、查询、分析、导入、
导出或提供服务的 Agent 轨迹数据。

一个 Dataset 可以表现为：

- 本地目录或文件（`./local/path`）；
- 对象存储中的 URI 前缀（`s3://bucket/prefix`）；
- 指向上述位置的用户 alias（`@alias-name`）。

pChronicle 会发现并规范化该位置中受支持的数据。开始使用命令行前，你不需要理解内部文件或
存储布局。

## 从这里开始

不准备任何数据，直接运行内建引导：

```bash
pchronicle onboard
```

或者浏览并查询已有 Dataset：

```bash
pchronicle ls ./trajectory-data
pchronicle analysis overview ./trajectory-data
pchronicle query ./trajectory-data \
  --sql 'SELECT COUNT(*) AS runs FROM dataset.runs'
```

## 按任务选择命令

| 我想要…… | 从这里开始 |
| --- | --- |
| 浏览 Dataset | `pchronicle ls DATASET` 或 `pchronicle status DATASET` |
| 运行常用分析 | `pchronicle analysis overview DATASET` |
| 用 SQL 提出自定义问题 | `pchronicle query DATASET --sql SQL` |
| 给 Dataset 设置短名称 | `pchronicle alias add NAME DATASET` |
| 导入或导出轨迹 | `pchronicle import` 或 `pchronicle export` |
| 使用 Codex 或 Claude 分析 | `pchronicle agent codex DATASET` |
| 打开本地只读 UI 和 API | `pchronicle serve DATASET` |

pChronicle 读取并组织轨迹数据，不执行或调度 Agent。要在受控工作区中运行 Agent，请从
[pVisor](../pvisor/index.md)开始。

## 继续阅读

- [探索第一个 Dataset](get-started.md)
- [完成常见工作流](guides/index.md)
- [查看完整命令行手册](reference/cli.md)
- [理解数据模型](concepts/index.md)
- [了解存储与目录设计](design/index.md)
