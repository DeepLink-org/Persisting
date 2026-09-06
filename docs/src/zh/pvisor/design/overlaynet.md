# OverlayNet 透明拦截

本文负责拦截机制、强制缺口与验收门。用户策略流程属于
[网络指南](../guides/network.md)；能力模型属于
[Capability 与 Evidence](../concepts/capabilities-and-evidence.md)。

!!! note "Target architecture"
    文首描述的 libkrun VM driver 已经实现。Design A、Design B 以及交付计划第 1–5
    项描述的是目标 host/container 拦截与验收门，不是当前公开能力。

## 已实现的 VM driver

libkrun VM Attempt 在 `[overlaynet].mode = "auto"` 时使用 `vm-smoltcp`。
guest virtio-net 设备通过 libkrun 带长度前缀的 UnixStream Ethernet 传输连到
pVisor。pVisor 提供 DHCP（`192.0.2.1` 路由器、`192.0.2.2` guest）、合成 DNS
（`198.18.0.0/15`，每个 Attempt 稳定）以及 IPv4 TCP。SYN 会在 smoltcp 中暂停，
直到 hostname/IP、解析后的地址或 scoped host connector alias、端口以及注入的
Control 策略全部授权，并且 host 连接成功。TSI 保持关闭，因此不存在绕过该
data plane 的 guest 路径。

部分 host DNS/TUN connector 会为已授权 hostname 返回不透明的 `198.18.0.0/15`
假 IP。VM connector 只有在逻辑 hostname 与端口通过策略和 Control 授权后才接受
该结果；同一范围内的 guest IP 字面量仍然被拦。因为 connector 隐藏了真实最终
地址，IP/CIDR 策略无法检查该 alias 背后的端点。需要最终地址策略的部署应使用
会暴露具体地址的 resolver。

MVP 对通用 UDP、IPv6、ICMP、QUIC、入站连接、virtual/link-local/multicast/
broadcast 目的地，以及耗尽的 flow/DNS 容量，刻意 fail closed。显式 Gateway
capture 是内部 virtual-router 路由；所有普通出口共享同一策略和带宽注册表。
下文描述的 host 与 container 透明拦截仍是后续工作。

> 状态：libkrun VM driver 已在 Linux 和 Apple Silicon macOS 上实现。本文后面
> 描述的 host-process 透明 driver 仍是已接受的设计。Host/container 选择性
> 策略仍使用显式代理；host deny-all 保持现有平台 sandbox 行为。

## 问题

OverlayNet 的 host/container data plane 是显式 HTTP/HTTPS 代理。pVisor 注入
代理环境变量，并对已知 Agent CLI 注入代理配置参数。覆盖因此是 opt-in：任何
忽略代理环境变量的子进程——静态 Go 二进制、原始 socket、清洗环境的
subprocess——都会直接访问网络。这就是 host `ProcessExecutor` 不能声称
enforcement，并拒绝网络 Capability 的 `PolicyMode::Enforce` 的原因。

剩余 host-driver 设计的目标是**完整拦截且占用轻**：无论语言 runtime、链接
方式还是 syscall 纪律，Agent 进程树发出的每个字节都必须经过 pVisor 拥有的
choke point——且不需要 VM、root daemon 或持久提升权限。

关键动作是把拦截点从*约定*（子进程可以忽略的环境变量）移到*子进程无法选择
绕过的一层*。

## Design A（主路径）：无特权 network namespace + 进程内 userspace 网络栈

这是具备条件的 Linux host 上的默认 driver。它镜像 pVisor 文件系统路径的设计：

```text
filesystem: pVisor embeds a FUSE server and IS the child's filesystem
network:    pVisor embeds a userspace TCP/IP stack and IS the child's network
```

### 机制

1. Attempt 子进程以 `CLONE_NEWUSER | CLONE_NEWNET` 派生。在新的 user
   namespace 内创建 network namespace **不需要特权**；namespace 所有者在其中
   持有 `CAP_NET_ADMIN`。
2. 在 namespace 内，setup 代码创建 `tun` 设备，分配 link-local 子网，并安装
   指向它的默认路由。loopback 被拉起，以便 Run 本地服务继续工作。
3. `tun` 文件描述符在 `exec` 之前经 `socketpair` 回传给 pVisor 父进程。此后
   pVisor 拥有整棵进程树的唯一出口路径。
4. pVisor 在 `tun` fd 上运行基于 `smoltcp` 的 userspace 栈。入站 TCP 流在栈
   内终止，通过 `persisting-agentctl` 策略门后在 host 侧重起源。现有
   OverlayNet 代理 / Gateway sink 仍是 LLM capture 路径，保持不变。
5. DNS：栈回答经 namespace `resolv.conf` 通告的虚拟 resolver 地址。查询在
   host 侧解析，从而在任何连接存在之前给出域名级策略点。

### 性质

- **拓扑完整。** libc interposition、静态二进制、原始 syscall 和 fork 出的
  孙进程都在 namespace 内；没有第二条出路。不需要、也不假设子进程配合。
- **运行时零特权。** 无 root、无 setuid helper、无 daemon。唯一的 host 前提
  是无特权 user namespace（`kernel.unprivileged_userns_clone` / 发行版等价
  项）。
- **进程内。** 与嵌入 FUSE 的决策一致：pVisor 不派生 `passt` /
  `slirp4netns` 一类 helper。

### 策略评估点

| 层 | 信号 | 说明 |
|---|---|---|
| DNS | 查询名 | 虚拟 resolver；最便宜的 allowlist 点 |
| L4 | 目的 IP:port | 字面量 IP 流量的最后手段 |
| TLS | ClientHello 中的 SNI | 被动解析，无 MITM，无注入 CA |
| QUIC | Initial 包中的 SNI，或被拦截 | 默认：拒绝 UDP/443 以迫使 TCP fallback |

### 失败与探测

Driver 可用性在 Attempt prepare 时探测。若 user namespace 不可用，行为取决于
请求的策略模式：

- `PolicyMode::Observe`：回退到显式代理 driver，并在 implant plan notes 中
  记录降级。
- `PolicyMode::Enforce`：让 Run 准备失败。Enforce 下的降级绝不能静默。

## Design B（受限 fallback）：seccomp user-notify + socket broker

在无特权 user namespace 被关闭的 host 上（加固发行版、部分 container
runtime），第二个 driver 可以在没有 namespace 的情况下强制一组刻意更小的
socket 面。除非未覆盖通道都被拒绝，它不被视为与 netns driver 等价。

### 机制

1. Attempt 子进程安装 seccomp filter，把 `socket`、`connect`、`sendto` 和
   `sendmsg` 路由到 `SECCOMP_RET_USER_NOTIF`。当该 driver 声称 enforcement
   时，`io_uring_setup`、原始 packet socket、namespace 变更和未中介的描述符
   传递都被拒绝。
2. `socket` 被 broker：pVisor 创建 socket，保留同一 open file description
   的副本，并用 `SECCOMP_IOCTL_NOTIF_ADDFD` 注入子进程描述符。
3. 在 `connect` 上，pVisor 复制一次 socket 地址，重新校验 notification
   cookie，评估策略，并通过它保留的描述符执行 `connect`。然后返回真实结果，
   而不允许子进程原来带指针的 syscall 继续。这避免了 check-then-`CONTINUE`
   的 TOCTOU 窗口。
4. 初始 seccomp driver 仅 TCP。未连接的 UDP 会被拒绝，直到 OverlayNet 能安全
   复制并 broker 每个 datagram。DNS 必须走 pVisor 提供的 resolver 路径；否则
   无法从 `connect` 观察到的目的 IP 重建域名 allowlist。

### 性质与注意点

- 覆盖显式 broker 的 socket 族上的静态二进制和原始 syscall。`AF_UNIX` 有单独
  的路径策略；不是一律放行。
- 无 namespace、无 tun、无 userspace 栈——但描述符来源、`SCM_RIGHTS`、UDP、
  DNS 和 `io_uring` 必须全部关闭或中介，该 driver 才能被描述为不可绕过。
- Seccomp 在 `connect` 时看到的是 IP 地址，不是应用最初解析的 hostname。域名
  allowlist 需要中介 DNS、显式代理流量或 SNI 关联；IP/CIDR 策略可以直接强制。
- 按 Attempt 选择；两者都可用时仍优先 Design A。

## Enforcement 与 capture 是分开的层

透明拦截提供 **enforcement**（deny / allowlist）和流量记账。它刻意不解密：

- Enforcement 不需要 MITM CA：中介 DNS 名和已授权目的地址对 VM MVP 已经足够；
  未来的 host netns driver 可以增加被动 SNI 解析。
- LLM payload 的 **Capture** 仍走现有显式代理路径：Gateway 向已知 Agent CLI
  注入代理配置并看到明文。在不可绕过 driver 下，不配合的流量不能离开
  allowlist，但不会被解密。

已知侵蚀：Encrypted ClientHello 最终会隐藏 SNI。当这很重要时，部署在 capture
级可见性的 opt-in MITM CA 与回退到 DNS/IP 级 enforcement 之间选择。这是行业
约束，不是某个 driver 特有的。

## Capability 报告

每个 Attempt 的选择决定是否挂上不可绕过 driver；runtime capability catalog
另行通告 VM 网络支持。每次 Run 记录一份 `InterceptionProfile`，描述 driver、
强度和协议覆盖：

- 当 VM smoltcp、netns 或 seccomp 激活时为 `enforce`；
- 仅有显式代理时为 `observe`。

显式代理基础已经把该 profile 发成 `cooperative`，并发布
intercepted/allowed/denied/CONNECT/HTTP/sink/failure 计数。这些计数证明什么
到达了 OverlayNet；它们不估计被绕过的流量。

诚实不变量得以保持：host `ProcessExecutor` 本身仍然从不声称网络
enforcement；声称由当前 OverlayNet driver 做出，且只有在该 driver 已挂上时
`PolicyMode::Enforce` 才可满足。

## 配置

已实现的公开 mode 选择器有三个值：

```toml
[overlaynet]
mode = "auto"        # auto | off | proxy
policy = "allowlist"

[[overlaynet.rules]]
host = "api.openai.com"
ports = [443]
transports = ["tcp_tunnel"]
```

对 libkrun VM，`auto` 选择 `vm-smoltcp`；`off` 让 VM 离线；`proxy` 被拒绝，
因为它是仅 host/container 的协作 driver。对 host/container Run，显式网络标志
选择 `proxy`；已接受的 `netns` 与 `seccomp` host driver 仍是未来内部候选，
而不是暴露的配置值。`run.json` 记录实际挂上的 driver，因此 `pvisor status`
报告真实 enforcement 级别。

## 非目标

- macOS 透明*选择性*拦截（Network Extension、基于 pf 的 UID 路由）。选择性
  策略仍是 observe 级；deny-all 是单独的 Seatbelt 强制边界，并按此报告。
- eBPF（`cgroup/connect4`）driver。优雅，但需要 CAP_BPF/root 以及 setup-host
  部署模型；目前不在范围内。
- 默认 TLS 解密。MITM 始终是显式 opt-in（如果将来有）。

## 交付计划与验收门

本文开头描述的 VM milestone 已经完成。下面剩余计划适用于透明 host/container
拦截。

0. **显式代理基础（已实现）：** 诚实的 cooperative profile、拦截计数、严格
   CONNECT 解析、connect-before-200、流式转发、动态 hop-header 剥离、redirect
   再校验、无隐式 loopback 或 Gateway-upstream 出口信任、结构化
   host/IP/CIDR + port + transport 规则、DNS 后地址授权，以及钉住已授权目的
   地以关闭策略/connector DNS 竞态。
1. **Driver 探测与选择：** 在已实现的公开 `off | proxy | auto` 选择器上扩展
   内部 netns/seccomp 探测；在子进程 exec 前记录所选 profile。若没有不可绕过
   profile，`Enforce` fail closed。
2. **Netns TCP + DNS 最小集：** spawn plumbing、tun 交接、TCP relay、中介
   resolver、DNS/IP allowlist、进程树与 namespace 逃逸测试。在原始 syscall
   和环境被清洗的孙进程被证明包含之前，不得声称 enforcement。
3. **协议闭合：** SNI 策略、字面量 IP 行为、UDP 策略、被拦 QUIC fallback、
   `AF_UNIX`、raw/netlink socket、`SCM_RIGHTS`、namespace 变更以及
   `io_uring` 符合性用例。
4. **受限 seccomp fallback：** 先 broker TCP socket；拒绝未覆盖通道。只有在
   描述符和 datagram 语义有专门测试后才加入 UDP/DNS。
5. **运维：** 把最终计数和降级原因持久化到 `run.json`，通过 `pvisor status`
   暴露，并分别对 Python、Node、Rust、静态 Go 以及 fork 出的孙进程做
   proxy/netns/seccomp 模式 benchmark。

## 相关文档

- [网络指南](../guides/network.md)：为一次 Run 配置并检查策略。
- [隔离架构](isolation.md)：比较完整的 provider 边界。
- [Gateway 架构](gateway.md)：网络层之上的模型路由与 capture。
- [安全与 Evidence](../../system-design/security-evidence.md)：跨产品解读
  enforcement 声称。
