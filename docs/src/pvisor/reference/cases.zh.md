# pVisor `run` 用户场景与回归示例

从最简单的命令开始，逐步加入资源限制、stage、VM、容器和网络功能。
每个 case 先说明用途、准备和预期结果，再给出可执行命令；编号便于单独回归。
本文只讨论 `run`，其它子命令请参阅各自的使用文档。编号（如 A01）只用于回归报告和问题定位；阅读时请按场景选择命令。



## 按需求选择

| 你的需求 | 建议先看 |
|---|---|
| 只想运行一个命令 | A01–A03 |
| 需要超时、内存或文件限制 | A04、B01–B04 |
| 想保留、丢弃或检查文件改动 | C01–C05 |
| 需要组合多个 OverlayFS 层 | D01 |
| 想了解 host 默认隔离 | D02–D03 |
| 使用 VM、宿主 rootfs 或 OCI 镜像 | E01–E06 |
| 使用原生 OCI 容器 | F01–F04 |
| 配置网络代理或禁止网络 | G01–G06 |
| 接入 Gateway 或记录轨迹 | H01–H03 |
| 从配置文件或 RunSpec 执行 | I01–I03 |
| 参考完整生产组合 | J01–J03 |

每个场景都包含三层信息：命令是用户实际输入，正文说明适用场景和预期，
折叠的断言是自动回归使用的实现检查。你可以只复制命令，也可以运行脚本做完整验证。

## 如何使用

手工执行时，先准备一个测试工作目录。各例中的 `pvisor` 应已在 `PATH` 中；
`/path/to/...` 需要替换成自己的路径或镜像引用。

```bash
mkdir -p /tmp/pvisor-cases/workspace
cd /tmp/pvisor-cases/workspace
```

也可以让脚本逐条执行，并自动检查各例的结果：

```bash
python3 scripts/run-pvisor-cases.py --list
python3 scripts/run-pvisor-cases.py --pvisor target/release/pvisor --case A01,C01 --keep
python3 scripts/run-pvisor-cases.py --pvisor target/release/pvisor --report target/pvisor-case-report.md
```

脚本为每个 case 创建独立的临时 workspace，把文档中的 `/tmp/pvisor-cases`
换成该 case 的目录。测试断言折叠在命令下方，由脚本执行，不需要手工复制。
断言中的 `bundle_expect` 等函数由脚本提供，用于读取本次运行的
`run-bundle.json` 和 `run.json`；它们不是 pVisor 命令。

case 注释中的 `requires` 只描述建议的运行环境，不会导致 case 被跳过；脚本总是
使用默认值执行，并把缺少 KVM、rootfs、Lance 或 OCI runtime 的真实错误记录为
`FAIL`。`--keep` 保留现场；`--strict-skips` 为兼容旧用法保留，但正常情况下不会产生
`SKIP`。Linux 未提供 rootfs 时，目录 rootfs case 使用宿主 `/` 进行 smoke test，
这只能验证流程，不能代表独立的 guest rootfs，也不应作为生产隔离边界。

| 测试资源 | 脚本配置 |
|---|---|
| 已准备好的 Linux rootfs | `PVISOR_CASE_ROOTFS`，用于替换 `/path/to/rootfs`；未设置时 Linux 使用宿主 `/` |
| VM 镜像引用 | `PVISOR_CASE_IMAGE`，用于替换 `/path/to/image`；未设置时使用 `ubuntu:latest` |
| 容器镜像引用 | `PVISOR_CASE_CONTAINER_IMAGE`，用于替换 `alpine:latest`；未设置时使用 `ubuntu:latest`（与动态 pVisor ABI 兼容） |
| OCI runtime | 安装 `crun`；F04 显式使用 `runc`，也需要安装它 |
| 自定义 libkrunfw 目录 | `PVISOR_CASE_FIRMWARE`；未设置时脚本尝试查找本机缓存 |
| VM 中要执行的 Agent | `PVISOR_CASE_AGENT`，用于替换 `/usr/local/bin/agent`；此路径必须在 guest 中也可执行 |
| Lance 记录 | 安装带相应支持的组件，并设置 `PVISOR_CASE_LANCE=1` |

Linux host stage 示例需要可用的 user/mount namespace。VM 示例需要可访问的
`/dev/kvm`。容器需要本机 OCI runtime 具备实际启动容器的权限；
装有 runtime 可执行文件本身并不保证权限齐备。

## 可执行 Case

### A. 基础调用与身份

这一组适合第一次使用 pVisor。先从 A01 开始；只有需要固定显示名、采集输出或显式传递环境变量时，再选择后续例子。

- [ ] **A01：省略 `run` 的最简调用**

  建议场景：适合第一次使用 pVisor、确认命令和 Run 身份。

  用途：在当前目录执行一个命令，不需要显式写出 `run`。`--` 后全部是交给 Agent 的命令和参数。

  预期：输出当前 workspace 的绝对路径并成功退出。默认使用 host executor，不启用 stage；未配置网络策略时记录为 ambient。

  ```bash
  pvisor -- /bin/pwd
  ```

  <details>
  <summary>自动回归断言（由脚本执行）</summary>

  <!-- pvisor-assert -->

  ```bash
  stdout_has "$(cd "$PVISOR_CASE_WORKSPACE" && pwd -P)"
  bundle_expect run.state completed
  bundle_expect run.exit_code 0
  bundle_expect run.agent pwd
  bundle_expect network.policy.mode ambient
  ```

  </details>

- [ ] **A02：显式 `run` 与省略形式等价**

  建议场景：适合第一次使用 pVisor、确认命令和 Run 身份。

  用途：对比省略和显式写出 `run` 的两种调用。分别保存 Agent 的标准输出，便于比较。

  预期：两个输出文件内容相同，都是当前工作目录。两次运行会各自生成记录，Run ID 和时间可以不同。

  ```bash
  pvisor -- /bin/pwd > implicit.txt
  pvisor run -- /bin/pwd > explicit.txt
  ```

  <details>
  <summary>自动回归断言（由脚本执行）</summary>

  <!-- pvisor-assert -->

  ```bash
  diff implicit.txt explicit.txt
  test "$(cat implicit.txt)" = "$(cd "$PVISOR_CASE_WORKSPACE" && pwd -P)"
  bundle_expect run.agent pwd
  ```

  </details>

- [ ] **A03：Run 名称和 stdio capture**

  建议场景：适合第一次使用 pVisor、确认命令和 Run 身份。

  用途：为这次运行命名，并将 Agent 输出保存到运行结果。`--name smoke` 指定显示名，`--stdio capture` 开启输出采集。

  预期：Run 名称为 `smoke`，结果中的标准输出为 `hello`，未被截断。

  ```bash
  pvisor --name smoke --stdio capture -- /bin/sh -c 'printf hello'
  ```

  <details>
  <summary>自动回归断言（由脚本执行）</summary>

  <!-- pvisor-assert -->

  ```bash
  bundle_expect run.agent smoke
  bundle_expect run.output.stdout hello
  bundle_expect run.output.stdout_truncated false
  ```

  </details>

- [ ] **A04：超时**

  建议场景：适合第一次使用 pVisor、确认命令和 Run 身份。

  用途：给运行设置墙钟超时。`100ms` 是从运行开始计时的持续时间，不是 CPU 时间；命令故意睡眠 10 秒。

  预期：pVisor 非零退出，运行结果的失败类型为 `deadline_exceeded`。

  <!-- pvisor-case: expect=nonzero -->

  ```bash
  pvisor --timeout 100ms -- /bin/sleep 10
  ```

  <details>
  <summary>自动回归断言（由脚本执行）</summary>

  <!-- pvisor-assert -->

  ```bash
  bundle_expect run.state failed
  bundle_expect run.failure.kind deadline_exceeded
  bundle_expect run.failure.retryable false
  ```

  </details>

- [ ] **A05：严格执行模式拒绝 best-effort 边界**

  建议场景：适合第一次使用 pVisor、确认命令和 Run 身份。

  用途：要求严格执行能力检查。`--strict` 不接受所请求能力缺少强制执行证据；这里同时要求禁止网络。

  准备：Linux user/mount namespace 可用。

  预期：当前 host 执行路径在启动 Agent 前拒绝请求，并列出缺少执行证据的能力。此例验证拒绝路径，不代表 strict 在所有 executor 上都不可用。

  <!-- pvisor-case: expect=nonzero requires=rootless -->

  ```bash
  pvisor --strict --overlaynet-deny-all -- /bin/true
  ```

  <details>
  <summary>自动回归断言（由脚本执行）</summary>

  <!-- pvisor-assert -->

  ```bash
  stdout_has "lacks enforced evidence for requested capability dimensions"
  ```

  </details>

- [ ] **A06：显式环境投影**

  建议场景：适合第一次使用 pVisor、确认命令和 Run 身份。

  用途：只把指定的宿主环境变量传给子进程。变量仅为这条命令设置，通过 `--pass-env` 显式允许投影。

  预期：子进程可见 `TEST_PVISOR_VALUE=visible`；运行记录列出这个变量，但不声明整体继承宿主环境。

  ```bash
  TEST_PVISOR_VALUE=visible pvisor --pass-env TEST_PVISOR_VALUE -- /usr/bin/env
  ```

  <details>
  <summary>自动回归断言（由脚本执行）</summary>

  <!-- pvisor-assert -->

  ```bash
  stdout_has "TEST_PVISOR_VALUE=visible"
  bundle_contains environment.projected_keys TEST_PVISOR_VALUE
  bundle_expect environment.inherits_host false
  ```

  </details>

### B. 资源限制

这一组展示“请求限制”和“实际强制”之间的区别。B01 用于查看完整配置，B02 才真正尝试触发文件大小限制。

- [ ] **B01：组合使用所有资源限制**

  建议场景：适合需要控制或验证资源限制的任务。

  用途：组合设置内存、进程数、CPU 时间、打开文件数和单文件大小。`MiB` 是二进制单位；`--max-cpu-time` 与墙钟超时不同。

  预期：命令成功退出，五个请求值出现在运行记录中，同时报告生效值和限制机制。`/bin/true` 不消耗这些额度，此例不测试超限行为。

  ```bash
  pvisor \
    --memory 256MiB \
    --max-processes 32 \
    --max-cpu-time 5s \
    --max-open-files 128 \
    --max-file-size 1MiB \
    -- /bin/true
  ```

  <details>
  <summary>自动回归断言（由脚本执行）</summary>

  <!-- pvisor-assert -->

  ```bash
  bundle_expect resources.requested.memory_bytes 268435456
  bundle_expect resources.requested.processes 32
  bundle_expect resources.requested.cpu_time_ms 5000
  bundle_expect resources.requested.open_files 128
  bundle_expect resources.requested.file_size_bytes 1048576
  bundle_expect resources.effective.file_size_bytes 1048576
  bundle_contains resources.mechanisms rlimit
  ```

  </details>

- [ ] **B02：文件大小限制实际生效**

  建议场景：适合需要控制或验证资源限制的任务。

  用途：验证单文件大小限制：将上限设为 1KiB，再尝试用 `dd` 写入 4KiB。

  预期：写入命令失败，落盘文件如果存在，其大小不超过 1024 字节；运行结果记录进程退出失败。

  <!-- pvisor-case: expect=nonzero -->

  ```bash
  pvisor --max-file-size 1KiB -- /bin/sh -c 'dd if=/dev/zero of=large bs=4096 count=1'
  ```

  <details>
  <summary>自动回归断言（由脚本执行）</summary>

  <!-- pvisor-assert -->

  ```bash
  bundle_expect resources.requested.file_size_bytes 1024
  bundle_expect run.state failed
  bundle_expect run.failure.kind process_exit
  test ! -f large || [ "$(wc -c < large)" -le 1024 ]
  ```

  </details>

- [ ] **B03：内存参数短别名**

  建议场景：适合需要控制或验证资源限制的任务。

  用途：使用 `--memory` 的别名 `--mem`，为一个简单命令设置 256MiB 内存额度。

  预期：命令成功，记录中的请求值为 268435456 字节，与 `--memory 256MiB` 一致。

  ```bash
  pvisor --mem 256MiB -- /bin/true
  ```

  <details>
  <summary>自动回归断言（由脚本执行）</summary>

  <!-- pvisor-assert -->

  ```bash
  bundle_expect resources.requested.memory_bytes 268435456
  ```

  </details>

- [ ] **B04：Stage 总大小限制**

  建议场景：适合需要控制或验证资源限制的任务。

  用途：为持久 stage 请求 1GiB 的总大小限制。它限制的是 stage 总量，和 B02 的单个文件大小不是同一个概念。

  准备：Linux user/mount namespace 可用。

  预期：stage 成功建立并保存在指定路径。此例只验证参数可用和目录建立；当前产物未记录该上限，也未在此例中尝试写满 stage。

  <!-- pvisor-case: requires=rootless -->

  ```bash
  pvisor --stage /tmp/pvisor-cases/limited-stage --max-stage-size 1GiB -- /bin/true
  ```

  <details>
  <summary>自动回归断言（由脚本执行）</summary>

  <!-- pvisor-assert -->

  ```bash
  bundle_expect filesystem.state staged
  bundle_expect safety.filesystem_changes_staged true
  record_expect storage "$PVISOR_CASE_ROOT/limited-stage"
  ```

  </details>

### C. Stage 与 whole-rootfs

当你希望 Agent 可以自由修改文件、但不污染当前 workspace 时使用这一组。C01 是最常用的持久模式；C02/C03 适合一次性试运行。

- [ ] **C01：持久 stage**

  建议场景：适合隔离文件变更、保留 stage 或验证 whole-rootfs 的任务。

  用途：把本次运行的文件改动放进一个保留的 stage。命令在 workspace 里创建 `result.txt`。

  准备：Linux user/mount namespace 可用。

  预期：原 workspace 没有 `result.txt`；变更清单中出现该文件，指定 stage 内保留 `run-bundle.json`，便于之后查看。

  <!-- pvisor-case: requires=rootless -->

  ```bash
  pvisor --stage /tmp/pvisor-cases/stage-keep -- /bin/sh -c 'printf changed > result.txt'
  ```

  <details>
  <summary>自动回归断言（由脚本执行）</summary>

  <!-- pvisor-assert -->

  ```bash
  bundle_expect filesystem.state staged
  bundle_contains filesystem.changes result.txt
  bundle_expect safety.filesystem_non_bypassable true
  test ! -e result.txt
  test -f "$PVISOR_CASE_ROOT/stage-keep/run-bundle.json"
  ```

  </details>

- [ ] **C02：自动临时 stage**

  建议场景：适合隔离文件变更、保留 stage 或验证 whole-rootfs 的任务。

  用途：运行一次不需要保留改动的任务。`--stage drop` 自动选择系统临时目录，退出后删除该目录。

  准备：Linux user/mount namespace 可用。

  预期：命令成功，日志中给出的临时存储目录已删除，原 workspace 也没有新建的文件。

  <!-- pvisor-case: requires=rootless -->

  ```bash
  pvisor --stage drop -- /bin/sh -c 'printf changed > result.txt'
  ```

  <details>
  <summary>自动回归断言（由脚本执行）</summary>

  <!-- pvisor-assert -->

  ```bash
  storage=$(grep -m1 '^Run storage: ' "$PVISOR_CASE_STDOUT" | cut -d' ' -f3-)
  test -n "$storage"
  test ! -e "$storage"
  test ! -e result.txt
  ```

  </details>

- [ ] **C03：指定自动删除目录**

  建议场景：适合隔离文件变更、保留 stage 或验证 whole-rootfs 的任务。

  用途：自己选择临时 stage 路径，但仍要求运行结束后自动删除。示例先创建一个空目录。

  准备：Linux user/mount namespace 可用。

  预期：运行完成后 `stage-drop` 目录不存在。请使用专用空目录，不要指定含有用户文件的目录。

  <!-- pvisor-case: requires=rootless -->

  ```bash
  mkdir -p /tmp/pvisor-cases/stage-drop
  pvisor --stage drop:/tmp/pvisor-cases/stage-drop -- /bin/true
  ```

  <details>
  <summary>自动回归断言（由脚本执行）</summary>

  <!-- pvisor-assert -->

  ```bash
  test ! -e "$PVISOR_CASE_ROOT/stage-drop"
  ```

  </details>

- [ ] **C04：拒绝删除非空目录**

  建议场景：适合隔离文件变更、保留 stage 或验证 whole-rootfs 的任务。

  用途：验证误用保护：把 `drop:` 指向已有用户文件的目录。

  预期：启动前报错，提示临时 stage 必须为空；原有的 `user-file` 保持完整。此例预期非零退出。

  <!-- pvisor-case: expect=nonzero -->

  ```bash
  mkdir -p /tmp/pvisor-cases/not-owned
  touch /tmp/pvisor-cases/not-owned/user-file
  pvisor --stage drop:/tmp/pvisor-cases/not-owned -- /bin/true
  ```

  <details>
  <summary>自动回归断言（由脚本执行）</summary>

  <!-- pvisor-assert -->

  ```bash
  stdout_has "temporary stage must be empty before use"
  test -f "$PVISOR_CASE_ROOT/not-owned/user-file"
  ```

  </details>

- [ ] **C05：whole-rootfs 捕获与 tmpfs 隔离**

  建议场景：适合隔离文件变更、保留 stage 或验证 whole-rootfs 的任务。

  用途：比较 workspace 写入和 sandbox 临时目录写入。前者用于保留任务改动，后者只供本次运行临时使用。

  准备：Linux user/mount namespace 可用。

  预期：workspace 的改动出现在 stage，宿主 workspace 和宿主 `/tmp` 均不出现新文件。这里不验证 workspace 以外普通 rootfs 路径的持久化。

  <!-- pvisor-case: requires=rootless -->

  ```bash
  pvisor --stage /tmp/pvisor-cases/root-stage -- /bin/sh -c \
    'printf workspace > ./workspace-change; printf tmp > /tmp/pvisor-root-change'
  ```

  <details>
  <summary>自动回归断言（由脚本执行）</summary>

  <!-- pvisor-assert -->

  ```bash
  bundle_contains filesystem.changes workspace-change
  test ! -e workspace-change
  test ! -e /tmp/pvisor-root-change
  ```

  </details>

### D. OverlayFS 与 Host 安全边界

D01 讲视图层组合，D02/D03 讲 host executor 的默认隔离和 workspace 可见性。生产使用前建议先阅读这组三个例子。

- [ ] **D01：高级 OverlayFS 组合**

  建议场景：适合检查 OverlayFS 视图和 host 安全边界。

  用途：把宿主的两个目录依次叠加到工作区视图，并指定 Agent 看到的路径。`directory` 选择目录后端，`manual` 表示退出后不自动应用改动。

  准备：Linux user/mount namespace 可用。

  预期：记录的目标为 `view`，从顶层到底层依次为 `layer`、`base`、当前 workspace。目录为空，因此此例检查配置顺序，不检查同名文件覆盖内容。

  <!-- pvisor-case: requires=rootless -->

  ```bash
  mkdir -p /tmp/pvisor-cases/base /tmp/pvisor-cases/layer "$PWD/view"
  pvisor \
    --stage /tmp/pvisor-cases/composed-stage \
    --overlayfs-path "$PWD/view" \
    --overlayfs-compose /tmp/pvisor-cases/base \
    --overlayfs-compose /tmp/pvisor-cases/layer \
    --overlayfs-backend directory \
    --overlayfs-commit manual \
    -- /bin/true
  ```

  <details>
  <summary>自动回归断言（由脚本执行）</summary>

  <!-- pvisor-assert -->

  ```bash
  bundle_expect filesystem.state staged
  record_expect overlay_lowers.0 "$PVISOR_CASE_ROOT/layer"
  record_expect overlay_lowers.1 "$PVISOR_CASE_ROOT/base"
  record_expect overlay_lowers.2 "$PVISOR_CASE_WORKSPACE"
  ```

  </details>

- [ ] **D02：显式 host executor**

  建议场景：适合检查 OverlayFS 视图和 host 安全边界。

  用途：显式选择 host executor，观察当前系统上的隔离类型。

  预期：在支持 user namespace 的 Linux 上，记录为 `rootless_process`。本例断言针对该环境；若系统正常降级到 host process，这组断言会失败，需要结合 warning 判断。

  ```bash
  pvisor --executor host -- /bin/true
  ```

  <details>
  <summary>自动回归断言（由脚本执行）</summary>

  <!-- pvisor-assert -->

  ```bash
  bundle_expect run.executor.kind process
  bundle_expect run.executor.isolation rootless_process
  bundle_expect safety.host_process false
  ```

  </details>

- [ ] **D03：host stage 隐藏原 workspace**

  建议场景：适合检查 OverlayFS 视图和 host 安全边界。

  用途：观察启用 stage 后子进程的 cwd 和 procfs 路径。三条命令的输出保存到 `views.txt`。

  准备：Linux user/mount namespace 可用。

  预期：cwd 指向 stage 的 merged 目录，输出中不出现原 workspace 路径。此例只检查路径显示，不证明所有原路径或继承 FD 访问都已被禁止。

  <!-- pvisor-case: requires=rootless -->

  ```bash
  pvisor --executor host --stage /tmp/pvisor-cases/host-stage -- /bin/sh -c \
    'pwd; readlink /proc/self/root; readlink /proc/self/cwd' > views.txt
  ```

  <details>
  <summary>自动回归断言（由脚本执行）</summary>

  <!-- pvisor-assert -->

  ```bash
  merged="$PVISOR_CASE_ROOT/host-stage/merged"
  test "$(sed -n 1p views.txt)" = "$merged"
  test "$(sed -n 3p views.txt)" = "$merged"
  ! grep -Fq -- "$(cd "$PVISOR_CASE_WORKSPACE" && pwd -P)" views.txt
  ```

  </details>

### E. VM 与 rootfs

需要更强边界、独立 guest kernel 或 OCI rootfs 时使用 VM。E01 最接近“直接运行”，E02/E03 展示目录和镜像来源，E04/E05 再加入资源与 stage。

- [ ] **E01：`--vm` 简写与 host rootfs**

  建议场景：适合需要 VM guest kernel、独立 rootfs 或更强隔离的任务。

  用途：用 `--vm` 选择 VM executor，并以宿主根目录作为 guest rootfs。该方式扩大了 guest 可读取的宿主文件范围，只应在可信测试环境使用。

  准备：Linux；可访问 /dev/kvm。

  预期：guest 输出与宿主 workspace 相同的绝对路径。运行结果标记为虚拟机，网络使用 pVisor 的 smoltcp 驱动。

  <!-- pvisor-case: requires=linux,kvm -->

  ```bash
  pvisor --vm --rootfs host -- /bin/pwd > guest-cwd.txt
  ```

  <details>
  <summary>自动回归断言（由脚本执行）</summary>

  <!-- pvisor-assert -->

  ```bash
  test "$(cat guest-cwd.txt)" = "$(cd "$PVISOR_CASE_WORKSPACE" && pwd -P)"
  bundle_expect run.executor.kind virtual_machine
  bundle_expect run.executor.isolation virtual_machine
  bundle_expect network.interception.driver vm-smoltcp
  bundle_expect network.interception.strength non-bypassable
  bundle_expect safety.network_non_bypassable true
  ```

  </details>

- [ ] **E02：显式 VM executor 与目录 rootfs**

  建议场景：适合需要 VM guest kernel、独立 rootfs 或更强隔离的任务。

  用途：已有 Linux rootfs 时，直接把目录交给 VM 使用。目录内需要有可执行的 `/bin/pwd` 及其运行依赖。

  准备：Linux；可访问 /dev/kvm；准备好 Linux rootfs，并为脚本设置 PVISOR_CASE_ROOTFS。

  预期：虚拟机成功执行命令，guest cwd 与宿主 workspace 路径一致。

  <!-- pvisor-case: requires=linux,kvm,rootfs -->

  ```bash
  pvisor --executor vm --rootfs /path/to/rootfs -- /bin/pwd > guest-cwd.txt
  ```

  <details>
  <summary>自动回归断言（由脚本执行）</summary>

  <!-- pvisor-assert -->

  ```bash
  test "$(cat guest-cwd.txt)" = "$(cd "$PVISOR_CASE_WORKSPACE" && pwd -P)"
  bundle_expect run.executor.isolation virtual_machine
  ```

  </details>

- [ ] **E03：image rootfs**

  建议场景：适合需要 VM guest kernel、独立 rootfs 或更强隔离的任务。

  用途：使用 OCI 镜像准备 VM 的 rootfs，不依赖 Docker/Podman daemon。将 `image=` 后的占位符替换为可获取的镜像引用。

  准备：Linux；可访问 /dev/kvm；为脚本设置 PVISOR_CASE_IMAGE。

  预期：镜像准备后启动 VM，guest 的工作目录与宿主 workspace 路径一致。

  <!-- pvisor-case: requires=linux,kvm,image -->

  ```bash
  pvisor --vm --rootfs image=/path/to/image -- /bin/pwd > guest-cwd.txt
  ```

  <details>
  <summary>自动回归断言（由脚本执行）</summary>

  <!-- pvisor-assert -->

  ```bash
  test "$(cat guest-cwd.txt)" = "$(cd "$PVISOR_CASE_WORKSPACE" && pwd -P)"
  bundle_expect run.executor.isolation virtual_machine
  ```

  </details>

- [ ] **E04：VM 资源和 firmware 配置**

  建议场景：适合需要 VM guest kernel、独立 rootfs 或更强隔离的任务。

  用途：使用已有 rootfs 启动 VM，同时指定 firmware 目录、2GiB 内存和 2 个虚拟 CPU。

  准备：Linux；可访问 /dev/kvm；准备好 Linux rootfs，并为脚本设置 PVISOR_CASE_ROOTFS；libkrunfw 目录或本机缓存可用。

  预期：VM 成功运行，内存请求记录为 2147483648 字节。这里使用目录 rootfs，`--image-store` 不会触发镜像下载；CPU 数量未由本例断言核验。

  <!-- pvisor-case: requires=linux,kvm,rootfs,firmware -->

  ```bash
  pvisor --vm \
    --rootfs /path/to/rootfs \
    --image-store /tmp/pvisor-cases/images \
    --vm-library-dir /path/to/libkrunfw \
    --memory 2GiB \
    --cpu 2 \
    -- /bin/true
  ```

  <details>
  <summary>自动回归断言（由脚本执行）</summary>

  <!-- pvisor-assert -->

  ```bash
  bundle_expect run.executor.isolation virtual_machine
  bundle_expect resources.requested.memory_bytes 2147483648
  ```

  </details>

- [ ] **E05：VM workspace 与 whole-rootfs stage 组合**

  建议场景：适合需要 VM guest kernel、独立 rootfs 或更强隔离的任务。

  用途：在 VM 镜像运行基础上增加持久 stage，并显式要求工作区视图位于宿主 cwd 的同一路径。

  准备：Linux；可访问 /dev/kvm；为脚本设置 PVISOR_CASE_IMAGE。

  预期：guest cwd 保持一致，stage 保留在 `vm-stage`。本例只执行 `pwd`，验证路径和 stage 建立，不验证写入捕获。

  <!-- pvisor-case: requires=linux,kvm,image -->

  ```bash
  pvisor --vm \
    --rootfs image=/path/to/image \
    --stage /tmp/pvisor-cases/vm-stage \
    --overlayfs-path "$PWD" \
    -- /bin/pwd > guest-cwd.txt
  ```

  <details>
  <summary>自动回归断言（由脚本执行）</summary>

  <!-- pvisor-assert -->

  ```bash
  test "$(cat guest-cwd.txt)" = "$(cd "$PVISOR_CASE_WORKSPACE" && pwd -P)"
  bundle_expect filesystem.state staged
  record_expect storage "$PVISOR_CASE_ROOT/vm-stage"
  ```

  </details>

- [ ] **E06：拒绝 executor 冲突**

  建议场景：适合需要 VM guest kernel、独立 rootfs 或更强隔离的任务。

  用途：验证互相冲突的 executor 参数不能一起使用：`--vm` 选择虚拟机，`--executor host` 却选择宿主。

  预期：参数归一化阶段失败，错误信息明确指出 `--vm` 与非 VM executor 冲突。

  <!-- pvisor-case: expect=nonzero -->

  ```bash
  pvisor --vm --executor host --rootfs host -- /bin/true
  ```

  <details>
  <summary>自动回归断言（由脚本执行）</summary>

  <!-- pvisor-assert -->

  ```bash
  stdout_has "--vm cannot be combined with a non-vm --executor"
  ```

  </details>

### F. Container

需要复用 OCI rootfs、但不想运行 Docker/Podman daemon 时使用原生 OCI container。请按 F01 → F04 逐步增加复杂度；F04 适合验证跨 ABI 注入和 mount 配置。

这些例子使用原生 OCI bundle，由 pVisor 准备文件系统并调用 runc/crun，
不依赖 Docker/Podman daemon。F01–F03 使用默认 runtime crun，F04 显式选择 runc。
镜像或目录需与本机架构兼容；如果当前 pVisor 是动态链接构建，guest 必须提供
相应的动态加载器和库，否则应像 F04 一样指定兼容的静态构建。
默认注入当前 pVisor，不需要在最小命令中显式指定 binary。

当前容器执行仍有 OCI 命令行兼容性问题，这些例子可能在 runner 启动阶段失败。
下面保留期望成功的命令和断言，便于重构完成后直接回归。

- [ ] **F01：最小 container Run**

  建议场景：适合由 runc/crun 直接启动 OCI 容器的任务。

  用途：以 OCI 镜像启动最小容器运行。`--executor container` 选择原生 OCI runtime，`--rootfs image=...` 指定容器文件系统来源。

  准备：OCI runtime 可运行，并为脚本设置 PVISOR_CASE_CONTAINER_IMAGE。

  预期：pVisor 准备 OCI bundle、注入自身并执行 `/bin/true`，运行结果记录为 container 且退出码为 0。

  <!-- pvisor-case: requires=container -->

  ```bash
  pvisor --executor container --rootfs image=alpine:latest -- /bin/true
  ```

  <details>
  <summary>自动回归断言（由脚本执行）</summary>

  <!-- pvisor-assert -->

  ```bash
  bundle_expect run.executor.kind container
  bundle_expect run.state completed
  bundle_expect run.exit_code 0
  ```

  </details>

- [ ] **F02：container rootfs 与隔离网络**

  建议场景：适合由 runc/crun 直接启动 OCI 容器的任务。

  用途：在 F01 基础上只增加 `--container-network none`，让容器使用独立的网络 namespace，不配置外部连接。

  准备：OCI runtime 可运行，并为脚本设置 PVISOR_CASE_CONTAINER_IMAGE。

  预期：容器中的 `/bin/true` 成功退出。此命令不发起网络请求，因此断言只检查启动成功，不验证网络是否能被绕过。

  <!-- pvisor-case: requires=container -->

  ```bash
  pvisor --executor container \
    --rootfs image=alpine:latest \
    --container-network none \
    -- /bin/true
  ```

  <details>
  <summary>自动回归断言（由脚本执行）</summary>

  <!-- pvisor-assert -->

  ```bash
  bundle_expect run.executor.kind container
  bundle_expect run.state completed
  ```

  </details>

- [ ] **F03：使用宿主 rootfs 的 OCI bundle**

  建议场景：适合由 runc/crun 直接启动 OCI 容器的任务。

  用途：不提供容器镜像，直接以宿主 `/` 作为只读 lower。pVisor 会先建立独立 synthetic rootfs，再把宿主标准目录以只读方式映射进去。

  准备：Linux、可运行的 OCI runtime，以及允许 rootless container 的 user namespace。

  预期：以指定目录作为容器根文件系统执行命令，不需要配置镜像仓库。使用专用测试 rootfs，不要把宿主 `/` 当作此例的测试目录。

  <!-- pvisor-case: requires=container-runtime,linux -->

  ```bash
  pvisor --executor container \
    --rootfs host \
    --container-network none \
    -- /bin/true
  ```

  <details>
  <summary>自动回归断言（由脚本执行）</summary>

  <!-- pvisor-assert -->

  ```bash
  bundle_expect run.executor.kind container
  bundle_expect run.state completed
  ```

  </details>

- [ ] **F04：显式 OCI runtime 与高级 container 参数**

  建议场景：适合由 runc/crun 直接启动 OCI 容器的任务。

  用途：显式覆盖高级容器选项：使用 runc 和指定 pVisor 构建，声明平台、uid/gid、工作目录、只读 rootfs，并把宿主目录绑定到 `/workspace`。

  准备：OCI runtime 可运行，并为脚本设置 PVISOR_CASE_CONTAINER_IMAGE。

  预期：容器成功退出。`read_only=false` 使绑定目录可写，即使 rootfs 只读；`--container-workdir` 在 Run 没有 cwd 时才作为回退。此例仅检查组合启动，未分别检查用户身份和读写行为。

  <!-- pvisor-case: requires=container -->

  ```bash
  pvisor --executor container \
    --container-runtime runc \
    --rootfs image=alpine:latest \
    --container-pvisor-binary ./target/release/pvisor \
    --container-platform linux/amd64 \
    --container-network none \
    --container-workdir /workspace \
    --container-user 1000:1000 \
    --container-read-only-rootfs \
    --container-mount 'source="/tmp/pvisor-cases/workspace",target="/workspace",read_only=false' \
    -- /bin/true
  ```

  <details>
  <summary>自动回归断言（由脚本执行）</summary>

  <!-- pvisor-assert -->

  ```bash
  bundle_expect run.executor.kind container
  bundle_expect run.state completed
  ```

  </details>

### G. OverlayNet

这一组只讨论网络边界。proxy 适合需要 host Gateway 的协作式访问，VM auto 和 host deny-all 才适合需要更强网络边界的场景。

- [ ] **G01：启用默认 proxy**

  建议场景：适合配置出站网络、代理访问或禁止网络的任务。

  用途：只给出 `--overlaynet`，省略值时启用默认 proxy 模式。

  预期：记录为 explicit-proxy，拦截强度为 cooperative。它只约束经过代理的流量，不能据此认为直接 socket 已被禁止。

  ```bash
  pvisor --overlaynet -- /bin/true
  ```

  <details>
  <summary>自动回归断言（由脚本执行）</summary>

  <!-- pvisor-assert -->

  ```bash
  bundle_expect network.interception.driver explicit-proxy
  bundle_expect network.interception.strength cooperative
  bundle_contains artifacts capture
  ```

  </details>

- [ ] **G02：显式 proxy 地址和 mode**

  建议场景：适合配置出站网络、代理访问或禁止网络的任务。

  用途：显式选择 proxy，并指定代理监听地址。手工运行时确保 18080 端口未被占用；脚本会替换为空闲端口。

  预期：记录的 OverlayNet 监听地址与请求一致，网络驱动为 explicit-proxy。

  ```bash
  pvisor --overlaynet proxy --overlaynet-listen 127.0.0.1:18080 -- /bin/true
  ```

  <details>
  <summary>自动回归断言（由脚本执行）</summary>

  <!-- pvisor-assert -->

  ```bash
  record_expect overlaynet_listen 127.0.0.1:18080
  bundle_expect network.interception.driver explicit-proxy
  ```

  </details>

- [ ] **G03：allow、deny 和带宽限制组合**

  建议场景：适合配置出站网络、代理访问或禁止网络的任务。

  用途：组合配置允许目标、拒绝网段和针对目标的带宽上限。只有通过代理的流量才受这些规则约束。

  预期：记录为 allowlist 模式，允许 `api.example.com:443`，拒绝 `10.0.0.0/8`，并把 `1mbps` 记录为每秒 125000 字节。本例不实际发请求。

  ```bash
  pvisor --overlaynet proxy \
    --overlaynet-allow api.example.com:443 \
    --overlaynet-deny 10.0.0.0/8 \
    --overlaynet-limit api.example.com=1mbps \
    -- /bin/true
  ```

  <details>
  <summary>自动回归断言（由脚本执行）</summary>

  <!-- pvisor-assert -->

  ```bash
  bundle_expect network.policy.mode allowlist
  bundle_expect network.policy.rules.0.host api.example.com
  bundle_expect network.policy.rules.0.ports.0 443
  bundle_expect network.policy.deny_rules.0.host 10.0.0.0/8
  bundle_expect network.policy.limits.0.host api.example.com
  bundle_expect network.policy.limits.0.bytes_per_second 125000
  ```

  </details>

- [ ] **G04：deny-all 不可通过环境变量绕过**

  建议场景：适合配置出站网络、代理访问或禁止网络的任务。

  用途：验证禁止网络后，清除常见代理变量仍不能访问外网。需要宿主安装 curl；命令故意发起网络请求。

  准备：Linux user/mount namespace 可用；安装 curl。

  预期：命令失败，结果记录 no-network 和不可绕过边界。外网自身不可用也会使 curl 失败，因此本例不能单独证明隔离有效。

  <!-- pvisor-case: expect=nonzero requires=rootless,curl -->

  ```bash
  pvisor --overlaynet-deny-all -- /bin/sh -c \
    'unset HTTP_PROXY HTTPS_PROXY ALL_PROXY http_proxy https_proxy all_proxy; curl --max-time 2 https://example.com'
  ```

  <details>
  <summary>自动回归断言（由脚本执行）</summary>

  <!-- pvisor-assert -->

  ```bash
  bundle_expect network.policy.mode no-network
  bundle_expect safety.network_non_bypassable true
  bundle_expect run.state failed
  ```

  </details>

- [ ] **G05：VM OverlayNet auto**

  建议场景：适合配置出站网络、代理访问或禁止网络的任务。

  用途：显式为 VM 选择 OverlayNet auto，使流量经过虚拟机的 smoltcp 网络驱动。

  准备：Linux；可访问 /dev/kvm；准备好 Linux rootfs，并为脚本设置 PVISOR_CASE_ROOTFS。

  预期：记录为 vm-smoltcp 和 non-bypassable，而不是 host 的协作式代理。

  <!-- pvisor-case: requires=linux,kvm,rootfs -->

  ```bash
  pvisor --vm --rootfs /path/to/rootfs --overlaynet auto -- /bin/true
  ```

  <details>
  <summary>自动回归断言（由脚本执行）</summary>

  <!-- pvisor-assert -->

  ```bash
  bundle_expect network.interception.driver vm-smoltcp
  bundle_expect network.interception.strength non-bypassable
  ```

  </details>

- [ ] **G06：关闭 OverlayNet 时拒绝策略参数**

  建议场景：适合配置出站网络、代理访问或禁止网络的任务。

  用途：检查关闭 OverlayNet 后不能继续提供网络策略。

  预期：启动前失败，错误提示策略需要 `auto` 或 `proxy`。如果只想关闭 OverlayNet，请不要附带 allow/deny/limit。

  <!-- pvisor-case: expect=nonzero -->

  ```bash
  pvisor --overlaynet off --overlaynet-allow example.com:443 -- /bin/true
  ```

  <details>
  <summary>自动回归断言（由脚本执行）</summary>

  <!-- pvisor-assert -->

  ```bash
  stdout_has "OverlayNet policy options require --overlaynet auto or proxy"
  ```

  </details>

### H. Gateway 与记录

需要审计、模型路由或轨迹回放时使用这一组。H01 是 Gateway 配置示例，H02 适合轻量 JSONL，H03 适合 Lance warehouse。

- [ ] **H01：Gateway capture 完整组合**

  建议场景：适合接入 Gateway、模型路由或记录轨迹的任务。

  用途：为需要模型请求记录的 Agent 配置 Gateway。示例设置路由、管理监听端口、完整记录级别、会话头、诊断输出和 Markdown 投影，并保留 stage。

  准备：Linux user/mount namespace 可用。

  预期：Gateway 与 stage 成功建立。示例上游是占位地址，`/bin/true` 不发送模型请求；此例不验证对话内容。管理监听地址与运行记录中的 `gateway_listen` 不是同一个服务地址。

  <!-- pvisor-case: requires=rootless -->

  ```bash
  pvisor \
    --stage /tmp/pvisor-cases/gateway-stage \
    --gateway-mode capture \
    --gateway-admin-listen 127.0.0.1:19090 \
    --gateway-level full \
    --gateway-session-header X-Session-ID \
    --gateway-debug \
    --gateway-stream-markdown \
    --gateway-route 'name="default",upstream="https://example.com/v1"' \
    -- /bin/true
  ```

  <details>
  <summary>自动回归断言（由脚本执行）</summary>

  <!-- pvisor-assert -->

  ```bash
  record_get gateway_listen | grep -Eq '^127\.0\.0\.1:[0-9]+$'
  bundle_expect network.interception.driver explicit-proxy
  bundle_expect filesystem.state staged
  ```

  </details>

- [ ] **H02：JSON 记录**

  建议场景：适合接入 Gateway、模型路由或记录轨迹的任务。

  用途：把本次运行事件写成轻量的 JSONL 文件，不启动 Lance warehouse。

  预期：指定文件非空，首行具有 JSON 对象形式。每行应是一个事件；本例只检查文件建立和首行外观。

  ```bash
  pvisor --record-format json --record-destination /tmp/pvisor-cases/events.jsonl -- /bin/true
  ```

  <details>
  <summary>自动回归断言（由脚本执行）</summary>

  <!-- pvisor-assert -->

  ```bash
  test -s "$PVISOR_CASE_ROOT/events.jsonl"
  head -n 1 "$PVISOR_CASE_ROOT/events.jsonl" | grep -q '^{'
  ```

  </details>

- [ ] **H03：Lance 记录**

  建议场景：适合接入 Gateway、模型路由或记录轨迹的任务。

  用途：需要 warehouse 形式的记录时选择 Lance。先安装相应构建和运行依赖，再让脚本启用该例。

  准备：Lance 运行依赖齐备，脚本设置 PVISOR_CASE_LANCE=1。

  预期：运行完成，指定位置建立 warehouse 目录；本例未查询其中的记录。

  <!-- pvisor-case: requires=lance -->

  ```bash
  pvisor --record-format lance --record-destination /tmp/pvisor-cases/warehouse -- /bin/true
  ```

  <details>
  <summary>自动回归断言（由脚本执行）</summary>

  <!-- pvisor-assert -->

  ```bash
  bundle_expect run.state completed
  test -d "$PVISOR_CASE_ROOT/warehouse"
  ```

  </details>

### I. Spec 与控制面

已有自动化控制面或需要把 RunSpec 作为文件传递时使用这一组。I01 是 TOML 配置，I02 是 JSON 委托，I03 验证无扩展名文件。

- [ ] **I01：TOML config**

  建议场景：适合从 TOML/JSON 文件或控制面执行 RunSpec 的任务。

  用途：把命令写进 TOML 后通过 `--spec` 运行。手工执行前创建 `pvisor.toml`，内容为 `[run]` 下的 `command = ["/bin/true"]`；脚本会预置此文件。

  预期：命令来自配置文件，无需在 CLI 重复；运行正常结束。

  ```bash
  pvisor --spec ./pvisor.toml
  ```

  <details>
  <summary>自动回归断言（由脚本执行）</summary>

  <!-- pvisor-assert -->

  ```bash
  bundle_expect run.state completed
  bundle_expect run.agent true
  ```

  </details>

- [ ] **I02：JSON RunSpec**

  建议场景：适合从 TOML/JSON 文件或控制面执行 RunSpec 的任务。

  用途：执行已准备好的 JSON RunSpec，并把结果原子写入指定文件。手工运行前准备包含 run_id、agent 和 process invocation 的 `run-spec.json`；脚本预置的是运行 `/bin/true` 的 `case-i02`。

  准备：Linux user/mount namespace 可用。

  预期：运行名称为 `case-i02`，`run-result.json` 非空。该委托路径当前只支持 host executor，不套用普通 Run 的 rootless safe profile；不要把此例视为隔离模式示例。

  <!-- pvisor-case: requires=rootless -->

  ```bash
  pvisor --spec ./run-spec.json --result-file ./run-result.json --stage ./delegated-stage
  ```

  <details>
  <summary>自动回归断言（由脚本执行）</summary>

  <!-- pvisor-assert -->

  ```bash
  bundle_expect run.agent case-i02
  bundle_expect run.executor.isolation host_process
  test -s run-result.json
  ```

  </details>

- [ ] **I03：无扩展名 spec**

  建议场景：适合从 TOML/JSON 文件或控制面执行 RunSpec 的任务。

  用途：验证 spec 的识别不依赖扩展名。手工执行时把 I01 的 TOML 内容保存成 `spec-without-extension`；脚本会预置该文件。

  预期：按内容识别 TOML 并完成运行，不要求文件名以 `.toml` 结尾。

  ```bash
  pvisor --spec ./spec-without-extension
  ```

  <details>
  <summary>自动回归断言（由脚本执行）</summary>

  <!-- pvisor-assert -->

  ```bash
  bundle_expect run.state completed
  ```

  </details>

### J. 复杂组合

这些不是入门命令，而是上线前的组合参考：J01 偏 host 安全，J02 偏 VM 生产链路，J03 偏容器链路。遇到问题时请拆回对应的 A–I 场景定位。

- [ ] **J01：host + persistent stage + deny-all + capture + limits**

  建议场景：适合上线前验证多项能力组合的端到端任务。

  用途：组合使用 host stage、禁止网络、输出采集、JSON 事件和资源限制。命令在隔离视图中写入一个结果文件。

  准备：Linux user/mount namespace 可用。

  预期：原 workspace 不变；stage 记录 `result.txt`，结果保存 stdout 和资源请求，事件写入指定 JSONL 文件，网络标记为禁止连接。

  <!-- pvisor-case: requires=rootless -->

  ```bash
  pvisor --name host-full \
    --executor host \
    --stage /tmp/pvisor-cases/host-full \
    --overlaynet-deny-all \
    --stdio capture \
    --record-format json \
    --record-destination /tmp/pvisor-cases/host-full/trajectory/events.jsonl \
    --memory 512MiB \
    --max-processes 64 \
    --max-stage-size 2GiB \
    --max-cpu-time 30s \
    -- /bin/sh -c 'pwd; printf changed > result.txt'
  ```

  <details>
  <summary>自动回归断言（由脚本执行）</summary>

  <!-- pvisor-assert -->

  ```bash
  bundle_expect run.agent host-full
  bundle_contains run.output.stdout "$PVISOR_CASE_ROOT/host-full/merged"
  bundle_expect network.policy.mode no-network
  bundle_expect safety.network_non_bypassable true
  bundle_expect safety.filesystem_changes_staged true
  bundle_contains filesystem.changes result.txt
  bundle_expect resources.requested.memory_bytes 536870912
  bundle_expect resources.requested.processes 64
  bundle_expect resources.requested.cpu_time_ms 30000
  test ! -e result.txt
  test -s "$PVISOR_CASE_ROOT/host-full/trajectory/events.jsonl"
  ```

  </details>

- [ ] **J02：VM + image rootfs + stage + OverlayNet + Gateway**

  建议场景：适合上线前验证多项能力组合的端到端任务。

  用途：在 VM 中运行真实 Agent，同时保留 stage、使用 smoltcp 网络、Gateway 和 Lance 轨迹记录。需提供含 Agent 及其依赖的镜像，并把示例上游替换为实际服务。

  准备：Linux；可访问 /dev/kvm；为脚本设置 PVISOR_CASE_IMAGE；PVISOR_CASE_AGENT 指向 guest 中也可执行的 Agent；Lance 运行依赖齐备，脚本设置 PVISOR_CASE_LANCE=1。

  预期：VM 以请求的内存运行，保留 stage 和轨迹目录。是否产生模型对话取决于 Agent 是否真的调用 Gateway；当前断言不检查对话内容。

  <!-- pvisor-case: requires=linux,kvm,image,agent,lance -->

  ```bash
  pvisor --name vm-full \
    --vm \
    --rootfs image=/path/to/image \
    --stage /tmp/pvisor-cases/vm-full \
    --overlaynet auto \
    --gateway-mode capture \
    --gateway-level dialogue \
    --gateway-route 'name="default",upstream="https://example.com/v1"' \
    --record-format lance \
    --record-destination /tmp/pvisor-cases/vm-full/trajectory \
    --memory 4GiB \
    --cpu 4 \
    -- /usr/local/bin/agent
  ```

  <details>
  <summary>自动回归断言（由脚本执行）</summary>

  <!-- pvisor-assert -->

  ```bash
  bundle_expect run.agent vm-full
  bundle_expect run.executor.isolation virtual_machine
  bundle_expect network.interception.driver vm-smoltcp
  bundle_expect filesystem.state staged
  bundle_expect resources.requested.memory_bytes 4294967296
  test -d "$PVISOR_CASE_ROOT/vm-full/trajectory"
  ```

  </details>

- [ ] **J03：Container + stage + read-only root + no network**

  建议场景：适合上线前验证多项能力组合的端到端任务。

  用途：在容器中组合持久 stage、只读 rootfs、隔离网络和 stdout 采集。只读 rootfs 与可写 stage 是不同层面的设置。

  准备：OCI runtime 可运行，并为脚本设置 PVISOR_CASE_CONTAINER_IMAGE。

  预期：容器成功退出并留下 stage。命令为 `/bin/true`，不会生成文件变更或有意义的 stdout；本例不验证 stage 写入和网络阻断行为。

  <!-- pvisor-case: requires=container -->

  ```bash
  pvisor --name container-full \
    --executor container \
    --rootfs image=alpine:latest \
    --container-read-only-rootfs \
    --container-network none \
    --stage /tmp/pvisor-cases/container-full \
    --stdio capture \
    -- /bin/true
  ```

  <details>
  <summary>自动回归断言（由脚本执行）</summary>

  <!-- pvisor-assert -->

  ```bash
  bundle_expect run.agent container-full
  bundle_expect run.executor.kind container
  bundle_expect run.state completed
  bundle_expect filesystem.state staged
  ```

  </details>
