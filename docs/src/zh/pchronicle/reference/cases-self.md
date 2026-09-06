# pChronicle 单机与自助使用场景

本文覆盖不依赖 Catalog Server 的基础工作流。每个案例都可以在一台开发机上独立执行，Dataset 可以是本地目录或对象存储 URI。

## 准备

```bash
mkdir -p /tmp/pchronicle-cases
cd /tmp/pchronicle-cases
pchronicle onboard
```

## S01：浏览本地 Dataset

```bash
pchronicle ls ./trajectory-data
pchronicle status ./trajectory-data
```

预期：命令列出 Dataset 中的 runs、steps 和 tool calls；空 Dataset 返回明确的空结果。

## S02：执行 SQL 查询

```bash
pchronicle query ./trajectory-data \
  --sql 'SELECT COUNT(*) AS runs FROM dataset.runs'
```

预期：查询成功并返回确定的 runs 数量。

## S03：运行内建分析

```bash
pchronicle analysis overview ./trajectory-data
```

预期：输出运行数、步骤数、工具调用数和时间范围。

## S04：导入和导出

```bash
pchronicle import input.jsonl --output ./trajectory-data
pchronicle export ./trajectory-data --output output.jsonl
```

预期：导出内容可以再次导入，记录的 ID 和事件顺序保持一致。

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
pchronicle ls s3://bucket/trajectory
```

预期：pChronicle 通过 S3 兼容接口发现并查询 Dataset。endpoint 和凭据不会写入 Dataset URI。
