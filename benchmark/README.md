# Persisting Queue 压测

本目录用于对 Persisting Queue 做吞吐压测，可配置生产者/消费者数量，测试极限吞吐。

## 环境

```bash
pip install persisting[lance]
```

从**仓库根目录**运行（以便正确解析 `persisting` 包）：

```bash
python benchmark/throughput_stress.py [选项]
```

## 参数说明

| 选项 | 简写 | 默认值 | 说明 |
|------|------|--------|------|
| `--producers` | `-p` | 2 | 生产者协程数 |
| `--consumers` | `-c` | 2 | 消费者协程数 |
| `--duration` | `-d` | 10.0 | 压测时长（秒） |
| `--batch-size` | `-b` | 20 | 每批 put/get 条数 |
| `--record-size` | `-r` | 0 | 每条记录 payload 字节数（0=最小） |
| `--storage-path` | | (临时目录) | 持久化目录 |
| `--warmup` | | 1.0 | 预热秒数 |

## 示例

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

## 输出说明

- **总生产 / 总消费**：压测窗口内生产/消费的总条数。
- **生产吞吐 / 消费吞吐**：按配置的 `duration` 折算的条/s。
- **综合吞吐(消费)**：实际耗时内的消费条/s（含 drain 阶段）。

当前实现为单进程内多 asyncio 协程，共享同一 Queue（本地 Lance 模式），用于评估队列与 Lance 后端的极限吞吐。若已启动 Pulsing，Queue 会走分布式模式，吞吐与延迟特性会不同。
