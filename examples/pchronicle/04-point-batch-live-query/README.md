# 2.4 pChronicle 点查、批查与实时查询

问题：查询一个 step、一个完整轨迹、一批 key，以及仍在写入的 Run 时，分别应该走哪条
pChronicle 路径，端到端性能如何？

这个示例把两种数据语义明确分开：

- 已提交轨迹使用 Storyline `runs` / `steps` / `tool_calls` 三表；
- 仍在运行的轨迹使用 canonical `events.lance`，`query follow` 读取每个已提交
  micro-batch。它不会把尚未稳定的事件伪装成 normalized step。

运行默认快速档（64 组、512 条轨迹、约 7k–10k steps）：

```bash
./run.sh
```

脚本执行并校验以下查询：

```bash
# 单个 step
ppilot query point .work/storyline \
  --session-id example-0000-fixture-dialogue_10 --step-id 1

# 单个完整轨迹
ppilot query point .work/storyline \
  --session-id example-0000-fixture-dialogue_10

# 一批 session 的同一 step；--session-id 可重复或使用逗号分隔
ppilot query batch .work/storyline \
  --session-id session-a,session-b --step-id 1

# 从 offset=0 回放已有事件，然后持续输出新提交的 EventRecord JSONL
persisting query follow .work/live \
  --agent-id query-mode-benchmark --session-id live-run \
  --poll-interval-ms 10 --limit 64
```

完整轨迹可用一次 snapshot-consistent API 点查并重建三表内容：

```rust,no_run
# async fn example() -> anyhow::Result<()> {
use persisting_pchronicle::StorylineLanceStore;

let store = StorylineLanceStore::open(".work/storyline").await?;
let story = store.get_storyline("example-0000-fixture-dialogue_10").await?;
assert!(story.is_some());
# Ok(())
# }
```

性能报告同时给出单 step、完整轨迹、64 次独立冷 CLI 点查、一次批量查询，以及实时
producer-to-visible 的 p50/p95/max 和 events/s。批量加速包含进程启动、
DataSource 打开、SQL 计划与执行的摊销；实时延迟包含写入命令、Lance commit 和 follow
轮询等待，因此不同指标不能被解读为相同 payload 下的普适快慢。

可调整规模：

```bash
PCHRONICLE_QUERY_MODE_SCALE=128 \
PCHRONICLE_QUERY_MODE_ITERS=10 \
PCHRONICLE_QUERY_MODE_BATCH_IDS=128 \
PCHRONICLE_QUERY_MODE_FOLLOW_POLL_MS=25 \
./run.sh
```
