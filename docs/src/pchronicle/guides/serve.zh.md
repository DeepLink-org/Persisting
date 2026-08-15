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
访问。刷新会先构造新 Catalog Snapshot，再切换 reader。

如需在同一进程中转发、改写并捕获新的 LLM 流量，继续阅读
[Gateway 转发、改写与捕获](serve-gateway.md)。精确参数见
[`pchronicle` 命令参考](../reference/cli.md)；Snapshot 行为见
[Dataset Catalog 设计](../design/catalog.md)。
