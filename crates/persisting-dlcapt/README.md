# persisting-dlcapt

`persisting-dlcapt` 是 Persisting workspace 内独立运行的 OpenAI 兼容代理与轨迹采集服务。
第一阶段不依赖 `persisting-capture` 或 `persisting-engine`，以保持现有 dlcapt HTTP、session 和存储契约。

## Source and license

The maintained runtime source is this `crates/persisting-dlcapt` package. It was
migrated from Capture `external/dlcapt`; that path describes the source
provenance, not the current runtime location. The package follows the
workspace's Apache-2.0 license policy; see the repository `NOTICE`.

## Run (development)

```bash
cd /path/to/Persisting
cargo run -p persisting-dlcapt -- crates/persisting-dlcapt/config/proxy.example.toml
```

CLI is positional only: `dlcapt <config-path>`. There is no `serve -c`.

## Run (release archive)

Build the precompiled archive from the workspace root:

```bash
./scripts/package-dlcapt.sh --incremental
```

Only publishers need Rust, Cargo, and `protoc` to build this archive. End users
who download and extract a release archive need none of those build tools.

After extracting `target/dlcapt/dlcapt-<version>-<target>.tar.gz`, run:

```bash
./bin/dlcapt config/proxy.lance-s3.deploy.toml
```

`store_dir` is resolved relative to the process **cwd**, not the config file path.

## Public / admin

- Public: `listen` (default example `127.0.0.1:19081`) — `/healthz`, `/readyz`, `/v1/models`, chat/completions, session routes
- Admin: `admin_listen` (default example `127.0.0.1:19082`) — `/admin/sessions`, `/admin/errors`

Session priority: URL `{id}` > Header > body `metadata.session_id` > `default_session_id`.

## Storage

See `config/*.example.toml` for `json_file`, local Lance, and S3 Lance (`fail_open` dead letter at `.capture/lance_dead_letter.jsonl`).

## Safety

- Examples bind `127.0.0.1` only. Binding `0.0.0.0` is for private deploys with firewall / reverse-proxy ACL; admin must not be exposed to untrusted networks.
- Trajectories may contain prompts, responses, and header-derived metadata — never commit `var/` or `.capture/`.
- Do not publish online/beta configs with real upstreams, buckets, or credentials.
