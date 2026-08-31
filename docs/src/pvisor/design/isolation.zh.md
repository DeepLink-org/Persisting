# pVisor 隔离架构

本文比较各 provider 机制及其真实安全性质。用
[执行指南](../guides/execution.md) 选择 provider，用
[Capability 与 Evidence](../concepts/capabilities-and-evidence.md) 解读保证。

!!! note "Target architecture"
    本文把当前实现与明确标出的目标架构写在一起。Linux `pvisor run --safe`
    实现第 2 节描述的 FUSE + 合成 root + rootless user/mount namespace +
    Landlock 路径。macOS `--safe` 实现 Seatbelt 强制的 staged 写入和 deny-all
    socket 约束；文件系统读取仍是 ambient，并单独报告。Docker 与
    libkrun/KVM 传输也已存在。第 2.5 节的 Virtualization.framework backend、
    第 3 节的 LiteBox VFS、第 4.2 节的 Docker 生产 profile，以及第 5 节的
    Firecracker 架构是目标，不是已实现 backend。Seccomp 与完整资源强制仍是
    验收标准和路线图工作。

pVisor 需要不止一种隔离 backend。本地 coding Agent 看重快速启动和对开发者
工作区的精确视图；不受信任的租户则需要在 guest runtime 被攻破后仍然有用的
边界。因此设计把**事务性工作区**与**强制边界**分开，而不是让一种机制同时
承担两种角色。

多种 backend 是实现组合，**不是强加给用户的配置面**。正常产品体验仍然是：

```bash
pvisor run --safe <agent> [args...]
```

pVisor 探测 host、工作负载和可用 placement，选择 backend，构造工作区并应用
策略。用户不配置 Landlock 权限、mount 传播、UID map、9P 传输、seccomp JSON、
container capability、TAP 设备或 microVM 镜像。专家级 backend 标志可以存在
于开发和诊断，但不得成为正常路径的前提。

易用并不意味着不可见的安全降级。若请求的保证无法提供，pVisor 要么选择另一
个可用 backend，要么返回一条可行动的错误。它从不把仅 `cwd` 的 Run 报告为
已安全沙箱化。

## 1. 共同模型

```text
RunSpec / capability policy
            |
            v
        pVisor supervisor (trusted)
            |
            +-- WorkspaceOverlay
            |     lower + compose + writable upper
            |     review / checkpoint / apply / drop
            |
            +-- IsolationBackend
                  workspace-landlock | workspace-seatbelt
                  litebox | container | microvm
            |
            v
       Agent process tree (untrusted)
```

`WorkspaceOverlay` 是文件改动的 data plane。它提供隔离的 Run 视图、
copy-on-write、whiteout 和可审计 changeset。它**本身**并不能阻止进程打开
该视图之外的路径。

`IsolationBackend` 是安全平面。它决定 Agent 能到达哪些 kernel、syscall
面、namespace、host 路径、文件描述符和网络路径。每个 backend 消费同一逻辑
工作区，并必须返回具有相同 review/apply/drop 语义的 changeset。

以下不变量适用于声称完整 capability enforcement 的 backend。部分原生
backend 可以只强制更小的维度，前提是 Run Bundle 明确标识该维度并记录剩余
ambient 访问：

1. 默认拒绝；每个 host 文件、socket、凭据、设备和端点都是显式 capability。
2. Agent 从不收到 host 控制面凭据或 Docker socket。
3. 只有 `stdin`、`stdout`、`stderr`、Run 范围的 AgentCtl 传输，以及显式授予
   的资源句柄穿过执行边界。
4. 可写工作区与只读 base 分离。进程成功退出从不意味着有权 apply 其改动。
5. 请求的 enforcement 与有效 enforcement 分开记录。enforce 请求在 backend
   不可用时 fail closed；从不静默回退到当前仅 `cwd` 的行为。
6. pVisor 记录足够的 Evidence 以审计边界：backend/version、工作区 digest、
   有效 UID/capability、kernel 特性探测、网络模式、资源限制、image/rootfs
   digest 以及降级原因。

## 2. 原生 host 路径

### 2.1 Linux：FUSE + Workspace + Landlock

这是首选的轻量 Linux host 路径。它保留今天的嵌入式 FUSE OverlayFS，并为
Agent 进程树加上内核强制、无特权的文件系统策略。

Landlock 完全是内部的：不向用户暴露系统策略文件、root helper、daemon 或
按项目规则配置。pVisor 从工作区、可执行文件/runtime 闭包、显式输入和 Run
范围的 scratch 目录推导规则。

#### 当前实现

原生 Linux safe 路径已对普通本地可执行文件可用：

- pVisor 在 Agent 代码启动前自执行一个隐藏 launcher；
- launcher 创建 one-ID user 以及私有 mount 与子 PID namespace，不需要
  `/etc/subuid`、`newuidmap`、setuid 二进制或 daemon；
- 私有 tmpfs root 只 bind-project runtime、staged 工作区、精确设备节点、
  Run 范围的 AgentCtl socket 和显式 capability，然后 launcher 用 `chroot`
  进入；任意 host pathname Unix socket 因此是缺席的，而不是留给 Landlock；
- stderr 以上的继承描述符被关闭，Landlock ABI v3 通过 `TRUNCATE` 处理全部
  文件系统权限，设置 `no_new_privs`，清空 namespace 与 ambient capability，
  并由受信任的 namespace PID 1 监督并回收 Agent 树；
- 主进程成功退出、取消和强制终止都在 PID namespace 边界结束，因此 `setsid`
  和 double-fork 后代无法在 Run 之后存活；
- 可写的 FUSE 合并工作区和显式读/写 capability 被准入，可执行文件和宽 host
  runtime 只读；
- `NetworkCapability::Deny` 还会创建私有 network namespace。Public 与
  allowlist 代理策略仍是协作式，不会被报告为不可绕过；
- 任何 namespace 或 Landlock setup 错误都会在 Agent 执行前以保留的
  infrastructure 结果和 Run Bundle 降级警告终止。

当前宽不可变 runtime 包括已有的 `/bin`、`/sbin`、`/usr`、`/lib*`、`/etc`
以及进程本地 procfs 视图。这有利于与 shell、Python、Node 和动态链接本地
工具兼容。后续可用可度量的 runtime-closure builder 收窄它；当前策略从不让
这些层级可写。

默认 pVisor 依赖图不包含 pChronicle 存储 backend、Lance 或 DataFusion。
Durable Attempt 与轨迹发布使用轻量 `persisting-events` control feature 来
启动并与 `pchronicle serve` 的 Control 组件通信；存储引擎和云 SDK 依赖留在
该进程。`jujutsu-overlay` 增加 Jujutsu OverlayFS upper。依赖边界见
[RFC-0007](../../rfcs/0007-events-contract-pchronicle-sidecar.md)。

```text
pVisor process
  +-- embedded FUSE server
  |     base/compose (read-only) + upper (writable)
  |                         |
  |                         v
  |                    merged workspace
  |
  +-- small sandbox launcher
        close unrelated FDs
        synthetic bind-projected root + chroot
        PR_SET_NO_NEW_PRIVS
        Landlock ruleset
             |
             v
        Agent process tree
```

pVisor supervisor 和 FUSE 请求循环留在 chroot 与 Landlock 域外。子进程获得
合并工作区的读/写、runtime 的读/执行，以及显式输入的只读。未投影路径在合成
root 中缺席；Landlock 独立对已投影层级强制访问权。绝对路径在该 root 内解析，
而不是重定向进工作区。

### 2.2 最小策略

| 层级 | 有效访问 |
|---|---|
| 合并工作区 | 按需读、写、创建、删除、重命名、链接 |
| Agent 可执行文件与 loader | 读、执行 |
| 所需共享库与 runtime 数据 | 只读 |
| 显式输入 Dataset | 只读 |
| Run scratch 目录 | 读/写；最好是有大小限制的 tmpfs |
| pVisor 状态、pChronicle、源凭据、home 目录 | 拒绝 |
| `/proc`、`/sys`、host socket | 除非显式投影或另行虚拟化，否则不给 Agent 访问 |
| 最小设备 | 仅精确的 null、zero/full、random/urandom 和 tty 节点 |

Landlock 叠加在普通 DAC/ACL/LSM 检查之上；它不授予进程原本没有的访问。
launcher 必须协商运行中 kernel 的 Landlock ABI，并处理该 ABI 支持的全部
安全相关权限。较旧 ABI 可能缺少跨目录 refer 或 truncate 等控制，因此
pVisor 必须发布有效保证，而不是一个布尔的 “Landlock enabled” 标志。

在 `landlock_restrict_self` 之前打开的文件不会被回溯约束。因此 FD 卫生是
边界的一部分：准备目录句柄，关闭未授予的一切，安装 `no_new_privs` 和
ruleset，然后 `exec`。这套 setup 属于小型可审计 launcher，而不是多线程
supervisor 的复杂 post-fork 闭包。

### 2.3 安全与运维性质

**优势**

- 不需要 host root 或持久特权 daemon。
- 启动与稳态开销小；文件内容仍走现有 FUSE/OverlayFS 路径。
- 工作区保真度是原生 host 路径中最好的，包括当前 review、checkpoint、apply
  和 drop 行为。
- 子进程不能再仅靠 `..` 或绝对 host 路径逃逸。

**限制**

- Agent 仍使用 host kernel 及其原生 syscall ABI。
- Landlock 是文件系统访问控制层，不是 root 文件系统、network namespace、
  资源控制器或完整进程 sandbox。
- 除非 pVisor 构建最小 runtime bundle，动态语言栈的 runtime allowlist 很难。
- 工作区 I/O 热路径上仍有 FUSE 上下文切换。
- 仅 Linux。macOS 姊妹路径有不同且明确更窄的 Seatbelt 边界。

实现已经把 Landlock 与空 capability 集、`no_new_privs`、rootless
user/mount/PID namespace，以及 deny-all Run 的 network namespace 组合在
一起。Seccomp、完整聚合资源限制，以及选择性出口的透明强制，仍是必要加固，
且不改变工作区契约。

### 2.4 macOS：FUSE + Seatbelt

原生 macOS safe 路径已对普通本地可执行文件可用。它保留同一 staged macFUSE
工作区，同时为完整 Agent 后代进程树加上内核强制的 Seatbelt 策略：

- pVisor 只调用固定系统 `/usr/bin/sandbox-exec`，从不做 PATH 查找或使用
  项目提供的 wrapper；
- 生成的 SBPL 对每个可写路径使用 `-D` 参数，因此工作区名不能注入策略文本；
- 路径参数化的 `file-write*` 规则只准入已挂载的 staged 工作区、显式读写
  文件系统 capability、精确 terminal/设备句柄、Run 拥有的临时目录，以及
  一次性 setup attestation；
- 隐藏 launcher 在 `exec` Agent 之前写入并 unlink 该 attestation。因此
  profile 编译/应用失败不能被误认为 Agent 退出，而会把 Run 作为
  infrastructure 失败终止；
- `NetworkCapability::Deny` 从默认拒绝 profile 开始，拦截 IP socket 和出站
  ambient host Unix socket，只保留精确的 Run 范围 AgentCtl 以及根植于 Run
  拥有目录的 Unix IPC；
- public 与选择性代理模式仍是协作式，因为第一版实现尚未把直接 socket
  约束到仅进程内代理端点。

兼容 profile 刻意让文件系统读取保持 ambient。这避免硬编码脆弱的 Homebrew、
Xcode、Python、Node、Rustup、SDK、framework 和用户安装 runtime 路径闭包。
因此 Run Bundle 设置 `filesystem_write_non_bypassable=true`，但保持
`filesystem_read_non_bypassable=false` 以及聚合
`filesystem_non_bypassable=false`。未来可度量的 runtime-closure 模式可以让
读取默认拒绝，而不改变工作区契约。

Seatbelt 实质改善了本地 macOS 边界，但它不是 VM 或完整进程 sandbox：host
kernel、PID namespace、syscall 面和资源记账仍然共享。`sandbox-exec` 接口
已被 Apple 弃用，尽管仍随系统提供，因此 pVisor 探测该固定二进制并 fail
closed，而不是承诺无限期的平台可用性。在 FSKit backend 可用之前，事务性
staging 仍需要 macFUSE。

### 2.5 macOS：Virtualization.framework + host-root overlay

这是为原生 Mach-O 工作负载提出的 macOS 内核隔离路径。它是已实现 Linux
libkrun full-root executor 的 macOS 对应物，但不能使用 libkrun：macOS
guest 必须由 Apple Silicon 上的 Virtualization.framework 启动。目标是让
可执行文件看到调用方 host 的根文件系统，所有写入由 pVisor upper 捕获，
同时在单独的 macOS 内核边界后执行。

guest 的 boot disk 不是 Agent 的逻辑 root。它只包含兼容的 macOS 安装和一个
特权 pVisor guest supervisor。host 通过 OverlayFS/macFUSE 构造
`host / + Run upper`，用 VirtioFS 导出合并视图，并要求 guest supervisor 在
执行目标前进入该视图：

```text
host pVisor (Rust)
  +-- host / (lower, access still limited by the invoking host identity)
  +-- per-Run upper
  +-- merged pVisor root (macFUSE / future FSKit)
  +-- MacVmExecutor
          |
          | private Unix socket / framed control protocol
          v
     pvisor-vz-helper (Swift, one helper process per active VM)
       Virtualization.framework
       +-- compatible macOS boot disk
       +-- stable VirtioFS share tag -> merged pVisor root
       +-- VZVirtioSocket control and AgentCtl transports
                    |
                    v
          pvisor-guestd (root LaunchDaemon)
            mount VirtioFS at a private path
            chroot into the pVisor root
            setgroups / setgid / setuid
            set cwd, environment, limits, and stdio
            execve host Mach-O
```

Swift helper 刻意放在 Rust supervisor 之外。它只拥有 Objective-C/Swift
Virtualization.framework 生命周期，并把它转换成小型带版本协议。现有
`RunExecutor` 契约仍是产品边界，因此选择、取消、Evidence、review、
checkpoint、apply 和 drop 与其他 pVisor executor 保持相同语义。

#### GhostVM 研究

[GhostVM](https://github.com/groundwater/GhostVM) 是最近考察的参考实现。
在 commit
[`fe88d586`](https://github.com/groundwater/GhostVM/tree/fe88d5862f74ddb05ce79e04028b84c7f70482f6)
它演示了所需的控制面原语：

- `VZMacOSBootLoader`、Mac 平台身份、macOS disk、headless 显示配置、
  VirtioFS 和 virtio socket 在一个
  [configuration builder](https://github.com/groundwater/GhostVM/blob/fe88d5862f74ddb05ce79e04028b84c7f70482f6/macOS/GhostVMKit/Configuration/VMConfigurationBuilder.swift)
  中组装；
- 一个 helper 进程拥有活动 VM，并暴露 host Unix-socket API；
- host 请求经 `VZVirtioSocket` 到达 guest agent，后者可以执行原生 macOS
  程序；
- 运行中的 VirtioFS 设备可通过
  [FolderShareService](https://github.com/groundwater/GhostVM/blob/fe88d5862f74ddb05ce79e04028b84c7f70482f6/macOS/GhostVM/Services/FolderShareService.swift)
  接收重建的目录 share；
- VM suspend/resume 使用 `saveMachineStateTo` 和 `restoreMachineStateFrom`；
  APFS `clonefile()` 在
  [VMController](https://github.com/groundwater/GhostVM/blob/fe88d5862f74ddb05ce79e04028b84c7f70482f6/macOS/GhostVMKit/Operations/VMController.swift#L519)
  中创建 copy-on-write VM clone。

这些是架构参考，不是文件系统执行方案。GhostVM 对着它私有的 `disk.img`
启动和执行；VirtioFS 目录仍是挂在普通 guest root 下的 share。它的 guest
`exec` 端点是调用 Swift `Process.run()` 并用缓冲 stdout/stderr 的用户
LaunchAgent。它不 chroot、不复现凭据、不流式 stdio、不转发信号、不控制
进程组，也不暴露 pVisor changeset。因此 pVisor guest supervisor 必须是独立
实现的 root LaunchDaemon。

以下拆分是有意的：

| GhostVM 机制 | pVisor 决策 |
|---|---|
| VM configuration builder | 在 `pvisor-vz-helper` 中复现最小 headless 子集 |
| 每个 VM 一个 helper 进程 | 保留，用于 VMM 崩溃与生命周期隔离 |
| host Unix socket 加 vsock | 保留拓扑；使用有界、带版本、流式协议 |
| 运行时 VirtioFS 替换 | 适配到一个稳定的 pVisor-root tag |
| VM suspend/resume | 用来摊薄启动成本，并严格要求模板兼容 |
| APFS VM clone | 可选用于创建干净 boot 模板 |
| GhostTools 命令执行 | 用特权 `pvisor-guestd` 替换 |
| NAT、bridge、剪贴板、音频、GUI 自动化 | 从默认进程隔离 VM 中省略 |
| 把私有 guest root 当作工作负载 root | 拒绝；导出的 pVisor 合并 root 才是工作负载 root |

GhostVM 的 README 目前写明其源码许可尚未确定。pVisor 可以研究公开行为与
架构，但在兼容许可发布之前不得复制其实现。helper 与 guest supervisor 是
对着 Apple 公开 API 的干净独立实现。

#### 文件系统与身份语义

“使用 host UID 和权限”意味着保留普通 POSIX 文件语义，而不是继承每一种
macOS 安全身份。host pVisor 在调用方 host 身份下打开并服务 lower 文件；
guest supervisor 随后在 `execve` 前安装匹配的数字 UID、GID 和补充组。实现
必须证明 VirtioFS 如何表示 owner、mode、ACL、symlink、hard-link、xattr、
设备和 rename 语义，而不是假定数字身份就足够。

仅因数字 UID 匹配，以下 host 设施不会自动变得透明：

- TCC 决策、Keychain access group、代码签名身份与 entitlement；
- host login/GUI bootstrap 会话、launchd 服务、Mach port、Apple Events 和
  host Unix socket；
- host kernel 状态、设备、导出 root 中不可见的已挂载卷，以及仅由 host 进程
  持有的凭据。

现代 macOS 还通过密封系统卷、可写数据卷和 firmlink 呈现 `/`。pVisor 必须
验证导出 host root 能呈现一个连贯命名空间，并且 whiteout/copy-up 行为在
`/System/Volumes/Data` 上仍然正确。访问受隐私保护的 host 文件可能要求受
信任 host 组件具备 Full Disk Access；pVisor 必须报告该要求，而不是静默
返回部分 root。

host 与 guest 最初应要求相同架构和精确 macOS build。host 可执行文件可以
依赖匹配的 dyld shared cache、framework ABI、代码签名策略和 kernel 行为。
在兼容矩阵证明之前，跨 build 执行不受支持。

#### 生命周期与安全 profile

按 Run 冷安装 macOS 不可行。预期生命周期是：

1. 供应并证明一个最小、匹配的 macOS boot 模板；
2. 启动一次，安装 `pvisor-guestd`，并保存干净的挂起状态；
3. 恢复 warm VM，或从小型版本匹配池中获取一个；
4. 只附加该 Run 的 VirtioFS root 和每 Run 的 vsock 端点；
5. 在 guest 执行前轮换 Run 身份、鉴权材料、熵、IPC 和网络状态；
6. 恰好执行一棵不受信任的进程树，经普通 pVisor review 路径导出 upper，然后
   销毁或把擦洗后的 VM 还回池中。

默认 VM 没有 NAT 或桥接网络设备。网络访问穿过 OverlayNet 拥有的显式 vsock
relay。剪贴板、host 音频、GUI 设备、任意共享文件夹、端口转发和 ambient
host socket 都不存在。host VMM helper 只获得对已准备合并 root、VM 模板、
其私有 control socket 以及所需 Virtualization.framework 资源的访问；它不得
继承 pChronicle、源凭据或不相关描述符。

VirtioFS 是最大的可行性风险。GhostVM 有一份关于 macOS guest 下
[空 mount 与不可读文件](https://github.com/groundwater/GhostVM/issues/255)
的公开报告。pVisor 的设计把 dyld、framework、SDK、包管理器和元数据密集
工具链放在这条路径上，比共享项目目录更苛刻。因此 VM 启动成功不是 backend
可用或安全的证据。

#### 可行性门

该 backend 保持实验性，直到一个聚焦原型在受支持的 host/guest build 对上
通过以下全部条件：

1. 用稳定 VirtioFS tag 导出 pVisor 合并 root，并在没有 Finder 或 login-session
   自动化的情况下挂载它；
2. 在 `chroot` 后运行 `/usr/bin/true`、`/bin/zsh` 以及有代表性的
   `xcrun`/编译器工具，并具备正确的 cwd、环境、UID、GID、组、退出状态、
   流式 stdio、信号、取消和后代清理；
3. 证明 lower 文件不变，且所有创建、修改、重命名、删除、whiteout、xattr、
   ACL、symlink 和 hard link 进入 Run upper，并在 review/checkpoint/apply/drop
   后存活；
4. 演练 dyld/framework 加载、代码签名、密封系统/数据 firmlink 布局、大输出、
   大文件、大量小文件、并发改写、崩溃恢复以及 warm-restore 附加变更；
5. 证明网络、剪贴板、任意 share、过期 vsock token、先前 Run 的 upper 或
   不相关 host 描述符都不可达；
6. 发布冷/热延迟和 RSS，并与 Seatbelt 以及 Linux libkrun Run 比较。

通过该门确立透明 CLI 与开发工具执行。GUI 应用和 host-session 服务需要单独
Evidence，不由进程级 backend 的成功隐含。

## 3. 路径 B：LiteBox + VFS 中的 OverlayFS 语义

### 3.1 定位

这是高密度 libOS 路径。LiteBox 在 userspace 处理 guest Linux ABI 和路径
解析。pVisor 应把它的 overlay 语义实现为 LiteBox 文件系统 backend 或
composer，而不是 FUSE 挂载一条 host 路径，再把 guest 路径字符串转发给 host
`openat`。

```text
pVisor supervisor
  +-- build content-addressed root/workspace bundle
  +-- pass sealed bundle FD + policy + AgentCtl FD
  |
  `-- LiteBox runner process
        LiteBox Linux shim
                 |
                 v
        LiteBox VFS resolver
          +-- read-only root/runtime
          +-- read-only workspace layers
          `-- writable in-memory/delta upper
                         |
                         v
                 exported changeset
                         |
                         v
          pVisor review / apply / drop
```

适配器保留 pVisor 的逻辑操作：

- 有序只读 base 与 compose 层；
- 首次写入时 copy-up；
- whiteout 与不透明目录语义；
- 确定性目录合并；
- mode、时间戳、symlink、hard link 和 xattr 的元数据策略；
- 有界可写 upper，可以在不遍历不相关 host 路径的情况下导出。

初始实现可以复用 LiteBox 的只读 tar 和内存文件系统，但生产采用需要一份
文件系统语义矩阵。不支持的元数据必须显式失败，或按文档化策略规范化；静默
丢失会破坏 pVisor 的 changeset 契约。

### 3.2 安全与运维性质

**优势**

- guest 路径终止于 LiteBox VFS；正常路径不包含 host pathname 查找。
- 比原生 Linux 进程或通用 OCI container 更小的 host 接口，使 syscall 级策略
  和确定性 I/O 变得可行。
- 只读内容寻址 bundle 可以跨 Run 缓存和共享；可写状态保持每 Run。
- 完全在 runner 内处理的 VFS 操作不需要 kernel FUSE 往返，这可能有利于
  元数据密集工作负载。

**限制**

- Linux syscall 与文件系统兼容面比 Docker 或 VM 更窄。
- LiteBox 及其 pVisor 适配器仍在演进，并扩大 pVisor 的可信计算基。
- userspace libOS 并不自动成为硬件或 kernel 安全边界。必须假定 runner、
  loader、syscall 拦截或共享地址空间中的缺陷可能存在。
- 打包原生库、动态 runtime、JIT 和异常文件系统行为需要显式兼容测试。

因此 LiteBox runner 必须在单独的无特权进程中执行，并带 Landlock、seccomp、
`no_new_privs`、空 capability、关闭的 FD 和资源限制。若 guest 逃出 LiteBox
抽象，外层 kernel 策略才是包含边界。禁止把不受信任的 LiteBox guest 嵌入
pVisor supervisor 进程。

### 3.3 工作区传输

避免长期、任意 pathname broker。优先不可变且有界的对象：

1. pVisor 快照逻辑 lower 层并计算 digest。
2. 向 runner 提供密封 `memfd` 或只读文件描述符。
3. LiteBox 通过其 VFS 读取 root 和工作区。
4. 写入进入带字节/inode 配额的每 Run upper。
5. runner 导出规范、有界的 changeset。
6. pVisor 在把 changeset 暴露给 review/apply 之前校验路径、条目类型、元数据、
   大小和 digest。

## 4. 路径 C：Docker / OCI container

### 4.1 定位

这是兼容与生态路径。它支持现有 Agent 镜像和常规 Linux runtime，隔离强于
host executor，同时共享 host kernel。

当前 pVisor Docker/Podman executor 已经向镜像注入匹配的静态 pVisor，并委托
同一 `RunSpec`。它挂载最终工作区并返回类型化 `RunResult`。它尚未把每项
pVisor capability 翻译成 OCI 限制，且注入的 pVisor 当前以 container root
启动；这些是实现缺口，不是目标设计的性质。

```text
host pVisor
  +-- WorkspaceOverlay / merged view
  +-- Docker or Podman transport
          |
          v
     OCI container
       read-only image rootfs
       /workspace -> pVisor Run view
       tmpfs /tmp
       injected pVisor -> Agent
```

Docker 的镜像层 OverlayFS 与 pVisor 的 WorkspaceOverlay 角色不同。前者组装
OCI 根文件系统；后者拥有 Agent 改动和 review/apply/drop。container 拆除不得
把 OCI 可写层提交为 Run 结果。

### 4.2 生产 profile

目标 profile 是：

- 支持时使用 rootless Docker/Podman，或 user namespace remapping；
- 去掉注入 pVisor 的 bootstrap 问题后，Agent UID 非 root；
- 丢掉全部 capability，`no-new-privileges`，默认或更紧的 seccomp；
- 只读 container rootfs 和私有、有界的 `/tmp`；
- PID、内存、CPU、文件大小和进程数限制；
- 默认无网络，否则使用连到 pVisor 拥有 broker 的专用 namespace；
- 无 host PID/IPC namespace、privileged 模式、设备透传、任意可写 mount 或
  Docker socket；
- 镜像 digest pinning 以及可审计的 mount/capability manifest。

### 4.3 安全与运维性质

**优势**

- 除 VM 外最高的工作负载兼容性。
- 成熟的镜像构建、分发、缓存、可观测性与运维工具。
- Namespace、cgroup、capability、seccomp 和 host LSM 组合成实用生产边界。
- 对 Kubernetes 和现有 CI 基础设施是自然部署路径。

**限制**

- Container 共享 host kernel；kernel 或 container-runtime 逃逸在 pVisor
  自身强制之外。
- 冷启动成本、镜像存储、daemon/runtime 依赖和 mount plumbing 高于本地和
  LiteBox 路径。
- Rootful daemon 部署会制造更大的特权控制面。
- Host 网络、宽 bind mount、`--privileged` 或 Docker socket 可以抹掉大部分
  隔离价值。
- 当前 Gateway loopback 集成在某些配置下需要 host 网络；生产 enforcement
  需要 guest 可见 broker 之后才能去掉该限制。

Docker 是推荐的兼容 fallback，不是 pVisor capability 模型的定义。

## 5. 路径 D：Firecracker microVM

### 5.1 定位

这是最强的多租户路径。每个 Run 或 warm Run 槽在 KVM 下获得单独的 guest
kernel。Firecracker 刻意暴露小型设备模型，并提供 jailer，为主机侧
namespace/cgroup 隔离并丢掉 VMM 特权。

现有 pVisor `vm` executor 静态链接 libkrun，可以使用显式 Linux rootfs，或
在没有 container daemon 的情况下拉取公开 OCI 镜像。已校验的镜像层形成不可变
缓存 lower rootfs，guest 系统写入留在可审查 upper。host 路径不会被隐式暴露。
显式 `--overlayfs-base` 加 `--overlayfs-target` 在所选 guest 路径挂载 staged
视图。vendored libkrun 在 Linux 和 macOS 上通过 virtio-fs 直接服务 root 与
工作区 copy-on-write union，没有 host FUSE mount、物化或对账。Linux 使用
KVM，Apple Silicon macOS 使用 HVF。Linux 另外用 user/mount/network
namespace 和 Landlock 约束 VMM。macOS VMM 尚未包进等价的 host 文件系统
sandbox，因此 libkrun 的 virtio-fs proxy 仍处于调用用户的安全上下文。
OverlayNet 与 AgentCtl guest relay 在本阶段尚未实现。这不是下面的敌对多租户
Firecracker 设计：

```text
host pVisor / microVM manager
  +-- immutable kernel + rootfs image
  +-- read-only workspace/base block image
  +-- per-Run writable delta block image
  +-- vsock control and AgentCtl transport
  +-- TAP/network broker under policy
          |
          v
  Firecracker + jailer
          |
          v
  guest kernel + injected pVisor + Agent
```

rootfs 和工作区作为文件后备块设备附加。Run 完成时，guest 静止文件系统并
经 vsock 返回 manifest；host 校验并把 delta 转成普通 pVisor changeset。
Firecracker snapshot 可以摊薄启动成本，但 VM 状态、guest 内存、块设备、网络
设备和 vsock 端点有各自的生命周期与兼容要求。Snapshot 复用必须轮换 Run
身份、熵、凭据和网络状态。

### 5.2 安全与运维性质

**优势**

- 单独 guest kernel 为互不信任租户和敌对原生代码提供最清晰的边界。
- 最小设备模拟相对通用机器模拟器减少 VMM 攻击面。
- 资源记账和网络拓扑在 VM 边界上是显式的。
- Warm pool 与 snapshot 可以让重复 Run 启动变得可行。

**限制**

- 需要 Linux、KVM、kernel/rootfs 镜像生产、jailer、TAP/网络 setup 以及
  microVM 生命周期服务。
- 即使 VMM 很轻，基线内存和运维复杂度也高于进程/container 路径。
- 工作区块镜像创建和 delta 提取，不如直接挂载的 FUSE 工作区交互。
- Kernel、rootfs、snapshot 和 VMM 版本形成更大的兼容与补丁管理面。
- 直接共享 host 目录会削弱干净边界，不应成为生产工作区设计。

生产 Firecracker 执行必须使用 jailer 或等价更强的 host 策略、专用无特权
VMM 身份、cgroup、seccomp、隔离网络、受信任不可变输入，并且没有对 host
路径的 ambient 访问。

## 6. 比较与选择

该表描述预期生产形态，而不只是仓库里当前已有的代码。性能刻意保持相对，
直到共同 benchmark 测过冷/热启动、RSS、syscall 密集与数据密集工作负载，
以及拆除。

| 维度 | FUSE + Landlock | FUSE + Seatbelt | macOS VM + VirtioFS | LiteBox VFS | Docker/OCI | Firecracker |
|---|---|---|---|---|---|---|
| 主要目标 | 最快的 Linux 最小特权 | 零配置 macOS 写约束 | 带 guest-kernel 边界的透明 Mach-O 执行 | 高密度 libOS 隔离 | 兼容与部署 | 敌对多租户隔离 |
| 安全边界 | 合成 root + host LSM/namespace 策略 | host 进程上的 Seatbelt 写/socket 策略 | macOS guest kernel + Virtualization.framework VMM | libOS 加外层 host 策略 | namespace/cgroup/LSM，共享 kernel | guest kernel + KVM + jailed VMM |
| 是否需要 host root | 否 | 否 | 作为 pVisor 合并 root 导出 | 否 | rootless 模式下否 | 通常需要 host 供应 |
| Guest 兼容性 | 原生 Linux ABI | 原生 macOS ABI；ambient 读取 | 原生 Mach-O，最初仅精确 host/guest build | 受约束的 Linux ABI | 宽 Linux userspace | 完整 guest Linux |
| 工作区保真度 | 最高 | 配合 macFUSE 最高 | 目标是 full-root 保真；VirtioFS 上未证明 | 需要语义适配器 | 经 mount/volume 高 | 显式块/delta 转换 |
| 启动成本 | 最低 | 最低 | 冷启动高；目标是热恢复/池 | 目标低 | 中等，取决于镜像 | 冷启动最高；目标是热 snapshot |
| 每 Run 内存 | 最低 | 最低 | 高 | 目标低 | 中等 | 最高 |
| Kernel 逃逸爆炸半径 | host | host | 先 guest，再 VMM 边界 | 外层逃逸后是 host | host | 先 guest，再 VMM/KVM 边界 |
| 可移植性 | Linux | macOS；依赖已弃用 launcher | 支持 macOS 虚拟化的 Apple Silicon Mac | 取决于平台/ABI | 宽 OCI host | Linux + KVM |
| 当前 pVisor 状态 | 已实现；seccomp/limits 待定 | 写约束与 deny-all socket 策略已实现 | 已研究设计；需要可行性原型 | 计划中 | 已实现，存在加固缺口 | libkrun full-root 模式已存在；Firecracker 计划中 |

### 推荐组合

选择权属于 pVisor 和 placement 控制面：

1. 普通 Linux `pvisor run --safe` 今天使用 FUSE + Workspace + 合成 root +
   rootless namespace + Landlock。所需控制 fail-closed 安装；不可用的 user
   namespace、mount、chroot 或 Landlock ABI 从不静默回退。
2. 当 LiteBox 能为兼容的打包工作负载提供更小、可度量的 host 接口时，pVisor
   可以自动选择它；用户仍调用同一命令。
3. 提供 OCI 镜像自然选择 Docker/Podman。否则 pVisor 可以把已经可用的
   rootless runtime 当作兼容 fallback；它不要求用户构造 capability 或 mount
   标志。
4. 为敌对多租户执行配置的集群把 Run 放到 Firecracker worker。Kernel 镜像、
   snapshot、网络和 jailer setup 是运营商拥有的集群基础设施，不是每用户
   配置。
5. macOS 保持同一命令，并对已实现的低延迟本地路径使用 Seatbelt 写约束。
   可行性门通过后，要求 guest-kernel 边界的策略可以自动选择
   Virtualization.framework backend。在此之前，Bundle 单独报告 ambient 读取
   和协作式选择性网络；更严的请求路由到另一有能力的 placement，或带着一条
   修复建议失败。

这些路径是组合，不是强制迁移阶梯。客户声明工作负载意图，并在必要时声明
最低安全要求；placement 只选择其已度量能力满足该要求的 backend。客户不选择
kernel 机制。

## 7. 一份 backend 契约

所有实现都应把一次请求编译成一份带 Evidence 的结果：

```text
IsolationRequest {
  minimum_boundary,
  filesystem_capabilities,
  network_capabilities,
  compute_limits,
  credential_refs,
  require_enforcement,
}

IsolationEvidence {
  requested_class,
  effective_backend,
  backend_version,
  effective_controls,
  unsupported_controls,
  workspace_digest,
  runtime_or_image_digest,
  identity_and_capabilities,
  kernel_features,
}
```

只有当测试证明完整 Agent 进程树无法到达未授予层级时，
`RuntimeCapabilities.filesystem = true` 才有效。仅有已挂载工作区或成功的
setup 调用不是 Evidence。

该契约是 admission、placement 与 runtime driver 之间的内部契约。它不是用户
必须理解或配置 backend 特定机制的要求。

## 8. 零配置验收标准

默认本地路径只有在以下全部成立时才算完整：

- 一次 pVisor 安装和一条 `pvisor run --safe` 命令就足够；
- 不需要 root shell、setuid pVisor daemon、手工加组、手写策略、mount 命令
  或 container 安全标志；
- pVisor 发现可执行文件及其最小 runtime 依赖；
- 工作区 setup、隔离、清理和 changeset 恢复是自动的；
- 不受支持的 host 产生一条稳定错误，带具体修复或自动可用的 placement，而
  不是一串 kernel 细节；
- `pvisor status` 和 Run Bundle 解释有效边界供审计，但不把该解释当作使用
  前提；
- 升级保留高层命令和 Run 契约，同时允许所选 backend 变化。

该标准排除把 9P 当作面向用户的 Docker setup 步骤。当远程 backend 需要时，
pVisor 可以在内部使用文件系统协议，但用户绝不能为普通 Run 供应 9P server、
挂载它，或授予 container mount capability。

## 9. 校验与 benchmark

每个 backend 必须跑同一套对抗套件：

- 绝对路径、`..`、symlink 链、hard link、rename 竞态、magic link、
  `/proc/self/fd`、继承目录 FD、UNIX socket、设备节点和描述符传递；
- fork/clone/exec 后代、原始 syscall、静态二进制、JIT 生成代码、信号、
  ptrace 尝试，以及适用处的 namespace 操作；
- 直接 socket、DNS rebinding、字面量 IP、UDP/QUIC、loopback、link-local
  和元数据服务地址；
- 字节/inode/进程/CPU/内存/网络耗尽以及取消清理；
- 工作区导出、review 和 apply 期间的掉电或 supervisor 崩溃；
- 同一 changeset 在全部四种 backend 上的语义比较。

共享 benchmark 报告分布，而不是单个演示数字：

- 冷启动与热启动 P50/P95/P99；
- 空闲与峰值 RSS；
- 顺序与随机工作区吞吐；
- 每秒小文件元数据操作；
- syscall 密集以及 Python/Node/原生 Agent 工作负载；
- checkpoint/export/apply 延迟与产出字节；
- host CPU 成本、上下文切换、缺页，以及 FUSE/VMM/broker 开销。

已实现的 Linux 套件目前证明 staged 写入，以及拒绝绝对路径读/写、symlink
逃逸、`/proc/self/root` 逃逸、未授予 pathname Unix socket、精确 AgentCtl
socket 的保留，以及 deny-all 模式下的 host-loopback 访问。它也证明 setup
失败在 Agent 执行前报告。这是有用的回归下限，还不是上面列出的完整对抗/
kernel 矩阵。没有 backend 仅凭架构预期就能成为生产默认；它必须发布可重复
测量，并在每个受支持 host/kernel 上通过该矩阵。

## 参考

- [Apple: Running macOS in a virtual machine on Apple silicon](https://developer.apple.com/documentation/virtualization/running-macos-in-a-virtual-machine-on-apple-silicon)
- [Apple: VZVirtioFileSystemDeviceConfiguration](https://developer.apple.com/documentation/virtualization/vzvirtiofilesystemdeviceconfiguration)
- [GhostVM](https://github.com/groundwater/GhostVM)
- [GhostVM VirtioFS instability report](https://github.com/groundwater/GhostVM/issues/255)
- [Linux Landlock userspace API](https://docs.kernel.org/userspace-api/landlock.html)
- [Docker rootless mode](https://docs.docker.com/engine/security/rootless/)
- [Docker default seccomp profile](https://docs.docker.com/engine/security/seccomp/)
- [Firecracker](https://github.com/firecracker-microvm/firecracker)
- [Firecracker jailer](https://github.com/firecracker-microvm/firecracker/blob/main/docs/jailer.md)
- [Firecracker snapshot support](https://github.com/firecracker-microvm/firecracker/blob/main/docs/snapshotting/snapshot-support.md)
