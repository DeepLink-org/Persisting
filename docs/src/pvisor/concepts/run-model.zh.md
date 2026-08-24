# Run、Attempt 与 Effect

pVisor 管理的是 **Agent Run**。Run 不是碰巧执行任务的进程，也不是某次执行选择的
Container 或虚拟机。

## Run

Run 是一次 Agent 任务稳定、对用户有意义的身份。它的 identity、请求的 capability、
父子 lineage、已接受 Effect、Artifact 与终态结果不会因为 executor 切换或进程退出而消失。

## Attempt

Attempt 是 Run 在某个 Provider 上的一次物理实现。基础设施故障可以创建新的 Attempt，
而不改变 Run。语义重试代表一次新的决策，因此应创建派生 Run。

```text
Run
├── Attempt 1 → 基础设施失败
└── Attempt 2 → 终态结果
```

这个区分允许基础设施重试，同时让历史仍然可解释。

## Effect

Effect 是 Agent reasoning loop 之外有现实意义的后果。文件修改、网络请求、工具调用、
凭据使用和外部 API 修改属于不同维度。捕获 Effect 不等于阻止 Effect；一个维度被 stage，
也不代表另一个维度已经隔离。

staged workspace 的生命周期是：

```text
execute → inspect stage → apply selected paths zero or more times → drop stage
```

`apply` 只提升选中的文件修改，不表示网络或远程服务 Effect 已经回滚。

## Checkpoint 与终态结果

Checkpoint 记录 Provider 能保存状态的一致性前沿。Run 终态结果记录最终状态，以及 Evidence
和 Artifact 的引用。两者都不应只从进程退出码推断。

下一步阅读 [Capability 与 Evidence](capabilities-and-evidence.md)，再通过
[执行指南](../guides/execution.md)选择 Provider。
