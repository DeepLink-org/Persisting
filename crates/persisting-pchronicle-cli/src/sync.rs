use super::*;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::time::{Duration, SystemTime};

use clap::Args;
use persisting_pchronicle::storage::DatasetLocation;

#[derive(Debug, Args)]
pub(crate) struct SyncArgs {
    /// Source Dataset path, URI, or pin (for example `@origin/agentcompass`).
    #[arg(long, value_name = "DATASET")]
    pub(crate) from: String,

    /// Compact JSONL Lance Dataset receiving each snapshot (record-level ingest).
    #[arg(long, value_name = "DATASET")]
    pub(crate) mirror: Option<String>,

    /// Storyline Lance Dataset receiving each converted snapshot.
    #[arg(long = "to", value_name = "DATASET")]
    pub(crate) to: Option<String>,

    /// Input format for --to trajectory conversion. Auto detects run data.
    /// Compact-jsonl sources are only valid with --mirror (not --to).
    #[arg(long = "input-format", value_enum, default_value_t = ExchangeFormat::Auto)]
    pub(crate) input_format: ExchangeFormat,

    /// When --input-format auto cannot decide for --to, try this format if weakly compatible.
    #[arg(long = "suggested-format", value_enum, value_name = "FORMAT")]
    pub(crate) suggested_format: Option<ExchangeFormat>,

    /// Compact JSONL column mapping for --mirror. Same rules as import --column.
    #[arg(long = "column", value_name = "NAME=JSON_PATH", action = clap::ArgAction::Append)]
    pub(crate) columns: Vec<String>,

    /// Polling and update interval. Supports ms, s, and h.
    #[arg(long = "interval", value_name = "DURATION", value_parser = super::parse_duration_seconds, default_value = "1s")]
    pub(crate) interval_seconds: u64,

    /// Run one initial batch and exit instead of staying resident.
    #[arg(long)]
    pub(crate) once: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileStamp {
    size: u64,
    modified: Option<SystemTime>,
}

pub(crate) async fn run(
    args: SyncArgs,
    settings_override: Option<&Path>,
    stderr: &mut dyn Write,
    stderr_is_terminal: bool,
) -> Result<()> {
    anyhow::ensure!(
        args.mirror.is_some() || args.to.is_some(),
        "sync requires --mirror and/or --to"
    );
    anyhow::ensure!(
        args.columns.is_empty() || args.mirror.is_some(),
        "--column is only valid with --mirror"
    );
    if let Some(suggested) = args.suggested_format {
        anyhow::ensure!(
            args.to.is_some(),
            "--suggested-format is only valid with --to"
        );
        anyhow::ensure!(
            args.input_format == ExchangeFormat::Auto,
            "--suggested-format is only valid with --input-format auto"
        );
        anyhow::ensure!(
            suggested != ExchangeFormat::Auto,
            "--suggested-format cannot be auto"
        );
        anyhow::ensure!(
            suggested != ExchangeFormat::CompactJsonl,
            "--suggested-format cannot be compact-jsonl"
        );
    }
    if args.input_format == ExchangeFormat::CompactJsonl {
        anyhow::ensure!(
            args.to.is_none(),
            "sync --input-format compact-jsonl cannot use --to; pass --mirror only"
        );
        anyhow::ensure!(
            args.mirror.is_some(),
            "sync --input-format compact-jsonl requires --mirror"
        );
    }

    let source_uri = expand_dataset_reference(&args.from, settings_override, true)
        .with_context(|| format!("resolve sync source '{}'", args.from))?;
    let mirror_uri = match args.mirror.as_deref() {
        Some(mirror) => Some(prepare_destination(
            &expand_dataset_reference(mirror, settings_override, false)
                .with_context(|| format!("resolve sync mirror '{mirror}'"))?,
            "mirror",
        )?),
        None => None,
    };
    let to_uri = match args.to.as_deref() {
        Some(to) => Some(prepare_destination(
            &expand_dataset_reference(to, settings_override, false)
                .with_context(|| format!("resolve sync --to '{to}'"))?,
            "to",
        )?),
        None => None,
    };

    if let (Some(mirror), Some(to)) = (&mirror_uri, &to_uri) {
        anyhow::ensure!(mirror != to, "sync --mirror and --to must be different");
    }
    ensure_targets_outside_source(&source_uri, mirror_uri.as_deref(), to_uri.as_deref())?;

    let mut banner = format!("sync from={source_uri}");
    if let Some(mirror) = &mirror_uri {
        banner.push_str(&format!(" mirror={mirror}"));
    }
    if let Some(to) = &to_uri {
        banner.push_str(&format!(" to={to}"));
    }
    writeln!(stderr, "{banner}").context("write sync resolved targets")?;

    let interval = Duration::from_secs(args.interval_seconds.max(1));
    let input_format = args.input_format;
    let suggested_format = args.suggested_format;
    let columns = args.columns.clone();

    if args.once {
        let initial = scan_source(&source_uri).await?;
        anyhow::ensure!(
            !initial.is_empty(),
            "sync source contains no supported JSON files"
        );
        super::exchange::sync_snapshot(
            &source_uri,
            mirror_uri.as_deref(),
            to_uri.as_deref(),
            input_format,
            suggested_format,
            &columns,
            stderr,
            stderr_is_terminal,
        )
        .await?;
        writeln!(stderr, "sync batch={} status=ok", initial.len())
            .context("write sync progress")?;
        return Ok(());
    }

    let (changes_tx, mut changes_rx) = tokio::sync::mpsc::channel::<PathBuf>(1024);
    let watcher_source = source_uri.clone();
    let watcher = tokio::spawn(async move {
        let mut previous = BTreeMap::new();
        loop {
            // ponytail: dependency-free polling; use an OS watcher when tree size or latency
            // makes recursive scans measurable.
            let current = scan_source(&watcher_source).await?;
            for path in changed_paths(&previous, &current) {
                if changes_tx.send(path).await.is_err() {
                    return Ok::<(), anyhow::Error>(());
                }
            }
            previous = current;
            tokio::time::sleep(interval).await;
        }
    });

    let result = async {
        let mut pending = BTreeSet::new();
        let mut failures = 0u32;
        loop {
            tokio::time::sleep(interval).await;
            while let Ok(path) = changes_rx.try_recv() {
                pending.insert(path);
            }
            if pending.is_empty() {
                continue;
            }

            match super::exchange::sync_snapshot(
                &source_uri,
                mirror_uri.as_deref(),
                to_uri.as_deref(),
                input_format,
                suggested_format,
                &columns,
                stderr,
                stderr_is_terminal,
            )
            .await
            {
                Ok(()) => {
                    writeln!(stderr, "sync batch={} status=ok", pending.len())
                        .context("write sync progress")?;
                    pending.clear();
                    failures = 0;
                }
                Err(error) => {
                    failures = failures.saturating_add(1);
                    let exponent = failures.saturating_sub(1).min(8);
                    let backoff = interval
                        .checked_mul(1u32 << exponent)
                        .unwrap_or(Duration::MAX)
                        .min(Duration::from_secs(60));
                    writeln!(
                        stderr,
                        "sync batch={} status=error retry_ms={} error={}",
                        pending.len(),
                        backoff.as_millis(),
                        error
                    )
                    .context("write sync error")?;
                    tokio::time::sleep(backoff).await;
                }
            }
        }
    };

    tokio::pin!(result);
    tokio::pin!(watcher);
    tokio::select! {
        result = &mut result => {
            watcher.abort();
            result
        }
        watcher_result = &mut watcher => {
            match watcher_result {
                Ok(Ok(())) => anyhow::bail!("sync watcher stopped unexpectedly"),
                Ok(Err(error)) => Err(error.context("sync watcher failed")),
                Err(error) if error.is_cancelled() => anyhow::bail!("sync watcher stopped unexpectedly"),
                Err(error) => Err(error.into()),
            }
        }
    }
}

fn prepare_destination(uri: &str, name: &str) -> Result<String> {
    anyhow::ensure!(!uri.is_empty(), "sync {name} target must not be empty");
    let location = DatasetLocation::parse(uri)?;
    let Some(path) = location.local_path() else {
        return Ok(location.as_str().to_owned());
    };
    anyhow::ensure!(
        !path.as_os_str().is_empty(),
        "sync {name} target must not be empty"
    );
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let parent = fs::canonicalize(parent)
        .with_context(|| format!("canonicalize sync {name} target parent"))?;
    let filename = path
        .file_name()
        .with_context(|| format!("sync {name} target must name a directory"))?;
    Ok(parent.join(filename).to_string_lossy().into_owned())
}

fn ensure_targets_outside_source(
    source: &str,
    mirror: Option<&str>,
    to: Option<&str>,
) -> Result<()> {
    let source = DatasetLocation::parse(source)?;
    let Some(source_path) = source.local_path() else {
        return Ok(());
    };
    if let Some(mirror) = mirror {
        let mirror = DatasetLocation::parse(mirror)?;
        if let Some(mirror_path) = mirror.local_path() {
            anyhow::ensure!(
                !mirror_path.starts_with(source_path),
                "sync mirror target must be outside the source directory"
            );
        }
    }
    if let Some(to) = to {
        let to = DatasetLocation::parse(to)?;
        if let Some(to_path) = to.local_path() {
            anyhow::ensure!(
                !to_path.starts_with(source_path),
                "sync --to target must be outside the source directory"
            );
        }
    }
    Ok(())
}

async fn scan_source(uri: &str) -> Result<BTreeMap<PathBuf, FileStamp>> {
    let location = DatasetLocation::parse(uri)?;
    if let Some(root) = location.local_path() {
        anyhow::ensure!(root.is_dir(), "sync source must be a directory");
        let mut files = BTreeMap::new();
        for path in crate::exchange::collect_visible_json_files(root)? {
            let metadata = fs::metadata(&path)
                .with_context(|| format!("stat sync file {}", path.display()))?;
            files.insert(
                path.strip_prefix(root)?.to_path_buf(),
                FileStamp {
                    size: metadata.len(),
                    modified: metadata.modified().ok(),
                },
            );
        }
        return Ok(files);
    }

    let stamps = location
        .list_importable_json_object_stamps(
            persisting_pchronicle::storage::DEFAULT_MAX_LOCAL_QUERY_FILES,
        )
        .await
        .with_context(|| format!("list sync source objects under {uri}"))?;
    let mut files = BTreeMap::new();
    for (key, size, modified) in stamps {
        files.insert(
            PathBuf::from(key),
            FileStamp {
                size,
                modified: modified.and_then(parse_rfc3339_system_time),
            },
        );
    }
    Ok(files)
}

fn parse_rfc3339_system_time(value: String) -> Option<SystemTime> {
    chrono::DateTime::parse_from_rfc3339(&value)
        .ok()
        .map(|value| SystemTime::UNIX_EPOCH + Duration::from_secs(value.timestamp().max(0) as u64))
}

fn changed_paths(
    previous: &BTreeMap<PathBuf, FileStamp>,
    current: &BTreeMap<PathBuf, FileStamp>,
) -> BTreeSet<PathBuf> {
    previous
        .keys()
        .chain(current.keys())
        .filter(|path| previous.get(*path) != current.get(*path))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepare_destination_preserves_object_store_uri() {
        assert_eq!(
            prepare_destination("s3://bucket/prod/infra/agent/agentcompass", "mirror").unwrap(),
            "s3://bucket/prod/infra/agent/agentcompass"
        );
    }

    #[tokio::test]
    async fn sync_pin_source_is_resolved_not_canonicalized() {
        let mut stderr = Vec::new();
        let error = run(
            SyncArgs {
                from: "@origin/agentcompass".into(),
                mirror: None,
                to: Some("/tmp/pchronicle-sync-convert".into()),
                input_format: ExchangeFormat::Auto,
                suggested_format: None,
                columns: Vec::new(),
                interval_seconds: 1,
                once: true,
            },
            None,
            &mut stderr,
            false,
        )
        .await
        .expect_err("pin must expand through settings, not local canonicalize");
        let message = format!("{error:#}");
        assert!(!message.contains("canonicalize sync source"), "{message}");
        assert!(
            message.contains("unknown Dataset pin") || message.contains("resolve sync source"),
            "{message}"
        );
    }

    #[test]
    fn changed_paths_include_create_modify_and_delete() {
        let old = BTreeMap::from([(
            PathBuf::from("old.json"),
            FileStamp {
                size: 1,
                modified: None,
            },
        )]);
        let new = BTreeMap::from([(
            PathBuf::from("new.json"),
            FileStamp {
                size: 2,
                modified: None,
            },
        )]);
        assert_eq!(
            changed_paths(&old, &new),
            BTreeSet::from([PathBuf::from("old.json"), PathBuf::from("new.json")])
        );
    }

    #[tokio::test]
    async fn sync_once_requires_mirror_or_to() {
        let mut stderr = Vec::new();
        let error = run(
            SyncArgs {
                from: "/tmp/unused".into(),
                mirror: None,
                to: None,
                input_format: ExchangeFormat::Auto,
                suggested_format: None,
                columns: Vec::new(),
                interval_seconds: 1,
                once: true,
            },
            None,
            &mut stderr,
            false,
        )
        .await
        .expect_err("at least one destination required");
        assert!(
            format!("{error:#}").contains("requires --mirror and/or --to"),
            "{error:#}"
        );
    }

    #[tokio::test]
    async fn sync_once_rebuilds_storyline() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let source = temporary.path().join("source");
        fs::create_dir_all(&source)?;
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/onboard/support-ticket.json"),
            source.join("support-ticket.json"),
        )?;

        let storyline = temporary.path().join("storyline");
        let mut stderr = Vec::new();
        run(
            SyncArgs {
                from: source.to_string_lossy().into_owned(),
                mirror: None,
                to: Some(storyline.to_string_lossy().into_owned()),
                input_format: ExchangeFormat::Atif,
                suggested_format: None,
                columns: Vec::new(),
                interval_seconds: 1,
                once: true,
            },
            None,
            &mut stderr,
            false,
        )
        .await?;

        assert!(storyline.join("CURRENT").is_file());
        Ok(())
    }

    #[tokio::test]
    async fn sync_once_mirror_builds_compact_lance() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let source = temporary.path().join("source");
        fs::create_dir_all(&source)?;
        fs::write(
            source.join("events.jsonl"),
            r#"{"id":"a","timestamp":"2026-01-01T00:00:00Z","payload":1}
{"id":"b","timestamp":"2026-01-01T00:00:01Z","payload":2}
"#,
        )?;

        let mirror = temporary.path().join("mirror");
        let mut stderr = Vec::new();
        run(
            SyncArgs {
                from: source.to_string_lossy().into_owned(),
                mirror: Some(mirror.to_string_lossy().into_owned()),
                to: None,
                input_format: ExchangeFormat::CompactJsonl,
                suggested_format: None,
                columns: Vec::new(),
                interval_seconds: 1,
                once: true,
            },
            None,
            &mut stderr,
            false,
        )
        .await?;

        assert!(
            mirror.join("CURRENT").is_file()
                || mirror.join("_versions").is_dir()
                || mirror.exists()
        );
        Ok(())
    }
}
