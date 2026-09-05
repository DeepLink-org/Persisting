# pVisor CLI 设计

`pvisor` 是 Run、OverlayFS、OverlayNet、Gateway 与 replay 服务的统一前端。所有命令都解析为共用的 `RunConfig`/`RunSpec`，确保 TOML、容器委托和 host 执行语义一致。

`run` 执行 Agent。首个参数不是保留命令或帮助/版本选项时会自动改写为 `pvisor run`，因此 `pvisor -- codex` 等价于 `pvisor run -- codex`。`replay` 是独立轨迹流程，不会隐式开启 Gateway 或 pChronicle capture。`env` 管理具名可复用 stage，提供 `create`、`start`、`stop`、`exec`、`shell`、`list`、`status`、`inspect`、`apply`、`drop`、`delete`。生命周期选择器支持 Run id、记录目录、`run.json`、upper/merged 路径或工作区。

`--spec` 接受 TOML `RunConfig` 或准备好的 JSON `RunSpec`。显式 CLI 标量覆盖 TOML，重复列表选项替换整个列表，`--` 后命令替换 `run.command`；提供 `--container-image` 或 `--rootfs` 时可推断 executor。

`review`、`checkpoint`、`fork`、`apply`、`drop` 是独立事务操作。Checkpoint 保证停止一致；`apply` 按依赖闭包提交并记录 `apply-ledger.json`；live Run 拒绝变更。重置会创建新的 environment generation，避免旧 metadata 覆盖新 stage。
