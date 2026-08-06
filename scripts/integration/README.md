# CLI 集成测试

全部实现为 `scripts/integration/*.sh`；`just` 只做薄封装。

| 脚本 | 说明 |
|------|------|
| [`traj_e2e.sh`](traj_e2e.sh) | **轨迹读写链路**：`history import/stats` → `eval judge/stats` |
| [`capture_integration.sh`](capture_integration.sh) | `persisting traj`：proxy 守护进程、admin/list、mock 代理、`traj import` |
| [`capture_stress.sh`](capture_stress.sh) | 实时写入压测：并发请求、延迟分位、Lance 行数、append 失败日志 |
| [`capture_run_e2e.sh`](capture_run_e2e.sh) | **`traj capture -f lance`**（或 `bin`）：mock LLM + 多轮 agent，Lance drain → materialize → lance replay |
| [`capture_run_agent.py`](capture_run_agent.py) | 在 `traj capture` 子进程内调 `OPENAI_BASE_URL` |
| [`mock_llm_api_server.py`](mock_llm_api_server.py) | 可记录请求的 mock LLM API（上游） |

**统一入口：**

```bash
./scripts/test_suite.sh list
./scripts/test_suite.sh smoke
./scripts/test_suite.sh traj-e2e
./scripts/test_suite.sh capture-all
./scripts/test_suite.sh all-integration
```

**或 just 薄封装：**

```bash
just smoke
just integration
just traj-e2e
just capture-integration
just capture-stress
just capture-run-e2e
just capture-all
```

直接跑脚本：

```bash
./scripts/integration/capture_integration.sh
./scripts/integration/traj_e2e.sh
```

环境变量：`PERSISTING_CLI`、`SKIP_BUILD=1`、`PERSISTING_BUILD_PROFILE=release`。

`capture_run_e2e.sh` 额外变量：`TURNS`、`DRAIN_SEC`、`CAPTURE_FORMAT`（`lance` 或 legacy 别名 `bin`）。

压测：

```bash
just capture-stress
REQUESTS=200 CONCURRENCY=20 MIN_SUCCESS_RATE=0.99 just capture-stress
```

`capture_stress.sh` 额外变量：`DRAIN_SEC`、`MIN_ROW_RATIO`、`MAX_P99_MS`。

**`traj capture` 全链路：**

```bash
just capture-run-e2e
TURNS=5 CAPTURE_FORMAT=bin just capture-run-e2e
```
