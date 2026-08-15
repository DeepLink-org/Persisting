# 使用 pVisor 运行工作负载

pVisor 是 Persisting 对 [AgentVisor 契约](../design/agentvisor.md) 的实现：无论选择
哪个 execution provider，它都统一管理单个 Agent Run 的生命周期、能力、外部效果、
检查点与证据。

本文详细说明 pVisor 当前支持的 host 与 VM 执行方式。命令行刻意把三个彼此独立的
问题分开表达：

1. `--executor` 决定进程使用 host kernel，还是进入 libkrun VM。
2. `--image`、`--vm-rootfs`、`--host-rootfs` 决定 VM 使用哪套 Linux rootfs。
3. `--overlayfs-*` 决定哪个 host 目录作为工作区、映射到哪里，以及如何暂存改动。

这里不引入 `--workspace`、`--mount` 等别名，以下参数就是规范接口。

## 参数模型

| 参数 | 含义 |
| --- | --- |
| `--executor host` | 在 host kernel 上执行命令，也是默认 executor。 |
| `--executor vm` | 用 libkrun 启动 Linux guest kernel。 |
| `--host-rootfs` | 仅 Linux：把 host 的 `/` 作为 VM rootfs 的只读 lower；未写 `--executor` 时自动选择 `vm`。 |
| `--image IMAGE` | 直接拉取 OCI 镜像作为 VM rootfs，不依赖 Docker/Podman daemon。VM 默认镜像为 `ubuntu:latest`；为保证可复现性，建议显式固定 tag 或 digest。 |
| `--vm-rootfs DIR` | 使用已经准备好的 Linux rootfs 目录。 |
| `--overlayfs-base DIR` | host 上的工作区只读 lower，同时是默认 apply 目标。 |
| `--overlayfs-target PATH` | 仅 VM：把工作区挂载到 guest 内的绝对路径，该路径同时成为 guest 工作目录；要求同时指定 base，且不能为 `/`。 |
| `--overlayfs-stage DIR` | 保存工作区改动的持久 writable stage；并发 Run 或不同模式应使用不同 stage。 |
| `--overlayfs-commit manual` | 保留改动供 review；之后用 `apply` 写回 base，或用 `drop` 丢弃。 |
| `--overlayfs-commit apply` | Run 成功后自动把改动写回 base。 |
| `--overlayfs-commit drop` | Run 结束后自动丢弃改动。 |

`--host-rootfs`、`--image`、`--vm-rootfs` 是互斥的三种 VM rootfs 来源。
`--host-rootfs` 是一个表达明确语义的开关，并不是 `--vm-rootfs /` 或任何
OverlayFS 参数的别名。

## 支持方式总览

| Host 平台 | Executor | VM rootfs | 命令看到的工作区 |
| --- | --- | --- | --- |
| macOS | `host` | 不适用 | `--overlayfs-base` 的 staged host cwd |
| macOS | `vm` | OCI 镜像或准备好的 Linux rootfs | base 挂载在 `--overlayfs-target` |
| Linux | `host` | 不适用 | `--overlayfs-base` 的 staged host cwd |
| Linux | `vm --host-rootfs` | 通过 virtio-fs 使用 Linux host `/` | base 挂载在 `--overlayfs-target` |
| Linux | `vm` | OCI 镜像或准备好的 Linux rootfs | base 挂载在 `--overlayfs-target` |

### macOS：host executor

```bash
./target/release/pvisor run --executor host \
  --overlayfs-base /Users/reiase/workspace \
  --overlayfs-stage ./tmp/macos-host \
  --overlayfs-commit manual \
  -- /bin/bash
```

命令使用 macOS kernel 和 host 二进制；工作目录是 base 的 COW 视图。这个方式不会
提供 Linux kernel。需要 pVisor 当前支持的更强 host 写入约束时，可使用 `--safe`。

### macOS：OCI rootfs VM

```bash
./target/release/pvisor run --executor vm \
  --image ubuntu:24.04 \
  --overlayfs-base /Users/reiase/workspace \
  --overlayfs-target /home/workspace \
  --overlayfs-stage ./tmp/macos-vm \
  --overlayfs-commit manual \
  -- /bin/bash
```

此时 `/bin/bash` 在 Linux guest 内解析，而不是在 macOS 上解析。OCI rootfs 是不可变
lower，系统目录写入落到临时 root upper；工作区改动才进入指定的持久 stage。
macOS 自己的 `/` 不能拿来做这个 VM 的 rootfs，因为 Mach-O 程序和 macOS userland
不能运行在 Linux guest kernel 上。

### Linux：host executor

```bash
./target/release/pvisor run --executor host \
  --overlayfs-base /home/reiase/workspace \
  --overlayfs-stage ./tmp/linux-host \
  --overlayfs-commit manual \
  -- /bin/bash
```

它与 macOS host 方式的结构一致，命令使用 Linux host kernel 和 host userland。

### Linux：透明使用 host rootfs 的 VM

```bash
./target/release/pvisor run --executor vm \
  --host-rootfs \
  --overlayfs-base /home/reiase/workspace \
  --overlayfs-target /home/workspace \
  --overlayfs-stage ./tmp/linux-host-rootfs \
  --overlayfs-commit manual \
  -- /bin/bash
```

这是透明 rootfs 路径：guest 换成独立的 Linux kernel，但通过 virtio-fs 把 host `/`
作为 rootfs lower 读取。对 rootfs 的系统级写入进入 VM 的临时 upper，并在 VM 退出时
丢弃；单独挂载的工作区使用持久 stage，可以 review、apply 或 drop。

该方式会让 guest 读取 host rootfs 中当前用户本来可以读取的内容，适合相同所有者的
本地隔离，不应当被描述成不可信多租户边界。建议始终同时使用
`--overlayfs-target`。如果省略 target，持久 OverlayFS stage 将描述整个 host `/` 的
改动，后续 apply 可能以 host rootfs 为目标，操作风险明显更高。

### Linux：OCI rootfs VM

```bash
./target/release/pvisor run --executor vm \
  --image ubuntu:24.04 \
  --overlayfs-base /home/reiase/workspace \
  --overlayfs-target /home/workspace \
  --overlayfs-stage ./tmp/linux-vm \
  --overlayfs-commit manual \
  -- /bin/bash
```

该方式同时提供 guest kernel 和由镜像定义的 userland。与 `--host-rootfs` 相比，它的
可复现性更好、暴露的 host 数据更少，代价是需要下载或维护镜像。

### 两个平台：使用准备好的 rootfs 目录

在支持 VM 的 macOS 或 Linux 上，都可以用已经解包的 Linux rootfs 代替 OCI 镜像：

```bash
./target/release/pvisor run --executor vm \
  --vm-rootfs /opt/pvisor/rootfs \
  --overlayfs-base /path/to/project \
  --overlayfs-target /home/workspace \
  --overlayfs-stage ./tmp/prepared-rootfs \
  --overlayfs-commit manual \
  -- /bin/bash
```

该目录必须包含与 host CPU 架构匹配的 Linux userland，并包含准备执行的命令。

## manual stage 的 review、apply 与 drop

Run 结束后，使用输出的 Run id，或在没有歧义时使用 `last`：

```bash
./target/release/pvisor review last
./target/release/pvisor inspect last -- git status --short
./target/release/pvisor apply last --path src
./target/release/pvisor apply last --include 'tests/**' --exclude 'tests/generated/**'
./target/release/pvisor apply last --all
# 或者丢弃：
./target/release/pvisor drop last
```

`apply` 默认写回 `--overlayfs-base`，也可以给 apply 子命令显式传 `--target`。
过滤后的 apply 只消费依赖闭包内的选中变更，其余变更继续留在 stage，可再次
apply 或最终 drop。opaque 目录和硬链接组不会被不安全地拆开；每次成功批次都会
记录到 `apply-ledger.json`。
stage 不能包含 base 或 compose layer。stage 位于 base 内时会从 merged view 隐藏，
但把不同 Run 的 stage 放进独立的 `tmp` 子目录通常更容易审计和清理。

## Rootfs 与工作区的关系

对 VM 来说，rootfs 和工作区是两个不同的 COW 树：

```text
OCI image / prepared rootfs / Linux host /
                 │
                 └── guest /（临时 root upper）

--overlayfs-base（host 项目目录）
                 │
                 └── --overlayfs-target（持久 workspace stage）
```

因此 `--overlayfs-base` 不负责选择 VM 操作系统，`--host-rootfs` 也不负责选择项目目录。
这正是两组参数都保留且不使用别名的原因。

## 构建要求和常见错误

- macOS 源码构建请执行 `just pvisor`。该 recipe 会构建 release、附加
  `macos-hypervisor.entitlements`、做 ad-hoc 签名并验证签名。未签名的二进制会在
  `krun_start_enter` 阶段失败。
- Linux VM 需要当前用户能访问 `/dev/kvm`；macOS VM 需要 Apple Silicon、HVF 和
  Hypervisor entitlement。
- `--overlayfs-target` 仅支持 VM，必须是非 `/` 的绝对路径，并要求
  `--overlayfs-base`。
- Linux 机器不能照搬 `/Users/...` 这样的 macOS 路径，应使用真实 Linux 路径，
  例如 `/home/reiase/workspace`。
- VM 网络默认使用 OverlayNet `auto`：pVisor 通过 smoltcp 提供 DHCP、合成 DNS 和受策略控制的
  IPv4 TCP；`mode = "off"` 可让 guest 彻底离线。当前不支持通用 UDP、IPv6、ICMP、QUIC
  或入站连接。启用 Gateway capture 时，guest 通过虚拟路由器访问它，不直接暴露 host loopback。
