# CLI 集成测试

| 脚本 | 说明 |
|------|------|
| [`capture_integration.sh`](capture_integration.sh) | `persisting capture`：参数、守护进程、admin/list、mock 代理 |
| [`capture_stress.sh`](capture_stress.sh) | 实时写入压测：并发请求、延迟分位、Lance 行数、append 失败日志 |
| [`capture_run_e2e.sh`](capture_run_e2e.sh) | **`capture run`**：mock LLM + 多轮 agent，校验 replay 含全部对话 |
| [`capture_run_agent.py`](capture_run_agent.py) | 在 `capture run` 子进程内调 `OPENAI_BASE_URL` |
| [`mock_llm_api_server.py`](mock_llm_api_server.py) | 可记录请求的 mock LLM API（上游） |

```bash
just capture-integration
# 等价
cargo build -p persisting-cli
./scripts/integration/capture_integration.sh
```

环境变量：`PERSISTING_CLI`、`PERSISTING_ENGINE_LIB`（import 写 Lance 时需要）、`SKIP_BUILD=1`、`PERSISTING_BUILD_PROFILE=release`。

压测（需 `persisting-engine`）：

```bash
just capture-stress
REQUESTS=200 CONCURRENCY=20 MIN_SUCCESS_RATE=0.99 just capture-stress
```

`capture_stress.sh` 额外变量：`DRAIN_SEC`、`MIN_ROW_RATIO`、`MAX_P99_MS`；`serve` 日志写入临时目录便于统计 `trajectory append failed`。

**`capture run` 全链路**（mock 上游 + 多轮 agent，断言 replay 含每轮 user/assistant）：

```bash
just capture-run-e2e
TURNS=5 just capture-run-e2e
```
