# RFC-0013: pChronicle path Directory

| Field | Value |
|---|---|
| **Status** | Proposed |
| **Date** | 2026-09-18 |
| **Component** | pChronicle CLI、`pchronicle serve`、pChronicle Web |
| **Related** | [RFC-0003 Ownership](0003-pchronicle-ownership.md) · [Warehouse 指南](../pchronicle/guides/serve.md) · [CLI 参考](../pchronicle/reference/cli.md) · [架构](../pchronicle/design/architecture.md) |

---

## 摘要

本 RFC 定义 pChronicle 平台部署时打开 **path** 的一种方式：**Directory**（名字 → path + ACL + 换票）。

Dataset 身份始终是 path（本机路径或 `s3://` / `az://` / `gs://` URI）。Directory 不是第三种 Dataset，也不替代 Snapshot。它只决定调用方可以解析到哪些 path；换票后的 `uri` 才是引擎打开的 Dataset。

CLI 标志、配置文件和 HTTP 路径为兼容性仍使用 `catalog` 一词（`--catalog-config`、`catalog.toml`、`catalog://`、`/api/v1/catalog/datasets`）。产品与 RFC 口径称 Directory。配置文件是 TOML（扩展名可为 `.toml` / `.yml` 等，内容仍按 TOML 解析）。

规范实现挂在现有 `pchronicle serve --catalog-config` 上，不引入独立 `catalog serve` 进程。
Listener 默认可为 loopback；也允许绑定非环回地址，但部署方 MUST 自行保证网络边界。

- **Serve**：父进程只做鉴权、目录/换票与 spawn worker（front-only），MUST NOT 在父进程打开
  `[datasets.*]`。授权范围内的数据面请求由一次性 `--catalog-query-worker` 打开对应 mount。
- **公开浏览**：`[[grants]]` 中 `user = "*"` 的 Dataset 对匿名调用可见；父进程可用其做浏览缓存，
  不含后端密钥。
- **CLI 配置**：`pchronicle serve catalog dataset add|remove|list` 改写 datasets；
  `issue|grant|revoke` 改写用户与授权。用户/授权热加载；Dataset URI 与后端凭证变更 MUST 重启。
- **Directory 换票**：`@team` 解析为 `catalog://…`；`@team/prod` 换票后客户端打开票里的 path。
  `/api/v1/catalog/datasets` 按用户钥过滤可见 library（无头时仅返回公开库）。

```text
pchronicle serve catalog dataset add --catalog-config catalog.toml prod --uri s3://bucket/prod \
  --access-key BACKEND_AK --secret-key BACKEND_SK
pchronicle serve catalog issue --catalog-config catalog.toml alice
pchronicle serve catalog grant --catalog-config catalog.toml alice prod
pchronicle serve --catalog-config catalog.toml --listen 127.0.0.1:8081
pchronicle dataset pin team catalog://127.0.0.1:8081 --ak USER_AK --sk USER_SK
pchronicle query @team/prod 'SELECT 1'
```

## 动机

本机路径和静态 Warehouse mount 假设操作者已经能看见全部 Dataset。把对象存储上的多个评测库交给一组人使用时，出现三个缺口：

1. **发现与授权混在一起**。用户需要一份目录，列出自己可以打开的 library 名，而不是把所有 bucket URI 写进每人的 `config.toml`。
2. **后端密钥不能进用户配置**。对象存储 ak/sk 属于存储账户；用户钥只用于 Directory 鉴权。把后端钥写入本机 dataset pin 会扩散到每台笔记本，也无法按人裁剪可见库。
3. **Web 与 CLI 的数据面不同**。CLI 可以在换票后自己打开 `s3://`。Web 的查询跑在 serve 进程里；若父进程加载全部 library 的后端密钥并执行 SQL，一次鉴权绕过就会看到未授权库。

本 RFC 把 Directory 定义为 **目录 + ACL + 换票**，把存储访问留给已有 `open(path)`，并把 Web 数据面隔离到一次性 worker。

## 目标与非目标

### 目标

- 用一份 Directory 配置描述 `meta`、`users`、`datasets` 和 `[[grants]]`。
- 用 CLI 签发用户钥并改写 ACL：`pchronicle serve catalog issue|grant|revoke` 不启动 HTTP。
- 让 `@name/library` 解析为一条 path（换票后的 `uri`）；引擎随后只打开该 path。
- 换票后 CLI 自己访问存储；后端密钥只出现在票和 worker stdin 中，不写入用户 `config.toml`。
- Web 用用户钥换授权范围，查询只看到该用户的 mounts；`user = "*"` 的公开库可匿名列出/浏览。
- 运行中每约 3 秒热加载用户与授权；Dataset 定义与后端凭证变更拒绝热加载，须重启。
- 允许 Warehouse 绑定任意 listen 地址；默认示例仍用 loopback。Catalog 头不是公网认证边界，不可信网络上的暴露由部署方负责。

### 非目标

- STS、临时凭证轮换、或把用户钥映射成短时 AWS session。
- 热加载 Dataset URI / endpoint / region / 后端 ak/sk（须重启 serve）。
- 在运行中的 Warehouse 上提供 HTTP 签发接口。
- 提供独立 `catalog serve` 二进制。
- 在已运行的 Tokio runtime 上 `fork(2)`（未定义行为）。
- 把后端对象存储密钥写入本机 dataset pin 配置。
- v1 细粒度 `permissions` 强制执行（字段可写，语义仍是库成员关系）。
- 改变 Snapshot 协议、SQL schema 或 Gateway/Control 协议。

本 RFC 的 Directory 与打开 path 之后的 **Snapshot**（见 [Snapshot 设计](../pchronicle/design/catalog.md)）不是同一对象。Directory 列出授权 path；Snapshot 钉住一条已打开 path 上的 Source 成员与版本。

## 角色与信任边界

| 角色 | 持有 | 用途 |
|---|---|---|
| 存储账户 | 后端 `access_key` / `secret_key`，以及可选 endpoint、region | 打开 `s3://` library |
| Directory 用户 | 用户 `access_key` / `secret_key` | 列出/领取被授权 library 的票 |
| 本机 CLI | 用户钥（存在 dataset pin 配置） | 换票后把后端钥注入进程环境并打开票中的 path |
| 浏览器 | 用户钥（`localStorage`） | 作为请求头发给 loopback serve |
| serve 父进程 | 完整 `catalog.toml` | 鉴权、返回票、spawn worker；不把后端钥写入 AWS 环境 |
| query worker | 该用户被授权 library 的票 | 一次性执行 Warehouse 数据面请求 |

ACL 是 **发现与授权** 边界，不是对象存储的强制隔离。持有后端密钥或能猜测 URI 的调用方，仍可能绕过 Directory 直接访问存储。Directory 不替代 bucket policy。

## 进程模型

Directory 挂在现有 Warehouse listener 上。未传 `--catalog-config` 时，`pchronicle serve` 行为不变：静态 mount、无用户鉴权。

```text
浏览器 / CLI
  → Warehouse listener
       ├─ GET /health
       ├─ GET /api/v1/catalog/datasets[/{name}]   父进程：鉴权 + 目录/票
       ├─ 静态 UI
       └─ 其余 /api/*                             父进程内挂载 / 或 spawn worker
              → pchronicle serve --catalog-query-worker
                    stdin:  mounts + HTTP 请求
                    stdout: status / content-type / body
                    退出
```

约束：

1. Listener MAY 绑定非 loopback 地址。本 RFC 不把 catalog 头当作公网认证边界；部署方 MUST 在不可信网络上自行加边界。
2. 父进程 MUST NOT 打开配置中的 datasets。父进程使用空 mount 的 front-only Warehouse；公开库的浏览缓存只持有 path，不把后端钥写入父进程 `AWS_*`。
3. Worker MUST 由 `Command` 启动新进程，MUST NOT `fork(2)` 已运行的 Tokio runtime。
4. Worker MUST NOT 监听端口、MUST NOT 读取 Directory 配置、MUST NOT 读取用户钥。它只消费 stdin 中过滤后的 mounts 和原始请求。
5. Worker 继承父进程环境（证书、`PATH` 等），但父进程 MUST NOT 预先把 catalog 后端密钥写入 `AWS_*`。Worker 在打开存储前为自己设置该用户票中的后端环境。
6. 每个 `[datasets.*]` MAY 自带 endpoint、region 和后端 ak/sk。同一 worker 进程的进程级 AWS 环境一次只能持有一套凭据；若用户同时授权了不兼容的多个 `s3://` 后端，请求 MUST 带 `dataset=` 显式选中其中一个，否则 MUST 失败。
7. 隐藏 flag `--catalog-query-worker` MUST NOT 出现在用户可见的 `serve --help` 中。

Worker 超时后父进程 MUST 返回 `unavailable`，不得把 stdin 中的密钥写进日志。

## 配置

Directory 配置只管理用户、Dataset 和授权关系。它是唯一事实来源；运行时服务配置仍由 `pchronicle serve` 参数提供。配置文件不存在时，Catalog 管理命令会创建一个空 Catalog（带 `[meta]`）。

权威 schema（与当前实现 / 部署样例一致）：

```toml
[meta]
version = 1
revision = 1
name = "default"

[users.alice]
access_key = "pcak_…"
secret_key = "…"

[datasets.default]
uri = "/data/warehouse"

[datasets.prod]
uri = "s3://prod/"
endpoint = "http://s3-a.example:8060"
region = "us-east-1"
access_key = "BACKEND_AK_A"
secret_key = "BACKEND_SK_A"

[datasets.prod2]
uri = "s3://prod"
endpoint = "http://s3-b.example:8060"
region = "us-east-1"
access_key = "BACKEND_AK_B"
secret_key = "BACKEND_SK_B"

[[grants]]
user = "*"
dataset = "prod"

[[grants]]
user = "*"
dataset = "prod2"

[[grants]]
user = "alice"
dataset = "default"
# permissions 可选；v1 忽略细粒度语义，仅表示库成员关系
# permissions = ["read", "query", "analyze"]
```

规则：

- `meta.version` 必须为支持的配置版本；CLI 成功写入后 SHOULD 维护 `meta.revision` / `meta.name`（可缺省）。
- 用户名和 Dataset 名必须是小写 `[A-Za-z_][A-Za-z0-9_]*`。
- `[users.*]` 只含 `access_key` / `secret_key`；`access_key` 必须全局唯一；第一版允许明文 `secret_key`。
- `[datasets.*]` 必含 `uri`；本地 path 不得设置后端密钥；`s3://` MUST 同时设置 `access_key` 与 `secret_key`，并可设 `endpoint` / `region`。
- 不同 Dataset MAY 使用不同的 endpoint / region / 后端密钥（见进程模型第 6 条）。
- `[[grants]]` 必含 `user` 与 `dataset`；`permissions` 可选，v1 不强制执行。
- `grants.user = "*"`：将该 Dataset 标为公开（匿名可列出/浏览），并在解析时展开给配置中**当前全部**用户；新签发用户在热加载后自动继承。
- `grants.user` 为具名用户时必须引用已存在用户；`grants.dataset` 必须引用已存在 Dataset。
- 同一用户与 Dataset 的 grant 不得重复（含 `*` 展开后的冲突）。
- 配置文件大小必须有上界；解析或校验失败时服务拒绝启动；热加载失败 MUST 保留上一份有效 ACL。
- TOML 是权威配置，后续 SQLite/Postgres 只能作为索引和派生投影。

## CLI 管理

Catalog 管理命令只修改配置文件，不启动 HTTP listener。文件不存在时，命令创建父目录和空配置文件。

```text
pchronicle serve catalog dataset add    --catalog-config FILE NAME --uri URI [OPTIONS]
pchronicle serve catalog dataset remove --catalog-config FILE NAME...
pchronicle serve catalog dataset list   --catalog-config FILE

pchronicle serve catalog issue  --catalog-config FILE NAME
pchronicle serve catalog grant  --catalog-config FILE NAME DATASET...
pchronicle serve catalog revoke --catalog-config FILE NAME DATASET...
```

`dataset add` 只登记 Dataset，不创建或删除后端数据。通过 CLI 追加 `s3://` 时，若文件中已有其它 `s3://`，新条目的 endpoint / region / 后端密钥 MUST 与之完全一致（手写多后端配置仍合法，但 worker 须按第 6 条选库）。`issue` 生成用户 AK/SK（secret 只在本次 stdout 输出），MUST NOT 写入任何 grant。`grant` / `revoke` 修改 `[[grants]]`；NAME 为 `*` 时对**当前全部用户**逐个写入/删除具名 grant（不是写入 `user = "*"` 公开行）。所有写操作 MUST 原子替换文件，失败时保留原文件。

## HTTP

Directory 路由与 Warehouse 共用 `/api` 与 `/api/v1` 前缀。鉴权头：

| Header | 含义 |
|---|---|
| `x-pchronicle-access-key` | 用户 access key |
| `x-pchronicle-secret-key` | 用户 secret key |

缺失、空白或密钥不匹配 MUST 返回 `401`，且 MUST NOT 区分“用户不存在”与“密钥错误”。
完全无 catalog 头时，列表接口 MAY 只返回 `user = "*"` 的公开 Dataset；带残缺头仍 MUST `401`。

未授权的 library 名与不存在的 library 名 MUST 都返回 `404`。

| 路由 | 父进程 | 响应 |
|---|---|---|
| `GET /api/v1/catalog/datasets` | 是 | 鉴权用户可见 library，或匿名时的公开 library：`name`、`uri`、可选 `endpoint`/`region`；**不含**后端密钥 |
| `GET /api/v1/catalog/datasets/{name}` | 是 | 鉴权且授权时返回完整票，含后端 `access_key` / `secret_key`；匿名 MUST `401`（公开库只允许无密钥列表/浏览） |
| `GET /api/health` | 是 | 无鉴权 |
| 静态 UI | 是 | 无鉴权 |
| 其余 `/api/*`（含 `GET /api/catalog`，返回当前 Snapshot） | 否，转发 worker | 先鉴权，再按用户 mounts 执行；多后端时须 `dataset=` |

错误 JSON 沿用 Warehouse 的 `code`、`message`、`request_id`。日志可以包含用户段名、library 名和 `request_id`，MUST NOT 打印用户钥或后端钥。

`GET /api/v1/catalog/datasets/{name}` 是 CLI 换票接口。拿到票的客户端随后直接打开 `uri`（Dataset path），不再把查询代理回 Directory。

## CLI dataset pin

`catalog://` 是 pin **类型**，不是 DatasetLocation 可解析的存储 URI。换票成功后 Dataset 身份是票里的 path，不是 `catalog://…` 本身。

```bash
pchronicle dataset pin team catalog://127.0.0.1:8081 --ak USER_AK --sk USER_SK
```

规范化规则：

- scheme MUST 为 `catalog`；
- host MUST 是环回 IP（如 `127.0.0.1`），MUST 带端口；
- MUST NOT 包含 userinfo、path、query 或 fragment；
- MUST NOT 接受 `--endpoint` / `--region`（那是对象存储参数，来自票而不是 pin）。

解析按 pin **类型** 分派，而不是把所有 `@name/suffix` 都做路径拼接：

| 引用 | catalog pin | 普通 URI pin |
|---|---|---|
| `@team` / `@team/` | `ls` 列出该用户可访问的 Datasets | 解析为 pin 根 URI |
| `@team/prod` | 向 Directory 领取 library `prod` 的票，打开票中 path | 根 URI 再拼接路径 `prod` |
| `@team/prod/more` | 先领 `prod`，再把 `more` 拼到票的 path 上 | 根 URI 拼接 `prod/more` |

用户 `--ak/--sk` 存入本机 dataset pin 凭据表，与 S3 pin 相同的隔离方式：不出现在 `dataset list` / `dataset show` 的 URI 里。后端密钥 MUST NOT 写入该文件。

换到的票缓存在 CLI 进程内（`thread_local`），按 catalog URL、用户 access key 和 library 名索引。长生命周期的 `serve` 进程不使用这份 CLI 缓存；Web 每次请求重新鉴权。进程退出即丢弃缓存。

## Web

Settings（左侧 **Keys**）保存 catalog 用户钥到 `localStorage`：

- `pchronicle.catalog.access_key`
- `pchronicle.catalog.secret_key`

浏览器把这两项作为上述 HTTP 头附加到 **发往当前 pChronicle serve 的** `/api/` 请求。这与 Assistant 的 Browser BYOK 相反：Assistant 钥只发给模型端点，catalog 钥必须到达 serve 才能鉴权。

未配置用户钥时，Web 仍可浏览 `user = "*"` 公开库；需鉴权的数据面请求 MUST `401`。无 `--catalog-config` 的普通 serve 不要求这些头。

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

### 把签发做成 Warehouse HTTP mint

拒绝。Catalog 头不是公网认证边界；loopback 上无认证的 mint 会把用户钥发给任何能打到端口的本机进程。签发入口是改写 `catalog.toml` 的 CLI。

### 独立 `catalog serve` 进程

拒绝。第二套 listener、端口和生命周期会与 Warehouse 文档分叉。Catalog 目录流量很小，适合挂在现有 `pchronicle serve` 上。

### 父进程打开全部 datasets 再按用户过滤 SQL

拒绝。DataFusion 与对象存储客户端一旦持有全量后端密钥和 mount，过滤错误就会越权。Web 查询必须在只含授权 mounts 的进程里执行。

### `fork(2)` 已运行的 Tokio 以“降权”

拒绝。在多线程 runtime 上 fork 是未定义行为。使用 `Command` 新进程。

### STS / 短时会话券

拒绝。当前目标是本机协作目录，不是云上身份联邦。透传后端密钥给已授权客户端，配置更简单，也与现有 S3 pin 注入 `AWS_*` 的方式一致。

### 把 catalog 做成普通路径拼接 pin

拒绝。`@prod/evals` 对 `s3://bucket` 是路径拼接；对 Directory locator 则是“名字 + library 名”，换票后打开票中 path。混用会让 `@team/prod` 被拼成非法 URI `catalog://127.0.0.1:8081/prod`。

### 非环回 bind + 把 catalog 头当公网认证

拒绝。Warehouse 仍是本机检查面。打开 `0.0.0.0` 需要独立的认证、TLS 与多租户威胁模型，超出本 RFC。

## 兼容性与演进

- 无 `--catalog-config` 时，现有 Dataset 引用、普通 pin 的 `@name/suffix` 路径拼接、以及无鉴权 loopback Warehouse MUST 保持不变。
- `catalog://` MUST NOT 成为 `DatasetLocation` 可打开的存储 scheme；只有 dataset pin 解析器认识它。
- 权威配置键为 `meta` / `users` / `datasets` / `grants`；旧式 `[libraries.*]` 或把授权嵌在 `users.*.datasets` 的写法 MUST NOT 再作为规范。
- 新增 Dataset 字段、鉴权头或 worker 协议属于破坏性变更，需要修订本 RFC。
- 未来的 STS 或 Dataset 热加载可以作为后续 RFC，不得 silently 改变“透传后端密钥 / Dataset 变更须重启”的语义。

本 RFC 修正架构文档中“loopback Warehouse 完全没有 authentication”的表述：在 `--catalog-config` 下，数据面和 Directory 路由使用用户钥请求头；公开库除外。它仍不是公网多租户服务。

## 实施状态

当前实现覆盖本 RFC 的核心范围：

- TOML Directory 配置解析与启动期校验（`meta` / `users` / `datasets` / `[[grants]]`，含 `user = "*"`）；
- `pchronicle serve catalog issue|grant|revoke|dataset …` 改写配置（签发不授权，sk 只打一次 stdout）；
- 用户与授权约 3 秒热加载；Dataset / 后端凭证变更拒绝热加载并保留旧 ACL；
- `GET /api/v1/catalog/datasets` 与 `/{name}`（含匿名公开列表）；
- `--catalog-config` front-only 父进程与 `--catalog-query-worker`；多后端时按 `dataset=` 收窄 mount；
- `catalog://` pin、`@team/prod` 换票与进程内票缓存；
- Web `localStorage` 用户钥与数据面请求头。

后续工作：

1. 覆盖真实 worker 子进程的集成测试（环境中不得出现未授权 library 的密钥）；
2. 评估是否为本地路径 library 提供与 S3 相同的显式审计日志字段；
3. `issue --rotate`：轮换已有用户密钥（当前重名签发直接拒绝）；
4. 是否让 CLI `dataset add` 正式支持写入多套 S3 后端（当前手写合法，CLI 追加仍要求与已有 s3 后端一致）。
