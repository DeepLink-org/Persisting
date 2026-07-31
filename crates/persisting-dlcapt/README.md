# persisting-dlcapt

`persisting-dlcapt` 是 Persisting workspace 内独立运行的 OpenAI 兼容代理与轨迹采集服务。
第一阶段不依赖 `persisting-capture` 或 `persisting-engine`，以保持现有 dlcapt HTTP、session 和存储契约。

## Source and license

The maintained runtime source is this `crates/persisting-dlcapt` package. It was
migrated from Capture `external/dlcapt`; that path describes the source
provenance, not the current runtime location. The package follows the
workspace's GPL-3.0-or-later license policy; see the repository `NOTICE`.

This package does not replace the separate attribution for vendored
`fuse-overlayfs` under `crates/persisting-fs-overlay`.

## Prepare a local configuration

```bash
cd /path/to/Persisting
export DLCAPT_CONFIG="$HOME/.config/persisting/dlcapt-openclaw.toml"
export DLCAPT_STORE_DIR="$HOME/.local/share/persisting/dlcapt"
export DLCAPT_UPSTREAM_BASE_URL="https://your-upstream.example/v1"
mkdir -p "$(dirname "$DLCAPT_CONFIG")"
cp crates/persisting-dlcapt/config/proxy.openclaw-test.example.toml "$DLCAPT_CONFIG"
sed -i \
  -e "s|__STORE_DIR__|$DLCAPT_STORE_DIR|g" \
  -e "s|__UPSTREAM_BASE_URL__|$DLCAPT_UPSTREAM_BASE_URL|g" \
  "$DLCAPT_CONFIG"
```

CLI is positional only: `dlcapt <config-path>`. There is no `serve -c`.

## Run (development)

```bash
cargo run -p persisting-dlcapt -- "$DLCAPT_CONFIG"
```

## Run through persisting-cli

Build the optional backend first:

```bash
cargo run -p persisting-cli --features dlcapt -- \
  traj proxy --backend dlcapt -c "$DLCAPT_CONFIG"
```

This is foreground-only. `store_dir`, storage sinks, model routes, and public/admin
listen addresses come from the dlcapt TOML; `-o`, `-f`, `--debug`, and daemon
actions remain capture-backend options.

## Public / admin

- Public: `listen` (default example `127.0.0.1:19081`) — `/healthz`, `/readyz`, `/v1/models`, chat/completions, session routes
- Admin: `admin_listen` (default example `127.0.0.1:19082`) — `/admin/sessions`, `/admin/errors`

Session priority: URL `{id}` > Header > body `metadata.session_id` > `default_session_id`.

## Storage

Use `config/proxy.openclaw-test.example.toml` as the supported safe template.

## Safety

- Examples bind `127.0.0.1` only. Binding a wildcard all-interface address is for private deploys with firewall / reverse-proxy ACL; admin must not be exposed to untrusted networks.
- Trajectories may contain prompts, responses, and header-derived metadata — never commit `var/` or `.capture/`.
- Do not publish online/beta configs with real upstreams, buckets, or credentials.
