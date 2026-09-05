use super::*;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::time::{Duration, SystemTime};

use clap::Args;

#[derive(Debug, Args)]
pub(crate) struct SyncArgs {
    /// Source directory to mirror.
    #[arg(long, value_name = "DIRECTORY")]
    pub(crate) from: PathBuf,

    /// Local Warehouse Dataset receiving source files; unused for compact-jsonl.
    #[arg(long = "to", alias = "warehouse", value_name = "DIRECTORY")]
    pub(crate) to: PathBuf,

    /// Local Storyline or compact JSONL Lance Dataset receiving each snapshot.
    #[arg(long = "convert", alias = "storyline", value_name = "DIRECTORY")]
    pub(crate) convert: PathBuf,

    /// Input format. compact-jsonl requires a tree of .jsonl files.
    #[arg(long = "input-format", value_enum, default_value_t = ExchangeFormat::Auto)]
    pub(crate) input_format: ExchangeFormat,

    /// Compact JSONL mapping; id/timestamp override $.id/$.timestamp defaults.
    #[arg(long = "column", value_name = "NAME=JSON_PATH", action = clap::ArgAction::Append)]
    pub(crate) columns: Vec<String>,

    /// Polling and update interval. Supports ms, s, m, and h.
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

pub(crate) async fn run(args: SyncArgs, stderr: &mut dyn Write) -> Result<()> {
    let source = fs::canonicalize(&args.from)
        .with_context(|| format!("canonicalize sync source {}", args.from.display()))?;
    anyhow::ensure!(source.is_dir(), "sync source must be a directory");
    let warehouse = prepare_target(&args.to, "Warehouse")?;
    let storyline = prepare_target(&args.convert, "conversion")?;
    anyhow::ensure!(warehouse != storyline, "sync targets must be different");
    anyhow::ensure!(
        !warehouse.starts_with(&source) && !storyline.starts_with(&source),
        "sync targets must be outside the source directory"
    );

    let interval = Duration::from_secs(args.interval_seconds.max(1));
    if args.once {
        let initial = scan_files(&source)?;
        anyhow::ensure!(
            !initial.is_empty(),
            "sync source contains no supported JSON files"
        );
        super::exchange::sync_snapshot(
            &source,
            &warehouse,
            &storyline,
            args.input_format,
            &args.columns,
        )
        .await?;
        writeln!(stderr, "sync batch={} status=ok", initial.len())
            .context("write sync progress")?;
        return Ok(());
    }

    let (changes_tx, mut changes_rx) = tokio::sync::mpsc::channel::<PathBuf>(1024);
    let watcher_source = source.clone();
    let watcher = tokio::spawn(async move {
        let mut previous = BTreeMap::new();
        loop {
            // ponytail: dependency-free polling; use an OS watcher when tree size or latency
            // makes recursive scans measurable.
            let current = scan_files(&watcher_source)?;
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
                &source,
                &warehouse,
                &storyline,
                args.input_format,
                &args.columns,
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

fn prepare_target(path: &Path, name: &str) -> Result<PathBuf> {
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
    Ok(parent.join(filename))
}

fn scan_files(root: &Path) -> Result<BTreeMap<PathBuf, FileStamp>> {
    let mut pending = vec![root.to_path_buf()];
    let mut files = BTreeMap::new();
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory)
            .with_context(|| format!("read sync directory {}", directory.display()))?
        {
            let entry = entry?;
            let file_type = entry.file_type()?;
            let path = entry.path();
            if file_type.is_dir() {
                pending.push(path);
            } else if file_type.is_file() && is_sync_candidate(&path) {
                let metadata = entry.metadata()?;
                files.insert(
                    path.strip_prefix(root)?.to_path_buf(),
                    FileStamp {
                        size: metadata.len(),
                        modified: metadata.modified().ok(),
                    },
                );
            }
        }
    }
    Ok(files)
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

fn is_sync_candidate(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "json" | "jsonl" | "ndjson"
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

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
    async fn once_mirrors_files_and_builds_storyline() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let source = temporary.path().join("source");
        let warehouse = temporary.path().join("warehouse");
        let storyline = temporary.path().join("storyline");
        fs::create_dir_all(&source)?;
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/onboard/support-ticket.json"),
            source.join("support-ticket.json"),
        )?;
        let source_bytes = fs::read(source.join("support-ticket.json"))?;
        let mut stderr = Vec::new();
        run(
            SyncArgs {
                from: source,
                to: warehouse,
                convert: storyline.clone(),
                input_format: ExchangeFormat::Auto,
                columns: Vec::new(),
                interval_seconds: 1,
                once: true,
            },
            &mut stderr,
        )
        .await?;
        assert_eq!(
            fs::read(temporary.path().join("warehouse/support-ticket.json"))?,
            source_bytes
        );
        assert!(storyline.join("CURRENT").is_file());
        Ok(())
    }
}
