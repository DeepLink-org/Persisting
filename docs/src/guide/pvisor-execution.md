# Run workloads with pVisor

pVisor is Persisting's implementation of the
[AgentVisor contract](../design/agentvisor.md): it governs one Agent Run's
lifecycle, capabilities, effects, checkpoints, and evidence independently of
the selected execution provider.

This guide covers the supported host and VM execution layouts. The command-line
surface deliberately keeps three independent decisions separate:

1. `--executor` chooses where the process runs: the host kernel or a libkrun VM.
2. `--image`, `--vm-rootfs`, or `--host-rootfs` chooses the VM's Linux root
   filesystem.
3. `--overlayfs-*` chooses the host directory exposed as the writable workspace
   and how its changes are retained.

There are no workspace aliases. The canonical options are shown below.

## Option model

| Option | Meaning |
| --- | --- |
| `--executor host` | Run the command with the host kernel. This is the default. |
| `--executor vm` | Boot a Linux guest kernel with libkrun. |
| `--host-rootfs` | Linux only: export the host `/` as the VM rootfs lower layer. Implies `vm` when `--executor` is omitted. |
| `--image IMAGE` | Use a daemonless OCI image as the VM rootfs. The default VM image is `ubuntu:latest`; pin an explicit tag or digest for reproducibility. |
| `--vm-rootfs DIR` | Use an already prepared Linux rootfs directory. |
| `--overlayfs-base DIR` | Host directory used as the read-only workspace lower layer and default apply destination. |
| `--overlayfs-target PATH` | VM only: absolute guest path where the workspace is mounted; it also becomes the guest working directory. Requires `--overlayfs-base` and cannot be `/`. |
| `--overlayfs-stage DIR` | Durable writable stage containing workspace changes. Use a separate stage for each concurrent run or mode. |
| `--overlayfs-commit manual` | Keep changes staged for review. `apply` writes them to the base; `drop` discards them. |
| `--overlayfs-commit apply` | Apply changes automatically after a successful run. |
| `--overlayfs-commit drop` | Discard changes automatically after the run. |

`--host-rootfs`, `--image`, and `--vm-rootfs` are mutually exclusive rootfs
sources. `--host-rootfs` is a semantic selection, not an alias for
`--vm-rootfs /` or any OverlayFS option.

## Supported layouts

| Host platform | Executor | VM rootfs | Workspace inside the command |
| --- | --- | --- | --- |
| macOS | `host` | not applicable | `--overlayfs-base` at the staged host cwd |
| macOS | `vm` | OCI image or prepared Linux rootfs | `--overlayfs-base` at `--overlayfs-target` |
| Linux | `host` | not applicable | `--overlayfs-base` at the staged host cwd |
| Linux | `vm --host-rootfs` | Linux host `/` through virtio-fs | `--overlayfs-base` at `--overlayfs-target` |
| Linux | `vm` | OCI image or prepared Linux rootfs | `--overlayfs-base` at `--overlayfs-target` |

### macOS host executor

```bash
./target/release/pvisor run --executor host \
  --overlayfs-base /Users/reiase/workspace \
  --overlayfs-stage ./tmp/macos-host \
  --overlayfs-commit manual \
  -- /bin/bash
```

The command uses the macOS kernel and host binaries. It sees a copy-on-write
view of the base as its working directory. This mode does not provide a Linux
kernel. Use `--safe` when the stronger supported host write-confinement profile
is desired.

### macOS VM with an OCI rootfs

```bash
./target/release/pvisor run --executor vm \
  --image ubuntu:24.04 \
  --overlayfs-base /Users/reiase/workspace \
  --overlayfs-target /home/workspace \
  --overlayfs-stage ./tmp/macos-vm \
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
  --overlayfs-base /home/reiase/workspace \
  --overlayfs-stage ./tmp/linux-host \
  --overlayfs-commit manual \
  -- /bin/bash
```

This is the Linux equivalent of the macOS host layout. The command uses the
host kernel and host userland.

### Linux VM with the host rootfs

```bash
./target/release/pvisor run --executor vm \
  --host-rootfs \
  --overlayfs-base /home/reiase/workspace \
  --overlayfs-target /home/workspace \
  --overlayfs-stage ./tmp/linux-host-rootfs \
  --overlayfs-commit manual \
  -- /bin/bash
```

This is the transparent-rootfs layout: the guest uses a different Linux kernel
but reads the host's `/` as its rootfs lower layer through virtio-fs. Rootfs
writes go to a temporary VM upper and are discarded at VM exit. The separately
mounted workspace has the durable stage and can be reviewed or applied.

This mode exposes host-rootfs contents to the guest for reading. It is intended
for same-owner local isolation, not for untrusted multi-tenant workloads. Keep
`--overlayfs-target` in this layout. Without it, the durable OverlayFS stage
would describe changes to the whole host `/`, and a later apply could target
the host rootfs.

### Linux VM with an OCI rootfs

```bash
./target/release/pvisor run --executor vm \
  --image ubuntu:24.04 \
  --overlayfs-base /home/reiase/workspace \
  --overlayfs-target /home/workspace \
  --overlayfs-stage ./tmp/linux-vm \
  --overlayfs-commit manual \
  -- /bin/bash
```

This provides a guest kernel and image-defined userland. It is more
reproducible and exposes less host data than `--host-rootfs`, at the cost of
maintaining or downloading an image.

### VM with a prepared rootfs directory

On either supported VM platform, an unpacked Linux rootfs can replace the OCI
image:

```bash
./target/release/pvisor run --executor vm \
  --vm-rootfs /opt/pvisor/rootfs \
  --overlayfs-base /path/to/project \
  --overlayfs-target /home/workspace \
  --overlayfs-stage ./tmp/prepared-rootfs \
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

`apply` targets `--overlayfs-base` unless `--target` is supplied to the apply
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
- `--overlayfs-target` is VM-only, must be absolute, cannot be `/`, and requires
  `--overlayfs-base`.
- Do not use a macOS path such as `/Users/...` for a Linux host. Use the actual
  Linux path, such as `/home/reiase/workspace`.
- VM networking defaults to OverlayNet `auto`: pVisor supplies DHCP, synthetic
  DNS, and policy-controlled IPv4 TCP through smoltcp. Use `mode = "off"` for
  an offline guest. UDP, IPv6, ICMP, QUIC, and inbound connections are not part
  of the current VM network surface. Gateway capture uses the virtual guest
  router and does not expose host loopback directly.
