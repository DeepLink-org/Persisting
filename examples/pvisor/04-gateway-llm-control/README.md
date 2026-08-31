# 1.4 Gateway 捕获与管控 LLM 交互

**问题：Agent 使用注入的 OpenAI endpoint 时，Gateway 能否选择 upstream 并记录完整的 request/response 对？可复现结论：2 次 upstream POST、2 次 Gateway sink request、4 个 AgenticMD blocks、0 次失败。**

`run.sh` 启动无需 API key 的 Mock LLM，再通过配置好的 pVisor Gateway 执行两轮 Agent，
并展示 Mock server 日志、Run Bundle 和生成的 AgenticMD。`test.sh` 检查这些产物的指标。
`configs/` 另外保留真实 DeepSeek、多厂商和 allowlist 配置模板。

## Run

```bash
./run.sh
./test.sh  # 执行同一场景并验证预期结果
```

预期：2 次 upstream POST、2 次 Gateway sink request、4 个 AgenticMD blocks、0 次失败。

## Links

- [pVisor examples](../README.md)
- [Capture trajectories](../../../docs/src/pvisor/guides/capture.zh.md)
- [Gateway architecture](../../../docs/src/pvisor/design/gateway.zh.md)
