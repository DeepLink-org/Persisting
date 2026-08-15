# 捕获 Agent 轨迹

Gateway capture 是 pVisor 的 Run 驱动，由 Run 启停；系统不再提供独立 Gateway 命令或
守护进程。

```bash
cargo build --release -p persisting-pvisor --bin pvisor
cd examples/pvisor/04-gateway-llm-control
./run.sh
```

真实 Agent 可直接通过 `pvisor run` 配置：

```bash
export DEEPSEEK_API_KEY=sk-...
pvisor run \
  --agent deepseek \
  --gateway-mode capture \
  --gateway-route 'name="deepseek", upstream="https://api.deepseek.com/v1", api_key_env="DEEPSEEK_API_KEY"' \
  --gateway-route 'name="*", forward="deepseek"' \
  --gateway-stream-markdown \
  -- claude
```

pVisor 启动内嵌 Gateway、向子进程注入代理或 base URL、等待执行、排空捕获并停止
Gateway。`--gateway-stream-markdown` 生成实时人读投影，`--chronicle-mode lance` 写
canonical structured events。Dataset 目录、查询、分析、导入导出和只读 Web UI 由
[`pchronicle`](../../pchronicle/get-started.md) 提供。

客户端只有使用注入的代理或 base URL 才能被观察；直接 socket 是否受限取决于 executor，
实际隔离边界以 Run Bundle 为准。

下一步：[查询捕获的历史](../../pchronicle/get-started.md)，或者阅读 [Gateway 内部实现](../design/gateway.md)。
