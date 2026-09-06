# pChronicle 集群平台与 Catalog Server 场景

本文覆盖平台化部署。Catalog 配置只管理用户、Dataset 和授权；Warehouse 的服务参数仍由 `pchronicle serve` 提供。

## P01：从空配置创建 Catalog 用户

```bash
pchronicle serve catalog user create \
  --catalog-config ./catalog.toml alice
```

如果文件不存在，命令创建配置文件、生成用户 AK/SK，并只在本次输出 secret。

## P02：登记 Dataset

```bash
pchronicle serve catalog dataset create \
  --catalog-config ./catalog.toml \
  prod s3://bucket/prod \
  --endpoint http://127.0.0.1:9000 \
  --region us-west-2 \
  --ak BACKEND_AK \
  --sk BACKEND_SK
```

该命令只登记 Dataset，不创建或删除后端数据。

## P03：授权用户

```bash
pchronicle serve catalog grant \
  --catalog-config ./catalog.toml \
  alice prod \
  --permission read \
  --permission query \
  --permission analyze
```

预期：配置中出现独立的 `[[grants]]` 记录。

## P04：启动 Catalog Server

```bash
pchronicle serve \
  --catalog-config ./catalog.toml \
  --listen 127.0.0.1:8081
```

父进程负责用户认证、Dataset 列表和 ticket；查询数据面在授权 mounts 的 worker 中执行。

## P05：访问授权 Dataset

```bash
pchronicle alias add team catalog://127.0.0.1:8081 \
  --ak USER_AK --sk USER_SK
pchronicle query @team/prod \
  --sql 'SELECT COUNT(*) AS runs FROM dataset.runs'
```

预期：授权用户可以查询 `prod`；未授权用户或未知 Dataset 返回相同的 404 资源错误。

## P06：撤销授权

```bash
pchronicle serve catalog revoke \
  --catalog-config ./catalog.toml \
  alice prod --permission query
```

预期：后续查询被拒绝，但 `read` 和其它仍保留的权限不受影响。

## P07：RustFS Warehouse 回归

准备 RustFS，并设置：

```bash
export PCHRONICLE_RUSTFS_ENDPOINT=http://127.0.0.1:9000
export PCHRONICLE_RUSTFS_ACCESS_KEY=rustfsadmin
export PCHRONICLE_RUSTFS_SECRET_KEY=rustfsadmin
export PCHRONICLE_RUSTFS_BUCKET=pchronicle-cases
```

然后运行 RustFS 回归测试，验证 Dataset 写入、Catalog discovery、SQL 查询、Explorer 和 refresh 行为。

平台验收重点：

- Catalog 文件可从空文件开始构建；
- 用户、Dataset 和 grants 修改是确定性的；
- Dataset 后端凭据只在授权 ticket 中使用；
- Worker 只收到当前用户被授权的 mounts；
- Catalog refresh 不影响已完成查询的 snapshot；
- RustFS 上的 Warehouse 行为与本地 Dataset 一致。
