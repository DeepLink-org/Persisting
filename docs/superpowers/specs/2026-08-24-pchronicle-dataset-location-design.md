# pChronicle DatasetLocation

日期：2026-08-24
状态：已同意
范围：第一刀 Dataset 根 I/O 门面，以及 CLI `import` / `export` / `serve` 改为只走该门面
非目标：CAS、events manifest、Storyline `CURRENT`、跨进程 flock、default Warehouse、Gateway `--gateway-state`、把对象存储伪装成 POSIX VFS

## 1. 决策

在 `persisting-pchronicle` 增加 `DatasetLocation`：解析、归一化、exists、打开 Lance、读写单个对象。本地仍用 staging + `rename_noreplace` 做 create-only；对象存储仍用 prefix 空检查 + Lance / `object_store`。**scheme 分支只允许出现在这个类型内部。**

凭证继续只走 `AWS_*`（以及 Lance 已识别的同类变量）。URI 内禁止用户名、密码、query、fragment。

CLI 不再手写 `contains("://")` 来决定 import / export / serve 行为。

## 2. API

```rust
pub struct DatasetLocation { /* 归一化 URI */ }

pub enum DatasetLocationKind {
    Local,
    ObjectStore,
}

impl DatasetLocation {
    pub fn parse(input: &str) -> Result<Self>;
    pub fn into_existing(self) -> Result<Self>;
    pub fn into_create_target(self) -> Result<Self>;
    pub fn as_str(&self) -> &str;
    pub fn kind(&self) -> DatasetLocationKind;
    pub fn local_path(&self) -> Option<&Path>;
    pub async fn exists(&self) -> Result<bool>;
    pub async fn put_bytes(&self, bytes: &[u8], overwrite: bool) -> Result<()>;
}
```

| 方法 | 语义 |
|---|---|
| `parse` | 别名由 CLI 先展开。校验 scheme / 凭证 / query / fragment，去掉尾部 `/`。本地路径**不**要求已存在（serve 挂载可稍后 `create_dir_all`）。 |
| `into_existing` | 本地必须 canonicalize；给 `ls` / `query` / `export --from`。 |
| `into_create_target` | 本地 parent 必须是目录，target 不得已存在。对象 URI 只做语法校验。给 `import --to`。 |
| `exists` | 本地 `path.exists()`。对象存储 list prefix，有任意 object 即为 true。观测性调用，不创建 key。 |
| `put_bytes` | 把该 URI 当作**单个对象/文件**写入。本地 staging + rename；对象存储 `PutMode::Create`，`overwrite` 时 Overwrite。给 `export --to`。 |

允许的 scheme：`local`、`file`、`s3`、`az`、`gs`，以及 Lance 测试后端 `memory` / `shared-memory`。帮助文档只提生产 scheme。无 scheme 视为本地路径。

`StorylineLanceStore::open_uri` / `destination_exists` 继续作为 Dataset 根的打开入口；`DatasetLocation` 负责 URI 合同，不重写三表写入。

## 3. CLI 切面

| 命令 | 之后 |
|---|---|
| `import --to` | `parse` + `into_create_target`；对象存储仍要求 `--output-format storyline`；`exists()` 为 true 则 Conflict |
| `import --from` | 同一套 `parse`；canonical 探测用 `kind()` / `as_str()`，不再 `contains("://")` |
| `export --to` | `put_bytes`；允许 `s3://bucket/out.json` |
| `serve` 位置参数 | `parse`；拒绝内嵌凭证。相对本地路径可尚不存在 |

明确不改：default Warehouse 仍必须是本地目录；Gateway 写对象 Dataset 仍要本地 `--gateway-state`。

## 4. 验收

```bash
pchronicle import --from ./data --to s3://bucket/ds --output-format storyline
pchronicle export --from s3://bucket/ds --to s3://bucket/out.json --output-format atif
pchronicle serve s3://bucket/ds
```

本地 import 的 create-only 与失败不留半成品行为不变。现有 CLI 单测继续通过；对象存储写路径可用 `shared-memory://` 覆盖，不必每次连 MinIO。

## 5. 非目标（第二刀）

Catalog `walkdir` vs `ObjectStore::list`、CasStore Local/Object、events `_manifest.json`、Storyline `CURRENT`、dataset write lock。那些不是 Dataset 根合同。
