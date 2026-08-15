# pVisor isolation architecture

This document compares provider mechanisms and their actual security
properties. Choose a provider with the [execution guide](../guides/execution.md)
and interpret guarantees with
[Capabilities and evidence](../concepts/capabilities-and-evidence.md).

> Status: implementation plus roadmap. Linux `pvisor run --safe` implements the
> FUSE + synthetic root + rootless user/mount namespace + Landlock path
> described in section 2. macOS `--safe` implements Seatbelt-enforced staged
> writes and deny-all socket confinement; filesystem reads remain ambient and
> are reported separately.
> Docker and libkrun/KVM transports also exist. A Virtualization.framework
> backend for transparent macOS executable isolation is a researched design,
> not an implemented backend. Seccomp/resource enforcement, LiteBox VFS, and
> Firecracker remain roadmap work unless stated otherwise.

pVisor needs more than one isolation backend. A local coding Agent values fast
startup and an exact view of the developer's workspace; an untrusted tenant
requires a boundary that remains useful after the guest runtime is compromised.
The design therefore separates the **transactional workspace** from the
**enforcement boundary** instead of trying to make one mechanism serve both
roles.

The multiple backends are an implementation portfolio, **not a configuration
surface imposed on the user**. The normal product experience remains:

```bash
pvisor run --safe <agent> [args...]
```

pVisor probes the host, workload, and available placement, selects a backend,
constructs the workspace, and applies the policy. Users do not configure
Landlock rights, mount propagation, UID maps, 9P transports, seccomp JSON,
container capabilities, TAP devices, or microVM images. Expert backend flags
may exist for development and diagnosis, but must not be required for the
normal path.

Easy to use does not mean an invisible security downgrade. If the requested
guarantee cannot be provided, pVisor either chooses another available backend
or returns one actionable error. It never reports a `cwd`-only Run as safely
sandboxed.

## 1. Common model

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

`WorkspaceOverlay` is the data plane for file changes. It provides an isolated
Run view, copy-on-write, whiteouts, and an auditable changeset. It does **not**
by itself prevent a process from opening a path outside that view.

`IsolationBackend` is the security plane. It determines which kernel, syscall
surface, namespace, host paths, file descriptors, and network paths the Agent
can reach. Every backend consumes the same logical workspace and must return a
changeset with the same review/apply/drop semantics.

The following invariants apply to backends that claim complete capability
enforcement. A partial native backend may enforce a smaller dimension only
when the Run Bundle identifies that dimension explicitly and records the
remaining ambient access:

1. Deny by default; every host file, socket, credential, device, and endpoint is
   an explicit capability.
2. The Agent never receives host control-plane credentials or the Docker socket.
3. Only `stdin`, `stdout`, `stderr`, the Run-scoped Agent ABI transport, and
   explicitly granted resource handles cross the execution boundary.
4. The writable workspace is separate from the read-only base. A successful
   process exit never implies permission to apply its changes.
5. Requested and effective enforcement are recorded separately. An enforce
   request fails closed when its backend is unavailable; it never silently
   falls back to the current `cwd`-only behavior.
6. pVisor records enough evidence to audit the boundary: backend/version,
   workspace digest, effective UID/capabilities, kernel feature probes,
   network mode, resource limits, image/rootfs digest, and downgrade reasons.

## 2. Native host paths

### 2.1 Linux: FUSE + Workspace + Landlock

This is the preferred lightweight Linux host path. It keeps today's embedded
FUSE OverlayFS and adds a kernel-enforced, unprivileged filesystem policy to
the Agent process tree.

Landlock is entirely internal: no system policy file, root helper, daemon, or
per-project rule configuration is exposed to the user. pVisor derives the
rules from the workspace, executable/runtime closure, explicit inputs, and
Run-scoped scratch directory.

#### Current implementation

The native Linux safe path is operational for ordinary local executables:

- pVisor self-executes a hidden launcher before Agent code starts;
- the launcher creates one-ID user and private mount namespaces without
  `/etc/subuid`, `newuidmap`, a setuid binary, or a daemon;
- a private tmpfs root bind-projects only the runtime, staged workspace, exact
  device nodes, Run-scoped Agent ABI socket, and explicit capabilities before
  the launcher enters it with `chroot`; arbitrary host pathname Unix sockets
  are therefore absent rather than left to Landlock;
- inherited descriptors above stderr are closed, Landlock ABI v3 handles all
  filesystem rights through `TRUNCATE`, `no_new_privs` is set, namespace and
  ambient capabilities are cleared, and the launcher `exec`s the Agent;
- the writable FUSE merged workspace and explicit read/write capabilities are
  admitted, while the executable and a broad host runtime are read-only;
- `NetworkCapability::Deny` also creates a private network namespace. Public
  and allowlist proxy policies remain cooperative and are not reported as
  non-bypassable;
- any namespace or Landlock setup error terminates before Agent execution with
  a reserved infrastructure result and a Run Bundle downgrade warning.

The broad immutable runtime currently includes existing `/bin`, `/sbin`,
`/usr`, `/lib*`, `/etc`, and the process-local procfs views. This favors
compatibility with shell, Python, Node, and dynamically linked local tools. A
measured runtime-closure builder may narrow it later; the current policy never
makes those hierarchies writable.

The default pVisor dependency graph is independent of the analytical storage
stack: it does not link Lance/DataFusion or protobuf/prost. Durable Lance
Chronicle integration and the Jujutsu OverlayFS upper are separately enabled
by `lance-chronicle` and `jujutsu-overlay`.

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

The pVisor supervisor and FUSE request loop remain outside the chroot and
Landlock domain. The child receives read/write access to the merged workspace,
read/execute access to a runtime, and read-only access to explicit inputs. An
unprojected path is absent from the synthetic root; Landlock independently
enforces access rights over projected hierarchies. Absolute paths are resolved
inside that root rather than redirected into the workspace.

### 2.2 Minimum policy

| Hierarchy | Effective access |
|---|---|
| merged workspace | read, write, create, remove, rename, link as required |
| Agent executable and loader | read, execute |
| required shared libraries and runtime data | read-only |
| explicit input datasets | read-only |
| Run scratch directory | read/write; preferably a size-limited tmpfs |
| pVisor state, pChronicle, source credentials, home directory | denied |
| `/proc`, `/sys`, host sockets | not admitted to Agent access unless explicitly projected or separately virtualized |
| minimal devices | exact null, zero/full, random/urandom, and tty nodes only |

Landlock is additive to normal DAC/ACL/LSM checks; it does not grant an access
the process did not already have. The launcher must negotiate the running
kernel's Landlock ABI and handle all security-relevant rights supported by that
ABI. Older ABIs may lack controls such as cross-directory refer or truncate,
so pVisor must publish the effective guarantee rather than a boolean
"Landlock enabled" flag.

Files opened before `landlock_restrict_self` are not retroactively constrained.
FD hygiene is consequently part of the boundary: prepare directory handles,
close everything not granted, install `no_new_privs` and the ruleset, then
`exec`. This setup belongs in a small auditable launcher, not in a complex
post-fork closure of the multithreaded supervisor.

### 2.3 Security and operational properties

**Strengths**

- No host root or persistent privileged daemon is required.
- Startup and steady-state overhead are small; file contents still use the
  existing FUSE/OverlayFS path.
- Workspace fidelity remains the best of the native host paths, including
  current review, checkpoint, apply, and drop behavior.
- A child can no longer escape merely by using `..` or an absolute host path.

**Limits**

- The Agent still uses the host kernel and its native syscall ABI.
- Landlock is a filesystem access-control layer, not a root filesystem,
  network namespace, resource controller, or complete process sandbox.
- Runtime allowlists are difficult for dynamic language stacks unless pVisor
  builds a minimal runtime bundle.
- FUSE context switches remain on the hot path for workspace I/O.
- Linux only. The macOS sibling path has a different, explicitly narrower
  Seatbelt boundary.

The implementation already combines Landlock with an empty capability set,
`no_new_privs`, rootless user/mount namespaces, and a network namespace for
deny-all Runs. Seccomp, PID namespaces, `rlimit`/cgroup limits, and transparent
enforcement for selective egress remain necessary hardening without changing
the workspace contract.

### 2.4 macOS: FUSE + Seatbelt

The native macOS safe path is operational for ordinary local executables. It
keeps the same staged macFUSE workspace while adding a kernel-enforced Seatbelt
policy around the complete Agent descendant process tree:

- pVisor invokes only the fixed system `/usr/bin/sandbox-exec`, never a PATH
  lookup or a project-supplied wrapper;
- the generated SBPL uses `-D` parameters for every writable path, so a
  workspace name cannot inject policy text;
- path-parameterized `file-write*` rules admit only the mounted staged
  workspace, explicit read-write filesystem capabilities, exact
  terminal/device handles, a Run-owned temporary directory, and a one-time
  setup attestation;
- the hidden launcher writes and unlinks that attestation before `exec` of the
  Agent. A profile compile/apply failure therefore cannot be mistaken for an
  Agent exit and terminates the Run as an infrastructure failure;
- `NetworkCapability::Deny` starts from a deny-by-default profile, blocks IP
  sockets and outbound ambient host Unix sockets, and retains only the exact
  Run-scoped Agent ABI plus Unix IPC rooted in Run-owned directories;
- public and selective proxy modes remain cooperative because the first
  implementation does not yet constrain direct sockets to only the in-process
  proxy endpoint.

The compatibility profile deliberately leaves filesystem reads ambient. This
avoids hard-coding a brittle closure of Homebrew, Xcode, Python, Node, Rustup,
SDK, framework, and user-installed runtime paths. Consequently the Run Bundle
sets `filesystem_write_non_bypassable=true` but keeps
`filesystem_read_non_bypassable=false` and the aggregate
`filesystem_non_bypassable=false`. A future measured runtime-closure mode may
make reads deny-by-default without changing the workspace contract.

Seatbelt improves the local macOS boundary materially, but it is not a VM or a
complete process sandbox: the host kernel, PID namespace, syscall surface, and
resource accounting remain shared. The `sandbox-exec` interface is deprecated
by Apple even though it remains shipped, so pVisor probes the fixed binary and
fails closed instead of promising indefinite platform availability. macFUSE is
still required for transactional staging until an FSKit backend is available.

### 2.5 macOS: Virtualization.framework + host-root overlay

This is the proposed macOS kernel-isolation path for native Mach-O workloads.
It is the macOS analogue of the implemented Linux libkrun full-root executor,
but it cannot use libkrun: a macOS guest must be booted by Apple's
Virtualization.framework on Apple Silicon. The objective is that an executable
sees the invoking host's root filesystem, with all writes captured by the
pVisor upper layer, while executing behind a separate macOS kernel boundary.

The guest's boot disk is not the Agent's logical root. It contains only a
compatible macOS installation and a privileged pVisor guest supervisor. The
host constructs `host / + Run upper` through OverlayFS/macFUSE, exports the
merged view with VirtioFS, and asks the guest supervisor to enter that view
before executing the target:

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
       +-- VZVirtioSocket control and Agent ABI transports
                    |
                    v
          pvisor-guestd (root LaunchDaemon)
            mount VirtioFS at a private path
            chroot into the pVisor root
            setgroups / setgid / setuid
            set cwd, environment, limits, and stdio
            execve host Mach-O
```

The Swift helper is deliberately outside the Rust supervisor. It owns only the
Objective-C/Swift Virtualization.framework lifecycle and converts it into a
small versioned protocol. The existing `RunExecutor` contract remains the
product boundary, so selection, cancellation, evidence, review, checkpoint,
apply, and drop keep the same semantics as other pVisor executors.

#### GhostVM research

[GhostVM](https://github.com/groundwater/GhostVM) is the closest examined
reference implementation. At commit
[`fe88d586`](https://github.com/groundwater/GhostVM/tree/fe88d5862f74ddb05ce79e04028b84c7f70482f6)
it demonstrates the required control-plane primitives:

- `VZMacOSBootLoader`, Mac platform identity, a macOS disk, headless display
  configuration, VirtioFS, and a virtio socket are assembled in one
  [configuration builder](https://github.com/groundwater/GhostVM/blob/fe88d5862f74ddb05ce79e04028b84c7f70482f6/macOS/GhostVMKit/Configuration/VMConfigurationBuilder.swift);
- one helper process owns an active VM and exposes a host Unix-socket API;
- host requests cross `VZVirtioSocket` to a guest agent, which can execute
  native macOS programs;
- a running VirtioFS device can receive a rebuilt directory share through
  [FolderShareService](https://github.com/groundwater/GhostVM/blob/fe88d5862f74ddb05ce79e04028b84c7f70482f6/macOS/GhostVM/Services/FolderShareService.swift);
- VM suspend/resume uses `saveMachineStateTo` and `restoreMachineStateFrom`;
  APFS `clonefile()` creates copy-on-write VM clones in
  [VMController](https://github.com/groundwater/GhostVM/blob/fe88d5862f74ddb05ce79e04028b84c7f70482f6/macOS/GhostVMKit/Operations/VMController.swift#L519).

These are architectural references, not a filesystem-execution solution.
GhostVM boots and executes against its private `disk.img`; VirtioFS directories
remain shares mounted under the normal guest root. Its guest `exec` endpoint is
a user LaunchAgent calling Swift `Process.run()` with buffered stdout and
stderr. It does not chroot, reproduce credentials, stream stdio, forward
signals, control a process group, or expose a pVisor changeset. The pVisor guest
supervisor must therefore be an independently implemented root LaunchDaemon.

The following split is intentional:

| GhostVM mechanism | pVisor decision |
|---|---|
| VM configuration builder | reproduce the minimal headless subset in `pvisor-vz-helper` |
| one helper process per VM | retain for VMM crash and lifecycle isolation |
| host Unix socket plus vsock | retain the topology; use a bounded, versioned, streaming protocol |
| runtime VirtioFS replacement | adapt to one stable pVisor-root tag |
| VM suspend/resume | use to amortize boot, with strict template compatibility |
| APFS VM clone | optionally use for creation of a clean boot template |
| GhostTools command execution | replace with privileged `pvisor-guestd` |
| NAT, bridge, clipboard, audio, GUI automation | omit from the default process-isolation VM |
| private guest root as workload root | reject; the exported pVisor merged root is the workload root |

GhostVM's README currently says its source-code license has not been
determined. pVisor may study the public behavior and architecture but must not
copy its implementation unless a compatible license is published. The helper
and guest supervisor are clean independent implementations against Apple's
public API.

#### Filesystem and identity semantics

"Use the host UID and permissions" means preservation of ordinary POSIX file
semantics, not inheritance of every macOS security identity. The host pVisor
opens and serves lower files under the invoking host identity; the guest
supervisor then installs matching numeric UID, GID, and supplementary groups
before `execve`. The implementation must prove how VirtioFS represents owner,
mode, ACL, symlink, hard-link, xattr, device, and rename semantics rather than
assuming numeric identity is sufficient.

The following host facilities do not become transparent merely because the
numeric UID matches:

- TCC decisions, Keychain access groups, code-signing identity and entitlements;
- the host login/GUI bootstrap session, launchd services, Mach ports, Apple
  Events, and host Unix sockets;
- host kernel state, devices, mounted volumes not visible through the exported
  root, and credentials held only by host processes.

Modern macOS also presents `/` through a sealed system volume, a writable data
volume, and firmlinks. pVisor must verify that exporting the host root presents
one coherent namespace and that whiteout/copy-up behavior remains correct
across `/System/Volumes/Data`. Access to privacy-protected host files may
require Full Disk Access for the trusted host component; pVisor must report
that requirement rather than silently returning a partial root.

Host and guest should initially require the same architecture and exact macOS
build. A host executable can depend on the matching dyld shared cache,
framework ABI, code-signing policy, and kernel behavior. Cross-build execution
is unsupported until a compatibility matrix proves otherwise.

#### Lifecycle and security profile

Cold-installing macOS per Run is infeasible. The intended lifecycle is:

1. provision and attest one minimal, matching macOS boot template;
2. boot it once, install `pvisor-guestd`, and save a clean suspended state;
3. restore a warm VM or acquire one from a small version-matched pool;
4. attach only the Run's VirtioFS root and per-Run vsock endpoints;
5. rotate Run identity, authentication material, entropy, IPC, and network
   state before guest execution;
6. execute exactly one untrusted process tree, export the upper through the
   normal pVisor review path, then destroy or return a scrubbed VM to the pool.

The default VM has no NAT or bridged network device. Network access crosses an
explicit vsock relay owned by OverlayNet. Clipboard, host audio, GUI devices,
arbitrary shared folders, port forwarding, and ambient host sockets are absent.
The host VMM helper receives access only to the prepared merged root, VM
template, its private control socket, and required Virtualization.framework
resources; it must not inherit pChronicle, source credentials, or unrelated
descriptors.

VirtioFS is the largest feasibility risk. GhostVM has an open report of
[empty mounts and unreadable files](https://github.com/groundwater/GhostVM/issues/255)
under macOS guests. pVisor's design puts dyld, frameworks, SDKs, package
managers, and metadata-heavy toolchains on that path, which is more demanding
than sharing a project directory. VM startup success is therefore not evidence
that the backend is usable or safe.

#### Feasibility gate

This backend remains experimental until one focused prototype passes all of
the following on a supported host/guest build pair:

1. export a pVisor merged root with a stable VirtioFS tag and mount it without
   Finder or login-session automation;
2. run `/usr/bin/true`, `/bin/zsh`, and representative `xcrun`/compiler tools
   after `chroot`, with correct cwd, environment, UID, GID, groups, exit status,
   streaming stdio, signals, cancellation, and descendant cleanup;
3. prove lower files do not change and all creates, modifications, renames,
   deletions, whiteouts, xattrs, ACLs, symlinks, and hard links enter the Run
   upper and survive review/checkpoint/apply/drop;
4. exercise dyld/framework loading, code signatures, the sealed-system/data
   firmlink layout, large output, large files, many small files, concurrent
   mutation, crash recovery, and warm-restore attachment changes;
5. demonstrate that no network, clipboard, arbitrary share, stale vsock token,
   prior-Run upper, or unrelated host descriptor is reachable;
6. publish cold/warm latency and RSS and compare them with Seatbelt and Linux
   libkrun Runs.

Passing this gate establishes transparent CLI and development-tool execution.
GUI applications and host-session services require separate evidence and are
not implied by success of the process-level backend.

## 3. Path B: LiteBox + OverlayFS semantics in the VFS

### 3.1 Positioning

This is the high-density libOS path. LiteBox handles the guest Linux ABI and
path resolution in userspace. pVisor should implement its overlay semantics as
a LiteBox filesystem backend or composer, rather than FUSE-mounting a host path
and forwarding guest path strings to host `openat`.

```text
pVisor supervisor
  +-- build content-addressed root/workspace bundle
  +-- pass sealed bundle FD + policy + Agent ABI FD
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

The adapter preserves pVisor's logical operations:

- ordered read-only base and compose layers;
- copy-up on first write;
- whiteout and opaque-directory semantics;
- deterministic directory merge;
- metadata policy for modes, timestamps, symlinks, hard links, and xattrs;
- a bounded writable upper that can be exported without traversing unrelated
  host paths.

The initial implementation can reuse LiteBox's read-only tar and in-memory
filesystems, but production adoption requires a filesystem semantic matrix.
Unsupported metadata must fail explicitly or be normalized by a documented
policy; silent loss would break pVisor's changeset contract.

### 3.2 Security and operational properties

**Strengths**

- Guest paths terminate in the LiteBox VFS; the normal path contains no host
  pathname lookup.
- A smaller host interface than a native Linux process or general OCI
  container makes syscall-level policy and deterministic I/O practical.
- Read-only content-addressed bundles can be cached and shared across Runs;
  writable state remains per-Run.
- No kernel FUSE round trip is needed for VFS operations handled entirely in
  the runner, which may benefit metadata-heavy workloads.

**Limits**

- Linux syscall and filesystem compatibility is narrower than Docker or a VM.
- LiteBox and its pVisor adapter are evolving code and expand pVisor's trusted
  computing base.
- A userspace libOS is not automatically a hardware or kernel security
  boundary. Bugs in the runner, loader, syscall interception, or shared
  address space must be assumed possible.
- Packaging native libraries, dynamic runtimes, JITs, and unusual filesystem
  behavior requires explicit compatibility testing.

The LiteBox runner must therefore execute in a separate unprivileged process
with Landlock, seccomp, `no_new_privs`, empty capabilities, closed FDs, and
resource limits. The outer kernel policy is the containment boundary if the
guest escapes the LiteBox abstraction. Embedding an untrusted LiteBox guest in
the pVisor supervisor process is forbidden.

### 3.3 Workspace transfer

Avoid a long-lived, arbitrary pathname broker. Prefer immutable and bounded
objects:

1. pVisor snapshots the logical lower layers and computes a digest.
2. It supplies a sealed `memfd` or read-only file descriptor to the runner.
3. LiteBox reads the root and workspace through its VFS.
4. Writes enter a per-Run upper with byte/inode quotas.
5. The runner exports a canonical, bounded changeset.
6. pVisor validates paths, entry types, metadata, sizes, and digest before
   exposing the changeset to review/apply.

## 4. Path C: Docker / OCI container

### 4.1 Positioning

This is the compatibility and ecosystem path. It supports existing Agent
images and conventional Linux runtimes with stronger placement isolation than
the host executor, while sharing the host kernel.

The current pVisor Docker/Podman executor already injects a matching static
pVisor into the image and delegates the same `RunSpec`. It mounts the final
workspace and returns a typed `RunResult`. It does not yet translate every
pVisor capability into an OCI restriction, and the injected pVisor currently
bootstraps as container root; those are implementation gaps, not properties of
the target design.

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

Docker's image-layer OverlayFS and pVisor's WorkspaceOverlay have distinct
roles. The former assembles an OCI root filesystem; the latter owns Agent
changes and review/apply/drop. Container teardown must not commit the OCI
writable layer as the Run result.

### 4.2 Production profile

The target profile is:

- rootless Docker/Podman when supported, or user namespace remapping;
- non-root Agent UID after the injected pVisor bootstrap issue is removed;
- all capabilities dropped, `no-new-privileges`, default or tighter seccomp;
- read-only container rootfs and a private, bounded `/tmp`;
- PID, memory, CPU, file-size, and process-count limits;
- no network by default, otherwise a dedicated namespace connected to a
  pVisor-owned broker;
- no host PID/IPC namespace, privileged mode, device passthrough, arbitrary
  writable mounts, or Docker socket;
- image digest pinning and an auditable mount/capability manifest.

### 4.3 Security and operational properties

**Strengths**

- Highest workload compatibility short of a VM.
- Mature image construction, distribution, caching, observability, and
  operational tooling.
- Namespaces, cgroups, capabilities, seccomp, and host LSMs compose into a
  practical production boundary.
- Natural deployment path for Kubernetes and existing CI infrastructure.

**Limits**

- Containers share the host kernel; a kernel or container-runtime escape is
  outside pVisor's own enforcement.
- Cold-start cost, image storage, daemon/runtime dependencies, and mount
  plumbing are higher than the local and LiteBox paths.
- Rootful daemon deployments create a larger privileged control plane.
- Host networking, broad bind mounts, `--privileged`, or the Docker socket can
  erase most of the isolation value.
- The current Gateway loopback integration requires host networking in some
  configurations; production enforcement needs a guest-visible broker before
  that restriction can be removed.

Docker is the recommended compatibility fallback, not the definition of
pVisor's capability model.

## 5. Path D: Firecracker microVM

### 5.1 Positioning

This is the strongest multi-tenant path. Each Run or warm Run slot receives a
separate guest kernel under KVM. Firecracker intentionally exposes a small
device model and provides a jailer that adds host-side namespace/cgroup
isolation and drops VMM privileges.

The existing pVisor `vm` executor statically links libkrun and can either use
an explicit Linux rootfs or pull a public OCI image without a container daemon.
Verified image layers form an immutable cached lower rootfs, guest system writes
stay in a reviewable upper layer. Host paths are not implicitly exposed. An
explicit `--overlayfs-base` plus `--overlayfs-target` mounts a staged view at
the selected guest path. A vendored libkrun serves both root and workspace
copy-on-write unions directly over virtio-fs on Linux and macOS, without a
host FUSE mount, materialization, or reconciliation. It uses KVM on Linux
and HVF on Apple Silicon macOS. Linux
additionally confines the VMM with user/mount/network namespaces and Landlock.
The macOS VMM is not yet wrapped in an equivalent host filesystem sandbox, so
libkrun's virtio-fs proxy remains in the invoking user's security context.
OverlayNet and Agent ABI guest relays are not implemented in this phase. This
is not the hostile multi-tenant Firecracker design below:

```text
host pVisor / microVM manager
  +-- immutable kernel + rootfs image
  +-- read-only workspace/base block image
  +-- per-Run writable delta block image
  +-- vsock control and Agent ABI transport
  +-- TAP/network broker under policy
          |
          v
  Firecracker + jailer
          |
          v
  guest kernel + injected pVisor + Agent
```

The rootfs and workspace are attached as file-backed block devices. At Run
completion, the guest quiesces the filesystem and returns a manifest over
vsock; the host validates and converts the delta into the normal pVisor
changeset. Firecracker snapshots can amortize boot cost, but VM state, guest
memory, block devices, network devices, and vsock endpoints have separate
lifecycle and compatibility requirements. Snapshot reuse must rotate Run
identity, entropy, credentials, and network state.

### 5.2 Security and operational properties

**Strengths**

- A separate guest kernel provides the clearest boundary for mutually
  untrusted tenants and hostile native code.
- Minimal device emulation reduces VMM attack surface relative to a general
  machine emulator.
- Resource accounting and network topology are explicit at the VM boundary.
- Warm pools and snapshots can make repeated Run startup practical.

**Limits**

- Requires Linux, KVM, kernel/rootfs image production, a jailer, TAP/network
  setup, and a microVM lifecycle service.
- Baseline memory and operational complexity are higher than process/container
  paths even when the VMM is lightweight.
- Workspace block-image creation and delta extraction are less interactive
  than a directly mounted FUSE workspace.
- Kernel, rootfs, snapshot, and VMM versions form a larger compatibility and
  patch-management surface.
- Direct host directory sharing would weaken the clean boundary and should not
  become the production workspace design.

Production Firecracker execution must use the jailer or an equivalent stronger
host policy, a dedicated unprivileged VMM identity, cgroups, seccomp, isolated
networking, trusted immutable inputs, and no ambient access to host paths.

## 6. Comparison and selection

The table describes the intended production shape, not just the code currently
present in the repository. Performance is deliberately relative until a common
benchmark has measured cold/warm startup, RSS, syscall-heavy and data-heavy
workloads, and teardown.

| Dimension | FUSE + Landlock | FUSE + Seatbelt | macOS VM + VirtioFS | LiteBox VFS | Docker/OCI | Firecracker |
|---|---|---|---|---|---|---|
| Primary goal | fastest Linux least privilege | zero-config macOS write confinement | transparent Mach-O execution with a guest-kernel boundary | dense libOS isolation | compatibility and deployment | hostile multi-tenant isolation |
| Security boundary | synthetic root + host LSM/namespace policy | Seatbelt write/socket policy on host process | macOS guest kernel + Virtualization.framework VMM | libOS plus outer host policy | namespaces/cgroups/LSM, shared kernel | guest kernel + KVM + jailed VMM |
| Host root required | no | no | exported as a pVisor merged root | no | no in rootless mode | host provisioning normally required |
| Guest compatibility | native Linux ABI | native macOS ABI; ambient reads | native Mach-O, initially exact host/guest build only | constrained Linux ABI | broad Linux userspace | full guest Linux |
| Workspace fidelity | highest | highest with macFUSE | target is full-root fidelity; unproven over VirtioFS | requires semantic adapter | high through mount/volume | explicit block/delta conversion |
| Startup cost | lowest | lowest | high cold; warm restore/pool target | low target | medium, image dependent | highest cold; warm snapshot target |
| Per-Run memory | lowest | lowest | high | low target | medium | highest |
| Kernel escape blast radius | host | host | guest first, then VMM boundary | host, after outer escape | host | guest first, then VMM/KVM boundary |
| Portability | Linux | macOS; deprecated launcher dependency | Apple Silicon Mac with supported macOS virtualization | platform/ABI dependent | broad OCI hosts | Linux + KVM |
| Current pVisor status | implemented; seccomp/limits pending | write confinement and deny-all socket policy implemented | researched design; feasibility prototype required | planned | implemented with hardening gaps | libkrun full-root mode exists; Firecracker planned |

### Recommended portfolio

The selection belongs to pVisor and the placement control plane:

1. A normal Linux `pvisor run --safe` uses FUSE + Workspace + synthetic root +
   rootless namespaces + Landlock today. Required controls are installed
   fail-closed; an unavailable user namespace, mount, chroot, or Landlock ABI
   never falls back silently.
2. pVisor may choose LiteBox automatically for a compatible packaged workload
   when it provides a smaller, measured host interface; the user still invokes
   the same command.
3. Supplying an OCI image naturally selects Docker/Podman. Otherwise pVisor
   may use an already available rootless runtime as a compatibility fallback;
   it does not ask users to construct capability or mount flags.
4. A fleet configured for hostile multi-tenant execution places the Run on a
   Firecracker worker. Kernel images, snapshots, networking, and jailer setup
   are operator-owned fleet infrastructure, not per-user configuration.
5. macOS keeps the same command and uses Seatbelt write confinement for the
   implemented low-latency local path. After the feasibility gate passes,
   policy requiring a guest-kernel boundary may select the
   Virtualization.framework backend automatically. Until then, the Bundle
   reports ambient reads and cooperative selective networking separately and a
   stricter request routes to another capable placement or fails with one
   remediation.

These paths are a portfolio, not a mandatory migration ladder. A customer
states workload intent and, where necessary, a minimum security requirement;
placement chooses only a backend whose measured capabilities satisfy it. The
customer does not select kernel mechanisms.

## 7. One backend contract

All implementations should compile one request into one evidence-bearing
result:

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

`RuntimeCapabilities.filesystem = true` is valid only when tests demonstrate
that the complete Agent process tree cannot reach a non-granted hierarchy. A
mounted workspace or successful setup call alone is not evidence.

This contract is internal between admission, placement, and runtime drivers.
It is not a requirement for users to understand or configure backend-specific
mechanisms.

## 8. Zero-configuration acceptance criteria

The default local path is complete only when all of the following hold:

- one pVisor installation and one `pvisor run --safe` command are sufficient;
- no root shell, setuid pVisor daemon, manual group membership, hand-written
  policy, mount command, or container security flags are required;
- pVisor discovers the executable and its minimal runtime dependencies;
- workspace setup, isolation, cleanup, and changeset recovery are automatic;
- unsupported hosts produce one stable error with a concrete remediation or
  an automatically available placement, rather than a cascade of kernel
  details;
- `pvisor status` and the Run Bundle explain the effective boundary for audit
  without making that explanation a prerequisite for use;
- upgrades preserve the high-level command and Run contract while allowing the
  selected backend to change.

This criterion rules out 9P as a user-facing Docker setup step. pVisor may use
a filesystem protocol internally when a remote backend requires it, but users
must never provision a 9P server, mount it, or grant a container mount
capability for a normal Run.

## 9. Validation and benchmarks

Every backend must run the same adversarial suite:

- absolute paths, `..`, symlink chains, hard links, rename races, magic links,
  `/proc/self/fd`, inherited directory FDs, UNIX sockets, device nodes, and
  descriptor passing;
- fork/clone/exec descendants, raw syscalls, static binaries, JIT-generated
  code, signals, ptrace attempts, and namespace operations where applicable;
- direct sockets, DNS rebinding, literal IPs, UDP/QUIC, loopback, link-local,
  and metadata-service addresses;
- byte/inode/process/CPU/memory/network exhaustion and cancellation cleanup;
- power loss or supervisor crash during workspace export, review, and apply;
- semantic comparison of the same changeset across all four backends.

The shared benchmark reports distributions rather than a single demo number:

- cold and warm start P50/P95/P99;
- idle and peak RSS;
- sequential and random workspace throughput;
- small-file metadata operations per second;
- syscall-heavy and Python/Node/native Agent workloads;
- checkpoint/export/apply latency and produced bytes;
- host CPU cost, context switches, page faults, and FUSE/VMM/broker overhead.

The implemented Linux suite currently proves staged writes plus denial of
absolute-path reads/writes, symlink escapes, `/proc/self/root` escapes,
ungranted pathname Unix sockets, preservation of the exact Agent ABI socket,
and host-loopback access in deny-all mode. It also proves setup failures are
reported before Agent execution. This is a useful regression floor, not yet
the complete adversarial/kernel matrix listed above. No backend becomes a
production default from architectural expectations alone; it must publish
repeatable measurements and pass that matrix on every supported host/kernel.

## References

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
