# pVisor 命令模型

`pvisor` 是单个受治理 Agent Run 的公共入口。命令接口围绕四类责任组织：启动 Run、查看
记录、决定 staged change 的去向，以及管理可复用环境。命令行与 `RunConfig` 描述同一个
模型；配置文件必须显式传入，不会被当作隐式项目策略。

## 使用 `run` 启动

短写法与完整写法等价：

```bash
pvisor -- codex
pvisor run --stage ./runs/task-001 -- codex
```

需要保留文件修改以便审查时使用 `--stage`。不指定 stage 时，pVisor 仍会记录 Run，但不会
产生可供 apply 的持久 workspace changeset。选中的 host、container 或 VM Provider 会在
Run Bundle 中分别记录实际 capability 与限制。

常用控制按目的分组：

| 目的 | 选项 | 结果 |
| --- | --- | --- |
| Workspace | `--stage`、`--overlayfs-path`、`--overlayfs-compose` | 创建 COW 视图并保留 changeset |
| Runtime | `--executor host\|container\|vm`、`--rootfs`、`--container-image` | 选择执行 Provider 与 rootfs |
| Network | `--overlaynet-deny-all`、`--overlaynet-allow`、`--overlaynet-limit` | 请求 deny、allowlist 或限速策略 |
| Gateway | `--gateway-mode`、`--gateway-route`、`--gateway-level` | 配置路由，并按需捕获模型流量 |
| Limits | `--timeout`、`--memory`、`--max-processes`、`--max-open-files` | 在 Provider 支持时限制 Attempt |
| Configuration | `--spec`、`--name`、`--pass-env` | 提供 RunSpec、身份和显式环境变量 |

Provider 选择不会改变 Run 契约，只会改变各 capability 维度的 enforcement 机制；最终
Evidence 会分维度记录。

## 查看并决定

完成后的 Run 仍然是一个记录，staged effect 必须显式接受或丢弃：

```bash
pvisor review last
pvisor inspect last -- git status --short
pvisor apply last --path src
pvisor apply last --include 'tests/**' --exclude 'tests/generated/**'
pvisor apply last --all
# 或：pvisor drop last
```

`review` 解释 Run Bundle 与 staged change；`inspect` 在 Run 视图中执行只读命令；`apply`
提交选定路径并保留其余内容；`drop` 丢弃 stage。两者都不会修改正在运行的 Run。重置会
创建新的 stage generation，避免旧 metadata 覆盖新的决定。

## Checkpoint 与 Fork

Checkpoint 保证停止一致的 filesystem 与 AgentCtl safe point，但不保存进程内存：

```bash
pvisor checkpoint last --name before-experiment
pvisor fork last --checkpoint before-experiment -- codex
```

Checkpoint 会等待参与的 AgentCtl session 进入 quiesce，记录 upper layer 与 lineage，然后
恢复 Run。

## 可复用环境

`env` 为具名 stage 提供跨命令的稳定生命周期：

```bash
pvisor env create dev --target ./project
pvisor env exec dev -- make test
pvisor env shell dev
pvisor env inspect dev -- git status --short
pvisor env apply dev --path src
pvisor env drop dev
pvisor env delete dev --force
```

Environment 是持久 stage，不是常驻 VM。`start` 与 `stop` 控制是否接受新的 session；
`apply` 与 `drop` 完成决定后会推进 stage generation。

## 配置优先级

`--spec` 接受 TOML `RunConfig` 或准备好的 JSON `RunSpec`。显式 scalar 选项覆盖文件值；
重复的 list 选项替换整个列表；`--` 后的命令替换 `run.command`。`--container-image` 与
`--rootfs` 可以推断匹配的 executor；自动化场景仍建议显式指定 `--executor`。

公共工作流保持简单：启动 Run，检查 Evidence，然后明确决定 staged effect 的去向。Provider
行为见[执行环境](../guides/execution.md)，完整选项见 [CLI 参考](../reference/cli.md)。
