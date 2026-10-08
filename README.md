# <img src="docs/static/img/logos/persisting-icon.png" alt="Persisting" width="72" /> Persisting

**Persistent infrastructure for the Agent era.**

[![CI](https://github.com/DeepLink-org/Persisting/actions/workflows/ci.yml/badge.svg)](https://github.com/DeepLink-org/Persisting/actions/workflows/ci.yml)
[![Documentation](https://img.shields.io/badge/docs-latest-blue)](https://deeplink-org.github.io/Persisting/)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

<img src="docs/static/img/logos/persisting-with-text.png" alt="Persisting logo" width="360" />

Persisting contains **pChronicle**, which imports, browses, queries, exchanges,
and serves durable Agent trajectory Datasets.

<img src="docs/static/img/logos/pchronicle-with-text.png" alt="pChronicle" width="220" />

## Install

```bash
pip install persisting[lance]
pchronicle --version
```

The rolling nightly build installs the same commands without a Rust toolchain:

```bash
curl -fsSL https://raw.githubusercontent.com/DeepLink-org/Persisting/main/scripts/install-nightly.sh | bash
```

See the [installation guide](https://deeplink-org.github.io/Persisting/installation/)
for supported platforms and setup.

## Query Agent trajectory history

```bash
pchronicle onboard
pchronicle onboard query
pchronicle agent codex ./trajectory-data --ask "Which tools fail most often?"
```

The onboarding flow creates a temporary example Dataset—no source checkout
required. `pchronicle import` accepts ATIF, ACTF, and OpenAI Messages;
`pchronicle serve` starts a loopback-only, read-only Dataset UI and API.

## Current maturity

| Capability | Status |
|---|---|
| pChronicle local/S3 catalog, bounded SQL, analysis, find, import/export | Implemented |
| pChronicle loopback-only read API and embedded Web UI | Implemented |
| Queue and document Search | Separate stable capabilities |
| Tensor Memory / TTAS | Experimental |

## Documentation

- [Choose a workflow](https://deeplink-org.github.io/Persisting/overview/) — pick the entry point that matches your task
- [Explore durable history](https://deeplink-org.github.io/Persisting/pchronicle/get-started/) — browse and query a trajectory Dataset
- [Project architecture](https://deeplink-org.github.io/Persisting/system-design/) — ownership and delivery boundaries

Criterion.rs microbenchmarks and hyperfine lifecycle scenarios are compared
against `main` in CI; see the [benchmark contract](benchmark/pchronicle/README.md).

<!-- pchronicle-benchmark:start -->
Latest nightly pChronicle benchmark: `f4716fe59995` on `linux/x86_64` (2026-10-08T03:08:23.982410+00:00).

| Case | Metric | Value |
|---|---:|---:|
| `criterion/atif_conversion/parse_corpus` | `latency_median_ns` | 3.381e+06 ns |
| `criterion/atif_conversion/roundtrip_corpus` | `latency_median_ns` | 4.398e+06 ns |
| `system/lance_vs_json/lifecycle` | `cold_query_ms` | 1928.315 ms |
| `system/lance_vs_json/lifecycle` | `get_storyline_full_ms` | 5.831 ms |
| `system/lance_vs_json/lifecycle` | `replace_storyline_ms` | 28.495 ms |
| `system/lance_vs_json/selective` | `lance_qps` | 524.2 ops/s |
| `system/lance_vs_json/group_by` | `lance_qps` | 684.4 ops/s |
| `system/lance_vs_json/summary` | `lance_over_json` | 0.244 ratio |
| `system/json_streaming_ndjson/json_streaming` | `p95_ms` | 8.48 ms |
| `system/json_streaming_ndjson/json_streaming` | `rows_s` | 4.948e+05 ops/s |
| `system/json_streaming_ndjson/json_streaming` | `process_peak_rss_mib` | 47.219 MiB |
| `hyperfine/lance_vs_json` | `wall_median_seconds` | 24.634 s |

[Open the complete benchmark run](https://github.com/DeepLink-org/Persisting/actions/runs/37721242118).
<!-- pchronicle-benchmark:end -->

## License

[Apache License 2.0](LICENSE). See [`NOTICE`](NOTICE) for third-party
attributions and separately licensed bundled components.
