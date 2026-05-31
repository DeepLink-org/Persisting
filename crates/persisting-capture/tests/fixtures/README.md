# LLM 测试数据（fixtures）

本目录大部分文件来自 [agentgateway](https://github.com/agentgateway/agentgateway) 的 LLM 协议转换回归测试集，供 `persisting-capture` 的 conversion 与 capture 测试使用。

## 来源

| 项目 | 路径 |
|------|------|
| **上游仓库** | https://github.com/agentgateway/agentgateway |
| **上游目录** | `crates/agentgateway/src/llm/tests/` |

手动更新时，将上游该目录内容复制到本目录（**保留** [`local/`](local/README.md) 子目录，勿覆盖）。

## 目录结构

与 agentgateway 保持一致：

```
fixtures/
├── requests/          # 各协议请求样例（.json）及期望转换 snap（.*.snap）
│   ├── completions/
│   ├── messages/
│   ├── responses/
│   ├── embeddings/
│   ├── count-tokens/
│   └── policies/
├── response/          # 各协议响应样例与跨协议 snap
│   ├── completions/
│   ├── anthropic/
│   ├── bedrock/
│   ├── responses/
│   └── detect/
└── local/             # Persisting 自有 fixture（非 AG），见 local/README.md
```

- **`.json`** / 无后缀文本：原始 wire 输入（请求或上游响应）。
- **`.snap`**：agentgateway 用 [insta](https://github.com/mitsuhiko/insta) 生成的期望输出；YAML frontmatter（`---` … `---`）后为 JSON 或 SSE 正文。

## 在本项目中的用途

| 测试 | 说明 |
|------|------|
| `tests/ag_fixture_tests.rs` | 对照 AG snap 做 messages/responses ↔ completions 转换；capture 用户/助手/usage 矩阵（见 `tests/support/ag_capture_cases.rs`） |
| `tests/llm_fixtures.rs` | 基础 smoke 回归 |
| `src/conversion/*`、`src/storage/dialogue_extract/*` | 部分单元测试通过 `include_str!` 引用本目录 |

Persisting 仅实现 **Messages ↔ Completions**、**Responses ↔ Completions** 两条 bridge；AG 中的 Bedrock、Vertex、Gemini native、Detect 等 snap 作为参考保留，未必全部有对应实现。

## 致谢与许可

测试数据源自 **agentgateway** 项目及其贡献者。agentgateway 以 [Apache License 2.0](https://github.com/agentgateway/agentgateway/blob/main/LICENSE) 发布。

在此复制并用于回归测试，遵循 Apache-2.0 对再分发与修改的要求。若上游 fixture 有更新，请同步复制并保留本 README 中的来源说明。

```
Copyright agentgateway contributors
SPDX-License-Identifier: Apache-2.0
```
