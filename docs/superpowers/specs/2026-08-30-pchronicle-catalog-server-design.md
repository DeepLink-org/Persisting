# pChronicle path Directory

日期：2026-08-30  
状态：已在对话中逐节确认，按用户要求直接进入实现  
范围：`pchronicle serve` 上的 Directory（名字→path + ACL）；本机 `catalog://` alias；Web 用用户 ak/sk 查数  
排除：STS、热加载 TOML、非环回 bind、独立 `catalog serve` 进程、`fork(2)` 已运行的 Tokio、Gateway/Control

## 目标

Directory 是平台部署时打开 **path** 的一种方式，不是第三种 Dataset。Dataset 身份始终是 path（本地路径或 `s3://`）。服务端配置列出对象存储（或同机本地）库及后端 ak/sk；用户用另一对 ak/sk 登录后，只能看见被授权的 path。换票后的 `uri` 才是引擎打开的 Dataset。

- **CLI**：`@team/prod` 向 Directory 换票，进程内缓存，客户端自己打开 path（透传后端密钥）。
- **Web**：ak/sk 存在 `localStorage`，查询在 **spawn 出的降权 worker** 里执行；父进程不拿全量库密钥跑 DataFusion。

## 进程

- 挂在现有 `pchronicle serve`（`--catalog-config`）。无该文件时行为不变。
- 默认只绑环回。Catalog 路由与带鉴权的查询共用 Warehouse listener。不开放 `0.0.0.0`。
- 父进程：静态 UI、catalog list/get、鉴权。数据面 `/api/*`（health 与 catalog 除外）鉴权后 spawn `pchronicle serve --catalog-query-worker`。
- Worker：不听端口、不读 `catalog.toml`、不读用户钥。stdin 给出过滤后的 mounts + 原始请求；stdout 回 HTTP 信封后退出。用 `Command` 新进程，不用 `fork(2)`。
- 同一 catalog 内所有 `s3://` 库必须共用同一组后端 endpoint/region/ak/sk（进程级 AWS 环境变量一次只能持有一套）。

## 配置

`catalog.toml`：`[libraries.<name>]`（uri、可选 endpoint/region、对象存储必填后端 ak/sk）+ `[users.<name>]`（access_key、secret_key、datasets）。授权引用未知 library、用户钥重复：启动失败。改 TOML 需重启。

本机：`pchronicle alias add team catalog://127.0.0.1:PORT --ak --sk`。`@team/prod` 换票；普通 URI alias 的 `@name/suffix` 仍为路径拼接。后端密钥不写入 `config.toml`。

## HTTP

头：`x-pchronicle-access-key`、`x-pchronicle-secret-key`。钥错 `401`，不区分用户是否存在。未授权与不存在的库名均为 `404`。

- `GET /api/v1/catalog/datasets`：名、uri、endpoint、region，无后端密钥。
- `GET /api/v1/catalog/datasets/{name}`：含后端 ak/sk。

错误 JSON 沿用 `code` / `message` / `request_id`。日志可含用户段名与库名，禁止打印任何 secret。

## Web

Settings 保存用户 ak/sk 到 `localStorage`。数据面请求附带上述头。无钥不查询。

## 非目标

STS、热加载、公网 bind、独立 catalog 进程、把后端钥写入本机 alias 配置、浏览器 E2E。
