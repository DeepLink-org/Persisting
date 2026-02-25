# Persisting Examples

演示 Persisting Queue 的用法。所有示例仅使用 `persisting.Queue` API，不直接操作底层 backend。

## 环境

```bash
pip install persisting[lance]
```

从仓库根目录运行：

```bash
python -m examples.01_core_components
python -m examples.02_queue_options
python -m examples.03_sampler
python -m examples.04_producer_consumer
python -m examples.05_multiprocess_producer_consumer
```

## 示例一览

| 示例 | 内容 |
|------|------|
| **01_core_components** | 核心流程：`Queue("name")` → put → flush → get |
| **02_queue_options** | 队列选项：batch_size、enable_metrics、stats |
| **03_sampler** | Sampler（TQ 对齐）：SequentialSampler、get_batch、自定义 ReverseSampler |
| **04_producer_consumer** | 生产者-消费者：单写单读、streaming 消费 |
| **05_multiprocess_producer_consumer** | 多进程：1 个生产者进程 + 1 个消费者进程，同时写/读，持久化到 /tmp |

## 与 TQ Tutorial 的对应关系

| TQ Tutorial | Persisting Example |
|-------------|---------------------|
| 01 Core Components | 01 Core Components（Queue put/flush/get） |
| 02 KV Interface | —（Persisting 用 record 字段表达，无独立 KV 接口） |
| 03 Metadata Concepts | —（无 get_meta/get_data 两阶段，get 一次返回） |
| 04 Understanding Controller | 04 Producer-Consumer |
| 05 Custom Sampler | 03 Sampler（SequentialSampler + 自定义 ReverseSampler） |

## 分布式模式

如果 Pulsing runtime 已启动（`pulsing.is_initialized() == True`），Queue 自动切换为分布式模式——数据通过 Pulsing actor 分发到多 bucket、多节点。用户代码无需修改。
