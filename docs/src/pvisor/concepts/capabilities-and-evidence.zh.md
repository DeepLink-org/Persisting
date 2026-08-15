# Capability 与 Evidence

Capability 是对某个资源和动作的有界权限。pVisor 按维度描述 capability，因为单一的
`safe` 或 `sandboxed` 标签无法准确表达 Agent 环境。

| 维度 | 请求示例 | 应检查的 Evidence |
| --- | --- | --- |
| 文件读取 | 只读项目和工具链中的指定路径 | 可见 root 与实际安装的读取控制 |
| 文件写入 | 只写 staged workspace | 写边界与 promotion 决策 |
| 网络 | 只访问声明的目标 | 截获路径与抗绕过能力 |
| 进程 | 启动受限子进程 | namespace/profile 与继承句柄 |
| 凭据 | 使用一个短期身份 | 交付、过期和实际使用 |
| 工具与模型 | 调用声明的 endpoint | 策略决策与路由记录 |

请求的权限与实际安装的 enforcement 是不同事实。必需维度无法满足时，admission 必须拒绝
该 Provider。可选控制只有在 Run record 明确报告降级时才能弱化。

Evidence 依次回答四个强度不同的问题：

1. **Declared**：请求了什么策略？
2. **Mediated**：哪些动作经过控制点？
3. **Enforced**：在声明的 threat model 中阻断了哪些绕过路径？
4. **Attested**：enforcement 是否绑定到这次 Run 和实际 Provider？

具体执行的答案应从 Run Bundle 检查。执行、编排和历史之间的完整信任链见
[安全与 Evidence](../../system-design/security-evidence.md)；平台机制见
[pVisor 隔离设计](../design/isolation.md)。
