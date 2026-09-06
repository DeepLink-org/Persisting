# 在本地服务 Dataset

`pchronicle serve` 把一个或多个 Dataset 挂载到内建的只读 Web UI 和 API。它用于本地检查，
不是公开或多租户数据服务。

## 命令原型

```text
pchronicle serve
  [--listen LOOPBACK_ADDR] [--control LOOPBACK_ADDR] [--open]
  [--gateway ADDRESS --gateway-dataset DATASET [--gateway-split TEMPLATE]
   [--gateway-split-idle DURATION]]
  [--gateway-config FILE --gateway-dataset DATASET [--gateway-state DIRECTORY]]
  [--gateway-stream-markdown] [--gateway-debug]
  [--catalog-config FILE]
  [<[NAME=]DATASET> ...]
pchronicle serve catalog issue  --catalog-config FILE NAME
pchronicle serve catalog grant  --catalog-config FILE NAME DATASET...
pchronicle serve catalog revoke --catalog-config FILE NAME DATASET...
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
挂载多个裸路径时，pChronicle 会从各路径末段生成名称；这些名称可能随路径变化，因此可复用的
命令仍应显式指定 mount name。

## 启动 Directory

```bash
pchronicle serve catalog issue --catalog-config catalog.toml alice
pchronicle serve catalog grant --catalog-config catalog.toml alice prod evals
pchronicle serve --catalog-config catalog.toml --listen 127.0.0.1:8081
```

`catalog.toml` 列出 libraries（每条都是 path）和 users。`serve catalog issue` 写入一个无授权
用户，并把 sk 只打印到这次 stdout；`grant` / `revoke` 改 `datasets`，不启动 HTTP。改文件后
必须重启 serve。父进程不打开这些 path。Web UI 通过请求头发送用户 ak/sk；查询在一次性
worker 中执行，worker 只拿到该用户被授权的 path。另一终端：

```bash
pchronicle alias add team catalog://127.0.0.1:8081 --ak USER_AK --sk USER_SK
pchronicle query @team/prod --sql 'SELECT 1'
```

`@team` 是 Directory locator，不是 Dataset。`@team/prod` 换票后打开票里的 `uri`（一条 path）。
同一 Directory 文件里所有 `s3://` 库必须共用同一组 endpoint、region 和后端密钥。
listener 仍只允许 loopback。设计见
[RFC-0013](../../rfcs/0013-pchronicle-warehouse-catalog.md)。

## 启用 Control 或 Gateway 集成

```bash
pchronicle serve \
  --control 127.0.0.1:0 \
  default=./trajectory-data

pchronicle serve \
  --gateway auto \
  --gateway-dataset ./trajectory-data \
  --gateway-split '{user}/{date}/{hour}'
```

`--gateway` 启动无需配置文件的 canonical event HTTP 入库端点。它接受
`POST /v1/events`，从 `x-persisting-user-id` 取得 `{user}`，并自动挂载输出 Dataset。
`{date}` 和 `{hour}` 使用 UTC；同一个 run/session 会固定到首次选择的分区，避免流式响应或
长会话被拆成多个 event source。`auto` 等价于 `127.0.0.1:0`。
已有 canonical source 默认在最后一条事件后等待 30 分钟才执行 Storyline projection；可用
`--gateway-split-idle DURATION` 覆盖。

启用 Warehouse listener 后，Gateway 模式的单 trace 查询会重新打开最新 canonical event
manifest。向已有 source 追加事件不需要等待 Snapshot 刷新或 Storyline projection；只有新建
source 文件和 projection 发布才需要更新全局 Snapshot。

Control 要求存在名为 `default` 的挂载。只提供 `--control`、`--gateway` 或
`--gateway-config`、不提供
`--listen` 时，只启动所请求的集成，不同时启动 Web UI。进程向 stdout 写一条机器可读的
readiness 记录；Control 凭据不会写入 stderr。

挂载的 Dataset 和 HTTP 操作均为只读。API 不暴露 import、export、maintenance 或任意文件
访问。刷新只会在新视图准备完成后替换当前可读视图；刷新失败时，旧视图继续可用。

## 日志与失败请求

`pchronicle serve` 把 Warehouse 请求日志写到 stderr，级别由 `--log-level` 控制（默认
`info`），tracing target 为 `pchronicle.serve`。启动时记录 listen 地址、Dataset 名和
Snapshot id。每个 `/api` 请求记录 method、path、status、耗时，以及截断后的 query
string。query 和 compile handler 还会记录截断后的 SQL。

失败响应包含 `code`、`message` 和 `request_id`。Web 横幅展示同一个 `request_id`。
内部失败在 JSON 中脱敏；stderr 上对应 id 的 ERROR 行带有 `root_cause` 和 `chain`。

`--log-level error` 只保留内部失败。`--log-level` 不读取 `RUST_LOG`。

Gateway 行为见 [Gateway 转发、改写与捕获](serve-gateway.md)，精确参数见
[`pchronicle` 命令行参考](../reference/cli.md)。内部刷新和版本固定机制属于
[Snapshot 设计](../design/catalog.md)。

要了解 Datasets、Runs、Analysis、Storage 和 Assistant 的实际操作，请继续阅读
[本地 Web UI 使用指南](ui.md)。
