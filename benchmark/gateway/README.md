# Gateway benchmark

这个黑盒 benchmark 使用本地 `pchronicle echo` 作为确定性 upstream，测量完整 Gateway
请求路径：HTTP 转发、OpenAI Chat payload 处理、WAL、Typed LLM capture，以及
pChronicle Lance durable append。它不会访问外部模型服务。

默认先压测 Echo 直连作为本机 HTTP 基线，再以相同并发压测 Gateway。Gateway 进程会优雅
退出，脚本随后逐个读取 capture session 的 canonical manifest，确认 published event 总数
等于成功请求数的两倍（请求和响应各一条），因此性能结果同时具备回归测试的持久化数量
约束。之后再通过真实 SQL 查询核对 `llm.request` / `llm.response` 数量，确保后台小文件合并
把 live fragment 数控制在可查询范围；manifest 校验本身不会扫描 Lance 历史版本。

增量合并只约束当前快照引用的 live fragment。为保护并发 snapshot reader，历史 Lance
version/file 不会在写入路径立即删除；磁盘回收仍由显式 maintenance/vacuum 按保留期执行。
在线布局采用 leveled compaction：8 个微小 fragment 形成一个 L0 segment，连续 8 个同层
sealed segment 自动晋升到下一层。因此 visible segment 数随数据量近似对数增长，而不是
持续按事件数线性增长；JSON 结果中的 `max_segment_level` 可用于观察是否发生层级晋升。

## 运行

从仓库根目录运行。benchmark 只使用已有的 release binary，不会隐式执行
`cargo build`；首次运行或代码变更后请先显式构建：

```bash
cargo build --release --locked -p persisting-pchronicle-cli --bin pchronicle
just benchmark-gateway

# 最多 30 秒、32 并发、1 KiB payload、2048 个请求
just benchmark-gateway 30 32 1024 0 16 2048

# 按并发度扫描吞吐峰值和饱和点（默认 1/2/4/8/16/32）
just benchmark-gateway-sweep
```

也可以使用已有 release binary 直接运行：

```bash
bash benchmark/gateway/run.sh \
  --duration 10 \
  --warmup 1 \
  --concurrency 16 \
  --sessions 16 \
  --requests 1024 \
  --payload-bytes 256
```

可通过 `PERSISTING_PCHRONICLE_BIN` 指定 binary。常用选项：

- `--skip-baseline`：只测 Gateway。
- `--sessions N`：固定 capture session 数，默认 16；它与并发度独立。
- `--requests N`：每个测量阶段的请求上限，默认 1024；`duration` 仍是时间上限。
- `--min-rps N`：设置机器相关的最低吞吐门槛；默认只检查请求和持久化正确性。
- `--output FILE`：指定 JSON 结果文件。
- `--keep-artifacts`：保留配置、日志、WAL 和 Dataset。

压测器是 closed-loop 模型：每个 worker 最多只有一个在途请求，所以
`concurrency` 就是最大在途请求数。JSON 中的 `estimated_mean_in_flight` 用吞吐和
平均延迟按 Little's law 估算实际在途数；它接近 `max_in_flight` 时，表示发压端已经
把该并发窗口持续占满。判断服务饱和点应使用 sweep：吞吐不再上升、延迟显著上升的第一个
并发档位，而不是只观察一个高并发数字。sweep 会在所有档位保持 `sessions`
不变，避免把「在途请求数」和「Lance Dataset 数」两个变量混在一起。
默认使用有限 request budget：请求阶段的 RPS/延迟表示 Gateway 转发能力，随后的
优雅退出会等待 capture drain，并仍严格校验每个请求的 request/response 两条事件都已
持久化。这避免用无限 closed-loop 负载必然打满 best-effort capture queue，却把排队时间误认为
HTTP 转发延迟。
JSON 中的 `shutdown_drain_seconds` 是停止发送后等待 apply、Lance publication、合并与
优雅退出的时间；`events_per_drain_second` 单独表示这次 burst 的 durable capture drain 能力。

默认结果写入 `benchmark/gateway/results/latest.json`，该目录不会提交到 Git。
sweep 结果分别写入 `benchmark/gateway/results/sweep-c<N>.json`。
