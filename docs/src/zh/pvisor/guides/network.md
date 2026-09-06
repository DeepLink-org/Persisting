# 使用 OverlayNet 控制网络访问

OverlayNet 让 pVisor 对网络出口执行允许、拒绝和限速规则。Host/container Run 使用进程内
HTTP proxy；libkrun VM Run 使用进程内 smoltcp 数据面处理 IPv4 TCP 和 DNS。请结合
[Capability 与 Evidence 模型](../concepts/capabilities-and-evidence.md)理解这些控制。

!!! warning "安全边界取决于 driver"
    Host/container 的显式 proxy 是 cooperative 的，程序可通过删除 proxy 变量或直接创建
    socket 绕过。VM `auto` 的 virtio-net 终止于 pVisor，因此对整个 guest 进程树不可绕过。
    VM MVP 只支持 IPv4 TCP 和 DNS；UDP、IPv6、ICMP、QUIC 与入站转发都会 fail closed。

## 只允许声明的目标

在 Agent 命令之前传入一个或多个 `--overlaynet-allow`：

```bash
pvisor run \
  --overlaynet-allow api.openai.com:443 \
  --overlaynet-allow pypi.org:443 \
  -- agent-command
```

只要出现 allow 规则，pVisor 就会启用 OverlayNet，并把默认动作切换为拒绝。上例中，
经过代理的流量只能访问两个列出的 HTTPS 目标，其他目标都会被拒绝。

## 选择 driver 模式

使用 `--overlaynet off|auto|proxy` 作为 OverlayNet 的主要开关：

| 模式 | Executor | 边界 |
|---|---|---|
| `off` | 任意 | 关闭 OverlayNet |
| `proxy` | Host/container | cooperative host proxy |
| `auto` | VM（推荐） | 不可绕过的 smoltcp 数据面 |

省略该参数时，策略参数和 Gateway capture 会按 executor 自动推导模式。

## 选择策略

策略参数用于配置已选择的 driver（未显式指定模式时会自动推导）：

| 目标 | 参数 | 对其他代理流量的处理 |
|---|---|---|
| 只允许指定目标 | `--overlaynet-allow TARGET` | 拒绝 |
| 拒绝指定目标 | `--overlaynet-deny TARGET` | 允许 |
| 拒绝全部被接管的出口 | `--overlaynet-deny-all` | 拒绝 |
| 限制带宽 | `--overlaynet-limit [TARGET=]RATE` | 不改变允许/拒绝动作 |

allow、deny 和 limit 参数都可以重复。显式 deny 的优先级高于 allow。
`--overlaynet-deny-all` 是独立策略，不能与其他策略参数组合。

目标可以是精确 hostname、通配后缀、IP 或 CIDR，并可附带端口：

```bash
pvisor run \
  --overlaynet-allow '*.example.com:443' \
  --overlaynet-allow 203.0.113.10:443 \
  --overlaynet-deny 169.254.0.0/16 \
  -- agent-command
```

### 拒绝全部代理流量

```bash
pvisor run --overlaynet-deny-all -- agent-command
```

对于 host/container Run，它会拒绝到达注入代理的 HTTP/HTTPS 请求，但不会禁用 direct
socket，也不会阻止本地 Gateway route。对于 VM `auto`，同一策略会拒绝普通 guest TCP
出口；启用 capture 时，内部 Gateway route 仍可用。

`--overlaynet-deny-all` 不支持再叠加 allow 例外。如果目标是“默认全部拒绝，只允许少数
地址”，不要先写 deny-all，直接声明允许的目标即可：

```bash
pvisor run \
  --overlaynet-allow api.openai.com:443 \
  --overlaynet-allow pypi.org:443 \
  -- agent-command
```

只要存在 `--overlaynet-allow`，pVisor 就会自动采用 allowlist 策略：匹配的目标允许，
其余经过代理的目标默认拒绝。

### 限制带宽

同时设置全局限制和更严格的目标限制：

```bash
pvisor run \
  --overlaynet-limit 10mbps \
  --overlaynet-limit api.openai.com:443=2mbps \
  -- agent-command
```

多个匹配的限制会叠加，最终采用最严格的有效速率。`kbps`、`mbps`、`gbps` 表示每秒
比特数；`kb/s`、`mb/s`、`gb/s` 表示每秒字节数。限速只约束流量，不会授予访问权限。

## 使用结构化规则

当规则需要多个端口、transport 约束，或者 hostname 有意解析到私网地址时，使用 TOML：

```toml
[run]
command = ["agent-command"]

[overlaynet]
mode = "auto" # VM 使用 smoltcp；host/container 使用 "proxy"
policy = "allowlist"

[[overlaynet.rules]]
host = "api.example.com"
ports = [443]
transports = ["tcp_tunnel"]
allow_private_ips = false

[[overlaynet.deny]]
host = "169.254.0.0/16"

[[overlaynet.limits]]
host = "api.example.com"
port = 443
bytes_per_second = 250000
```

运行：

```bash
pvisor run --spec run.toml
```

transport 支持 `http`、`https` 和 `tcp_tunnel`。`ports` 或 `transports` 为空时，表示该
维度不受限制。

hostname 规则默认拒绝解析到私网或 loopback 的地址。有意访问私有服务时，优先使用
明确的 IP/CIDR 规则；也可以在范围足够窄的 hostname 规则上设置
`allow_private_ips = true`。link-local 等其他特殊地址段仍需显式 IP 或 CIDR 规则。

如果 host 使用 DNS/TUN fake-IP connector，VM 出站只会在逻辑 hostname 与 port 已通过
授权后，把 `198.18/15` 结果视为不透明的 connector alias；guest 不能把该网段作为 IP
literal 直接连接。connector 不暴露最终真实地址，因此需要对 hostname 解析结果执行
IP/CIDR 策略时，应使用能返回具体地址的 resolver。

## 理解哪些客户端会被控制

对于 host/container Run，pVisor 会向 Agent 进程注入 `HTTP_PROXY`、`HTTPS_PROXY`、
对应的小写形式和 `ALL_PROXY`。遵守这些设置的 HTTP 客户端会经过 OverlayNet；代理
支持普通 HTTP 转发和 HTTPS `CONNECT` 隧道。

以下路径不在这个 cooperative host/container 策略边界内：

- 客户端忽略或删除代理环境变量；
- 目标被加入 `NO_PROXY`；
- 程序直接创建 socket；
- 不经过 HTTP proxy 的 DNS 和 UDP 流量。

因此 host/container cooperative-proxy Run 会报告
`safety.network_non_bypassable = false`。如果必须
彻底阻止直接联网，使用 `pvisor -- --overlaynet-deny-all`：Linux 会创建私有
network namespace；macOS 会用 Seatbelt 阻断非 loopback IP 与宿主 ambient Unix socket，同时保留
loopback proxy、精确的 AgentCtl 和 Run 私有目录内 IPC。Container Run 也可以使用 `--container-network none`。
两种本地 host 路径上的 selective allow/deny 仍是协作式。VM executor 默认使用
`[overlaynet] mode = "auto"`，由 smoltcp 提供 DHCP、合成 DNS 与受策略控制的 IPv4 TCP；
`mode = "off"` 会让 VM 离线。Gateway capture 通过 guest 虚拟路由器暴露；container
executor 使用进程内 proxy 时仍要求 `--container-network host`。

## 检查运行结果

当前目录默认就是可重复使用的 workspace；每次调用都会在 pVisor 默认记录根目录下保留
一条独立 Run：

```bash
pvisor run \
  --overlaynet-deny 169.254.0.0/16 \
  -- agent-command

pvisor review --json last | jq '{policy: .network.policy,
     interception: .network.interception,
     counters: .network.intercepted,
     non_bypassable: .safety.network_non_bypassable}'
```

这些 counter 描述由当前 OverlayNet driver 处理的流量。它们无法统计绕过 cooperative
host/container proxy 的流量；VM smoltcp profile 在已支持的 TCP/DNS 数据面之外没有
guest 网络旁路。

## 常见问题

| 现象 | 检查项 |
|---|---|
| 已允许的 hostname 解析到 loopback 或私网地址后仍被拒绝 | 使用显式 IP/CIDR，或仅在范围足够窄的结构化规则上设置 `allow_private_ips = true` |
| 使用 `--overlaynet-deny-all` 后请求仍然成功 | 确认客户端遵守注入的 proxy，且没有使用 `NO_PROXY` 或 direct socket |
| pVisor 无法绑定代理端口 | 使用 `--overlaynet-listen 127.0.0.1:19082` 选择一个空闲的非零端口 |
| 容器无法连接代理 | 使用 `--container-network host` |
| VM 的 `proxy` 模式被拒绝 | 使用 `auto` 选择 smoltcp，或使用 `off` 让 guest 离线 |

可以运行
[`examples/pvisor/03-network-isolation`](https://github.com/DeepLink-org/Persisting/tree/main/examples/pvisor/03-network-isolation)
离线复现 allowlist、deny-all 和 direct-socket bypass。需要捕获 LLM 请求或配置模型路由时，
继续阅读 [Capture 指南](capture.md)。
