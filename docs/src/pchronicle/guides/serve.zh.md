# 在本地服务 Dataset

`pchronicle serve` 把一个或多个 Dataset 挂载到内建的只读 Web UI 和 API。它用于本地检查，
不是公开或多租户数据服务。

## 命令原型

```text
pchronicle serve
  [--listen LOOPBACK_ADDR] [--control LOOPBACK_ADDR] [--open]
  [--gateway-config FILE] [--gateway-dataset NAME] [--gateway-state DIRECTORY]
  [--gateway-stream-markdown] [--gateway-debug]
  <[NAME=]DATASET> ...
```

所有 listener 都必须使用 loopback 地址，因为 Web UI 和只读 API 不提供公开网络所需的认证边界。

## 打开一个 Dataset

```bash
pchronicle serve --open ./trajectory-data
```

单个裸 Dataset 会挂载为 `default`。不提供 listener 参数时，本地 Web UI 使用一个可用的
loopback 端口。

## 挂载多个 Dataset

```bash
pchronicle serve \
  --listen 127.0.0.1:8081 \
  evals=../data/atif archive=s3://example/archive
```

Mount name 会成为 SQL schema 和 API 名称。需要稳定名称时使用 `NAME=DATASET`。

## 启用 Control 或 Gateway 集成

```bash
pchronicle serve \
  --control 127.0.0.1:0 \
  default=./trajectory-data

pchronicle serve \
  --listen 127.0.0.1:8080 \
  --gateway-config gateway.toml \
  --gateway-dataset evals \
  evals=./trajectory-data
```

Control 要求存在名为 `default` 的挂载。只提供 `--control` 或 `--gateway-config`、不提供
`--listen` 时，只启动所请求的集成，不同时启动 Web UI。进程向 stdout 写一条机器可读的
readiness 记录；Control 凭据不会写入 stderr。

挂载的 Dataset 和 HTTP 操作均为只读。API 不暴露 import、export、maintenance 或任意文件
访问。刷新只会在新视图准备完成后替换当前可读视图；刷新失败时，旧视图继续可用。

Gateway 行为见 [Gateway 转发、改写与捕获](serve-gateway.md)，精确参数见
[`pchronicle` 命令行参考](../reference/cli.md)。内部刷新和版本固定机制属于
[Dataset Catalog 设计](../design/catalog.md)。
