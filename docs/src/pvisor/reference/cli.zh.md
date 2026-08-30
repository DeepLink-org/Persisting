# `pvisor` 命令参考

`pvisor` 是单个 Run 和持久环境的产品命令。
Host、OCI VM 和透明 host-rootfs VM 的完整命令示例见
[使用 pVisor 运行工作负载](../guides/execution.md)。

```text
pvisor
├── run                 execute one Agent Run
├── replay              replay and continue an Agent-native trajectory
├── env                 manage durable reusable environments
├── status              aggregate Run, filesystem, and network state
├── inspect             open a read-only Run view
├── review              review the durable Run Bundle
├── checkpoint          snapshot a stopped transactional upper
├── fork                start a child Run from a logical checkpoint
├── apply               commit a stopped Run's filesystem stage
└── drop                discard a stopped Run's filesystem stage
```

## 安全的第一次运行

```bash
pvisor run --safe codex
pvisor review last
```

`--safe` 把当前目录当作可复用项目工作区和 OverlayFS base，在
`PERSISTING_RUN_HOME`（默认 `~/.persisting/runs`）下创建独立 Run 和可写
stage，保留改动供人工审查，并以 `0600` 写入 `run-bundle.json`。
在 Linux 上，默认 host executor 会在异步 runtime 到达 Agent 之前，通过
pVisor 的 rootless launcher 自执行。User/mount/PID namespace、namespace 内
PID 1 后代回收器、最小 bind-projected root 加 `chroot`、Landlock ABI v3
策略、关闭继承描述符、`no_new_privs` 以及空 capability 集，使工作区约束对
Agent 进程树不可绕过。
`--overlaynet-deny-all` 再加一个私有 network namespace；public/allowlist
代理模式仍是协作式。在 macOS 上，默认 safe host executor 安装生成的
Seatbelt 策略，使 staged 写入不可绕过。对 deny-all Run，它拦截 IP 和
ambient host Unix socket，同时保留精确的 AgentCtl 与 Run 本地 IPC。读取和
选择性网络策略仍是 ambient/协作式，并在 Bundle 中单独标注。Docker 和 KVM
传输保留同样的外层 Run、OverlayFS、AgentCtl 状态观察和 pChronicle 控制面。

完成后：

```bash
pvisor review last
pvisor checkpoint last --name before-experiment
pvisor fork last --checkpoint before-experiment -- codex
pvisor apply last --all # or: pvisor drop last
```

CLI checkpoint 是 stopped-consistent。嵌入式 host 可以调用
`RunHandle::checkpoint`：pVisor 发布 AgentCtl quiesce 指令，要求每个被冻进
checkpoint 的 Session 报告匹配的 quiesced 状态，快照 raw upper，再发布
`continue`。逻辑 checkpoint 保留文件系统和协作客户端 safe-point 边界，不
保留进程内存。

持久环境拥有稳定名称和可复用 OverlayFS upper：

```bash
pvisor env create dev --target ./project
pvisor env exec dev -- make test
pvisor env shell dev
pvisor env inspect dev -- git status --short
pvisor env stop dev
pvisor env start dev
pvisor env apply dev --path src   # 提交选中部分，其余继续 staged
pvisor env apply dev --all        # 提交剩余修改并重置为空 stage
pvisor env drop dev        # 丢弃修改并重置为空 stage
pvisor env delete dev --force
```

默认元数据位于 `~/.persisting/envs`，可用 `--root` 或 `PERSISTING_ENV_HOME`
覆盖。`start` / `stop` 控制是否接受新会话，并不表示常驻虚拟机；每次 `exec` / `shell`
都会挂载同一个 writable upper，所以修改会跨命令保留。`inspect` 使用内核强制的只读视图。
`apply --all` 或 `drop` 不会把 terminal Overlay 原地改回 `staged`；它们会创建单调递增的
Overlay generation。命令取得环境 lease 后会重新读取 generation，避免用 reset 前的
metadata 覆盖新 stage。

## 回放一条 Agent 轨迹

`pvisor replay` 假定调用方已经正常创建了新 sandbox。它通过 `after_step`
回放完整 tool batch，用新鲜 observation 重建所选 Agent 原生上下文，然后
启动 live Agent：

```bash
pvisor replay \
  --agent claude-code \
  --trajectory /input/session.jsonl \
  --after-step 30 \
  --agent-entrypoint /usr/bin/claude \
  --boundary-user-prompt 'Review the fresh observation before continuing.'
```

OpenHands、mini-swe-agent、Pi agent 和 SWE-agent 使用环境中已有的模型端点
和凭据。Pi 要求精确的 `0.83.0` runtime，并接受包含核心 `read`、`bash`、
`edit` 和 `write` 工具的原生 RPC event JSONL。Claude Code 使用
SandboxReplay 拥有的临时 bridge，因为它的原生 resume 传输会插入 wake-up
消息。该 bridge 在转发模型请求前校验并去掉那份精确的 Resume Transport
envelope。它不启用 pVisor Gateway、不捕获模型流量、也不持久化 bridge
审计。

等价的严格 replay TOML 是：

```toml
[replay]
agent = "claude-code"
trajectory = "/input/session.jsonl"
after_step = 30
agent_entrypoint = "/usr/bin/claude"
max_steps = 200
session_id = "task-291-attempt-1"
replay_only = false
disable_thinking = true
boundary_user_prompt = "Review the fresh observation before continuing."
```

Pi 使用同一套 CLI/TOML 面。它的默认 SweEval entrypoint 是
`/opt/pi-agent/bin/pi`，例如：

```bash
pvisor replay --agent pi-agent \
  --trajectory /input/pi-agent.events.jsonl \
  --after-step 30 \
  --agent-entrypoint /opt/pi-agent/bin/pi
```

Replay 有三种模式。默认回放前缀并继续；`--replay-only` 执行前缀并在模型
请求前停止；`--prepare-only` 构造前缀，不执行工具、也不要求 runtime。
`--max-steps` 是包含已回放动作的总动作预算。
`--allow-stale-observations` 是显式的仅 Claude 逃生口，会把 v3 结果标为
`degraded`。

`--boundary-user-prompt TEXT` 在最后一条新鲜 observation 之后、第一次 live
模型推理之前追加一条用户消息。TOML 写法是 `replay.boundary_user_prompt`。
prepare-only 和 replay-only 模式下它不参与推理；省略该选项则保持未修改的
replay 边界。结构化结果和 replay journal 只存储注入状态、长度和 digest；
Agent 原生的 prepared 或 continued 轨迹可以包含这条用户消息。

结果 schema 是 `sandbox-playback.result/v3`，带类型化的 `phase`、`quality`
和 `agent_status` 字段，以及 state/output 位置、artifacts 和可选结构化失败。
原先只用 `replay_only = true` 来构造前缀的非 Claude 调用方必须迁移到
`prepare_only = true`。

`disable_thinking` 属于 `[replay]`，也暴露为 `--disable-thinking`；它由
Claude 协议 bridge 应用，且不会打开 Gateway capture。可选的 `[run]`、
`[overlayfs]` 和 `[overlaynet]` 段会创建外层受管 `pvisor run`；它们不改变
内部 replay 模型路径。

默认情况下，replay 的内部状态、WAL、manifest、新鲜 observation 比较和原生
工作文件留在 `/tmp/pvisor-sandbox-replay`，并随 sandbox 消失。Replay 不启用
pVisor Gateway、pChronicle、模型流量 capture store 或 Claude Resume
Transport 审计。显式选择 `--state-dir` 或 `--output-dir` 的调用方拥有这些
文件。用 `--replay-only` 执行前缀并在 live 推理前停止，或用 `--prepare-only`
在不执行的情况下构造它。

## 一套配置模型

`pvisor run` 只有一份规范 `RunConfig`。TOML 和命令行选项是同一组字段的两种
表示。`--config` 是可选且显式的；pVisor 不会发现隐藏的项目配置文件。

```bash
pvisor run \
  --agent codex \
  --overlayfs-base /path/to/project \
  --overlayfs-backend directory \
  --overlayfs-commit manual \
  --overlaynet-allow api.openai.com:443 \
  --overlaynet-deny 169.254.0.0/16 \
  --overlaynet-limit 10mbps \
  --gateway-mode capture \
  --gateway-level dialogue \
  --gateway-route \
    'name="openai", provider="openai", upstream="https://api.openai.com/v1", api_key_env="OPENAI_API_KEY"' \
  --record-format lance \
  --record-destination ./warehouse \
  -- codex
```

`--record-format lance` 启动 `pchronicle serve --control 127.0.0.1:0 DATASET`；
pVisor 发送共享 `EventRecord` 并等待 durable acknowledgement。本地 JSONL
或 JSON warehouse 归档使用 `--record-format json`。

所有新持久化的记录都同时包含 `timestamp`（RFC3339 UTC）和
`timestamp_unix_ms`（Unix 毫秒）。它们描述同一观测时间，必须在一毫秒内
一致。记录顺序仍由 `source + seq` 定义；时间戳是关联元数据，不是顺序的
事实源。

等价 TOML 是：

```toml
[run]
agent = "codex"
executor = "container"
command = ["codex"]

[container]
runtime = "docker"
image = "example/codex-agent:latest"
network = "host"

[overlayfs]
base = "/path/to/project"
backend = "directory"
commit = "manual"

[overlaynet]
mode = "proxy"
policy = "allowlist"

[[overlaynet.rules]]
host = "api.openai.com"
ports = [443]

[[overlaynet.deny]]
host = "169.254.0.0/16"

[[overlaynet.limits]]
bytes_per_second = 1250000

[gateway]
mode = "capture"
level = "dialogue"

[[gateway.routes]]
name = "openai"
provider = "openai"
upstream = "https://api.openai.com/v1"
api_key_env = "OPENAI_API_KEY"

[chronicle]
mode = "lance"
```

用 `pvisor run --config run.toml` 运行。显式 CLI 标量替换 TOML 标量。提供
任一重复 CLI 字段（`--overlayfs-compose`、`--overlaynet-allow`、
`--overlaynet-deny`、`--overlaynet-limit` 或 `--gateway-route`）会替换该
完整 TOML 列表。`--` 之后的命令替换 `run.command`。

`--container-image IMAGE` 自动选择 OCI container executor；
`--executor container` 让选择显式。传输层解析匹配的静态
`linux-amd64`/`linux-arm64` pVisor，挂进镜像，覆盖 entrypoint，并走普通
`pvisor run --executor host --run-spec ...` 路径。Agent 命令放在 RunSpec
内，而不是暴露在 Docker/Podman argv。注入的 pVisor 创建自己的 AgentCtl 并
返回类型化 RunResult。最终 OverlayFS cwd 和会话 Gateway 配置挂在稳定路径。
用户 mount 是可重复的 TOML inline table，例如：

```bash
pvisor run \
  --container-image example/codex-agent:latest \
  --container-pvisor-binary ./dist/pvisor-linux-amd64 \
  --container-platform linux/amd64 \
  --container-network none \
  --container-mount \
    'source="/host/cache", target="/cache", read_only=false' \
  -- codex
```

进程内 Gateway 和显式 OverlayNet 代理当前要求 `container.network = "host"`，
因为它们注入的地址是 host loopback 端点。关闭这些 driver 时，bridge 和无网络
模式有效。executor 记录 container 隔离，但不声称完整 capability
enforcement。

`--executor vm` 使用静态链接的 libkrun 及其嵌入 init 启动最小 Linux guest。
`--image IMAGE` 选择该 executor，并直接拉取 OCI/Docker 镜像，不调用 Docker、
Podman 或 Buildah。未提供显式 `--vm-rootfs` 时，默认是 `ubuntu:latest`。
manifest 和 layer digest 会被校验，host 架构选择 `linux/arm64` 或
`linux/amd64`，解包后的 rootfs 成为 pVisor OverlayFS 的不可变 lower。
`--image-store` 覆盖平台缓存目录。OCI 缓存目标被标为不可变，且该保护在逻辑
checkpoint/fork 后仍然有效，因此 `pvisor apply` 不能改写被其他 Run 共享的
rootfs。

在 Linux 上，`--host-rootfs` 选择 host `/` 作为 VM rootfs lower，并在省略
`--executor` 时选择 VM executor。它与 `--image` 和 `--vm-rootfs` 互斥，并在
macOS 上被拒绝。这是独立语义选项，而不是 CLI 别名：`--overlayfs-base` 和
`--overlayfs-target` 继续独立选择项目工作区。带 guest 工作区 target 时，
工作区外的写入使用临时 root upper，并在 VM 退出时丢弃；工作区改动使用
durable OverlayFS stage。

合并后的 rootfs 是 guest `/`，`/workspace` 成为 guest cwd。在 Linux 和
macOS 上，vendored libkrun 通过 virtio-fs 直接服务 pVisor 的 rootfs 与
工作区 copy-on-write union。VMM 从不重新导出 host FUSE mount，也不物化或
对账这两棵树。Linux 使用 KVM，Apple Silicon macOS 通过同一 executor 使用
HVF。libkrunfw 随 wheel 安装在 pVisor 旁边。源码构建否则把 pinned 官方
release 下载到经 SHA-256 校验的平台缓存；在 macOS 上 `/usr/bin/cc` 把它的
预构建 kernel bundle 转成所需 dylib。仍可用 `--vm-library-dir` 选择系统
目录。OverlayNet `auto` 使用不可绕过的 VM smoltcp IPv4 TCP/DNS driver，而
Gateway capture 使用经 guest virtual router 的内部路由。Linux 另外用
namespace 和 Landlock 约束 VMM。macOS VMM 仍拥有调用用户的 host 权限，因此
尽管有 guest-kernel 隔离，第一版 OCI-image 也不应被当成敌对多租户边界。

在 host/container 执行上，四个可见 OverlayNet 策略标志和 Gateway capture
会自动启用代理 driver。任一 `--overlayfs-base`、`--overlayfs-compose`、
`--overlayfs-stage`、`--overlayfs-backend` 或 `--overlayfs-commit` 选项会
自动启用 OverlayFS；没有单独的 mode 开关。base 默认是工作区，stage 默认是
生成的 Run 目录。当 stage 嵌在 base 或 compose 层内时，pVisor 从合并视图
隐藏该子树，并拒绝 guest 重建它。libkrun Run 不创建 live host mountpoint，
防止 host indexer 递归进入 `<stage>/merged`。反向拓扑——stage 包含 lower
层——会被拒绝。在 pVisor 能安全物化完整 merged-vs-base diff 之前，组合 Run
拒绝 `commit=apply` 和随后的 `pvisor apply` 命令。
在 host/container 执行上，OverlayNet 策略作用于经显式代理路由的流量，并不
声称不可绕过的 host 网络隔离。在 libkrun VM 上，`auto` 挂上不可绕过的
smoltcp IPv4 TCP/DNS；`off` 让 guest 离线。`--overlaynet-deny-all` 把同一
default-deny 策略交给当前 driver。host/container 直接 socket 仍是 ambient，
而 VM Gateway 路由仍可通过 guest 的 virtual router 用于已配置的模型流量。

## Run 项目发现

当前目录是默认项目关联。启用 OverlayFS 时，`--overlayfs-base` 标识可复用
项目目录。每个 Run 在 `PERSISTING_RUN_HOME` 下获得独立目录。若该根会落在
所选 OverlayFS base 或 compose 层内，pVisor 改用系统临时 Run 根，以保持
可写 stage 分离：

```text
project/                         # reusable workspace / default base

~/.persisting/runs/
└── run-<uuid>/                  # one generated Run and default stage
    ├── run.json
    ├── run-bundle.json          # mode 0600; outcome + safety + changes + effects
    ├── overlay.json             # when OverlayFS is enabled
    ├── upper/                   # or a Run-named Jujutsu workspace upper
    ├── merged/
    ├── checkpoints/
    ├── lease.lock
    ├── control.sock             # while a live OverlayFS Run is available
    ├── .capture/                # when OverlayNet/Gateway is enabled
    └── chronicle/               # default pChronicle location
```

生命周期命令接受 Run id、Run 目录、项目工作区、`run.json`、upper 或 merged
路径。项目工作区选择其最新 Run：

```bash
pvisor status /path/to/project
pvisor inspect /path/to/project -- rg TODO .
pvisor apply /path/to/project --all
pvisor apply /path/to/project --path src --path tests/unit
pvisor apply /path/to/project --include 'docs/**' --exclude 'docs/generated/**'
pvisor apply /path/to/project --target /path/to/another-target --all
pvisor drop /path/to/project
```

`inspect` 创建单独的内核只读视图。`apply` 和 `drop` 拒绝改写 live Run。
过滤后的 apply 是依赖闭合且可重复的：未选路径保持 staged，不透明目录和
hard-link 组保持原子。每个成功 batch 持久化到 `apply-ledger.json`。
overlay 为每个被改写的目标路径记录 durable first-touch fingerprint。若所选
目标路径在 staging 之后发生变化，`apply` fail closed；已准备的 batch 向前
恢复，单个非目录替换用同目录原子 rename 提交。host 文件系统仍不为任意
多文件 batch 提供单一原子提交点。
提交全部剩余改动或丢弃 stage 是终态；`drop` 不能撤销已 apply 的 batch，
`apply` 也不能恢复已丢弃的改动。终态清理删除 `upper`、`work` 和其他一次性
staging 数据，但保留紧凑的 Run/Overlay 元数据、apply ledger 和 pChronicle
历史。

## 相关工作流

- [运行第一个 Agent](../get-started.md)：最短完整闭环。
- [执行环境](../guides/execution.md)：选择 provider。
- [审查并应用 Effect](../guides/review-apply.md)：过滤且可重复的 apply。
- [网络控制](../guides/network.md) 与 [捕获轨迹](../guides/capture.md)：其他
  Effect 维度。
