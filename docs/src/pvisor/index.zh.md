# pVisor

**pVisor 在受控执行环境中运行现有的 Agent 命令。** 它为每个 Run 提供独立的工作区边界，
记录实际生效的控制机制，并让你在文件变更进入项目之前先进行审查。

在 Persisting“从模型状态到 Agent 历史”的主线中，pVisor 负责一个 Run 的执行边界与可审查记录。

pVisor 不替代 Agent 自己的推理循环。你可以继续使用已有的 Agent CLI、脚本和 framework。

## 运行、审查、决定

在项目目录中运行：

```bash
pvisor run --safe codex
pvisor review last
pvisor apply last --path src
```

使用 `--safe` 时，Agent 写入项目的 staged 视图。Run 结束后，你可以接受全部变更、分多次接受
选定路径，或丢弃整个 stage：

```bash
pvisor apply last --all
# 或者
pvisor drop last
```

具体的文件系统和网络边界取决于平台与 executor。pVisor 会记录实际生效的控制机制，不会把一个
Run 描述得比它实际拥有的隔离程度更高。

## 按任务继续

| 我想要…… | 从这里开始 |
| --- | --- |
| 完成一个 staged Agent Run | [运行第一个 Agent](get-started.md) |
| 审查并选择性接受变更 | [审查和应用](guides/review-apply.md) |
| 选择 host、container 或 VM | [执行布局](guides/execution.md) |
| 控制网络访问 | [网络策略](guides/network.md) |
| 采集轨迹数据 | [采集轨迹](guides/capture.md) |
| 查找精确命令语法 | [命令行参考](reference/cli.md) |

pVisor 的本地 run—review—apply 闭环可以独立工作。pChronicle 是可选项：当你希望在 Run
结束后保留并查询轨迹 Dataset 时再使用它。

## 继续阅读

- [运行第一个 Agent](get-started.md)
- [理解 pVisor 概念](concepts/index.md)
- [完成常见工作流](guides/index.md)
- [了解运行时与隔离设计](design/index.md)
- [使用 pChronicle 探索轨迹历史](../pchronicle/index.md)
