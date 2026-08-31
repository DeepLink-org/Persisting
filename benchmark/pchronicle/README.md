# pChronicle benchmarks

**Criterion.rs 微基准加上 hyperfine 进程级存储/查询/内存场景，输出一份稳定报告。**

拥有 smoke / nightly 套件、JSONPath 测量契约和 compare 入口。不拥有 pChronicle
存储格式或查询引擎。

The guarded paths are:

- ATIF parse and round-trip conversion;
- canonical events → Storyline projection;
- Storyline split into and reconstruction from the three relational tables;
- canonical Lance event append;
- full and incremental Storyline projection, including verification;
- Storyline point reads, replacement, indexed SQL, and group-by queries;
- projected JSON streaming latency, throughput, allocations, and peak RSS.

One runner produces `raw-report.json`, a Bencher-compatible `bencher.json`,
`report.md`, and `report.html`. Criterion's detailed HTML and sample data stay
under the `criterion/` artifact directory.

## Run

Install `hyperfine`, then run from the repository root:

```bash
just benchmark-pchronicle
just benchmark-pchronicle nightly target/pchronicle-benchmark/nightly

python3 benchmark/pchronicle/bench.py run \
  --suite smoke \
  --output target/pchronicle-benchmark/current
```

Compare two reports generated on the same testbed:

```bash
just benchmark-pchronicle-compare \
  target/pchronicle-benchmark/main/raw-report.json \
  target/pchronicle-benchmark/current/raw-report.json

python3 benchmark/pchronicle/bench.py compare \
  --baseline target/pchronicle-benchmark/main/raw-report.json \
  --candidate target/pchronicle-benchmark/current/raw-report.json \
  --output target/pchronicle-benchmark/comparison
```

`just benchmark-pchronicle` defaults to `--suite smoke` and
`--output target/pchronicle-benchmark/current`. `just benchmark-pchronicle-compare`
takes baseline then candidate.

## JSONPath measurement contract

Measurements are stored as a nested JSON object rather than a positional
array. Every leaf has one stable JSONPath address:

```json
{
  "measurements": {
    "system": {
      "projection_pipeline": {
        "projection_incremental": {
          "sync_ms": {
            "value": 42.1,
            "unit": "ms",
            "direction": "lower",
            "source": "custom"
          }
        }
      }
    }
  }
}
```

The runner uses the bracketed-name JSONPath subset so keys never depend on dot
escaping. Read a value with:

```bash
python3 benchmark/pchronicle/bench.py jsonpath-get \
  --document target/pchronicle-benchmark/current/raw-report.json \
  --path '$["measurements"]["system"]["projection_pipeline"]["projection_incremental"]["sync_ms"]'
```

Future benchmark producers can insert a new leaf with `jsonpath-set`. Duplicate
paths fail unless `--replace` is explicit:

```bash
python3 benchmark/pchronicle/bench.py jsonpath-set \
  --document target/pchronicle-benchmark/current/raw-report.json \
  --path '$["measurements"]["external"]["scenario"]["latency_ms"]' \
  --value-json '{"value":12.5,"unit":"ms","direction":"lower","source":"external"}'
```

`--value-file` inserts a complete JSON document. Nightly uses it to replace
`$["latest"]` in `benchmark/pchronicle/nightly.json`; README generation then
reads that same JSONPath root.

## CI contract

The pChronicle benchmark workflow runs the main baseline and candidate as two
parallel matrix jobs. Each job uses its own target directory and uploads a
ref-scoped raw report; a short aggregation job then downloads both reports,
writes the comparison to the Actions Job Summary, and uploads the complete
HTML/JSON report. During the framework's first pull request, the missing main
baseline is reported as incomparable and the candidate report is
still published. The benchmark jobs disable unrelated toolchain installs
(nextest and Zig) to keep setup overhead out of this workflow.

The nightly workflow runs the larger suite, stores its latest complete report
under `$["latest"]` in `nightly.json`, and updates the generated benchmark block
in the repository README from that stored value. Per-run raw reports and
Criterion/Hyperfine details remain workflow artifacts rather than growing an
unbounded repository history.

## Links

- [pChronicle design](../../docs/src/pchronicle/design/index.md)
- [`persisting-pchronicle`](../../crates/persisting-pchronicle/README.md)
