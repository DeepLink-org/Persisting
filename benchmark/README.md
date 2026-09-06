# Persisting benchmarks

**仓库级黑盒与微基准入口：Gateway、pChronicle、pVisor，以及独立的 Queue 吞吐压测。**

拥有可复现的压测脚本与报告契约。不拥有被测组件的产品行为；Queue 子系统的语义以
Queue 文档为准，这里只保留既有压测入口。

## Gateway

Gateway 黑盒压测位于 [`gateway/`](gateway/README.md)，使用确定性的本地 Echo upstream，
同时测量转发、Typed LLM capture、WAL 和 Lance durable append：

```bash
just benchmark-gateway

# 回放 examples/data，生成可人工检查的 request/response/capture bundle
just benchmark-gateway-replay
```

结果包含吞吐、p50/p95/p99 延迟、Echo 直连基线，以及基于 canonical manifest 的
持久化事件数量校验。

## pChronicle

统一 Criterion + hyperfine 套件，输出 raw JSON、Markdown、HTML 和 Bencher 投影：

```bash
just benchmark-pchronicle
just benchmark-pchronicle nightly target/pchronicle-benchmark/nightly
just benchmark-pchronicle-compare \
  target/pchronicle-benchmark/main/raw-report.json \
  target/pchronicle-benchmark/current/raw-report.json
```

详见 [`pchronicle/`](pchronicle/README.md)。

## pVisor

进程启动与 durable Run Bundle 访问基准：

```bash
just benchmark-pvisor
just benchmark-pvisor nightly target/pvisor-benchmark/nightly
just benchmark-pvisor-compare \
  target/pvisor-benchmark/candidate/raw-report.json \
  target/pvisor-benchmark/main/raw-report.json
```

详见 [`pvisor/`](pvisor/README.md)。

## Queue

本目录用于对 Persisting Queue 做吞吐压测，可配置生产者/消费者数量，测试极限吞吐。

### 环境

```bash
pip install persisting
```

从**仓库根目录**运行（以便正确解析 `persisting` 包）：

```bash
python benchmark/throughput_stress.py [选项]
```

### 参数说明

| 选项 | 简写 | 默认值 | 说明 |
|------|------|--------|------|
| `--producers` | `-p` | 2 | 生产者协程数 |
| `--consumers` | `-c` | 2 | 消费者协程数 |
| `--duration` | `-d` | 10.0 | 压测时长（秒） |
| `--batch-size` | `-b` | 20 | 每批 put/get 条数 |
| `--record-size` | `-r` | 0 | 每条记录 payload 字节数（0=最小） |
| `--storage-path` | | (临时目录) | 持久化目录 |
| `--warmup` | | 1.0 | 预热秒数 |

### 示例

```bash
# 默认：2 生产者、2 消费者、10 秒
python benchmark/throughput_stress.py

# 4 生产者、4 消费者、30 秒、批大小 50
python benchmark/throughput_stress.py -p 4 -c 4 -d 30 -b 50

# 大 payload（256 字节/条）测带宽
python benchmark/throughput_stress.py -p 2 -c 2 -d 15 -r 256

# 极限压测：多协程、长时长
python benchmark/throughput_stress.py -p 8 -c 8 -d 60 -b 100 --warmup 2
```

### 输出说明

- **总生产 / 总消费**：压测窗口内生产/消费的总条数。
- **生产吞吐 / 消费吞吐**：按配置的 `duration` 折算的条/s。
- **综合吞吐(消费)**：实际耗时内的消费条/s（含 drain 阶段）。

当前实现为单进程内多 asyncio 协程，共享同一 Queue（本地 Lance 模式），用于评估队列与 Lance 后端的极限吞吐。若已启动 Pulsing，Queue 会走分布式模式，吞吐与延迟特性会不同。

## Links

- [Gateway architecture](../docs/src/pvisor/design/gateway.md)
- [pChronicle design](../docs/src/pchronicle/design/index.md)
- [pVisor design](../docs/src/pvisor/design/index.md)
