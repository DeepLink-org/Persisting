# pChronicle

**pChronicle 是 Agent 轨迹存储引擎。** 用于浏览、查询、交换和服务运行 Dataset；既可以读取
Persisting 产生的运行记录，也可以直接读取受支持的外部格式；不要求先运行 pVisor。

在 Persisting“从模型状态到 Agent 历史”的主线中，pChronicle 是 Agent 历史的持久存储与查询引擎。
它可以作为本地工具使用，也可以平台化部署在多条 path 前面。

:::tip 你将完成什么
第一次快速开始会创建临时数据，将它打开为 Dataset，运行一次只读摘要，再回答一个 SQL 问题。
你不需要先准备生产存储，就能熟悉查询模型。
:::

## 你只需要面对 Dataset

**Dataset 就是 path**：本地目录或文件，或对象存储 URI 前缀。pChronicle 发现并规范化该
path 中受支持的数据。alias（`@name`）是 locator；解析完成后引擎只看见 path。

一个 Dataset 可以写成：

- 本地目录或文件（`./local/path`）；
- 对象存储中的 URI 前缀（`s3://bucket/prefix`）；
- 解析到上述位置的用户 alias（`@alias-name`）。

pChronicle 会发现并规范化该 path 中受支持的数据。开始使用命令行前，你不需要理解内部文件或
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

## 选择下一步

第一次学习命令行时，从[探索第一个 Dataset](get-started.md)开始。这条路径会创建临时数据，
让你先完成一次只读查询，再接入真实 Source。

如果已经知道问题，可以直接进入对应路径：

- **浏览 Dataset：** `pchronicle ls DATASET` 或 `pchronicle status DATASET`
- **运行常用分析：** `pchronicle analysis overview DATASET`
- **用 SQL 提问：** `pchronicle query DATASET --sql SQL`
- **给 Dataset 命名：** `pchronicle alias add NAME DATASET`
- **导入或导出记录：** [交换数据](guides/exchange.md)
- **使用 Agent 分析：** `pchronicle agent codex DATASET`
- **打开本地 UI 与 API：** [提供 Dataset 服务](guides/ui.md)

pChronicle 读取并组织运行历史，不执行或调度 Agent。要在受控工作区中运行 Agent，请从
[pVisor](../pvisor/index.md)开始。

## 推荐阅读顺序

1. [探索第一个 Dataset](get-started.md)，先完成一次只读查询。
2. 准备检查真实数据时，阅读[发现并查询](guides/discover-and-query.md)。
3. 涉及受支持的外部格式时，阅读[交换数据](guides/exchange.md)。
4. 其他工具需要本地访问时，阅读[提供 Dataset 服务](guides/serve.md)。
5. 需要精确选项、预算或输出格式时，再查阅[命令行参考](reference/cli.md)。

## 继续阅读

- [探索第一个 Dataset](get-started.md)
- [完成常见工作流](guides/index.md)
- [查看完整命令行手册](reference/cli.md)
- [使用统一产品术语](reference/terminology.md)
- [理解数据模型](concepts/index.md)
- [了解存储与 Snapshot 设计](design/index.md)
