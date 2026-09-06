# 排查一次 Run

Run 没有按预期工作时，先查看 Run Bundle，不要立即用更强的参数重复命令。
Bundle 会记录请求的边界、实际安装的机制，以及限制 Run 能够声称的警告。

## 先做三个只读检查

在启动 Agent 的项目目录中执行：

```bash
pvisor status last
pvisor inspect last -- git status --short
pvisor review last
```

`status` 告诉你 Run 仍在运行还是已经停止；`inspect` 在 Run view 中执行只读命令，
可以区分 staged 修改与真实项目中的修改；`review` 展示持久化 Run Bundle 和 Evidence，
帮助你在 apply 或 drop 之前做决定。

## Agent 直接修改了项目

先检查启动命令是否使用 stage：

```bash
pvisor run --stage ./runs/task-001 -- AGENT_COMMAND
```

不使用 stage 时，host executor 仍可能提供 safe-best-effort 控制，但不会产生可以审查和
选择性 apply 的 staged filesystem Effect。如果本来就需要 stage，请检查记录的 stage 路径和
executor warning，再重新执行。

## 请求的 capability 没有被强制执行

把请求参数理解为意图，而不是证据。打开 Run Bundle，查看实际 capability record 与对应机制。
不同操作系统和 executor 的支持范围不同；例如 cooperative network proxy 不能保证所有
ambient connection 都被阻断。

继续阅读[Capabilities 与 Evidence](../concepts/capabilities-and-evidence.md)和[执行环境](execution.md)。

## stage 为空或出现了错误文件

先检查命令工作目录和 stage 路径：

```bash
pvisor inspect last -- pwd
pvisor inspect last -- git status --short
```

Agent 修改的是 Run-owned view。写入该 view 之外的路径可能被记录为 external Effect，也可能
无法被 executor 提供。保持 stage 位于预期项目边界内，不要只按文件名比较生成的 Run 目录与项目根目录。

## capture 或 Dataset 输出缺失

执行审查和轨迹捕获是两个独立决策。先确认 Run 已完成且 Bundle 可读，再使用 pChronicle 检查
capture 配置和目标 Dataset：

```bash
pchronicle ls ./trajectory-data
pchronicle analysis overview ./trajectory-data
```

如果 Dataset 不存在，阅读[捕获 Agent 轨迹](capture.md)，确认目标路径后再启动新的 Run。
本地 Run Bundle 不会自动变成 pChronicle Dataset。

## 提交 issue 前

请提供 pVisor 版本、操作系统、executor、完整命令，以及相关的 `status` 和 `review` 输出，
并移除凭据和私有 workspace 内容。最有帮助的问题描述会说明请求了什么 capability、Bundle
记录了什么机制，以及实际结果在哪里不同。
