# pChronicle 单机与自助使用场景

本文覆盖不依赖 Catalog Server 的基础工作流。每个案例都可以在一台开发机上独立执行，Dataset 可以是本地目录或对象存储 URI。

## 准备

`just cases pchronicle` 会把 `PCHRONICLE_CASE_FIXTURES` 指到仓库内的 `examples/data`。手工执行时先导出该变量：

```bash
export PCHRONICLE_CASE_FIXTURES=/path/to/Persisting/examples/data
mkdir -p /tmp/pchronicle-cases
cd /tmp/pchronicle-cases
```

## S01：浏览本地 Dataset

```bash
pchronicle import --from "$PCHRONICLE_CASE_FIXTURES/atif/support-ticket.json" --to ./trajectory-data --mode create
pchronicle list ./trajectory-data
pchronicle stats ./trajectory-data
```

预期：命令列出 Dataset 中的 runs、steps 和 tool calls。

## S02：执行 SQL 查询

```bash
pchronicle import --from "$PCHRONICLE_CASE_FIXTURES/atif/support-ticket.json" --to ./trajectory-data --mode create
pchronicle query ./trajectory-data \
  --sql 'SELECT COUNT(*) AS runs FROM dataset.runs'
```

预期：查询成功并返回确定的 runs 数量。

## S03：运行内建分析

```bash
pchronicle import --from "$PCHRONICLE_CASE_FIXTURES/atif/support-ticket.json" --to ./trajectory-data --mode create
pchronicle stats overview ./trajectory-data
```

预期：输出运行数、步骤数、工具调用数和时间范围。

## S04：导入和导出

```bash
pchronicle import --from "$PCHRONICLE_CASE_FIXTURES/atif/support-ticket.json" --to ./trajectory-data --mode create
pchronicle export --from ./trajectory-data --to ./output.atif.json --output-format atif
test -s ./output.atif.json
```

预期：导出文件非空，且可再次导入。

## S05：本地 Warehouse

```bash
pchronicle serve ./trajectory-data --listen 127.0.0.1:8081
```

预期：Web UI、`/api/query/tables`、`/api/catalog` 和 Explorer API 可用；未启用 Catalog 时不需要用户凭据。

## S06：对象存储 Dataset

```bash
export AWS_ENDPOINT_URL_S3=http://127.0.0.1:9000
export AWS_ACCESS_KEY_ID=rustfsadmin
export AWS_SECRET_ACCESS_KEY=rustfsadmin
export AWS_REGION=us-east-1
pchronicle list s3://bucket/trajectory
```

预期：pChronicle 通过 S3 兼容接口发现并查询 Dataset。endpoint 和凭据不会写入 Dataset URI。默认自动化运行会跳过本案例；设置 `PCHRONICLE_CASE_MODE=s3` 且提供可达 endpoint 后再执行。
