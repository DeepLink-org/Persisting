# 启动本地只读 Warehouse

`serve` 通过内置 Web 界面和只读 API 检查静态配置的 Dataset。它是本地 review surface，
不是多租户数据服务。

## 配置挂载

```toml
[[datasets]]
name = "evals"
uri = "../data/atif"

[[datasets]]
name = "archive"
uri = "s3://example/archive"
```

相对本地路径以配置文件目录为基准解析。Mount name 成为 SQL schema；Dataset identity 仍是
规范化 URI。

## 启动服务

```bash
pchronicle serve --config warehouse.toml \
  --listen 127.0.0.1:8081 --open
```

服务没有 authentication 或 authorization，因此只接受 loopback listener。不要把它放在
公开 listener 后冒充生产控制面。

挂载的 Dataset 和 API 操作均只读。HTTP 不暴露 import、export、maintenance 或任意文件
访问。刷新会在 reader lock 外完整构造新 Catalog Snapshot，再原子切换 reader；失败时保留
旧的可查询快照。

如需在同一进程中转发、改写并捕获新的 LLM 流量，继续阅读
[Gateway 转发、改写与捕获](serve-gateway.md)。精确参数见
[`pchronicle` 命令参考](../reference/cli.md)；Snapshot 行为见
[Dataset Catalog 设计](../design/catalog.md)。

`serve` 只启动命令行明确指定的服务。不传 `--listen` 就不会启动 Warehouse HTTP。也可以
用一个 storage URI 启动 pPilot 和 pVisor 使用的本地认证 Control 协议：

```bash
pchronicle serve --storage ./trajectory-data --control 127.0.0.1:0
pchronicle serve --storage ./tmp --storage ./data/evals --listen 127.0.0.1:9980
pchronicle serve --storage default=./tmp --storage evals=./data --control 127.0.0.1:0
```

`--config` 与 `--storage` 互斥，`--control` 要求使用 `--storage`。只传一次
`--storage URI` 时，Dataset 名为 `default`。重复 `--storage` 会挂载多个 Dataset；
默认名是 URI 的最后一段路径，也可用 `NAME=URI` 覆盖。`--control` 只绑定名为
`default` 的挂载（单次裸 URI 会隐式使用该名；多次时需显式 `default=URI`）。
进程只向 stdout 写一条机器可读的 readiness 记录，Control token 不会写入 stderr。

使用 `--storage` 时，`serve` 会先发现经过验证且非空的 canonical `events.lance` Store，
将每个投影收敛到确定的同级 `storyline`，全部 startup target 变为 fresh 后才输出 readiness。
随后以有界并发和重试继续发现、维护投影。projection failure 不进入 durable canonical write
路径，没有匹配 lineage 的外来目标绝不覆盖。若同时传入 `--listen`，每次成功发布投影都会
自动完整重建并原子切换 Warehouse Catalog。可用只读命令观察状态：

```bash
pchronicle status ./trajectory-data --format json
```
