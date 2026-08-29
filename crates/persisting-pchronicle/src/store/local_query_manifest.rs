//! Frozen local-file manifests shared by query format detection and readers.

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use anyhow::{Context, Result};

use crate::format::DocumentFormat;
use crate::formats::detect_format;

pub const DEFAULT_MAX_LOCAL_QUERY_FILES: usize = 1_000_000;
pub const DEFAULT_MAX_LOCAL_QUERY_ENTRIES: usize = 2_000_000;
pub const DEFAULT_MAX_LOCAL_QUERY_DETECTION_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalQueryManifestOptions {
    /// Hard limit for recursive manifest expansion.
    pub max_files: usize,
    /// Hard limit for visited directory entries, including non-input files.
    pub max_entries: usize,
    /// Hard limit for the first file read by automatic format detection.
    pub max_detection_bytes: u64,
}

impl Default for LocalQueryManifestOptions {
    fn default() -> Self {
        Self {
            max_files: DEFAULT_MAX_LOCAL_QUERY_FILES,
            max_entries: DEFAULT_MAX_LOCAL_QUERY_ENTRIES,
            max_detection_bytes: DEFAULT_MAX_LOCAL_QUERY_DETECTION_BYTES,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileFingerprint {
    len: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

impl FileFingerprint {
    fn read(path: &Path) -> Result<Self> {
        let metadata = fs::metadata(path)
            .with_context(|| format!("inspect trajectory query file {}", path.display()))?;
        anyhow::ensure!(
            metadata.is_file(),
            "trajectory query input is no longer a regular file: {}",
            path.display()
        );
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Ok(Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalQueryInputFile {
    path: PathBuf,
    relative_path: String,
    fingerprint: FileFingerprint,
}

impl LocalQueryInputFile {
    pub(crate) fn freeze(path: PathBuf, relative_path: String) -> Result<Self> {
        validate_relative_source_path(&relative_path)?;
        Ok(Self {
            fingerprint: FileFingerprint::read(&path)?,
            path,
            relative_path,
        })
    }

    pub(crate) fn detect_format_with_options(
        &self,
        options: LocalQueryManifestOptions,
    ) -> Result<DocumentFormat> {
        validate_options(options)?;
        self.validate_unchanged()?;
        let format = detect_query_format(&self.path, options)?;
        self.validate_unchanged()?;
        Ok(format)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn relative_path(&self) -> &str {
        &self.relative_path
    }

    pub fn size_bytes(&self) -> u64 {
        self.fingerprint.len
    }

    /// Reject files replaced or changed after the manifest was frozen.
    pub fn validate_unchanged(&self) -> Result<()> {
        let current = FileFingerprint::read(&self.path)?;
        anyhow::ensure!(
            current == self.fingerprint,
            "trajectory query file changed after manifest creation: {}",
            self.path.display()
        );
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct LocalQueryManifest {
    format: DocumentFormat,
    files: Arc<[LocalQueryInputFile]>,
}

impl LocalQueryManifest {
    /// Freeze the candidate file list and infer its format from the first file
    /// in stable relative-path order. Each selected file is fully validated by
    /// its format reader when a query scans it.
    #[cfg(test)]
    pub fn detect(path: impl AsRef<Path>) -> Result<Self> {
        Self::detect_with_options(path, LocalQueryManifestOptions::default())
    }

    pub fn detect_with_options(
        path: impl AsRef<Path>,
        options: LocalQueryManifestOptions,
    ) -> Result<Self> {
        let input = path.as_ref();
        validate_options(options)?;
        let paths = input_files_matching(
            input,
            crate::formats::registry::is_direct_query_candidate,
            options,
        )?;
        let first = paths
            .first()
            .with_context(|| format!("query input contains no JSON files: {}", input.display()))?;
        let format = detect_query_format(first, options)?;
        Self::from_paths(input, format, paths)
    }

    /// Freeze a local input for an explicitly selected query format.
    pub fn for_format(path: impl AsRef<Path>, format: DocumentFormat) -> Result<Self> {
        Self::for_format_with_options(path, format, LocalQueryManifestOptions::default())
    }

    pub fn for_format_with_options(
        path: impl AsRef<Path>,
        format: DocumentFormat,
        options: LocalQueryManifestOptions,
    ) -> Result<Self> {
        let input = path.as_ref();
        validate_options(options)?;
        validate_query_format(format, input)?;
        let handler = crate::formats::registry::get(format).ok_or_else(|| {
            anyhow::anyhow!(
                "unsupported direct query format '{}' in {}",
                format,
                input.display()
            )
        })?;
        anyhow::ensure!(
            handler.capabilities().direct_query,
            "unsupported direct query format '{}' in {}",
            format,
            input.display()
        );
        let paths = input_files_matching(input, |path| handler.is_candidate(path), options)?;
        anyhow::ensure!(
            !paths.is_empty(),
            "{} query input contains no supported files: {}",
            format,
            input.display()
        );
        Self::from_paths(input, format, paths)
    }

    fn from_paths(input: &Path, format: DocumentFormat, paths: Vec<PathBuf>) -> Result<Self> {
        let root = input.is_dir().then_some(input);
        let files = paths
            .into_iter()
            .map(|path| {
                let relative_path = relative_source_path(root, &path)?;
                Ok(LocalQueryInputFile {
                    fingerprint: FileFingerprint::read(&path)?,
                    path,
                    relative_path,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            format,
            files: files.into(),
        })
    }

    /// Build a frozen manifest from files already selected by a higher-level
    /// catalog. `relative_path` is the stable, mount-relative source key
    /// exposed to SQL as `_file_`.
    pub(crate) fn from_explicit_files(
        input: impl AsRef<Path>,
        format: DocumentFormat,
        files: Vec<(PathBuf, String)>,
    ) -> Result<Self> {
        let files = files
            .into_iter()
            .map(|(path, relative_path)| LocalQueryInputFile::freeze(path, relative_path))
            .collect::<Result<Vec<_>>>()?;
        Self::from_frozen_files(input, format, files)
    }

    /// Build a manifest from file identities frozen by a higher-level catalog.
    /// This lets the catalog defer content-based format detection until after
    /// `_file_` pruning without weakening its immutable snapshot boundary.
    pub(crate) fn from_frozen_files(
        input: impl AsRef<Path>,
        format: DocumentFormat,
        files: Vec<LocalQueryInputFile>,
    ) -> Result<Self> {
        validate_query_format(format, input.as_ref())?;
        anyhow::ensure!(!files.is_empty(), "query manifest contains no files");
        for file in &files {
            validate_relative_source_path(file.relative_path())?;
            file.validate_unchanged()?;
        }
        Ok(Self {
            format,
            files: files.into(),
        })
    }

    pub fn format(&self) -> DocumentFormat {
        self.format
    }

    pub fn files(&self) -> &[LocalQueryInputFile] {
        &self.files
    }

    pub fn file_count(&self) -> usize {
        self.files.len()
    }
}

fn validate_query_format(format: DocumentFormat, path: &Path) -> Result<()> {
    anyhow::ensure!(
        crate::formats::registry::supports_direct_query(format),
        "unsupported direct query format '{}' in {}",
        format,
        path.display()
    );
    Ok(())
}

fn detect_query_format(path: &Path, options: LocalQueryManifestOptions) -> Result<DocumentFormat> {
    let detection_len = fs::metadata(path)
        .with_context(|| {
            format!(
                "inspect query input for format detection: {}",
                path.display()
            )
        })?
        .len();
    anyhow::ensure!(
        detection_len <= options.max_detection_bytes,
        "format detection input {} is {detection_len} bytes, exceeding max_detection_bytes {}",
        path.display(),
        options.max_detection_bytes
    );
    let content = fs::read_to_string(path)
        .with_context(|| format!("read query input for format detection: {}", path.display()))?;
    let jsonl_excerpt;
    let detection_content = if is_json_lines(path) {
        jsonl_excerpt = content
            .lines()
            .filter(|line| !line.trim().is_empty())
            .take(32)
            .collect::<Vec<_>>()
            .join("\n");
        if jsonl_excerpt.is_empty() {
            content.as_str()
        } else {
            jsonl_excerpt.as_str()
        }
    } else {
        content.as_str()
    };
    let format = detect_format(Some(path), Some(detection_content))?
        .with_context(|| format!("cannot detect trajectory format: {}", path.display()))?;
    validate_query_format(format, path)?;
    Ok(format)
}

fn validate_options(options: LocalQueryManifestOptions) -> Result<()> {
    anyhow::ensure!(
        options.max_files > 0,
        "local query max_files must be greater than zero"
    );
    anyhow::ensure!(
        options.max_entries > 0,
        "local query max_entries must be greater than zero"
    );
    anyhow::ensure!(
        options.max_detection_bytes > 0,
        "local query max_detection_bytes must be greater than zero"
    );
    Ok(())
}

fn input_files_matching(
    path: &Path,
    mut is_candidate: impl FnMut(&Path) -> bool,
    options: LocalQueryManifestOptions,
) -> Result<Vec<PathBuf>> {
    if path.is_file() {
        return Ok(vec![path.to_path_buf()]);
    }
    anyhow::ensure!(
        path.is_dir(),
        "query input does not exist: {}",
        path.display()
    );
    let mut files = Vec::new();
    collect_input_files(path, &mut is_candidate, options, &mut files)?;
    files.sort();
    Ok(files)
}

fn collect_input_files(
    path: &Path,
    is_candidate: &mut impl FnMut(&Path) -> bool,
    options: LocalQueryManifestOptions,
    files: &mut Vec<PathBuf>,
) -> Result<()> {
    let mut pending = vec![path.to_path_buf()];
    let mut visited_entries = 0usize;
    while let Some(directory) = pending.pop() {
        let entries = fs::read_dir(&directory)
            .with_context(|| format!("read trajectory query directory {}", directory.display()))?;
        for entry in entries {
            let entry = entry?;
            visited_entries = visited_entries.saturating_add(1);
            anyhow::ensure!(
                visited_entries <= options.max_entries,
                "trajectory query traversal exceeds max_entries limit of {}",
                options.max_entries
            );
            let file_type = entry.file_type()?;
            let path = entry.path();
            if file_type.is_dir() {
                pending.push(path);
            } else if file_type.is_file() && is_candidate(&path) {
                files.push(path);
                anyhow::ensure!(
                    files.len() <= options.max_files,
                    "trajectory query manifest exceeds max_files limit of {}",
                    options.max_files
                );
            }
        }
    }
    Ok(())
}

fn relative_source_path(root: Option<&Path>, file: &Path) -> Result<String> {
    let relative = match root {
        Some(root) => file
            .strip_prefix(root)
            .with_context(|| format!("make {} relative to {}", file.display(), root.display()))?,
        None => Path::new(
            file.file_name()
                .with_context(|| format!("query input has no filename: {}", file.display()))?,
        ),
    };
    let components = relative
        .components()
        .map(|component| match component {
            Component::Normal(value) => value
                .to_str()
                .map(str::to_owned)
                .context("trajectory source path is not UTF-8"),
            _ => anyhow::bail!("trajectory source path is not a safe relative path"),
        })
        .collect::<Result<Vec<_>>>()?;
    anyhow::ensure!(!components.is_empty(), "trajectory source path is empty");
    Ok(components.join("/"))
}

fn validate_relative_source_path(path: &str) -> Result<()> {
    anyhow::ensure!(!path.is_empty(), "trajectory source path is empty");
    let components = Path::new(path)
        .components()
        .map(|component| match component {
            Component::Normal(value) => value
                .to_str()
                .context("trajectory source path is not UTF-8"),
            _ => anyhow::bail!("trajectory source path is not a safe relative path"),
        })
        .collect::<Result<Vec<_>>>()?;
    anyhow::ensure!(!components.is_empty(), "trajectory source path is empty");
    Ok(())
}

fn is_json_lines(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| matches!(value.to_ascii_lowercase().as_str(), "jsonl" | "ndjson"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_freezes_recursive_relative_paths() -> Result<()> {
        let temp = tempfile::tempdir()?;
        fs::create_dir(temp.path().join("nested"))?;
        fs::write(
            temp.path().join("nested/input.json"),
            r#"[{"session_id":"s1","step_id":0,"messages":[]}]"#,
        )?;
        let manifest = LocalQueryManifest::detect(temp.path())?;
        assert_eq!(manifest.format(), DocumentFormat::OpenaiMsg);
        assert_eq!(manifest.files()[0].relative_path(), "nested/input.json");
        Ok(())
    }

    #[test]
    fn manifest_limits_file_count_and_detects_replacement() -> Result<()> {
        let temp = tempfile::tempdir()?;
        for name in ["one.json", "two.json"] {
            fs::write(
                temp.path().join(name),
                r#"[{"session_id":"s1","step_id":0,"messages":[]}]"#,
            )?;
        }
        let error = LocalQueryManifest::detect_with_options(
            temp.path(),
            LocalQueryManifestOptions {
                max_files: 1,
                ..LocalQueryManifestOptions::default()
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("max_files"));

        let error = LocalQueryManifest::detect_with_options(
            temp.path(),
            LocalQueryManifestOptions {
                max_entries: 1,
                ..LocalQueryManifestOptions::default()
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("max_entries"));

        let error = LocalQueryManifest::detect_with_options(
            temp.path().join("one.json"),
            LocalQueryManifestOptions {
                max_detection_bytes: 1,
                ..LocalQueryManifestOptions::default()
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("max_detection_bytes"));

        let file = temp.path().join("one.json");
        let manifest = LocalQueryManifest::for_format(&file, DocumentFormat::OpenaiMsg)?;
        fs::write(&file, "[]")?;
        assert!(manifest.files()[0].validate_unchanged().is_err());
        Ok(())
    }

    #[test]
    fn frozen_file_detects_format_without_replacing_its_identity() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("input.json");
        fs::write(&path, r#"[{"session_id":"s1","step_id":0,"messages":[]}]"#)?;
        let file = LocalQueryInputFile::freeze(path.clone(), "input.json".into())?;
        assert_eq!(
            file.detect_format_with_options(LocalQueryManifestOptions::default())?,
            DocumentFormat::OpenaiMsg
        );

        fs::write(&path, "[]")?;
        assert!(
            file.detect_format_with_options(LocalQueryManifestOptions::default())
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn jsonl_detection_skips_non_transcript_preamble() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("session.jsonl");
        fs::write(
            &path,
            r#"{"type":"mode","mode":"normal","sessionId":"sess-1"}
{"type":"user","sessionId":"sess-1","uuid":"u1","message":{"role":"user","content":"hi"}}
"#,
        )?;
        let manifest = LocalQueryManifest::detect(&path)?;
        assert_eq!(manifest.format(), DocumentFormat::ClaudeCode);
        Ok(())
    }

    #[test]
    fn claude_code_skips_meta_json_sidecars() -> Result<()> {
        let temp = tempfile::tempdir()?;
        fs::write(
            temp.path().join(".meta.json"),
            r#"{"agentId":"agent-a","sessionId":"parent"}"#,
        )?;
        // Real Claude Code sessions place per-agent sidecars under
        // `<session>/subagents/agent-<id>.meta.json`; these must be skipped too.
        let subagents = temp.path().join("session").join("subagents");
        fs::create_dir_all(&subagents)?;
        fs::write(
            subagents.join("agent-a.meta.json"),
            r#"{"agentType":"general-purpose","description":"finder","toolUseId":"call_1"}"#,
        )?;
        fs::write(
            temp.path().join("session.jsonl"),
            r#"{"type":"user","sessionId":"parent","uuid":"u1","message":{"role":"user","content":"hi"}}
"#,
        )?;
        let manifest = LocalQueryManifest::for_format(temp.path(), DocumentFormat::ClaudeCode)?;
        assert_eq!(manifest.files().len(), 1);
        assert_eq!(manifest.files()[0].relative_path(), "session.jsonl");
        Ok(())
    }

    #[cfg(feature = "proptest")]
    mod proptests {
        use proptest::prelude::*;

        use super::*;

        proptest! {
            #[test]
            fn normal_relative_paths_are_accepted(
                components in proptest::collection::vec(
                    proptest::string::string_regex("[A-Za-z0-9_-][A-Za-z0-9_.-]{0,15}").unwrap(),
                    1..8,
                ),
            ) {
                let path = components.join("/");
                prop_assert!(validate_relative_source_path(&path).is_ok());
            }

            #[test]
            fn path_traversal_and_absolute_forms_are_rejected(
                component in proptest::string::string_regex("[A-Za-z0-9_-][A-Za-z0-9_.-]{0,15}").unwrap(),
            ) {
                let candidates = [
                    String::new(),
                    format!("../{component}"),
                    format!("{component}/../../escape"),
                    format!("/{component}"),
                    format!("./{component}"),
                ];
                for candidate in candidates {
                    prop_assert!(validate_relative_source_path(&candidate).is_err(), "{candidate}");
                }
            }

            #[test]
            fn json_lines_detection_is_case_insensitive(
                stem in proptest::string::string_regex("[A-Za-z0-9_-]{1,16}").unwrap(),
                uppercase in any::<bool>(),
            ) {
                let extension = if uppercase { "JSONL" } else { "jsonl" };
                prop_assert!(is_json_lines(Path::new(&format!("{stem}.{extension}"))), "stem={}", stem);
                prop_assert!(!is_json_lines(Path::new(&format!("{stem}.json"))), "stem={}", stem);
            }

            #[test]
            fn recursive_detection_preserves_generated_relative_paths(
                components in proptest::collection::vec(
                    proptest::string::string_regex("[A-Za-z0-9_-]{1,12}").unwrap(),
                    1..6,
                ),
            ) {
                let temp = tempfile::tempdir().unwrap();
                let relative = format!("{}.json", components.join("/"));
                let path = temp.path().join(&relative);
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent).unwrap();
                }
                fs::write(&path, r#"[{"session_id":"s1","step_id":0,"messages":[]}]"#).unwrap();

                let manifest = LocalQueryManifest::detect(temp.path()).unwrap();
                prop_assert_eq!(manifest.files().len(), 1);
                prop_assert_eq!(manifest.files()[0].relative_path(), relative);
            }
        }
    }
}
