# 探索第一个 Dataset

用示例数据学会三个日常命令，再选择一个专题，或换成自己的 Dataset。基础引导只读数据；
导入导出专题只在隔离的临时工作区里执行写入。

!!! tip "pChronicle 的工作循环"

    **打开 → 汇总 → 提问 → 定位 → 继续。** 先完成内置体验，再把同样的习惯
    带到真实轨迹数据中。

## 1. 运行引导体验

尚未安装时先运行 `pip install persisting`。不需要源码、账户、后台服务，也不需要已有 Dataset：

```bash
pchronicle onboard
```

默认引导（也可以显式运行 `onboard basics`）依次回答三个问题：

| 问题 | 学会的命令 | 重点看什么 |
| --- | --- | --- |
| 这里有哪些数据？ | `list` | 当前路径下的文件和子 Dataset |
| 发生了多少事情？ | `stats overview` | 轨迹数、步骤数和错误来源 |
| 哪个 session 的步骤最多？ | `query --sql …` | 一次真实只读查询的小结果集 |

引导会替你执行命令并展示真实结果。交互式终端中按 **Enter** 继续，输入 **q** 退出。
不需要另开终端复制临时命令。当前 CLI 的教学说明使用中文，两个语言版本的文档对应相同命令。

!!! success "可以尝试自己的数据了"

    你已经使用过 `pchronicle <操作> <数据路径> [选项]`：列出数据、读取汇总、提出一个问题。
    示例目录会在退出时清理，不要在结束后复用输出中的临时路径。

## 按方面学习

所有专题都可以不带路径，使用内置数据。直接选择当前需要的方面，不必先看完整教程。

| 我想…… | 运行 | 实际做什么 |
| --- | --- | --- |
| 再走一次最短路径 | `pchronicle onboard basics` | 三个只读操作 |
| 理解常用词汇 | `pchronicle onboard concepts` | 解释 Dataset、Source、Snapshot 和表 |
| 确认数据能否使用 | `pchronicle onboard inspect` | 列表和健康检查 |
| 不写 SQL 先看汇总 | `pchronicle onboard analyze` | 总览和工具使用报告 |
| 自己编写查询 | `pchronicle onboard query` | Schema、步骤、工具调用和输出格式 |
| 使用不同轨迹格式 | `pchronicle onboard formats` | 联合查询内置 ATIF、ACTF、OpenAI 示例 |
| 定位具体记录 | `pchronicle onboard find` | 记录坐标与匹配语法 |
| 导入数据或取回数据 | `pchronicle onboard exchange` | 在隔离的临时工作区内执行导入导出 |
| 使用 Web/API 浏览 | `pchronicle onboard serve` | 展示配置示例，不启动服务 |

需要完整课程时运行 `pchronicle onboard all`。专题同样支持 Enter/q。
无人值守时加 `--no-pause`；管道和重定向也会禁用暂停，输出 Markdown：

```bash
pchronicle onboard --no-pause > introduction.md
pchronicle onboard query --no-pause
pchronicle onboard all --no-pause > full-walkthrough.md
```

## 2. 打开自己的 Dataset

先用已有数据重走基础引导，再进入你需要的专题：

```bash
pchronicle onboard ./trajectory-data
pchronicle onboard query ./trajectory-data
```

`basics`、`inspect`、`analyze`、`query` 和 `find` 接受可选 Dataset 路径，演练只读你的数据。
`formats` 和 `exchange` 继续使用内置示例。把下面的 `./trajectory-data` 替换为本地路径、对象存储 URI 前缀，或 `@prod` 这样的
Dataset pin：

```bash
pchronicle list ./trajectory-data
pchronicle stats overview ./trajectory-data
```

`list` 列出当前目录一级的文件和子 Dataset；`stats overview` 在编写 SQL 前提供稳定汇总。
需要把结果交给脚本时使用 JSON：

```bash
pchronicle list ./trajectory-data --format json
```

## 3. 提出一个有边界的问题

先查看 schema，再从一个小而可复现的问题开始：

```bash
pchronicle query ./trajectory-data --sql "DESCRIBE dataset.steps"
pchronicle query ./trajectory-data \
  --sql "SELECT session_id, COUNT(*) AS steps
         FROM dataset.steps
         GROUP BY session_id
         ORDER BY steps DESC"
```

查询只读，并受明确的资源上限约束。需要把结果交给其他工具时，使用
`--format jsonl|csv` 和 `--output`。

!!! success "检查点：答案可以复现"

    记录 Dataset 路径、查询和输出格式。在数据不变时重跑可以比较结果；仅记录路径不会冻结之后的数据变化。

## 4. 定位答案背后的证据

从汇总继续定位匹配的步骤或 session：

```bash
pchronicle find ./trajectory-data --match "timeout" --format json
pchronicle find ./trajectory-data --session-id session-42
```

如果外部 ID 重复，带上 `--source`，让引用保持稳定：

```bash
pchronicle find ./trajectory-data \
  --source nested/source.json --session-id session-42
```

面对陌生数据时使用 `pchronicle list ./trajectory-data --errors report`；自动化中如果
不允许部分结果，则使用 `--errors strict` 让任务失败。

## 5. 选择下一步

- [深入发现并查询 Dataset](guides/discover-and-query.md)
- [打开本地 Web UI](guides/ui.md)
- [导入或导出 Run](guides/exchange.md)
- [在本地提供 Dataset 服务](guides/serve.md)
- [使用 pVisor 捕获新 Run](../pvisor/guides/capture.md)
- [理解 Dataset 与 Source 模型](concepts/index.md)

Walkthrough 创建的 Dataset 是临时的。继续导入、服务化或生产自动化前，请换成自己的路径。
