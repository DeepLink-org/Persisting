# Persisting Examples

## Python Queue 示例

演示 `persisting.Queue` API。从仓库根目录运行：

```bash
pip install persisting[lance]

python -m examples.01_core_components
python -m examples.02_queue_options
python -m examples.03_sampler
python -m examples.04_producer_consumer
python -m examples.05_multiprocess_producer_consumer
python -m examples.06_tiered_lazy_prefetch_eviction
```

| 示例 | 内容 |
|------|------|
| **01_core_components** | `Queue` → put → flush → get |
| **02_queue_options** | batch_size、metrics、stats |
| **03_sampler** | SequentialSampler、get_batch、自定义 ReverseSampler |
| **04_producer_consumer** | 单写单读、streaming 消费 |
| **05_multiprocess_producer_consumer** | 多进程写/读，持久化到 /tmp |
| **06_tiered_lazy_prefetch_eviction** | Tiered KV cache：prefetch、LRU/prefix-aware 驱逐 |

## Capture 与轨迹

| 目录 | 用途 |
|------|------|
| [**capture-walkthrough/**](capture-walkthrough/) | `./run.sh` — Mock LLM + capture run + 校验 |
| [**trajectory-tlv/**](trajectory-tlv/) | 静态 `trajectory.tlv.md`（裸对话正文），供 replay 与设计对照 |
| [**llm-proxy/**](llm-proxy/) | 真实 LLM 代理配置（DeepSeek、多厂商） |

`capture-walkthrough/store/` 为运行 demo 时本地生成，已在该目录 `.gitignore` 中忽略。
