# 在本地服务 Dataset

```bash
pchronicle serve ./trajectory-data
pchronicle serve --listen 127.0.0.1:8080 train=./train eval=./eval
pchronicle serve --catalog-config catalog.toml --listen 127.0.0.1:8081
```

监听地址只允许 loopback。命名挂载提供稳定的 SQL 别名。
`--open` 打开 Web UI，`--home-link TEXT=PATH` 增加同源导航链接。
`--catalog-config` 启用 Directory 和数据操作认证，不能与位置参数挂载同时使用。
通过 `pchronicle serve catalog` 配置 Dataset 和用户授权，存储凭证按后端与 worker
隔离。启动就绪 JSON 包含 `warehouse_endpoint`。

刷新先构建新 Snapshot 再切换读者，失败时保留旧视图。数据写入使用 CLI import。
日志写入 stderr，`--log-level` 控制详细程度。失败 API 响应包含 `code`、`message`
和 `request_id`，内部错误细节保留在服务日志。

[CLI 参考](../reference/cli.md) · [Web UI](ui.md)
