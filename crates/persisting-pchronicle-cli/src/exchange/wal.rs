//! Local checkpoint WAL for resumable Storyline imports.
//!
//! Stores only source-path completion state (not payload bytes). The remote
//! progressive Storyline generation remains the source of truth for written
//! data; the WAL avoids re-fetching / re-parsing sources that already committed.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const WAL_ROOT_DIRNAME: &str = ".pchronicle-import-wal";
const JOB_FILE: &str = "job.json";
const DONE_FILE: &str = "done.jsonl";
const FAILED_FILE: &str = "failed.jsonl";
const CURSOR_FILE: &str = "cursor.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ImportWalJob {
    pub(crate) job_id: String,
    pub(crate) from: String,
    pub(crate) to: String,
    pub(crate) output_format: String,
    pub(crate) suggested_format: Option<String>,
    pub(crate) created_unix_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DoneRecord {
    path: String,
    trajectories: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FailedRecord {
    path: String,
    error: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CursorRecord {
    path: String,
    updated_unix_secs: u64,
}

#[derive(Debug)]
pub(crate) struct ImportWal {
    dir: PathBuf,
    job: ImportWalJob,
    done: HashSet<String>,
    failed: HashSet<String>,
}

impl ImportWal {
    pub(crate) fn job_id(
        from: &str,
        to: &str,
        output_format: &str,
        suggested_format: Option<&str>,
    ) -> String {
        let mut hasher = blake3::Hasher::new();
        hasher.update(from.as_bytes());
        hasher.update(&[0]);
        hasher.update(to.as_bytes());
        hasher.update(&[0]);
        hasher.update(output_format.as_bytes());
        hasher.update(&[0]);
        hasher.update(suggested_format.unwrap_or("").as_bytes());
        hasher.finalize().to_hex()[..32].to_string()
    }

    pub(crate) fn default_root() -> PathBuf {
        PathBuf::from(WAL_ROOT_DIRNAME)
    }

    pub(crate) fn job_dir(root: &Path, job_id: &str) -> PathBuf {
        root.join(job_id)
    }

    pub(crate) fn open_or_create(
        root: &Path,
        from: &str,
        to: &str,
        output_format: &str,
        suggested_format: Option<&str>,
        resume: bool,
        reset: bool,
    ) -> Result<Self> {
        let job_id = Self::job_id(from, to, output_format, suggested_format);
        let dir = Self::job_dir(root, &job_id);
        if reset && dir.exists() {
            fs::remove_dir_all(&dir)
                .with_context(|| format!("reset import WAL {}", dir.display()))?;
        }
        if resume {
            anyhow::ensure!(
                dir.join(JOB_FILE).is_file(),
                "no import WAL at {} for --resume; omit --resume to start a new job or pass --reset",
                dir.display()
            );
        }
        fs::create_dir_all(&dir).with_context(|| format!("create import WAL {}", dir.display()))?;
        let job_path = dir.join(JOB_FILE);
        let job = if job_path.is_file() {
            let text = fs::read_to_string(&job_path)
                .with_context(|| format!("read import WAL job {}", job_path.display()))?;
            let existing: ImportWalJob = serde_json::from_str(&text)
                .with_context(|| format!("parse import WAL job {}", job_path.display()))?;
            anyhow::ensure!(
                existing.from == from && existing.to == to,
                "import WAL job fingerprint mismatch at {}",
                dir.display()
            );
            existing
        } else {
            let created = ImportWalJob {
                job_id: job_id.clone(),
                from: from.to_owned(),
                to: to.to_owned(),
                output_format: output_format.to_owned(),
                suggested_format: suggested_format.map(str::to_owned),
                created_unix_secs: unix_secs(),
            };
            let encoded = serde_json::to_vec_pretty(&created).context("encode import WAL job")?;
            fs::write(&job_path, encoded)
                .with_context(|| format!("write import WAL job {}", job_path.display()))?;
            created
        };
        let done = load_done_paths(&dir.join(DONE_FILE))?;
        let failed = load_failed_paths(&dir.join(FAILED_FILE))?;
        Ok(Self {
            dir,
            job,
            done,
            failed,
        })
    }

    pub(crate) fn dir(&self) -> &Path {
        &self.dir
    }

    pub(crate) fn job(&self) -> &ImportWalJob {
        &self.job
    }

    #[cfg(test)]
    pub(crate) fn should_skip(&self, path: &str) -> bool {
        self.done.contains(path) || self.failed.contains(path)
    }

    pub(crate) fn done_count(&self) -> usize {
        self.done.len()
    }

    pub(crate) fn failed_count(&self) -> usize {
        self.failed.len()
    }

    pub(crate) fn skip_paths(&self) -> HashSet<String> {
        self.done
            .iter()
            .chain(self.failed.iter())
            .cloned()
            .collect()
    }

    pub(crate) fn mark_done(&mut self, path: &str, trajectories: u64) -> Result<()> {
        if !self.done.insert(path.to_owned()) {
            return Ok(());
        }
        self.failed.remove(path);
        append_jsonl(
            &self.dir.join(DONE_FILE),
            &DoneRecord {
                path: path.to_owned(),
                trajectories,
            },
        )?;
        self.write_cursor(path)?;
        Ok(())
    }

    pub(crate) fn mark_failed(&mut self, path: &str, error: &str) -> Result<()> {
        if self.done.contains(path) {
            return Ok(());
        }
        let first = self.failed.insert(path.to_owned());
        if first {
            append_jsonl(
                &self.dir.join(FAILED_FILE),
                &FailedRecord {
                    path: path.to_owned(),
                    error: truncate_error(error),
                },
            )?;
        }
        self.write_cursor(path)?;
        Ok(())
    }

    fn write_cursor(&self, path: &str) -> Result<()> {
        let cursor = CursorRecord {
            path: path.to_owned(),
            updated_unix_secs: unix_secs(),
        };
        let encoded = serde_json::to_vec_pretty(&cursor).context("encode import WAL cursor")?;
        fs::write(self.dir.join(CURSOR_FILE), encoded)
            .with_context(|| format!("write import WAL cursor in {}", self.dir.display()))?;
        Ok(())
    }
}

fn unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn truncate_error(error: &str) -> String {
    const MAX: usize = 2_048;
    if error.len() <= MAX {
        error.to_owned()
    } else {
        format!("{}…", &error[..MAX])
    }
}

fn append_jsonl(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("open import WAL {}", path.display()))?;
    serde_json::to_writer(&mut file, value)
        .with_context(|| format!("encode import WAL record for {}", path.display()))?;
    file.write_all(b"\n")
        .with_context(|| format!("append import WAL newline to {}", path.display()))?;
    Ok(())
}

fn load_done_paths(path: &Path) -> Result<HashSet<String>> {
    if !path.is_file() {
        return Ok(HashSet::new());
    }
    let file = File::open(path).with_context(|| format!("read import WAL {}", path.display()))?;
    let mut out = HashSet::new();
    for (index, line) in BufReader::new(file).lines().enumerate() {
        let line =
            line.with_context(|| format!("read import WAL {} line {}", path.display(), index + 1))?;
        if line.trim().is_empty() {
            continue;
        }
        let record: DoneRecord = serde_json::from_str(&line)
            .with_context(|| format!("parse import WAL {} line {}", path.display(), index + 1))?;
        out.insert(record.path);
    }
    Ok(out)
}

fn load_failed_paths(path: &Path) -> Result<HashSet<String>> {
    if !path.is_file() {
        return Ok(HashSet::new());
    }
    let file = File::open(path).with_context(|| format!("read import WAL {}", path.display()))?;
    let mut out = HashSet::new();
    for (index, line) in BufReader::new(file).lines().enumerate() {
        let line =
            line.with_context(|| format!("read import WAL {} line {}", path.display(), index + 1))?;
        if line.trim().is_empty() {
            continue;
        }
        let record: FailedRecord = serde_json::from_str(&line)
            .with_context(|| format!("parse import WAL {} line {}", path.display(), index + 1))?;
        out.insert(record.path);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_id_is_stable_for_same_fingerprint() {
        let left = ImportWal::job_id("@a", "@b", "storyline-lance", Some("actf"));
        let right = ImportWal::job_id("@a", "@b", "storyline-lance", Some("actf"));
        assert_eq!(left, right);
        assert_ne!(left, ImportWal::job_id("@a", "@b", "storyline-lance", None));
    }

    #[test]
    fn resume_requires_existing_wal_and_skip_sets_work() {
        let root = tempfile::tempdir().unwrap();
        let err = ImportWal::open_or_create(
            root.path(),
            "from",
            "to",
            "storyline-lance",
            None,
            true,
            false,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("--resume"), "{err}");

        let mut wal = ImportWal::open_or_create(
            root.path(),
            "from",
            "to",
            "storyline-lance",
            None,
            false,
            false,
        )
        .unwrap();
        wal.mark_done("a.json", 2).unwrap();
        wal.mark_failed("b.json", "boom").unwrap();

        let resumed = ImportWal::open_or_create(
            root.path(),
            "from",
            "to",
            "storyline-lance",
            None,
            true,
            false,
        )
        .unwrap();
        assert!(resumed.should_skip("a.json"));
        assert!(resumed.should_skip("b.json"));
        assert!(!resumed.should_skip("c.json"));
        assert_eq!(resumed.done_count(), 1);
        assert_eq!(resumed.failed_count(), 1);
    }

    #[test]
    fn reset_clears_prior_state() {
        let root = tempfile::tempdir().unwrap();
        let mut wal = ImportWal::open_or_create(
            root.path(),
            "from",
            "to",
            "storyline-lance",
            None,
            false,
            false,
        )
        .unwrap();
        wal.mark_done("a.json", 1).unwrap();
        let reset = ImportWal::open_or_create(
            root.path(),
            "from",
            "to",
            "storyline-lance",
            None,
            false,
            true,
        )
        .unwrap();
        assert!(!reset.should_skip("a.json"));
        assert_eq!(reset.done_count(), 0);
    }
}
