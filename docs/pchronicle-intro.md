# pChronicle：Agent 轨迹的飞行记录仪

pChronicle 是 Agent 轨迹的飞行记录仪。无论是 Codex、Claude Code、PI Agent，还是自研的 Agent 框架，pChronicle 都能将每一次执行的轨迹完整保存下来，供事后分析与查询。pChronicle 提供出色的存储效率与查询性能，让海量轨迹数据随取随用。同时，pChronicle 提供可视化界面，帮助研究员分析轨迹、管理轨迹资产。

## 要解决的问题

Agent 正在承担越来越多的真实任务，但它的运行过程对研究者来说仍然是一个黑盒。一次执行结束后，留下来的往往只有终端里滚过的输出，或者散落在各处、格式不一的日志文件。这带来三个直接的麻烦：

- **失败难以复盘**。Agent 的任务失败了，想弄清它在哪一步、基于什么上下文做出了错误决定，往往只能重新跑一遍碰运气，或者在成百上千行日志里人工翻找。
- **优化难以对比**。改了 prompt、换了模型、调了工具，效果到底有没有变好？没有统一的轨迹数据，两次运行之间缺少可比较的基准，结论只能凭感觉。
- **数据难以沉淀**。每一次运行都包含模型调用、工具使用、决策过程等有价值的信息，但随着进程结束，这些数据就蒸发了，无法积累为可复用的评测和分析数据集。

<!-- 【占位：规模体感】可在此处补一个来自真实 example 的数字，例如"一次典型的 Agent 任务会产生数百条事件、数十次模型调用与工具调用"。从自己导入的轨迹里统计（pchronicle query 数一下 steps/tool_calls 行数即可），比泛指更有说服力。 -->

这些问题的根源是同一个：轨迹没有被当作正式的数据对待。它没有被完整地记录，没有统一的格式，也没有查询和分析的手段。pChronicle 就是为补齐这一层而做的。

## 命令行用法

pChronicle 随 Persisting 的 Python 包一起分发，安装后即可使用：

```bash
pip install persisting
pchronicle --version
```

### 快速体验

不需要准备任何数据，onboarding 流程会创建临时示例数据集，直接体验完整流程：

```bash
pchronicle onboard
pchronicle onboard query
```

### 直接读取 Codex 与 Claude Code 的历史

对于 Codex 和 Claude Code，连导入都可以省略——pChronicle 能直接读取两者在本机的历史存储目录，用 `@codex` 和 `@claude` 两个别名引用：

```bash
pchronicle ls @codex        # Codex 历史（~/.codex/sessions）
pchronicle ls @claude       # Claude Code 历史（~/.claude/projects）

pchronicle query @codex "SELECT COUNT(*) AS runs FROM dataset.runs"
pchronicle query @claude "SELECT * FROM dataset.runs LIMIT 10"
```

别名指向的目录可用环境变量覆盖：`CODEX_HOME` 对应 `@codex`，`CLAUDE_CONFIG_DIR` 对应 `@claude`（`@claude-code` 是等价写法）。别名后还可以拼接子路径，例如 `@codex/2026/05/29` 只查某一天的会话。

历史目录是只读引用，不复制、不搬运：每次查询针对目录当下内容建立快照，Codex 和 Claude Code 里新产生的会话，下一条查询就能看到。

### 导入轨迹

外部 Agent 的轨迹通过标准格式导入，目前支持 ATIF、ACTF 和 OpenAI Messages 三种格式。pChronicle 在导入时保留来源信息，并将不同格式的轨迹规范化为统一的模型：

```bash
pchronicle import --from input.json --output ./imported --format atif
pchronicle import --from ./corpus --output ./normalized --output-format storyline
```

导入是数据模型级无损的：源格式中的字段在转换后完整保留，写回同一格式时可以还原。昨天的录制格式，不会绑架明天的分析工具。

<!-- 【占位：无损佐证】可在此处补一句佐证，例如"三种格式的往返一致性由 【X】 组自动化测试保障"。数字来源：统计仓库中 ATIF/ACTF/OpenAI Messages 往返（roundtrip）测试用例数。 -->

### 查询与分析

导入后的数据集可以直接用 SQL 查询。pChronicle 提供规范化的 `runs`、`steps`、`tool_calls` 视图，一次运行的每一次模型调用、每一个工具动作，都是可以直接提问的数据：

```bash
pchronicle ls ./dataset
pchronicle status ./dataset
pchronicle query ./dataset "SELECT * FROM dataset.runs"
```

导出同样支持多种格式，方便与其他工具链交换：

```bash
pchronicle export --from ./imported --output output.json --format storyline
```

### 存储与查询性能

以下数字来自仓库自带的性能示例与一次导入实验，语料为 8 份 ATIF fixture 扩展而成的 512 个文档（共 7552 个 steps）：

| 指标 | 数值 | 说明 |
|---|---|---|
| 存储压缩率 | 约 7.5–13.6 倍 | 原始 ATIF JSON → Storyline Lance；7.26 MiB 语料导入后约 546 KiB（CLI 导入口径）；benchmark 的 NDJSON 口径为 7.5 倍 |
| 条件过滤查询 | 约 1 ms/次（约 1050 QPS） | session + step 条件过滤；为直接扫描 JSON 的 11.5 倍速 |
| 聚合分析（GROUP BY） | 约 0.9 ms/次（约 1110 QPS） | 统计类查询；为直接扫描 JSON 的 12.5 倍速 |
| 单条轨迹点查 | 约 3.8 ms | 按 id 取回一条完整轨迹（含全部 steps 与 tool calls）；冷查询约 2.7 ms |
| 导入吞吐 | 约 2800 文档/秒（约 40 MiB/s） | 512 个文档的语料导入 Storyline 数据集耗时约 0.18 秒 |

*测试环境：Apple M4，macOS 26.5；版本：pchronicle 0.2.0；测量日期：2026-08。数字为本机单次测量，不同机器与缓存状态下会有波动。*

#### 复现方法

查询与点查延迟来自仓库自带的性能示例（它会先校验各条查询结果等价，再输出对比报告）：

```bash
# 首次运行需先构建 benchmark 可执行文件
cargo build --release --example pchronicle_storage_query_benchmark -p persisting-pchronicle

cd examples/pchronicle/04-storage-query-performance
./run.sh   # 可用 PCHRONICLE_EXAMPLE_BENCH_SCALE / PCHRONICLE_EXAMPLE_BENCH_ITERS 调整规模
```

存储压缩率与导入吞吐可用任意轨迹语料复现（以下用仓库 fixture 构建同规模语料）：

```bash
# 构建 512 文档语料（8 份 fixture × 64）
mkdir -p /tmp/pch-bench/corpus
for i in $(seq 0 63); do
  for f in crates/persisting-pchronicle/tests/fixtures/atif/*.json; do
    cp "$f" "/tmp/pch-bench/corpus/$(basename "$f" .json)_$i.json"
  done
done

# 计时导入为 Storyline 数据集
time pchronicle import --from /tmp/pch-bench/corpus \
  --output /tmp/pch-bench/storyline --output-format storyline

# 对比逻辑字节数
find /tmp/pch-bench/corpus -name '*.json' -exec stat -f %z {} + | awk '{s+=$1} END {print s}'
find /tmp/pch-bench/storyline -type f -exec stat -f %z {} + | awk '{s+=$1} END {print s}'
```

*测试环境：【占位：机型 / OS】；数据集规模：【占位：N 个 runs / M 个 steps】；版本：【占位：x.y.z】；测量日期：【占位：YYYY-MM】。*

## 平台化用法（Web）

命令行适合脚本化和批量的工作，而当任务是"看一看这次运行到底发生了什么"时，可视化界面更直接。一条命令即可启动本地的只读服务和 Web 界面：

```bash
pchronicle serve --storage ./trajectory-data --control 127.0.0.1:0
```

`--storage` 同样支持别名和命名挂载，可以把多个来源一起挂进同一个界面：

```bash
pchronicle serve --storage codex=@codex --storage claude=@claude --control 127.0.0.1:0
```

服务只监听本机回环地址，且为只读，打开浏览器即可使用。在 Web 界面中可以：

- **浏览轨迹资产**：以目录方式查看所有数据集，了解每个数据集的来源、规模和版本；
- **走查单次运行**：沿着时间线查看一次运行的完整过程，每个步骤的输入、输出和工具调用清晰呈现；
- **分析与筛选**：在运行集合上做聚合分析和条件筛选，从单个问题定位到群体规律。

<!-- 【占位：界面截图】建议在此插入 1-2 张真实截图（数据集目录页 + 单次运行的时间线走查页），用你自己的 example 数据集实拍。图片放入同级 assets 目录后在此引用，例如：
![pChronicle Web：数据集目录](assets/intro/catalog.png)
![pChronicle Web：运行时间线走查](assets/intro/run-timeline.png)
-->

命令行与 Web 界面背后是同一个数据集：用 `pchronicle import` 收进来的轨迹，打开浏览器就能看到；在界面上发现的问题，也可以用 SQL 进一步追查。两种方式互为补充，覆盖从批量处理到交互式分析的完整链路。

## 小结

pChronicle 做的事情可以概括为一句话：让 Agent 的每一次执行都留下完整、可查询、可追溯的记录。当轨迹成为正式的数据资产，复盘一次失败、对比两次优化、沉淀一批评测数据，都从"重新跑一遍看看"变成"直接查一下"。

<!-- 【占位：收尾案例】如果手头有一个真实的复盘案例（某次失败通过轨迹查询定位到根因），可在小结前加一个简短的"一个真实的例子"小节，三五行讲清"问题 → 查询 → 定位"，比任何形容词都有说服力。 -->
