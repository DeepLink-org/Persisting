# 1.3 pVisor OverlayNet 网络策略

这个例子回答两个问题：怎样声明 Agent 的代理出口策略，以及这些策略的真实安全边界
是什么。它启动一个本地 HTTP server，然后执行四个相互独立的 Run；不需要访问外网。

```bash
./run.sh
```

需要已构建的 `target/release/pvisor`，以及 `python3`、`curl`、`jq`、`awk`。生成的 Run
Bundle 和日志位于 `.work/network-policy/`。

## 四个可运行场景

| 场景 | 写法 | 看到的行为 |
|---|---|---|
| allowlist | `--overlaynet-allow 127.0.0.0/8:19111` | 目标端口允许，同一 IP 的其他端口以 `port-not-allowed` 拒绝 |
| public + deny | `--overlaynet-deny 127.0.0.1:19111` | public 默认允许，但显式 deny 始终优先 |
| 结构化策略 | `--config advanced-policy.toml` | hostname 私网 opt-in、HTTP transport、全局和 CIDR 限速同时生效 |
| deny-all | `--overlaynet-deny-all` | 代理流量以 `no-network` 拒绝；显式直连仍能访问 server |

每个 case 使用 shell xtrace（`set -x`）自动回显实际执行的 pVisor 命令和
`curl-checks.sh` 发出的 curl 命令，随后打印 curl 测试结果和最终 case 结果。策略拒绝
会看到类似下面的输出，而不是只靠 counters 推断：

```text
persisting-overlaynet: egress to `127.0.0.1` denied (explicit-deny)
curl: (22) The requested URL returned error: 403
HTTP 403
curl exit: 22
CURL TEST RESULT: PASS (expected deny, exit=22, HTTP=403, reason=explicit-deny)
CASE 2 RESULT: PASS — Public 模式下 explicit deny 优先生效。
```

随后的一行 Bundle 摘要用于交叉验证最终 mode、边界强度和计数，例如：

```text
Bundle 摘要：mode=public, strength=cooperative, allowed=0, denied=1, failures=0
```

完整策略快照仍保留在各场景的 `run-bundle.json` 中。

这些 PASS 是 [`curl-checks.sh`](curl-checks.sh) 和外层脚本自动断言的结果，不是固定打印的说明文字：

- HTTP 状态、curl exit code、策略原因、响应内容和限速耗时都会被检查；
- 任一检查失败，脚本立即打印 `[FAIL]` 并以非零状态退出；
- 只有四个场景全部通过，末尾才会打印：

```text
OVERALL: PASS (4/4 cases, exit code 0)
```

## CLI：从最常用的三种意图开始

只允许列出的出口。第一次出现 `--overlaynet-allow` 时，CLI 自动选择 allowlist：

```bash
pvisor run --workspace /tmp/run \
  --overlaynet-allow api.openai.com:443 \
  --overlaynet-allow '*.pypi.org:443' \
  -- agent-command
```

保持 public 默认，但封禁明确目标；deny 的优先级高于 allow：

```bash
pvisor run --workspace /tmp/run \
  --overlaynet-deny 169.254.0.0/16 \
  --overlaynet-deny 127.0.0.0/8 \
  -- agent-command
```

拒绝全部 forward-proxy 出口：

```bash
pvisor run --workspace /tmp/run --overlaynet-deny-all -- agent-command
```

三类参数都可以重复。`HOST[:PORT]` 支持精确 hostname、`*.suffix`、IP 和 CIDR；IPv6
带端口时写成 `[2001:db8::1]:443`。

## TOML：端口、transport、私网解析和叠加限速

[`advanced-policy.toml`](advanced-policy.toml) 展示完整结构化形式：

```toml
[overlaynet]
mode = "proxy"
policy = "allowlist"

[[overlaynet.rules]]
host = "localhost"
ports = [19111]
transports = ["http"]
allow_private_ips = true

[[overlaynet.deny]]
host = "169.254.0.0/16"

[[overlaynet.limits]]
bytes_per_second = 1000000

[[overlaynet.limits]]
host = "127.0.0.0/8"
port = 19111
bytes_per_second = 4000
```

空的 `ports` 或 `transports` 表示该维度不限制。hostname 默认只能解析到公网地址；只有
确实需要访问内网服务时才设置 `allow_private_ips = true`。link-local、multicast 和其他
特殊用途地址仍需显式 IP/CIDR 规则。

所有匹配的限速规则都会应用，所以全局规则和目标规则可以叠加，实际效果由更严格的
调度约束决定。CLI 接受 `10mbps`（bit/s）和 `2mb/s`（byte/s），例如：

```bash
--overlaynet-limit 10mbps \
--overlaynet-limit 'api.openai.com:443=2mb/s'
```

HTTP 明文绝对 URI 对应 `http`，HTTPS 绝对 URI 对应 `https`；普通 HTTPS proxy 使用
CONNECT，因此结构化 transport 通常写 `tcp_tunnel`。

## 必须理解的边界

当前 OverlayNet 是显式 HTTP/HTTPS proxy，不是透明 network namespace：

- pVisor 会注入 proxy 环境变量，合作的 HTTP 客户端会经过策略；
- 删除 proxy 设置、设置 `NO_PROXY` 或直接创建 socket 可以绕过；
- DNS/UDP 不在当前驱动覆盖范围内；
- Run Bundle 的 `network_non_bypassable` 因此始终为 `false`。

第四个场景故意同时执行代理请求和直连请求，让这个边界可以亲眼验证。需要不可绕过的
网络隔离时，不应把当前 cooperative proxy 的 allowlist/deny-all 当作安全沙箱。
