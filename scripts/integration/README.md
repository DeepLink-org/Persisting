# CLI 集成测试

| 脚本 | 说明 |
|------|------|
| [`traj_e2e.sh`](traj_e2e.sh) | **`traj` 读写链路**：gateway `import` → `stats` → `judge --score` → `stats` / `judge-stats` |
| [`capture_integration.sh`](capture_integration.sh) | `persisting traj`：proxy 守护进程、admin/list、mock 代理、`traj import` |
| [`capture_stress.sh`](capture_stress.sh) | 实时写入压测：并发请求、延迟分位、Vortex 行数、append 失败日志 |
| [`capture_run_e2e.sh`](capture_run_e2e.sh) | **`traj capture -f vortex`**（或 `bin`）：mock LLM + 多轮 agent，Vortex drain → materialize → vortex replay |
| [`capture_run_agent.py`](capture_run_agent.py) | 在 `traj capture` 子进程内调 `OPENAI_BASE_URL` |
| [`mock_llm_api_server.py`](mock_llm_api_server.py) | 可记录请求的 mock LLM API（上游） |

```bash
just capture-integration
# 等价
cargo build -p persisting-cli
./scripts/integration/capture_integration.sh
```

**`traj` 子命令集成**（import → stats → judge → stats，无需 mock 代理）：

```bash
just traj-e2e
```

**统一测试选单**（交互或指定套件名）：

```bash
just test-suite          # 交互选单
just test-suite list     # 列出全部套件
just test-suite gate     # 直接运行
just regression          # 全部 shell + capture Rust 回归
```

环境变量：`PERSISTING_CLI`、`PERSISTING_ENGINE_LIB`（Vortex 写入时需要）、`SKIP_BUILD=1`、`PERSISTING_BUILD_PROFILE=release`。

`capture_run_e2e.sh` 额外变量：`TURNS`、`DRAIN_SEC`、`CAPTURE_FORMAT`（`vortex` 或 legacy 别名 `bin`）。

压测（需 `persisting-engine`）：

```bash
just capture-stress
REQUESTS=200 CONCURRENCY=20 MIN_SUCCESS_RATE=0.99 just capture-stress
```

`capture_stress.sh` 额外变量：`DRAIN_SEC`、`MIN_ROW_RATIO`、`MAX_P99_MS`；`traj proxy` 日志写入临时目录便于统计 `trajectory append failed`。

**`traj capture` 全链路**（`-f vortex`：mock 上游 + 多轮 agent，断言 Vortex 行数 + materialize + vortex replay）：

```bash
just capture-run-e2e
TURNS=5 CAPTURE_FORMAT=bin just capture-run-e2e
```
