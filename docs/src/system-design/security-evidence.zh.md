# 安全与 Evidence 模型

Persisting 不会把安全压缩成一个 `safe` 或 `sandboxed` 标签。每个 Run 都按 capability
维度报告保证。pVisor 拥有 admission 与 runtime enforcement；pPilot 保存 authority 与
lease generation；配置后的 pChronicle capture 保存 lifecycle fact，且只保存 Gateway 或
lifecycle event record 实际携带的 Evidence。完整 Run Bundle Evidence 清单仍留在本地，
除非另行搬运。

| 维度 | 示例机制 | Evidence 问题 |
| --- | --- | --- |
| 文件读取 | synthetic root、allowlisted projection | 哪些 host path 可见？ |
| 文件写入 | staged OverlayFS、Landlock、Seatbelt | 进程树可以写到哪里？ |
| 网络 | private namespace、virtio-net、proxy policy | direct socket 能否绕过策略？ |
| 进程 | namespace、sandbox profile、继承 FD 清理 | 哪些 descendant 共享边界？ |
| 凭据 | Run-scoped delivery 与 expiry | 哪个 identity 获得并使用 Secret？ |
| Effect | stage、promotion decision、compensation record | 哪些后果进入真实系统？ |

Evidence 可以分为四级：

1. **Declared**：配置请求了一条边界。
2. **Mediated**：Agent-facing 路径经过控制点。
3. **Enforced**：声明 threat model 内的绕过路径被阻断。
4. **Attested**：Enforcement evidence 与精确 Run 和 Provider 绑定。

一个维度的强保证不会自动升级其他维度。Staged workspace 不能证明网络已隔离，捕获到
流量也不能证明未观测 socket 不可能存在。

端到端链路是：

```text
requested capability
  → admission decision
  → installed mechanism
  → provider evidence
  → observed Effect
  → terminal result
  → configured event-carried history
```

最后一段 event 路径比 Run Bundle 更窄：当前不会发布完整的 Artifact、lineage、filesystem
Effect、AgentCtl/network/resource Evidence、output 或 metrics 清单。

用户模型见 [Capability 与 Evidence](../pvisor/concepts/capabilities-and-evidence.md)，平台机制见
[pVisor 隔离设计](../pvisor/design/isolation.md)与 [OverlayNet](../pvisor/design/overlaynet.md)，
历史边界见[事实与 Projection](../pchronicle/concepts/facts-and-projections.md)。
