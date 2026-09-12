# SandboxReplay

SandboxReplay 是 pVisor 的 Agent 轨迹回放能力。它面向用户已经创建好的新沙箱，重新执行原始轨迹中恢复边界之前的工具调用，用新沙箱产生的 observation 重建 Agent 原生上下文，然后从边界后继续运行。它会在
[Run 与 Attempt 模型](../concepts/run-model.md)中创建派生 Run。

## 1. 基本概念

一条 Agent 轨迹可以表示为：

~~~text
任务 → A1 → O1 → ... → AN → ON → A(N+1) → ...
~~~

- `Ai`：模型产生的第 i 个动作，可包含可见文本、reasoning 和一个或多个工具调用；
- `Oi`：执行 `Ai` 后返回给模型的 observation；
- `N`：选定的恢复边界；
- `A(N+1)`：原始轨迹在恢复边界后的下一次模型回复；
- `A′(N+1)`：在新沙箱回放前 N 个动作后，模型生成的第一次续跑回复。

如果一个 assistant response 包含多个并行工具调用，它们属于同一个动作批次，恢复边界不能位于批次内部。

## 2. 回放语义

SandboxReplay 依次完成两件事：

1. 在新沙箱中按原顺序重新执行 `A1...AN` 的工具调用，产生当前沙箱真实的 `O′1...O′N`；
2. 保留 Agent 原生 system prompt、工具定义、任务和历史动作，用 `O′1...O′N` 替换旧 observation，然后直接请求下一次模型推理。

第一次续跑请求必须准确结束在 `O′N`：

~~~text
Agent 原生 system prompt + 工具定义 + 原始任务
    + A1 → O′1 → ... → AN → O′N
                              ^ 请求在这里结束
~~~

`O′N` 后不得添加“请继续”“Continue from where you left off”等额外消息。回放保证恢复流程和消息边界正确，但不保证 `A′(N+1)` 与 `A(N+1)` 逐字一致；文件状态、工具输出中的动态字段以及模型采样都可能改变下一动作。

如果用户明确希望改变边界后的第一次推理，可以配置
`boundary_user_prompt`：

~~~text
Agent 原生 system prompt + 工具定义 + 原始任务
    + A1 → O′1 → ... → AN → O′N
    + boundary_user_prompt → A′(N+1)
~~~

该提示词只在 `O′N` 之后、第一次实时模型推理之前注入一次，不替换原始任务。
`prepare-only` 和 `replay-only` 不发起实时模型请求，因此不会注入。未配置时仍保持
“请求准确结束在 `O′N`”的原有语义。

## 3. Agent 适配

### 3.1 Claude Code

Claude Code 使用原生 JSONL/UUID session。SandboxReplay 解析活动 parent UUID 链，重放前 N 个完整工具批次，将新 observation 写入重建 session，然后通过 `claude --resume` 启动续跑。

Claude Code 的 resume transport 会在模型请求前插入临时消息，因此 SandboxReplay 启动一个仅供本次续跑使用的本地协议桥。该桥验证并删除准确匹配的临时 envelope，使第一次模型请求仍结束在 `O′N`。它不启用 pVisor Gateway，也不捕获或持久化模型流量。

异步子 Agent 的 `Agent` 与 `TaskOutput` 被视为 Claude Code 原生工具。恢复边界必须选在能够由原生 session 无歧义重建的位置。

### 3.2 OpenHands

OpenHands 使用原生 event trajectory 和 ReplayManager。SandboxReplay 提取前 N 个 Action，OpenHands Runtime 在新沙箱中执行这些 Action 并产生新的 Observation；replay queue 耗尽后，OpenHands 直接发起续跑请求。

### 3.3 mini-swe-agent

mini-swe-agent 使用原生 `mini-swe-agent-1.1` messages。配套 runner 保留原始 system 和任务消息，在新沙箱中重放前 N 个 action，并使用原生 observation formatter 将结果加入 `agent.messages`，随后直接调用下一次 `agent.step()`。

### 3.4 Pi agent

Pi agent 适配固定支持 `@earendil-works/pi-coding-agent` `0.83.0`，输入为
Pi 原生 RPC event JSONL。一个 replay step 对应一个完整的 `turn_end` 工具批次。
SandboxReplay 使用 Pi 自身的工具实现重新执行 `read`、`bash`、`edit`、`write`，
将新 observation 写入新建的 Pi v3 session，再通过 Pi SDK 从边界续跑。轨迹包含
这四种工具之外的调用时会拒绝执行，避免静默改变工具语义。未配置边界提示词时调用
Pi 的原生 `continue()`；配置后则在 `O′N` 后通过 `prompt()` 追加一次用户消息。

### 3.5 OpenCode

OpenCode 适配固定支持 `1.17.7`，输入为 `opencode run --format=json` 产生的原生
事件 JSONL。`user`、`step_start`、`text`、`reasoning`、`tool_use` 和
`step_finish` 事件会按 step 分组；一个 replay step 对应一个完整的工具调用批次。
SandboxReplay 在新沙箱中重新执行命令型工具以及 `read`、`write`、`edit` 文件工具，
用新的 `state.output` 重建前缀，并通过 `opencode run --format=json --session` 从边界
继续。工具执行期间的中间 `tool_use` 状态会合并，避免同一调用被重复回放。

`opencode run` 没有 CLI 步数参数，且 1.17.7 在 resume 会话上不执行
`agent.build.steps` 配置预算。SandboxReplay 因此为 OpenCode 引入了两个外部看门狗
（对其它 Agent 不启用），详见下文「OpenCode 看门狗」。

#### OpenCode 看门狗

OpenCode 是目前唯一既以黑盒 CLI 形式续跑、又缺乏可靠停止机制的 Agent：
Claude Code 的 `max_turns`、Codex 的 `agent_max_steps`、mini-swe-agent 的
`step_limit` 都在 resume 上原生生效，mini-swe 与 Pi 的循环本身由 pVisor 驱动；
而 OpenCode 两头都不占。看门狗由 `run_process` 的两个监督条件实现，仅对
OpenCode 续跑启用。

**为什么必须引入（两个实测问题）**

1. **resume 会话无视步数预算（上游缺陷）**。`agent.build.steps` 在全新会话上
   原生生效，但用 `opencode import` + `run --session` 恢复的会话完全忽略该
   配置。绕开 pVisor 的裸实验可直接复现：新会话设 `steps=2` 恰好 2 步停；
   resume 会话设 `steps=3` 连跑 32 步以上不停。pVisor 已把剩余预算
   （`max_steps - after_step`）写入隔离配置，但该值目前不被读取。贪心采样下
   模型还会陷入无限循环（实测单回合刷 24 万 token、本地循环 200+ 回合），
   若无外部干预，续跑会一直占用沙箱直到外层 agent 超时（小时级）。
2. **续跑进程偶发静默僵死**。实测抓到过：CLI 进程存活但无网络连接、无工具
   子进程、事件循环空转，既不推进也不退出。根因在 OpenCode/Bun 一侧，且不会
   自行恢复；同样会耗尽整个 agent 超时窗口。

**如何解决（两个看门狗的机制）**

| 看门狗 | 触发条件 | 动作 |
|---|---|---|
| 步数看门狗 | stderr 进度日志中的回合行数达到剩余预算 | SIGINT 优雅终止 |
| 空闲看门狗 | stdout/stderr 连续 10 分钟无任何输出（僵死特征） | 终止进程组 |

步数看门狗的信号源经过专门筛选。OpenCode 的 stdout JSONL 事件流被 Bun 运行时
按 ~8KB 块缓冲（管道、PTY、文件重定向均非实时），不能作为计数源；`--print-logs`
输出到 stderr 的进度日志实时流式，且每个模型回合固定产生一行
`message=loop ... step=N`。续跑命令因此固定附加 `--print-logs`，pVisor 实时统计
该行数，达到剩余预算即向进程组发 SIGINT——SIGINT 下 OpenCode 会优雅退出并刷出
缓冲的全部事件；若进程被更强信号杀死导致 stdout 缓冲丢失，则从 OpenCode 的
sqlite 会话库重建续跑事件（任务沙箱自带 python3，无需新增依赖），续跑轨迹不丢。

终止结果如实上报：步数看门狗触发时 `agent_status` 为 `max_steps`，结果 metadata
携带 `opencode_step_budget` 标记（`enforced_by: pvisor_event_watchdog`）。
隔离配置中的 `agent.build.steps` 仍然保留：一旦上游修复 resume 会话读取该配置，
原生预算将直接生效，看门狗自动退化为兜底保险。

### 3.6 Codex

Codex 适配固定支持 CLI `0.149.0`，输入为 Codex 原生 rollout JSONL。一个 replay step
对应一组完整的 `function_call`/`custom_tool_call` 及其 outputs。SandboxReplay 在新沙箱
中重放前缀工具，将原生 `response_item` 前缀写入隔离的 `CODEX_HOME`，再执行
`codex exec resume <session-id> --json`。native session ID 从 `session_meta` 自动提取；
未知工具或缺少 native session ID 时显式失败，不会退化成全新会话。

Codex 保留自身的 system prompt、工具定义和任务上下文，SandboxReplay 只替换边界前的
observation。为兼容需要 resume prompt 的旧版 CLI，SandboxReplay 使用本地 Responses
bridge 删除 transport nonce，再将请求转发到模型服务；续跑结束后也会从 native trajectory
中清理 nonce。默认模式的输入条件为 `replayed_boundary_only`，不会向模型注入额外提示词。

配置 `boundary_user_prompt` 时，提示词只在 `O′N` 后注入一次并保留；此时输入条件标记为
`boundary_user_prompt_appended`。bridge 校验失败会拒绝续跑，不降级为直连。

## 4. 使用方式

### 4.1 安装 pVisor

pVisor CLI 随 Persisting wheel 发布。沙箱内有 Python 3.10 或更高版本时，推荐直接
安装发布版：

~~~bash
python -m pip install persisting

command -v pvisor
pvisor --version
pvisor replay --help
~~~

如果需要测试尚未发布的 SandboxReplay 代码，可以在目标沙箱中从源码只安装 pVisor：

~~~bash
git clone https://github.com/DeepLink-org/Persisting.git
cd Persisting
cargo install --locked \
  --path crates/persisting-pvisor \
  --bin pvisor

export PATH="${CARGO_HOME:-$HOME/.cargo}/bin:$PATH"
pvisor replay --help
~~~

开发时也可以不安装，直接构建并使用仓库内二进制：

~~~bash
cargo build --release -p persisting-pvisor --bin pvisor
./target/release/pvisor replay --help
~~~

`pvisor replay` 通常直接运行在用户已经创建的新沙箱中，因此 pVisor 必须安装在该
沙箱内，或者以只读方式挂载到沙箱的 `PATH`。如果在沙箱外构建后复制二进制，构建机
与目标沙箱的操作系统、CPU 架构和动态链接运行时必须兼容。Agent runtime 也必须与
所选 replay profile 的固定版本一致。

### 4.2 CLI 与 TOML

SandboxReplay 默认假设用户已经创建了一个新沙箱，并在沙箱中直接运行：

~~~bash
pvisor replay \
  --agent claude-code \
  --trajectory /input/session.jsonl \
  --after-step 30 \
  --agent-entrypoint /usr/bin/claude \
  --boundary-user-prompt '请检查新的 observation 后继续任务'
~~~

Pi agent runtime 安装在 `/opt/pi-agent` 时，命令为：

~~~bash
pvisor replay \
  --agent pi-agent \
  --trajectory /input/pi-agent.events.jsonl \
  --after-step 30 \
  --agent-entrypoint /opt/pi-agent/bin/pi
~~~

Pi agent 等价的 pVisor TOML 配置为：

~~~toml
[replay]
agent = "pi-agent"
trajectory = "/input/pi-agent.events.jsonl"
after_step = 30
agent_entrypoint = "/opt/pi-agent/bin/pi"
max_steps = 200
disable_thinking = true
~~~

OpenCode 使用原生事件 JSONL：

~~~bash
pvisor replay \
  --agent opencode \
  --trajectory /input/opencode.jsonl \
  --after-step 30 \
  --agent-entrypoint /usr/bin/opencode
~~~

Codex 使用原生 rollout JSONL；SandboxReplay 会自动读取轨迹中的 Codex native
session ID，用户无需手工填写 `session_id`：

~~~bash
pvisor replay \
  --agent codex \
  --trajectory /input/rollout.jsonl \
  --after-step 30 \
  --agent-entrypoint /usr/bin/codex
~~~

两者也可使用同一个 TOML 文件，只需将 `[replay].agent`、`trajectory` 和
`agent_entrypoint` 分别改为 `opencode` 或 `codex`。

Claude Code 等价的 pVisor TOML 配置为：

~~~toml
[replay]
agent = "claude-code"
trajectory = "/input/session.jsonl"
after_step = 30
agent_entrypoint = "/usr/bin/claude"
max_steps = 200
session_id = "task-291-attempt-1"
replay_only = false
disable_thinking = true
boundary_user_prompt = "请检查新的 observation 后继续任务"
~~~

### 4.3 执行模式与结果

- 默认模式会执行选中的前缀，然后启动 Agent 继续运行；
- `--replay-only` 会执行前缀，但在下一次模型请求前停止；
- `--prepare-only` 只校验并构造前缀，不执行工具、不启动 Agent，也不要求 Agent runtime。

`--max-steps` 是包含回放前缀在内的 Agent 动作总预算。例如
`--after-step 30 --max-steps 50` 最多留下 20 个续跑动作。仅回放模式的预算必须覆盖前缀；续跑模式还必须至少留下一个实时动作。

结果协议为 `sandbox-playback.result/v3`：`phase` 为 `prepared`、`replayed`
或 `continued`；`quality` 为 `verified` 或 `degraded`；`agent_status` 区分
`not_started`、`completed`、`max_steps` 与 `failed`。失败结果会保留已经生成的日志和原生轨迹。即使 OpenHands 进程返回 0，只要控制器报告 fatal 状态，结果仍为失败。

成功结果的 metadata 记录边界提示词是否请求和注入，以及字符长度和 SHA-256；
replay journal 不记录提示词明文；Agent 原生的 prepared 或 continued trajectory
可能包含这条 user 消息。对于 Claude Code，内存桥只把提示词加入第一次清理后的
上游请求，不修改重建的原生 session。配置提示词后，
`next-action-comparison.json` 的输入条件为 `boundary_user_prompt_appended`。
此时文本相似度和工具一致性仅供观察，不能解释为相同输入下的 replay 一致性。

迁移说明：旧版非 Claude 配置有时使用 `replay_only = true` 表示只构造前缀、不执行。现在应改为 `prepare_only = true`；v3 的 replay-only 一定会执行选中的前缀，因此需要精确版本的 runtime。无法重新生成的 Claude observation 默认失败，只有显式指定 `--allow-stale-observations` 才会复用并把质量标记为 `degraded`。

`disable_thinking` 也可以通过 `--disable-thinking` 指定。只有显式提供 `--executor`、`--stage`、`--overlayfs-path` 或 `--overlayfs-compose` 等运行参数，或在 TOML 中增加 `[run]`、`[overlayfs]`、`[overlaynet]`，才会在回放外层创建受管的 `pvisor run`。

完整参数见 [`pvisor replay` 命令参考](../reference/cli.md#replay-an-agent-trajectory)。

## 5. Qwen3.6 实测结果

### 5.1 测试设置

- 模型：`Qwen3.6-35B-A3B`；
- reasoning/thinking：关闭；
- 题目：NodeBB（291）、Vuls（666）、qutebrowser（667）；
- Agent：Claude Code、OpenHands、mini-swe-agent、Pi agent、OpenCode；
- 每个 Agent 先在新沙箱中生成原始轨迹，再创建另一个新沙箱，仅使用 pVisor SandboxReplay 续跑；
- OpenCode 录制与续跑均使用贪心采样：OpenCode 不透传采样参数，SweEval 与续跑桥在
  wire 级注入 `temperature=0`、`top_p=1`，thinking 经 `chat_template_kwargs` 关闭；
- 每个沙箱资源：2 CPU、7 GiB 内存、70 GiB 存储；
- `N` 按 Rust 解析器识别出的完整原生工具批次序号统计；原始和续跑总步数按相应
  Agent 原生轨迹中的 turn/action 数统计；
- 文本相似度只比较归一化后的可见文本，不包含 reasoning；
- “工具完全一致”要求工具数量、顺序、名称和 JSON 参数均一致。

### 5.2 完整续跑与下一动作汇总

| Agent | 题目 | N | 原始总步数 | 续跑总步数 | 原始 Reward | 本次 Reward | A′(N+1) 工具完全一致 | 文本相似度 |
|---|---|---:|---:|---:|---:|---:|---|---:|
| Claude Code | NodeBB（291） | 1 | 69 | 101 | 1 | 1 | 是 | 0.85 |
| Claude Code | Vuls（666） | 28 | 76 | 200 | 0 | 0 | 否 | 0.45 |
| Claude Code | qutebrowser（667） | 36 | 46 | 46 | 1 | 1 | 否 | 0.48 |
| OpenHands | NodeBB（291） | 28 | 62 | 68 | 1 | 0 | 是 | 1.00 |
| OpenHands | Vuls（666） | 25 | 43 | 43 | 1 | 1 | 是 | 1.00 |
| OpenHands | qutebrowser（667） | 17 | 36 | 87 | 1 | 1 | 否 | 0.13 |
| mini-swe-agent | NodeBB（291） | 48 | 104 | 115 | 1 | 1 | 否 | 1.00 |
| mini-swe-agent | Vuls（666） | 39 | 94 | 91 | 1 | 1 | 是 | 1.00 |
| mini-swe-agent | qutebrowser（667） | 31 | 115 | 65 | 1 | 1 | 否 | 0.77 |
| Pi agent | NodeBB（291） | 54 | 111 | 92 | 1 | 1 | 是 | N/A |
| Pi agent | Vuls（666） | 38 | 77 | 62 | 1 | 1 | 否 | 1.00 |
| Pi agent | qutebrowser（667） | 37 | 71 | 91 | 1 | 1 | 否 | 0.39 |
| Codex | NodeBB（291） | 37 | 74 | 88 | 1 | 1 | 否 | 0.42 |
| Codex | Vuls（666） | 28 | 57 | 69 | 1 | 1 | 否 | 0.47 |
| Codex | qutebrowser（667） | 27 | 54 | 43 | 1 | 1 | 是 | 0.97 |
| OpenCode | NodeBB（291） | 20 | 40 | 44 | 1 | 1 | 否 | 0.65 |
| OpenCode | Vuls（666） | 7 | 16 | 31 | 1 | 1 | 否 | N/A |
| OpenCode | qutebrowser（667） | 15 | 31 | 39 | 1 | 1 | 是 | 0.76 |

Claude Code / NodeBB 使用 `N=1`，以避开异步子 Agent 完成后的 Resume Transport canonical-prefix 歧义。边界后的原始可见文本非空，且 `A′(N+1)` 成功复现同一个 `TaskOutput` 调用。

Pi agent 三题分别在新的任务沙箱中并发执行完成，均未发生模型请求重试，三次
verifier Reward 均为 1。NodeBB 边界两侧的下一动作可见文本均为空，因此文本相似度
按当前指标语义记为 N/A，而不是把两个空字符串报告为 1.00。

Codex 三题使用 `Ornith-1.5-35B-A3B`，原始轨迹来自 r24，续跑轨迹来自最终成功的
r30；续跑使用新的任务沙箱和默认 `replayed_boundary_only` 输入条件。Codex 轨迹中未
发现 transport nonce 或 `Continue from the replay boundary.` 消息。

OpenCode 三题（`1.17.7`）与上表其余 Agent 相同使用 `Qwen3.6-35B-A3B`，贪心采样，
三题并发录制与并发续跑均一次通过，verifier Reward 全部为 1；Vuls（666）与
qutebrowser（667）另复跑一轮续跑，Reward 与下一动作行为一致（可复现）。续跑的
`max_steps=N+25` 由 pVisor 事件看门狗在剩余预算处软停，三题 live 步数均为 24。
无污染在 wire 级验证：首个上游模型请求共 51 项，精确终止于边界观测 `O'N`
（`function_call_output`），传输 nonce 在全部上游请求中出现 0 次，
`boundary_user_prompt` 未注入。OpenCode 关闭 thinking 时常直接调用工具而不输出
可见文本，此时文本相似度按 Pi agent 的同一语义记为 N/A，而不是把两个空字符串
报告为 1.00。

### 5.3 A(N+1) 与 A′(N+1)

以下直接展示原始下一步和续跑下一步。reasoning 已移除；较长的整文件替换只保留修改目标和能够区分调用的关键内容。

#### Claude Code

##### NodeBB（291）

`A(N+1)`：

~~~text
Let me wait for the exploration agent to complete.

TaskOutput:
  task_id="a7e5a5bf351a66db5"
  block=true
  timeout=120000
~~~

`A′(N+1)`：

~~~text
Let me wait for the exploration agent to complete before proceeding.

TaskOutput:
  task_id="a7e5a5bf351a66db5"
  block=true
  timeout=120000
~~~

区别：工具及参数完全一致，仅文本增加了 “before proceeding”。文本相似度为 0.85。

##### Vuls（666）

`A(N+1)`：

~~~text
Now let me search for the `Supported` method in the Debian code:

Read:
  file_path="/app/libmanager/libmanager.go"
~~~

`A′(N+1)`：

~~~text
Now I have a clear picture. Let me also check the gost/debian.go file:

Read:
  file_path="/app/gost/debian.go"
~~~

区别：都是读取文件，但目标分别为 `libmanager/libmanager.go` 和 `gost/debian.go`。文本相似度为 0.45。

##### qutebrowser（667）

`A(N+1)`：

~~~text
All tests pass. Let me verify the implementation meets all PR requirements
by checking the final state of the file:

Read:
  file_path="/app/qutebrowser/browser/webengine/webview.py"
  offset=1
  limit=60
~~~

`A′(N+1)`：

~~~text
All test cases pass. Now let me verify the actual test file runs correctly:

Bash:
  command="python -m pytest tests/unit/browser/webengine/test_webview.py::test_suffixes_workaround_extras_returned -v 2>&1"
  description="Run the suffixes workaround tests"
~~~

区别：原轨迹读取实现文件，续跑执行指定 pytest。文本相似度为 0.48。

#### OpenHands

##### NodeBB（291）

`A(N+1)`：

~~~text
Now let me check if there are any tests already written for
`incrObjectFieldByBulk`:

run:
  command="grep -rn \"incrObjectFieldByBulk\\|incrByBulk\\|ByBulk\" /app/test/ 2>/dev/null"
  is_input="false"
~~~

`A′(N+1)`：

~~~text
Now let me check if there are any tests already written for
`incrObjectFieldByBulk`:

run:
  command="grep -rn \"incrObjectFieldByBulk\\|incrByBulk\\|ByBulk\" /app/test/ 2>/dev/null"
  is_input="false"
~~~

区别：文本、工具和参数完全一致。文本相似度为 1.00。

##### Vuls（666）

`A(N+1)`：

~~~text
Now I have a complete understanding of what needs to be changed.
Let me implement all the required changes:

edit:
  command="str_replace"
  path="/app/gost/debian.go"
  old_str="func (deb Debian) Supported(major string) bool { ... }"
  new_str="func (deb Debian) supported(major string) bool { ... }"
~~~

`A′(N+1)`：

~~~text
Now I have a complete understanding of what needs to be changed.
Let me implement all the required changes:

edit:
  command="str_replace"
  path="/app/gost/debian.go"
  old_str="func (deb Debian) Supported(major string) bool { ... }"
  new_str="func (deb Debian) supported(major string) bool { ... }"
~~~

区别：文本、工具和参数完全一致。文本相似度为 1.00。

##### qutebrowser（667）

`A(N+1)`：

~~~text
Now I have the original file. Let me make the changes properly:

1. First, I'll move the `extra_suffixes_workaround` function to
   module-level (before the `WebEnginePage` class)
2. Then update `chooseFiles` to use the module-level function and be
   callable without instance state

edit:
  command="str_replace"
  path="/app/qutebrowser/browser/webengine/webview.py"
  old_str="acceptNavigationRequest、类内 staticmethod
           extra_suffixes_workaround 以及 chooseFiles"
  new_str="保留 acceptNavigationRequest；chooseFiles 改为调用模块级函数；
           将 extra_suffixes_workaround 移到模块级"
~~~

`A′(N+1)`：

~~~text
Now I understand the structure. I need to:
1. Move `extra_suffixes_workaround` to module-level
2. Update `chooseFiles` to use the module-level function and be callable
   without instance state
3. Keep backward compatibility by assigning the function to the class

Let me make the changes:

edit:
  command="str_replace"
  path="/app/qutebrowser/browser/webengine/webview.py"
  old_str="类内 staticmethod extra_suffixes_workaround 以及 chooseFiles 开头"
  new_str="模块级 extra_suffixes_workaround；
           随后重新声明 class WebEnginePage(QWebEnginePage)"
~~~

区别：都编辑同一文件并尝试移动函数，但替换范围和生成的代码结构不同。文本相似度为 0.13。

#### mini-swe-agent

##### NodeBB（291）

`A(N+1)`：

~~~text
Let me just rewrite the entire function properly:

bash:
  cat > /tmp/rebuild_mongo.js << 'ENDOFFILE'
  # 读取 /app/src/database/mongo/hash.js
  # 用正则提取 incrObjectFieldBy
  # 重新生成 incrObjectFieldByBulk
  # 重写 mongo/hash.js
  ENDOFFILE

  node /tmp/rebuild_mongo.js
~~~

`A′(N+1)`：

~~~text
Let me just rewrite the entire function properly:

bash:
  cat > /tmp/rewrite_mongo_hash.js << 'ENDOFFILE'
  # 读取 /app/src/database/mongo/hash.js
  # 通过括号计数删除已有 incrObjectFieldByBulk
  # 在文件末尾插入新的 incrObjectFieldByBulk
  # 重写 mongo/hash.js
  ENDOFFILE

  node /tmp/rewrite_mongo_hash.js
~~~

区别：可见文本完全一致，但生成脚本、临时文件名和重写策略不同，因此工具参数不一致。文本相似度为 1.00。

##### Vuls（666）

`A(N+1)`：

~~~text
## Change 2: Fix "Unmarshall" to "Unmarshal" in error messages

bash:
  sed -i 's/Failed to Unmarshall/Failed to Unmarshal/g' /app/oval/oval.go
~~~

`A′(N+1)`：

~~~text
## Change 2: Fix "Unmarshall" to "Unmarshal" in error messages

bash:
  sed -i 's/Failed to Unmarshall/Failed to Unmarshal/g' /app/oval/oval.go
~~~

区别：文本、工具和参数完全一致。文本相似度为 1.00。

##### qutebrowser（667）

`A(N+1)`：

~~~text
The tests are failing due to Qt issues. Let me try a different approach -
let me just understand the code and make the changes based on the PR
description. Let me re-read the test file more carefully:

bash:
  cat /app/tests/unit/browser/webengine/test_webview.py |
    grep -A 20 "test_suffixes_workaround_choosefiles_args"
~~~

`A′(N+1)`：

~~~text
The test is failing due to Qt issues. Let me try a simpler approach -
just understand what the test expects and make the code changes.
Let me re-read the test carefully:

bash:
  sed -n '100,140p' /app/tests/unit/browser/webengine/test_webview.py
~~~

区别：都读取同一测试文件附近的内容，但分别使用 `grep -A 20` 和 `sed -n 100,140p`。文本相似度为 0.77。

#### Pi agent

##### NodeBB（291）

`A(N+1)`：

~~~text
[可见文本为空]

bash:
  command="cd /app && node test-bulk-increment.js 2>&1"
~~~

`A′(N+1)`：

~~~text
[可见文本为空]

bash:
  command="cd /app && node test-bulk-increment.js 2>&1"
~~~

区别：工具及参数完全一致；双方可见文本均为空，因此文本相似度为 N/A。

##### Vuls（666）

`A(N+1)`：

~~~text
**4. Fix "Unmarshall" typo in report/cve_client.go:**

bash:
  command="grep -n \"Unmarshall\" /app/report/cve_client.go"
~~~

`A′(N+1)`：

~~~text
**4. Fix "Unmarshall" typo in report/cve_client.go:**

bash:
  command="sed -i 's/Failed to Unmarshall/Failed to Unmarshal/g' /app/report/cve_client.go"
~~~

区别：可见文本完全一致，工具名称均为 `bash`，但命令参数不同。文本相似度为 1.00。

##### qutebrowser（667）

`A(N+1)`：

~~~text
Let me update the test file to call the module-level function instead of the
static method on the class. This is necessary for my implementation:

edit:
  path="/app/tests/unit/browser/webengine/test_webview.py"
  changes="调用模块级 extra_suffixes_workaround；调整 chooseFiles 测试参数和 expected 集合合并"
~~~

`A′(N+1)`：

~~~text
Let me look at the test more carefully and understand what's expected:

read:
  path="/app/tests/unit/browser/webengine/test_webview.py"
  offset=115
~~~

区别：原轨迹直接编辑测试文件，续跑先读取测试文件；工具和参数均不同。文本相似度为 0.39。

#### Codex

##### NodeBB（291），N=37

`A(N+1)`：

~~~text
Both files parse. Now let me write a reproduction script that exercises the real NodeBB Redis DB through `db.incrObjectFieldByBulk` to verify all the requirements.
~~~

工具：`exec_command`，在 `/app` 下创建并执行 `/tmp/repro.js`，验证 bulk increment、
负数递增、空输入、非法输入、危险字段名、原子性、缓存失效和多次累加。

`A′(N+1)`：

~~~text
Both files parse. Now let me update the plan and write a comprehensive test script to verify the Redis behavior against a real instance.
~~~

工具：`update_plan`，前三个实现步骤已完成，当前步骤为“Write reproduction script and
verify behavior”，后续执行 lint 和 hash test suite。

区别：工具和参数不一致，文本相似度为 0.42。

##### Vuls（666），N=28

`A(N+1)`：

~~~text
The build cache is now warm and builds succeed. Let me set up a plan and start implementing the changes.
~~~

工具：`update_plan`，计划从导出 Debian.Supported、修正 Unmarshall 拼写、补充文档注释、
增加 Oracle Linux 分支开始。

`A′(N+1)`：

~~~text
The build takes longer than the 10s poll window due to the large dependency tree. Let me run it in the background and poll the log.
~~~

工具：`exec_command`：

~~~bash
cd /app && export PATH=$PATH:/usr/local/go/bin && \
nohup go build ./gost/ ./scan/ ./oval/ ./report/ > /tmp/build.log 2>&1 &
echo "started $!"
~~~

区别：工具和参数不一致，文本相似度为 0.47。

##### qutebrowser（667），N=27

`A(N+1)`：

~~~text
Now let me run this gold-style verification test.
~~~

工具：`exec_command`：

~~~bash
cd /app && xvfb-run -a env QT_QPA_PLATFORM=offscreen \
QTWEBENGINE_CHROMIUM_FLAGS="--no-sandbox --disable-gpu --disable-dev-shm-usage" \
python -m pytest tests/unit/browser/webengine/test_webview_goldcheck.py -q 2>&1 | tail -20
~~~

`A′(N+1)`：

~~~text
Now let me run the gold-style verification test.
~~~

工具：`exec_command`，命令和参数与原始轨迹完全一致。

区别：工具完全一致，文本相似度为 0.97。

#### OpenCode

##### NodeBB（291），N=20

`A(N+1)`：

~~~text
The tests need Redis running. Let me check if there's a way to start it or if there are unit tests:

Bash:
  which redis-server 2>/dev/null || echo "redis-server not found"
  redis-cli ping 2>/dev/null || echo "Redis not running"
~~~

`A′(N+1)`：

~~~text
The tests need Redis running. Let me check if there's a way to run tests with MongoDB instead, or look at the test structure:

Bash:
  cat /app/config.json 2>/dev/null | head -30
~~~

区别：两边以同一句 "The tests need Redis running. Let me check..." 开头（相同前缀
上下文的强收敛），但探查动作不同：原轨迹检查 Redis 进程，续跑读取应用配置。文本
相似度为 0.65。

##### Vuls（666），N=7

`A(N+1)`：

~~~text
Now let me check if there's an `oracle` type in the scan package that I need to reference:

grep:
  pattern="type oracle struct" path="/app/scan"
~~~

`A′(N+1)`：

~~~text
[可见文本为空白，直接调用工具]

grep:
  path="/app/scan/*.go"
grep:
  pattern="Oracle" path="*.go"
~~~

区别：工具语义同向——都在第 8 步去 scan 包定位 oracle 类型定义，续跑用两个更宽的
grep 变体覆盖同一目标。续跑侧可见文本为空白（OpenCode 关闭 thinking 后的常见风格），
文本相似度按指标语义记为 N/A。复跑一轮行为一致。

##### qutebrowser（667），N=15

`A(N+1)`：

~~~text
Let me run the tests to see if my changes work:

Bash:
  python -m pytest tests/unit/browser/webengine/test_webview.py::test_suffixes_workaround_extras_returned -v --tb=short 2>&1 | tail -30
~~~

`A′(N+1)`：

~~~text
Now let me run the tests to see if they pass:

Bash:
  python -m pytest tests/unit/browser/webengine/test_webview.py::test_suffixes_workaround_extras_returned -v --tb=short 2>&1 | tail -30
~~~

区别：同一条 pytest 命令逐字符一致，仅解说文本措辞不同，文本相似度为 0.76。首轮
续跑的 `A′(N+1)` 可见文本为空白但命令同样逐字复现；两轮续跑 verifier Reward 均为 1。

精确参数见
[`pvisor replay` 命令参考](../reference/cli.md#replay-an-agent-trajectory)；执行边界见
[执行指南](execution.md)。
