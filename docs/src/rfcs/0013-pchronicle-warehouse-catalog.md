# RFC-0013: pChronicle Warehouse Catalog

| Field | Value |
|---|---|
| **Status** | Proposed |
| **Date** | 2026-08-30 |
| **Component** | pChronicle CLI、`pchronicle serve`、pChronicle Web |
| **Related** | [RFC-0003 Ownership](0003-pchronicle-ownership.md) · [Warehouse 指南](../pchronicle/guides/serve.md) · [CLI 参考](../pchronicle/reference/cli.md) · [架构](../pchronicle/design/architecture.md) |

---

## 摘要

本 RFC 定义 pChronicle 的第三种 Dataset 入口：**catalog 命名空间**。

前两种入口是本机路径和 `s3://` URI。Catalog 入口把一组已命名的 library（对象存储前缀或同机本地路径）放在一份 ACL 配置里；用户持有另一对 access/secret key，只能发现并打开被授权的 library。

规范实现挂在现有 `pchronicle serve --catalog-config` 上，不引入独立 `catalog serve` 进程，也不把 listener 从 loopback 打开。

- **CLI**：`@team` 解析为 `catalog://127.0.0.1:PORT` 命名空间；`@team/prod` 向 catalog 换票后，由客户端自己访问存储（透传后端密钥）。
- **Web**：用户钥存在浏览器 `localStorage`；数据面查询在 **新进程 worker** 中执行。父进程负责鉴权和换票，不拿全量后端密钥跑 DataFusion。

```text
pchronicle serve --catalog-config catalog.toml --listen 127.0.0.1:8081
pchronicle alias add team catalog://127.0.0.1:8081 --ak USER_AK --sk USER_SK
pchronicle query @team/prod 'SELECT 1'
```

## 动机

本机路径和静态 Warehouse mount 假设操作者已经能看见全部 Dataset。把对象存储上的多个评测库交给一组人使用时，出现三个缺口：

1. **发现与授权混在一起**。用户需要一份目录，列出自己可以打开的 library 名，而不是把所有 bucket URI 写进每人的 `config.toml`。
2. **后端密钥不能进用户配置**。对象存储 ak/sk 属于存储账户；用户钥只用于 catalog 鉴权。把后端钥写入本机 alias 会扩散到每台笔记本，也无法按人裁剪可见库。
3. **Web 与 CLI 的数据面不同**。CLI 可以在换票后自己打开 `s3://`。Web 的查询跑在 serve 进程里；若父进程加载全部 library 的后端密钥并执行 SQL，一次鉴权绕过就会看到未授权库。

本 RFC 把 catalog 定义为 **目录 + ACL + 换票**，把存储访问留给已有 Dataset 打开路径，并把 Web 数据面隔离到一次性 worker。

## 目标与非目标

### 目标

- 用一份 `catalog.toml` 同时描述 libraries 和 users。
- 让 `@name/library` 成为与本地路径、`s3://` 并列的 Dataset 引用。
- 换票后 CLI 自己访问存储；后端密钥只出现在票和 worker stdin 中，不写入用户 `config.toml`。
- Web 用用户钥换授权范围，查询只看到该用户的 mounts。
- 保持 Warehouse 为 loopback-only 本地检查面，而不是公网多租户服务。

### 非目标

- STS、临时凭证轮换、或把用户钥映射成短时 AWS session。
- 热加载 `catalog.toml`；改配置 MUST 重启 serve。
- 把 listener bind 到非环回地址，或提供独立 `catalog serve` 二进制。
- 在已运行的 Tokio runtime 上 `fork(2)`（未定义行为）。
- 把后端对象存储密钥写入本机 alias 配置。
- 改变 Dataset Catalog Snapshot、SQL schema 或 Gateway/Control 协议。

“Catalog” 在本 RFC 中指 **远程 library 目录服务**，与 Dataset 发现用的 Catalog Snapshot（见 [Dataset Catalog 设计](../pchronicle/design/catalog.md)）不是同一对象。

## 角色与信任边界

| 角色 | 持有 | 用途 |
|---|---|---|
| 存储账户 | 后端 `access_key` / `secret_key`，以及可选 endpoint、region | 打开 `s3://` library |
| Catalog 用户 | 用户 `access_key` / `secret_key` | 列出/领取被授权 library 的票 |
| 本机 CLI | 用户钥（存在 alias 配置） | 换票后把后端钥注入进程环境并打开 Dataset |
| 浏览器 | 用户钥（`localStorage`） | 作为请求头发给 loopback serve |
| serve 父进程 | 完整 `catalog.toml` | 鉴权、返回票、spawn worker；不把后端钥写入 AWS 环境 |
| query worker | 该用户被授权 library 的票 | 一次性执行 Warehouse 数据面请求 |

ACL 是 **发现与授权** 边界，不是对象存储的强制隔离。持有后端密钥或能猜测 URI 的调用方，仍可能绕过 catalog 直接访问存储。Catalog 不替代 bucket policy。

## 进程模型

Catalog 挂在现有 Warehouse listener 上。未传 `--catalog-config` 时，`pchronicle serve` 行为不变：静态 mount、无用户鉴权。

```text
浏览器 / CLI
  → loopback Warehouse
       ├─ GET /health
       ├─ GET /api/v1/catalog/datasets[/{name}]   父进程：鉴权 + 目录/票
       ├─ 静态 UI
       └─ 其余 /api/*                             父进程鉴权后 spawn worker
              → pchronicle serve --catalog-query-worker
                    stdin:  mounts + HTTP 请求
                    stdout: status / content-type / body
                    退出
```

约束：

1. Listener MUST 为 loopback。本 RFC 不把 catalog 头当作公网认证边界。
2. 父进程 MUST NOT 打开 `catalog.toml` 中的 libraries。父进程使用空 mount 的 front-only Warehouse。
3. Worker MUST 由 `Command` 启动新进程，MUST NOT `fork(2)` 已运行的 Tokio runtime。
4. Worker MUST NOT 监听端口、MUST NOT 读取 `catalog.toml`、MUST NOT 读取用户钥。它只消费 stdin 中过滤后的 mounts 和原始请求。
5. Worker 继承父进程环境（证书、`PATH` 等），但父进程 MUST NOT 预先把 catalog 后端密钥写入 `AWS_*`。Worker 在打开存储前为自己设置该用户票中的后端环境。
6. 同一 `catalog.toml` 内所有 `s3://` library MUST 共用同一组 endpoint、region 和后端 ak/sk。进程级 AWS 环境一次只能持有一套凭据。
7. 隐藏 flag `--catalog-query-worker` MUST NOT 出现在用户可见的 `serve --help` 中。

Worker 超时后父进程 MUST 返回 `unavailable`，不得把 stdin 中的密钥写进日志。

## 配置

`catalog.toml` 是唯一配置面：

```toml
[libraries.prod]
uri = "s3://bucket/prod"
endpoint = "http://127.0.0.1:9000"
region = "us-west-2"
access_key = "BACKEND_AK"
secret_key = "BACKEND_SK"

[libraries.evals]
uri = "s3://bucket/evals"
endpoint = "http://127.0.0.1:9000"
region = "us-west-2"
access_key = "BACKEND_AK"
secret_key = "BACKEND_SK"

[users.alice]
access_key = "USER_AK"
secret_key = "USER_SK"
datasets = ["prod", "evals"]

[users.bob]
access_key = "BOB_AK"
secret_key = "BOB_SK"
datasets = ["evals"]
```

规则：

- 至少需要一个 library 和一个 user。
- library 名 MUST 是合法 Dataset mount 名。
- `s3://` library MUST 同时设置后端 `access_key` 和 `secret_key`。
- 非 `s3://` library MUST NOT 设置后端密钥。
- 所有 `s3://` library 的 endpoint、region、后端密钥 MUST 完全一致。
- `users.*.datasets` 引用的名字 MUST 存在于 `libraries`。
- 用户 `access_key` MUST 全局唯一。
- 配置文件 MUST 是普通文件，大小有上界；解析失败则 serve 拒绝启动。

本地路径 library 允许不设后端密钥，便于同机目录通过 catalog 做授权发现。客户端换票后仍按票中的 URI 打开。

## HTTP

Catalog 与 Warehouse 共用 `/api` 与 `/api/v1` 前缀。鉴权头：

| Header | 含义 |
|---|---|
| `x-pchronicle-access-key` | 用户 access key |
| `x-pchronicle-secret-key` | 用户 secret key |

缺失、空白或密钥不匹配 MUST 返回 `401`，且 MUST NOT 区分“用户不存在”与“密钥错误”。

未授权的 library 名与不存在的 library 名 MUST 都返回 `404`。

| 路由 | 父进程 | 响应 |
|---|---|---|
| `GET /api/v1/catalog/datasets` | 是 | 该用户可见 library 的 `name`、`uri`、可选 `endpoint`/`region`；**不含**后端密钥 |
| `GET /api/v1/catalog/datasets/{name}` | 是 | 授权时返回完整票，含后端 `access_key` / `secret_key` |
| `GET /api/health` | 是 | 无鉴权 |
| 静态 UI | 是 | 无鉴权 |
| 其余 `/api/*`（含 `GET /api/catalog` Snapshot） | 否，转发 worker | 先鉴权，再按用户 mounts 执行 |

错误 JSON 沿用 Warehouse 的 `code`、`message`、`request_id`。日志可以包含用户段名、library 名和 `request_id`，MUST NOT 打印用户钥或后端钥。

`GET /api/v1/catalog/datasets/{name}` 是 CLI 换票接口。拿到票的客户端随后直接打开 `uri`，不再把查询代理回 catalog。

## CLI alias

`catalog://` 是 alias **类型**，不是 DatasetLocation 可解析的存储 URI。

```bash
pchronicle alias add team catalog://127.0.0.1:8081 --ak USER_AK --sk USER_SK
```

规范化规则：

- scheme MUST 为 `catalog`；
- host MUST 是环回 IP（如 `127.0.0.1`），MUST 带端口；
- MUST NOT 包含 userinfo、path、query 或 fragment；
- MUST NOT 接受 `--endpoint` / `--region`（那是对象存储参数，来自票而不是 alias）。

解析按 alias **类型** 分派，而不是把所有 `@name/suffix` 都做路径拼接：

| 引用 | catalog alias | 普通 URI alias |
|---|---|---|
| `@team` | 错误：命名空间不是 Dataset | 解析为 alias 根 URI |
| `@team/prod` | 向 catalog 领取 library `prod` 的票 | 根 URI 再拼接路径 `prod` |
| `@team/prod/more` | 先领 `prod`，再把 `more` 拼到票的 URI 上 | 根 URI 拼接 `prod/more` |

用户 `--ak/--sk` 存入本机 alias 凭据表，与 S3 alias 相同的隔离方式：不出现在 `alias list` / `alias get-url` 的 URI 里。后端密钥 MUST NOT 写入该文件。

换到的票缓存在 CLI 进程内（`thread_local`），按 catalog URL、用户 access key 和 library 名索引。长生命周期的 `serve` 进程不使用这份 CLI 缓存；Web 每次请求重新鉴权。进程退出即丢弃缓存。

## Web

Settings（左侧 **Keys**）保存 catalog 用户钥到 `localStorage`：

- `pchronicle.catalog.access_key`
- `pchronicle.catalog.secret_key`

浏览器把这两项作为上述 HTTP 头附加到 **发往当前 pChronicle serve 的** `/api/` 请求。这与 Assistant 的 Browser BYOK 相反：Assistant 钥只发给模型端点，catalog 钥必须到达 serve 才能鉴权。

未配置用户钥时，Web MUST NOT 假装本地 Warehouse 已授权；catalog 模式下无头请求在数据面得到 `401`。无 `--catalog-config` 的普通 serve 不要求这些头。

查询在 worker 中执行。浏览器不直接持有后端对象存储密钥。

## 数据面隔离

父进程在数据面中间件中：

1. 校验用户钥；
2. 过滤该用户的 library 票；
3. 把 HTTP method、path、query、body 和 mounts 写成 JSON job；
4. spawn 同源二进制 `serve --catalog-query-worker`；
5. 把 stdout 信封还原为 HTTP 响应。

Worker 用票构造 `ChronicleServerConfig` mounts，执行与普通 Warehouse 相同的只读路由，然后退出。

不得把未授权 library 的票放进 job。空授权集合 MUST 表现为 `404`，而不是启动一个空 Warehouse。

## 被拒绝的方案

### 独立 `catalog serve` 进程

拒绝。第二套 listener、端口和生命周期会与 Warehouse 文档分叉。Catalog 目录流量很小，适合挂在现有 `pchronicle serve` 上。

### 父进程打开全部 libraries 再按用户过滤 SQL

拒绝。DataFusion 与对象存储客户端一旦持有全量后端密钥和 mount，过滤错误就会越权。Web 查询必须在只含授权 mounts 的进程里执行。

### `fork(2)` 已运行的 Tokio 以“降权”

拒绝。在多线程 runtime 上 fork 是未定义行为。使用 `Command` 新进程。

### STS / 短时会话券

拒绝。当前目标是本机协作目录，不是云上身份联邦。透传后端密钥给已授权客户端，配置更简单，也与现有 S3 alias 注入 `AWS_*` 的方式一致。

### 把 catalog 做成普通路径拼接 alias

拒绝。`@prod/evals` 对 `s3://bucket` 是路径拼接；对 catalog 则是“命名空间 + library 名”。混用会让 `@team/prod` 被拼成非法 URI `catalog://127.0.0.1:8081/prod`。

### 非环回 bind + 把 catalog 头当公网认证

拒绝。Warehouse 仍是本机检查面。打开 `0.0.0.0` 需要独立的认证、TLS 与多租户威胁模型，超出本 RFC。

## 兼容性与演进

- 无 `--catalog-config` 时，现有 Dataset 引用、普通 alias 的 `@name/suffix` 路径拼接、以及无鉴权 loopback Warehouse MUST 保持不变。
- `catalog://` MUST NOT 成为 `DatasetLocation` 可打开的存储 scheme；只有 alias 解析器认识它。
- 新增 library 字段、鉴权头或 worker 协议属于破坏性变更，需要修订本 RFC。
- 未来的 STS 或热加载可以作为后续 RFC，不得 silently 改变“透传后端密钥 / 重启生效”的语义。

本 RFC 修正架构文档中“loopback Warehouse 完全没有 authentication”的表述：在 `--catalog-config` 下，数据面和 catalog 路由使用用户钥请求头；它仍不是公网多租户服务。

## 实施状态

当前实现覆盖本 RFC 的核心范围：

- `catalog.toml` 解析与启动期校验；
- `GET /api/v1/catalog/datasets` 与 `/{name}`；
- `--catalog-config` front-only 父进程与 `--catalog-query-worker`；
- `catalog://` alias、`@team/prod` 换票与进程内票缓存；
- Web `localStorage` 用户钥与数据面请求头。

后续工作：

1. 覆盖真实 worker 子进程的集成测试（环境中不得出现未授权 library 的密钥）；
2. 评估是否为本地路径 library 提供与 S3 相同的显式审计日志字段。
