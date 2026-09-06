# Run workloads with pVisor

pVisor is an [AgentVisor](../concepts/agentvisor.md): it governs one Agent Run's
lifecycle, capabilities, effects, checkpoints, and evidence independently of
the selected execution provider.

If this is your first Run, complete [Run your first Agent](../get-started.md)
before choosing a lower-level execution layout here.

This guide covers the supported host and VM execution layouts. The command-line
surface deliberately keeps three independent decisions separate:

1. `--executor` chooses where the process runs: the host kernel or a libkrun VM.
2. `--rootfs host`, `--rootfs <PATH>`, or `--rootfs image=<PATH>` chooses the VM's Linux root
   filesystem.
3. `--overlayfs-path` chooses the absolute path visible to the Agent, while
   repeated `--overlayfs-compose` layers host directories in bottom-to-top order.

There are no workspace aliases. The canonical options are shown below.

## Option model

| Option | Meaning |
| --- | --- |
| `--executor host` | Run the command with the host kernel. This is the default. |
| `--executor vm` | Boot a Linux guest kernel with libkrun. |
| `--rootfs host` | Linux only: export the host `/` as the VM rootfs lower layer. Implies `vm` when `--executor` is omitted. |
| `--rootfs image=<IMAGE>` | Use a daemonless OCI image as the VM rootfs. The default VM image is `ubuntu:latest`; pin an explicit tag or digest for reproducibility. |
| `--rootfs DIR` | Use an already prepared Linux rootfs directory. |
| `--overlayfs-path PATH` | Absolute path visible to the Agent after the overlay view is mounted. |
| `--overlayfs-compose DIR` | Host read-only layer; repeat in bottom-to-top order. The current workspace is the implicit bottom layer. |
| `--stage DIR` | Durable writable stage containing workspace changes. Use a separate stage for each concurrent run or mode. |
| `--overlayfs-commit manual` | Keep changes staged for review. `apply` writes them to the base; `drop` discards them. |
| `--overlayfs-commit apply` | Apply changes automatically after a successful run. |
| `--overlayfs-commit drop` | Discard changes automatically after the run. |

`--rootfs host`, `--rootfs image=<PATH>`, and `--rootfs <PATH>` are mutually exclusive rootfs
sources. `--rootfs host` is a semantic selection, not an alias for
`--rootfs /` or any OverlayFS option.

Without `--overlayfs-path`, the current directory is the default workspace view;
pVisor uses a managed per-Run merged mountpoint so the lower directory is never
mounted over itself.

## Supported layouts

| Host platform | Executor | VM rootfs | Workspace inside the command |
| --- | --- | --- | --- |
| macOS | `host` | not applicable | `--overlayfs-path` in the staged host view |
| macOS | `vm` | OCI image or prepared Linux rootfs | `--overlayfs-path` in the guest |
| Linux | `host` | not applicable | `--overlayfs-path` in the staged host view |
| Linux | `vm --rootfs host` | Linux host `/` through virtio-fs | `--overlayfs-path` in the guest |
| Linux | `vm` | OCI image or prepared Linux rootfs | `--overlayfs-path` in the guest |

### macOS host executor

```bash
./target/release/pvisor run --executor host \
  --overlayfs-path /workspace \
  --overlayfs-compose /Users/reiase/workspace \
  --stage ./tmp/macos-host \
  --overlayfs-commit manual \
  -- /bin/bash
```

The command uses the macOS kernel and host binaries. It sees a copy-on-write
view of the base as its working directory. This mode does not provide a Linux
kernel. Host isolation is safe-best-effort by default; unsupported controls are
reported and downgraded where the platform cannot provide them.

### macOS VM with an OCI rootfs

```bash
./target/release/pvisor run --executor vm \
  --rootfs image=ubuntu:24.04 \
  --overlayfs-path /home/workspace \
  --overlayfs-compose /Users/reiase/workspace \
  --stage ./tmp/macos-vm \
  --overlayfs-commit manual \
  -- /bin/bash
```

The command path is resolved in the Linux guest, not on macOS. The OCI rootfs
is an immutable lower layer with a temporary root upper. Workspace changes are
kept in the requested durable stage. A macOS rootfs cannot be reused here:
Mach-O programs and the macOS userland do not run under the Linux guest kernel.

### Linux host executor

```bash
./target/release/pvisor run --executor host \
  --overlayfs-path /workspace \
  --overlayfs-compose /home/reiase/workspace \
  --stage ./tmp/linux-host \
  --overlayfs-commit manual \
  -- /bin/bash
```

This is the Linux equivalent of the macOS host layout. The command uses the
host kernel and host userland.

### Linux VM with the host rootfs

```bash
./target/release/pvisor run --executor vm \
  --rootfs host \
  --overlayfs-path /home/workspace \
  --overlayfs-compose /home/reiase/workspace \
  --stage ./tmp/linux-host-rootfs \
  --overlayfs-commit manual \
  -- /bin/bash
```

This is the transparent-rootfs layout: the guest uses a different Linux kernel
but reads the host's `/` as its rootfs lower layer through virtio-fs. Rootfs
writes go to a temporary VM upper and are discarded at VM exit. The separately
mounted workspace has the durable stage and can be reviewed or applied.

This mode exposes host-rootfs contents to the guest for reading. It is intended
for same-owner local isolation, not for untrusted multi-tenant workloads. Keep
`--overlayfs-path` in this layout. Without it, the durable OverlayFS stage
would describe changes to the whole host `/`, and a later apply could target
the host rootfs.

### Linux VM with an OCI rootfs

```bash
./target/release/pvisor run --executor vm \
  --rootfs image=ubuntu:24.04 \
  --overlayfs-path /home/workspace \
  --overlayfs-compose /home/reiase/workspace \
  --stage ./tmp/linux-vm \
  --overlayfs-commit manual \
  -- /bin/bash
```

This provides a guest kernel and image-defined userland. It is more
reproducible and exposes less host data than `--rootfs host`, at the cost of
maintaining or downloading an image.

### VM with a prepared rootfs directory

On either supported VM platform, an unpacked Linux rootfs can replace the OCI
image:

```bash
./target/release/pvisor run --executor vm \
  --rootfs /opt/pvisor/rootfs \
  --overlayfs-path /home/workspace \
  --overlayfs-compose /path/to/project \
  --stage ./tmp/prepared-rootfs \
  --overlayfs-commit manual \
  -- /bin/bash
```

The directory must contain a userland for the host CPU architecture and the
requested command.

## Review and commit a manual stage

After a manual run, use the emitted Run id or `last`:

```bash
./target/release/pvisor review last
./target/release/pvisor inspect last -- git status --short
./target/release/pvisor apply last --path src
./target/release/pvisor apply last --include 'tests/**' --exclude 'tests/generated/**'
./target/release/pvisor apply last --all
# Or discard it instead:
./target/release/pvisor drop last
```

`apply` targets the implicit workspace unless `--target` is supplied to the apply
command. A filtered apply consumes only the selected dependency-closed batch;
the remaining changes stay staged and can be applied again or dropped. Opaque
directories and hard-link groups cannot be split unsafely. Successful batches
are recorded in `apply-ledger.json`. A stage must not contain its base or
compose layers. A stage nested inside a base is hidden from the merged view,
but keeping stages in a separate `tmp` directory is easier to operate and audit.

## Requirements and common errors

- Build the macOS source binary with `just pvisor`. The recipe builds release,
  applies `macos-hypervisor.entitlements`, signs ad hoc, and verifies the
  signature. An unsigned binary fails at `krun_start_enter`.
- Linux VM execution requires accessible `/dev/kvm`; macOS VM execution
  requires Apple Silicon, HVF, and the Hypervisor entitlement.
- `--overlayfs-path` must be an absolute Agent-visible path and cannot contain
  `..`; `--overlayfs-compose` entries must be readable host directories.
- Do not use a macOS path such as `/Users/...` for a Linux host. Use the actual
  Linux path, such as `/home/reiase/workspace`.
- VM networking defaults to OverlayNet `auto`: pVisor supplies DHCP, synthetic
  DNS, and policy-controlled IPv4 TCP through smoltcp. Use `mode = "off"` for
  an offline guest. UDP, IPv6, ICMP, QUIC, and inbound connections are not part
  of the current VM network surface. Gateway capture uses the virtual guest
  router and does not expose host loopback directly.

Next, use [Review and apply Agent changes](review-apply.md) for the complete
selective-apply workflow, or [Capture Agent trajectories](capture.md) to add
runtime evidence.
