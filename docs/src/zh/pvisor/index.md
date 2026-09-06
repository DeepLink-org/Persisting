# pVisor

<img src="/img/logos/pvisor-icon.png" alt="pVisor logo" width="96" />

**pVisor 在受控执行环境中运行现有的 Agent 命令。** 它为每个 Run 提供独立的工作区边界，
记录实际生效的控制机制，并让你在文件变更进入项目之前先进行审查。

在 Persisting“从模型状态到 Agent 历史”的主线中，pVisor 负责一个 Run 的执行边界与可审查记录。

:::tip 你将完成什么
完成第一次快速开始后，你会得到一个由 Run 拥有的 stage、一份解释实际控制机制的 Run Bundle，
以及一次明确的选择：应用或丢弃产生的 filesystem Effect。
:::

pVisor 不替代 Agent 自己的推理循环。你可以继续使用已有的 Agent CLI、脚本和 framework。

## 运行、审查、决定

在项目目录中运行：

```bash
pvisor run --stage ./runs/task-001 -- codex
pvisor review last
pvisor apply last --path src
```

使用 `--stage ./runs/task-001` 时，Agent 写入项目的 staged 视图。Run 结束后，你可以接受全部变更、分多次接受
选定路径，或丢弃整个 stage：

```bash
pvisor apply last --all
# 或者
pvisor drop last
```

具体的文件系统和网络边界取决于平台与 executor。pVisor 会记录实际生效的控制机制，不会把一个
Run 描述得比它实际拥有的隔离程度更高。

## 选择下一步

第一次使用时，从[运行第一个 Agent](get-started.md)开始。这条路径会以一份已审查的 stage
结束，并介绍其余文档使用的核心术语。

如果已经知道目标，可以直接进入对应路径：

- **保留或丢弃修改：** [审查和应用](guides/review-apply.md)
- **比较 host、container 与 VM：** [执行布局](guides/execution.md)
- **限制网络访问：** [网络策略](guides/network.md)
- **发布轨迹 event：** [采集轨迹](guides/capture.md)
- **查找精确选项：** [命令行参考](reference/cli.md)

pVisor 的本地 run—review—apply 闭环可以独立工作。pChronicle 是可选项：当你希望在 Run
结束后保留并查询轨迹 Dataset 时再使用它。

## 推荐阅读顺序

第一次使用时按下面的顺序阅读：

1. [运行第一个 Agent](get-started.md)，完成一次完整成功闭环。
2. 需要更细地控制修改时，阅读[审查和应用](guides/review-apply.md)。
3. 执行 Provider 会影响边界时，阅读[执行布局](guides/execution.md)。
4. 需要解释 Run Bundle 时，阅读[Capability 与 Evidence](concepts/capabilities-and-evidence.md)。
5. 只有需要精确选项或输出字段时，再查阅[命令行参考](reference/cli.md)。

## 继续阅读

- [运行第一个 Agent](get-started.md)
- [理解 pVisor 概念](concepts/index.md)
- [完成常见工作流](guides/index.md)
- [了解运行时与隔离设计](design/index.md)
- [使用 pChronicle 探索轨迹历史](../pchronicle/index.md)
