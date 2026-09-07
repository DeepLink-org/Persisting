# pChronicle Directory 与平台场景

面向平台部署的 Directory 配置。ACL 文件管理用户、datasets（libraries）和授权；
Warehouse 的 listen / Gateway 参数仍由 `pchronicle serve` 提供。

## P01：从空配置签发 Directory 用户

```bash
pchronicle serve catalog issue \
  --catalog-config ./catalog.toml alice
```

如果文件不存在，命令会创建配置文件、写入无授权用户，并只在本次 stdout 打印 secret。

## P02：登记 Dataset library

```bash
pchronicle serve catalog dataset add \
  --catalog-config ./catalog.toml \
  prod \
  --uri s3://bucket/prod \
  --endpoint http://127.0.0.1:9000 \
  --region us-west-2 \
  --access-key BACKEND_AK \
  --secret-key BACKEND_SK
```

该命令只登记 URI 与后端凭据，不创建或删除对象存储数据。同一文件中所有 `s3://`
library 必须共用同一组 endpoint、region 和后端密钥。

## P03：给用户授权 library

```bash
pchronicle serve catalog grant \
  --catalog-config ./catalog.toml \
  alice prod
```

预期：出现 `[[grants]]`，该用户可打开 `prod`。v1 授权是库成员关系，不是
`--permission` 细粒度标志。

## P04：用 catalog 挂载启动 serve

```bash
pchronicle serve \
  --catalog-config ./catalog.toml \
  --listen 127.0.0.1:8081
```

文件中每个 `[datasets.*]` 都会挂进 Warehouse；`catalog://` 换票路由仍可用。
改 ACL 后需重启 serve。

## P05：经 Directory pin 访问授权 Dataset

```bash
pchronicle dataset pin team catalog://127.0.0.1:8081 \
  --ak USER_AK --sk USER_SK
pchronicle query @team/prod \
  --sql 'SELECT COUNT(*) AS runs FROM dataset.runs'
```

预期：授权用户可查询 `prod`；未知 Dataset 失败关闭。

## P06：撤销 library 授权

```bash
pchronicle serve catalog revoke \
  --catalog-config ./catalog.toml \
  alice prod
```

预期：该用户后续无法再打开 `@team/prod`。

## P07：RustFS Warehouse 回归

准备 RustFS，并设置：

```bash
export PCHRONICLE_RUSTFS_ENDPOINT=http://127.0.0.1:9000
export PCHRONICLE_RUSTFS_ACCESS_KEY=rustfsadmin
export PCHRONICLE_RUSTFS_SECRET_KEY=rustfsadmin
export PCHRONICLE_RUSTFS_BUCKET=pchronicle-cases
```

然后跑 RustFS 回归，覆盖 Dataset 写入、Snapshot discovery（含
`chronicle.manifest`）、SQL、Explorer 与 refresh。

平台验收重点：

- ACL 可从空文件开始构建；
- 用户、dataset 与 grants 修改是确定性的；
- 后端对象存储密钥留在 catalog 文件 / ticket 路径，不出现在 `dataset list`；
- `--catalog-config` serve 会挂载全部已登记 library；
- Snapshot refresh 不改动进行中查询的 Snapshot；
- 覆盖路径上 RustFS Warehouse 行为与本地 Dataset 一致。
