//! Import candidates, decode, format resolution, and validation.

use super::super::*;
use super::progress::{CliProgress, StageId};
use anyhow::{Context, Result};
use persisting_pchronicle::document::{
    DocumentFormat, InputIssue, InputIssueKind, decode_json_storylines, detect_format,
    open_document,
};
use persisting_pchronicle::model::StorylineDocument;
use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub(crate) struct ImportFileCandidate {
    pub(crate) path: PathBuf,
    pub(crate) relative_path: PathBuf,
    pub(crate) output_relative_path: Option<PathBuf>,
    /// Prefetched bytes (tests / rare callers). Normal imports leave this empty
    /// and read local paths or object-store keys on demand.
    pub(crate) content: Option<Vec<u8>>,
    /// Object-store Dataset root URI; when set, bytes are fetched lazily.
    pub(crate) remote_root: Option<String>,
    /// Size from discovery (`stat` / object metadata) for progress totals.
    pub(crate) size_hint: u64,
}

#[derive(Debug)]
pub(crate) struct ImportedSource {
    pub(crate) source_path: String,
    pub(crate) format: DocumentFormat,
    pub(crate) trajectories: usize,
    pub(crate) input_bytes: usize,
}

pub(crate) fn exchange_document_format(format: ExchangeFormat) -> Option<DocumentFormat> {
    match format {
        ExchangeFormat::Atif => Some(DocumentFormat::Atif),
        ExchangeFormat::Actf => Some(DocumentFormat::Actf),
        ExchangeFormat::OpenaiMessages => Some(DocumentFormat::OpenaiMsg),
        ExchangeFormat::Storyline => Some(DocumentFormat::Storyline),
        ExchangeFormat::Codex => Some(DocumentFormat::Codex),
        ExchangeFormat::ClaudeCode => Some(DocumentFormat::ClaudeCode),
        ExchangeFormat::CompactJsonl | ExchangeFormat::Auto => None,
    }
}

pub(crate) fn apply_duplicate_document_policy(
    storyline: &mut StorylineDocument,
    seen_document_ids: &mut HashSet<String>,
    duplicate_policy: DuplicateIdPolicy,
) -> Option<String> {
    let original = storyline.document_id().to_string();
    match duplicate_policy {
        DuplicateIdPolicy::Suffix => uniquify_storyline_document_id(storyline, seen_document_ids)
            .map(|(original, renamed)| {
                format!("warning: duplicate document_id '{original}' renamed to '{renamed}'")
            }),
        DuplicateIdPolicy::Skip => {
            if !seen_document_ids.insert(original.clone()) {
                Some(format!(
                    "warning: duplicate document_id '{original}' skipped"
                ))
            } else {
                None
            }
        }
    }
}

pub(crate) fn collect_import_candidates(input: &Path) -> Result<(bool, Vec<ImportFileCandidate>)> {
    let metadata = std::fs::symlink_metadata(input)
        .with_context(|| format!("inspect import input {}", input.display()))?;
    let explicit_file = if metadata.file_type().is_symlink() {
        std::fs::metadata(input)
            .with_context(|| format!("inspect import input target {}", input.display()))?
            .is_file()
    } else {
        metadata.is_file()
    };
    if explicit_file {
        let relative_path = input
            .file_name()
            .map(PathBuf::from)
            .context("import input file has no filename")?;
        let size_hint = metadata.len();
        return Ok((
            false,
            vec![ImportFileCandidate {
                path: input.to_path_buf(),
                relative_path,
                output_relative_path: None,
                content: None,
                remote_root: None,
                size_hint,
            }],
        ));
    }
    anyhow::ensure!(
        metadata.is_dir(),
        "import input must be a regular file or directory"
    );

    let paths = collect_visible_json_files(input)?;
    let mut candidates = Vec::with_capacity(paths.len());
    for path in paths {
        let relative_path = path
            .strip_prefix(input)
            .context("derive Dataset-relative import source path")?
            .to_path_buf();
        let size_hint = std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
        candidates.push(ImportFileCandidate {
            path,
            output_relative_path: Some(relative_path.clone()),
            relative_path,
            content: None,
            remote_root: None,
            size_hint,
        });
    }
    candidates.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    if candidates.is_empty() {
        return Err(cli_boundary_error(
            BoundaryCode::InvalidRequest,
            "import directory contains no .json, .jsonl, or .ndjson files",
        ));
    }
    Ok((true, candidates))
}

/// Recursively collect absolute paths of visible `.json` / `.jsonl` / `.ndjson`
/// files under `root`. Shared by `import` and `sync`; not Catalog Directory
/// discovery (which is one-level and skips loose files).
pub(crate) fn collect_visible_json_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut pending = vec![root.to_path_buf()];
    let mut files = Vec::new();
    while let Some(directory) = pending.pop() {
        let mut entries = std::fs::read_dir(&directory)
            .with_context(|| format!("read directory {}", directory.display()))?
            .collect::<std::io::Result<Vec<_>>>()?;
        entries.sort_by_key(std::fs::DirEntry::path);
        for entry in entries {
            let file_type = entry.file_type()?;
            if file_type.is_symlink() {
                continue;
            }
            let path = entry.path();
            if file_type.is_dir() {
                pending.push(path);
            } else if file_type.is_file() && is_visible_json_file(&path) {
                let relative = path
                    .strip_prefix(root)
                    .unwrap_or(path.as_path())
                    .to_string_lossy()
                    .replace('\\', "/");
                if relative.split('/').any(|part| part == "_meta") {
                    continue;
                }
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}

pub(crate) fn is_visible_json_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "json" | "jsonl" | "ndjson"
            )
        })
}

pub(crate) async fn load_import_candidate_bytes(
    candidate: &ImportFileCandidate,
    max_input_bytes: usize,
    label: &str,
) -> Result<Vec<u8>> {
    if let Some(content) = &candidate.content {
        anyhow::ensure!(
            content.len() <= max_input_bytes,
            "{label} exceeds max_input_bytes limit of {max_input_bytes}"
        );
        return Ok(content.clone());
    }
    if let Some(remote_root) = &candidate.remote_root {
        let key = candidate.relative_path.to_string_lossy().replace('\\', "/");
        let location = DatasetLocation::parse(remote_root)?;
        let bytes = location
            .read_relative_bytes(&key)
            .await
            .with_context(|| format!("read import object {key} under {remote_root}"))?;
        anyhow::ensure!(
            bytes.len() <= max_input_bytes,
            "{label} exceeds max_input_bytes limit of {max_input_bytes}"
        );
        return Ok(bytes);
    }
    let file = std::fs::File::open(&candidate.path).with_context(|| format!("open {label}"))?;
    read_bounded(file, max_input_bytes, label)
}

pub(crate) fn scope_import_source_error(error: anyhow::Error, source_path: &Path) -> anyhow::Error {
    if let Some(boundary) = error.downcast_ref::<CliBoundaryError>() {
        return cli_boundary_error(
            boundary.code,
            format!("{}: {}", source_path.display(), boundary.message),
        );
    }
    error.context(format!("import source {}", source_path.display()))
}

pub(crate) struct DecodedImportSource {
    pub(crate) diagnostic_path: PathBuf,
    pub(crate) metadata: ImportedSource,
    pub(crate) storylines: Vec<StorylineDocument>,
}

pub(crate) enum DecodeImportOutcome {
    Imported(DecodedImportSource),
    Skipped { path: PathBuf, reason: String },
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ImportFormatResolution {
    Format(ExchangeFormat),
    Skip(String),
}

pub(crate) enum StorylineImportInputs<'a> {
    Stdin(Option<&'a mut dyn Read>),
}

pub(crate) struct StorylineImportIterator<'a> {
    pub(crate) requested_format: ExchangeFormat,
    pub(crate) suggested_format: Option<ExchangeFormat>,
    pub(crate) max_input_bytes: usize,
    pub(crate) progress: &'a mut CliProgress,
    pub(crate) inputs: StorylineImportInputs<'a>,
    pub(crate) current: std::vec::IntoIter<StorylineDocument>,
    pub(crate) imported_sources: Vec<ImportedSource>,
    pub(crate) unknown_field_warnings: persisting_pchronicle::model::UnknownFieldImportWarnings,
    pub(crate) skipped_warnings: Vec<String>,
    pub(crate) seen_document_ids: HashSet<String>,
    pub(crate) duplicate_policy: DuplicateIdPolicy,
    pub(crate) failed: bool,
}

impl<'a> StorylineImportIterator<'a> {
    pub(crate) fn stdin(
        requested_format: ExchangeFormat,
        suggested_format: Option<ExchangeFormat>,
        max_input_bytes: usize,
        stdin: &'a mut dyn Read,
        progress: &'a mut CliProgress,
        seen_document_ids: HashSet<String>,
        duplicate_policy: DuplicateIdPolicy,
    ) -> Self {
        Self {
            requested_format,
            suggested_format,
            max_input_bytes,
            progress,
            inputs: StorylineImportInputs::Stdin(Some(stdin)),
            current: Vec::new().into_iter(),
            imported_sources: Vec::new(),
            unknown_field_warnings:
                persisting_pchronicle::model::UnknownFieldImportWarnings::default(),
            skipped_warnings: Vec::new(),
            seen_document_ids,
            duplicate_policy,
            failed: false,
        }
    }

    pub(crate) async fn decode_next_source(&mut self) -> Result<Option<DecodedImportSource>> {
        loop {
            let outcome = match &mut self.inputs {
                StorylineImportInputs::Stdin(stdin) => {
                    let Some(stdin) = stdin.take() else {
                        return Ok(None);
                    };
                    self.progress.stage(StageId::Fetch).set_current("stdin");
                    let input = read_bounded(stdin, self.max_input_bytes, "stdin")?;
                    self.progress.note_fetched("stdin", input.len() as u64)?;
                    self.progress.stage(StageId::Parse).set_current("stdin");
                    decode_import_source(
                        self.requested_format,
                        self.suggested_format,
                        ImportOutputFormat::Storyline,
                        None,
                        None,
                        None,
                        &input,
                        &mut self.unknown_field_warnings,
                    )?
                }
            };
            match outcome {
                DecodeImportOutcome::Imported(decoded) => {
                    self.progress.note_parsed(
                        &decoded.diagnostic_path.to_string_lossy(),
                        decoded.metadata.input_bytes as u64,
                    )?;
                    return Ok(Some(decoded));
                }
                DecodeImportOutcome::Skipped { path, reason } => {
                    self.progress.note_parsed(&path.to_string_lossy(), 0)?;
                    self.skipped_warnings
                        .push(skipped_import_warning(&path, &reason));
                }
            }
        }
    }

    pub(crate) fn into_result_parts(
        self,
    ) -> (
        Vec<ImportedSource>,
        persisting_pchronicle::model::UnknownFieldImportWarnings,
        Vec<String>,
        &'a mut CliProgress,
    ) {
        (
            self.imported_sources,
            self.unknown_field_warnings,
            self.skipped_warnings,
            self.progress,
        )
    }

    pub(crate) async fn next_document(&mut self) -> Option<Result<StorylineDocument>> {
        loop {
            if let Some(mut storyline) = self.current.next() {
                let original = storyline.document_id().to_string();
                match self.duplicate_policy {
                    DuplicateIdPolicy::Suffix => {
                        if let Some((original, renamed)) = uniquify_storyline_document_id(
                            &mut storyline,
                            &mut self.seen_document_ids,
                        ) {
                            self.skipped_warnings.push(format!(
                                "warning: duplicate document_id '{original}' renamed to '{renamed}'"
                            ));
                        }
                    }
                    DuplicateIdPolicy::Skip => {
                        if !self.seen_document_ids.insert(original.clone()) {
                            self.skipped_warnings.push(format!(
                                "warning: duplicate document_id '{original}' skipped"
                            ));
                            continue;
                        }
                    }
                }
                let metadata = self
                    .imported_sources
                    .last_mut()
                    .expect("decoded Storyline has source metadata");
                metadata.trajectories = metadata
                    .trajectories
                    .checked_add(1)
                    .expect("import trajectory count overflow");
                return Some(Ok(storyline));
            }
            if self.failed {
                return None;
            }
            match self.decode_next_source().await {
                Ok(Some(decoded)) => {
                    let mut metadata = decoded.metadata;
                    metadata.trajectories = 0;
                    self.imported_sources.push(metadata);
                    self.current = decoded.storylines.into_iter();
                }
                Ok(None) => return None,
                Err(error) => {
                    self.failed = true;
                    return Some(Err(error));
                }
            }
        }
    }
}

pub(crate) fn uniquify_storyline_document_id(
    story: &mut StorylineDocument,
    seen: &mut HashSet<String>,
) -> Option<(String, String)> {
    let preferred = story.document_id().to_string();
    if seen.insert(preferred.clone()) {
        return None;
    }
    let mut suffix = 1u64;
    let renamed = loop {
        let candidate = format!("{preferred}#{suffix}");
        if seen.insert(candidate.clone()) {
            break candidate;
        }
        suffix = suffix
            .checked_add(1)
            .expect("document_id disambiguation suffix overflow");
    };
    if story
        .trajectory_id
        .as_deref()
        .is_some_and(|id| !id.is_empty())
    {
        story.trajectory_id = Some(renamed.clone());
    } else {
        story.session_id = renamed.clone();
    }
    Some((preferred, renamed))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn decode_import_source(
    requested_format: ExchangeFormat,
    suggested_format: Option<ExchangeFormat>,
    output_format: ImportOutputFormat,
    input_path: Option<&Path>,
    decode_relative_path: Option<&Path>,
    logical_source_path: Option<&Path>,
    input: &[u8],
    unknown_field_warnings: &mut persisting_pchronicle::model::UnknownFieldImportWarnings,
) -> Result<DecodeImportOutcome> {
    let diagnostic_path = decode_relative_path
        .unwrap_or_else(|| Path::new("stdin"))
        .to_path_buf();
    let text = std::str::from_utf8(input).map_err(|error| {
        cli_boundary_error(
            BoundaryCode::InvalidRequest,
            format!("{} is not UTF-8: {error}", diagnostic_path.display()),
        )
    })?;
    let allow_skip = requested_format == ExchangeFormat::Auto && logical_source_path.is_some();
    let format = match resolve_import_format(
        requested_format,
        suggested_format,
        input_path,
        text,
        allow_skip,
    )
    .map_err(|error| {
        if logical_source_path.is_some() {
            scope_import_source_error(error, &diagnostic_path)
        } else {
            error
        }
    })? {
        ImportFormatResolution::Format(format) => format,
        ImportFormatResolution::Skip(reason) => {
            return Ok(DecodeImportOutcome::Skipped {
                path: diagnostic_path,
                reason,
            });
        }
    };
    let document_format = exchange_document_format(format)
        .context("supported import format must map to a physical document format")?;
    let source_path = logical_source_path
        .map(PathBuf::from)
        .unwrap_or_else(|| single_import_source_path(format, output_format, input_path));
    let decode_relative_path = decode_relative_path.unwrap_or(&source_path);
    let storylines =
        decode_json_storylines(document_format, text, decode_relative_path).map_err(|issue| {
            let code = match issue.kind() {
                InputIssueKind::Invalid => BoundaryCode::InvalidRequest,
                InputIssueKind::Unsupported => BoundaryCode::Unsupported,
            };
            cli_boundary_error(
                code,
                import_input_issue_message(&issue, decode_relative_path),
            )
        });
    let storylines = match storylines {
        Ok(storylines) => storylines,
        Err(error) if allow_skip => {
            return Ok(DecodeImportOutcome::Skipped {
                path: diagnostic_path,
                reason: error.to_string(),
            });
        }
        Err(error) => return Err(error),
    };
    unknown_field_warnings
        .observe_storylines(&storylines)
        .map_err(|issue| {
            cli_boundary_error(
                BoundaryCode::InvalidRequest,
                import_input_issue_message(&issue, decode_relative_path),
            )
        })?;

    let metadata = ImportedSource {
        source_path: source_path
            .to_str()
            .context("Dataset-relative import Source path is not UTF-8")?
            .to_owned(),
        format: document_format,
        trajectories: storylines.len(),
        input_bytes: input.len(),
    };
    Ok(DecodeImportOutcome::Imported(DecodedImportSource {
        diagnostic_path,
        metadata,
        storylines,
    }))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn stage_preserved_import_source(
    requested_format: ExchangeFormat,
    suggested_format: Option<ExchangeFormat>,
    input_path: Option<&Path>,
    decode_relative_path: Option<&Path>,
    logical_source_path: Option<&Path>,
    input: &[u8],
    staging_root: &Path,
    unknown_field_warnings: &mut persisting_pchronicle::model::UnknownFieldImportWarnings,
    skipped_warnings: &mut Vec<String>,
) -> Result<Option<ImportedSource>> {
    let decoded = match decode_import_source(
        requested_format,
        suggested_format,
        ImportOutputFormat::Preserve,
        input_path,
        decode_relative_path,
        logical_source_path,
        input,
        unknown_field_warnings,
    )? {
        DecodeImportOutcome::Imported(decoded) => decoded,
        DecodeImportOutcome::Skipped { path, reason } => {
            skipped_warnings.push(skipped_import_warning(&path, &reason));
            return Ok(None);
        }
    };
    validate_import_storylines(&decoded.storylines).map_err(|error| {
        if logical_source_path.is_some() {
            scope_import_source_error(error, &decoded.diagnostic_path)
        } else {
            error
        }
    })?;

    let staged_source = staging_root.join(&decoded.metadata.source_path);
    let staged_parent = staged_source
        .parent()
        .context("staged import Source has no parent")?;
    std::fs::create_dir_all(staged_parent)
        .with_context(|| format!("create staged Source parent {}", staged_parent.display()))?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&staged_source)
        .with_context(|| format!("create staged Source {}", decoded.metadata.source_path))?;
    file.write_all(input)
        .with_context(|| format!("write staged Source {}", decoded.metadata.source_path))?;
    file.sync_all()
        .with_context(|| format!("sync staged Source {}", decoded.metadata.source_path))?;
    Ok(Some(decoded.metadata))
}

pub(crate) fn read_bounded(
    mut reader: impl Read,
    max_bytes: usize,
    label: &str,
) -> Result<Vec<u8>> {
    let mut input = Vec::new();
    if max_bytes == usize::MAX {
        reader
            .read_to_end(&mut input)
            .with_context(|| format!("read {label}"))?;
    } else {
        let limit = u64::try_from(max_bytes)
            .ok()
            .and_then(|limit| limit.checked_add(1))
            .ok_or_else(|| {
                cli_boundary_error(
                    BoundaryCode::InvalidRequest,
                    "--max-input-bytes is too large",
                )
            })?;
        reader
            .by_ref()
            .take(limit)
            .read_to_end(&mut input)
            .with_context(|| format!("read {label}"))?;
        if input.len() > max_bytes {
            return Err(cli_boundary_error(
                BoundaryCode::ResourceExhausted,
                format!("{label} exceeds max_input_bytes limit of {max_bytes}"),
            ));
        }
    }
    if input.is_empty() {
        return Err(cli_boundary_error(
            BoundaryCode::InvalidRequest,
            format!("{label} is empty"),
        ));
    }
    Ok(input)
}

pub(crate) fn resolve_import_format(
    requested: ExchangeFormat,
    suggested: Option<ExchangeFormat>,
    input_path: Option<&Path>,
    input: &str,
    allow_skip: bool,
) -> Result<ImportFormatResolution> {
    let format = match requested {
        ExchangeFormat::Auto => match detect_format(input_path, Some(input))? {
            Some(DocumentFormat::Atif) => ExchangeFormat::Atif,
            Some(DocumentFormat::Actf) => ExchangeFormat::Actf,
            Some(DocumentFormat::OpenaiMsg) => ExchangeFormat::OpenaiMessages,
            Some(DocumentFormat::Storyline) => ExchangeFormat::Storyline,
            Some(DocumentFormat::Codex) => ExchangeFormat::Codex,
            Some(DocumentFormat::ClaudeCode) => ExchangeFormat::ClaudeCode,
            Some(format) if allow_skip => {
                return Ok(ImportFormatResolution::Skip(format!(
                    "detected import format '{format}' is not a queryable JSON format"
                )));
            }
            Some(format) => {
                return Err(cli_boundary_error(
                    BoundaryCode::Unsupported,
                    format!("detected import format '{format}' is not a queryable JSON format"),
                ));
            }
            None => {
                if let Some(hint) = suggested.filter(|format| *format != ExchangeFormat::Auto)
                    && suggested_format_compatible(hint, input_path, input)
                {
                    hint
                } else if allow_skip && looks_like_json_document(input) {
                    return Ok(ImportFormatResolution::Skip(
                        "cannot detect import format".into(),
                    ));
                } else {
                    return Err(cli_boundary_error(
                        BoundaryCode::InvalidRequest,
                        if suggested.is_some() {
                            "cannot detect import format; --suggested-format did not match this file (pass --format to force)"
                        } else {
                            "cannot detect import format; pass --format explicitly or --suggested-format to assist"
                        },
                    ));
                }
            }
        },
        ExchangeFormat::Atif => ExchangeFormat::Atif,
        ExchangeFormat::Actf => ExchangeFormat::Actf,
        ExchangeFormat::OpenaiMessages => ExchangeFormat::OpenaiMessages,
        ExchangeFormat::Storyline => ExchangeFormat::Storyline,
        ExchangeFormat::Codex => ExchangeFormat::Codex,
        ExchangeFormat::ClaudeCode => ExchangeFormat::ClaudeCode,
        ExchangeFormat::CompactJsonl => ExchangeFormat::CompactJsonl,
    };
    if !matches!(
        format,
        ExchangeFormat::Atif
            | ExchangeFormat::Actf
            | ExchangeFormat::OpenaiMessages
            | ExchangeFormat::Storyline
            | ExchangeFormat::Codex
            | ExchangeFormat::ClaudeCode
            | ExchangeFormat::CompactJsonl
    ) {
        return Err(cli_boundary_error(
            BoundaryCode::Unsupported,
            format!(
                "import format '{format}' is not supported by the first queryable import increment"
            ),
        ));
    }
    Ok(ImportFormatResolution::Format(format))
}

/// Weak compatibility check used only with `--suggested-format`.
///
/// Stronger than blind force, weaker than auto fingerprint: the file must still
/// look like the suggested family before we accept the hint.
pub(crate) fn suggested_format_compatible(
    suggested: ExchangeFormat,
    input_path: Option<&Path>,
    input: &str,
) -> bool {
    match suggested {
        ExchangeFormat::Actf => weakly_compatible_actf(input),
        ExchangeFormat::Atif => weakly_compatible_json_keys(input, &["agent", "steps"]),
        ExchangeFormat::Storyline => {
            weakly_compatible_json_keys(input, &["schema_version", "session", "turns"])
                || weakly_compatible_json_keys(input, &["schema_version", "session", "agent"])
        }
        ExchangeFormat::OpenaiMessages => {
            weakly_compatible_json_keys(input, &["session_id", "messages"])
                || weakly_compatible_json_keys(input, &["messages", "step_id"])
        }
        ExchangeFormat::Codex | ExchangeFormat::ClaudeCode => {
            looks_like_json_document(input)
                && input_path.is_some_and(|path| {
                    path.extension()
                        .and_then(|ext| ext.to_str())
                        .is_some_and(|ext| {
                            matches!(ext.to_ascii_lowercase().as_str(), "jsonl" | "ndjson")
                        })
                })
        }
        ExchangeFormat::CompactJsonl | ExchangeFormat::Auto => false,
    }
}

fn weakly_compatible_actf(input: &str) -> bool {
    // Assist only: root shape, not trajectory schema fingerprint.
    // Avoid full JSON parse so Python NaN dumps still qualify.
    let trimmed = input.trim_start();
    (trimmed.starts_with('{') || trimmed.starts_with('['))
        && trimmed.contains("\"task_id\"")
        && trimmed.contains("\"attempts\"")
}

fn weakly_compatible_json_keys(input: &str, required: &[&str]) -> bool {
    let trimmed = input.trim_start();
    let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        return false;
    };
    let Some(object) = value.as_object() else {
        return false;
    };
    required.iter().all(|key| object.contains_key(*key))
}

pub(crate) fn looks_like_json_document(input: &str) -> bool {
    let trimmed = input.trim_start();
    if !(trimmed.starts_with('{') || trimmed.starts_with('[')) {
        return false;
    }
    if serde_json::from_str::<serde_json::Value>(trimmed).is_ok() {
        return true;
    }
    trimmed
        .lines()
        .find(|line| !line.trim().is_empty())
        .is_some_and(|line| serde_json::from_str::<serde_json::Value>(line).is_ok())
}

pub(crate) fn skipped_import_warning(path: &Path, reason: &str) -> String {
    format!(
        "warning: skipped import source {}: {reason}",
        path.display()
    )
}

pub(crate) fn empty_auto_directory_import_error(directory_input: bool) -> anyhow::Error {
    cli_boundary_error(
        BoundaryCode::InvalidRequest,
        if directory_input {
            "import directory contains no detectable trajectory files"
        } else {
            "cannot detect import format; pass --format explicitly"
        },
    )
}

pub(crate) fn import_source_name(format: ExchangeFormat) -> &'static str {
    match format {
        ExchangeFormat::Atif => "trajectories.atif.json",
        ExchangeFormat::Actf => "trajectories.actf.json",
        ExchangeFormat::OpenaiMessages => "session_steps.json",
        ExchangeFormat::Storyline => "trajectories.storyline.json",
        ExchangeFormat::Codex => "session.codex.jsonl",
        ExchangeFormat::ClaudeCode => "session.claude-code.jsonl",
        ExchangeFormat::CompactJsonl => "compact.jsonl",
        _ => unreachable!("unsupported import format was rejected"),
    }
}

pub(crate) fn single_import_source_path(
    format: ExchangeFormat,
    output_format: ImportOutputFormat,
    input_path: Option<&Path>,
) -> PathBuf {
    if format == ExchangeFormat::Atif && output_format == ImportOutputFormat::Preserve {
        let line_extension = input_path
            .and_then(Path::extension)
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase)
            .filter(|extension| matches!(extension.as_str(), "jsonl" | "ndjson"));
        if let Some(extension) = line_extension {
            return PathBuf::from(format!("trajectories.atif.{extension}"));
        }
    }
    PathBuf::from(import_source_name(format))
}

pub(crate) fn import_input_issue_message(issue: &InputIssue, source_path: &Path) -> String {
    match issue.location() {
        Some(location) => format!("{} {location}: {}", source_path.display(), issue.message()),
        None => format!("{}: {}", source_path.display(), issue.message()),
    }
}

pub(crate) fn validate_import_storylines(storylines: &[StorylineDocument]) -> Result<usize> {
    Ok(storylines.len())
}

pub(crate) async fn validate_import_source(format: ExchangeFormat, path: &Path) -> Result<usize> {
    let format = exchange_document_format(format)
        .context("supported import format must map to a physical document format")?;
    let source = open_document(format, path).await?;
    let mut seen = HashSet::new();
    let mut document_count = 0usize;
    source
        .for_each_storyline(|story| {
            let document_id = story.document_id();
            if !seen.insert(document_id.to_string()) {
                return Err(cli_boundary_error(
                    BoundaryCode::InvalidRequest,
                    "import contains duplicate document_id",
                ));
            }
            document_count = document_count
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("import document count overflow"))?;
            Ok(())
        })
        .await?;
    Ok(document_count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn visible_json_extensions() {
        assert!(is_visible_json_file(Path::new("a.json")));
        assert!(is_visible_json_file(Path::new("a.JSONL")));
        assert!(is_visible_json_file(Path::new("a.ndjson")));
        assert!(!is_visible_json_file(Path::new("a.txt")));
    }

    #[test]
    fn looks_like_json_document_smoke() {
        assert!(looks_like_json_document(r#"{"a":1}"#));
        assert!(looks_like_json_document("\n[1,2]\n"));
        assert!(!looks_like_json_document("not json"));
    }

    #[test]
    fn exchange_document_format_maps_known() {
        assert_eq!(
            exchange_document_format(ExchangeFormat::Atif),
            Some(DocumentFormat::Atif)
        );
        assert!(exchange_document_format(ExchangeFormat::Auto).is_none());
    }

    #[test]
    fn suggested_actf_assists_when_auto_fingerprint_misses() {
        // Object trajectory with steps but no ACTF_ schema_version: auto stays None.
        let input = r#"{
            "task_id":"travel-planning",
            "attempts":{"1":{
                "correct":false,
                "trajectory":{
                    "steps":[],
                    "started_at":"2026-06-17T07:26:27Z",
                    "finished_at":"2026-06-17T07:26:28Z"
                }
            }}
        }"#;
        let err = resolve_import_format(ExchangeFormat::Auto, None, None, input, false)
            .unwrap_err()
            .to_string();
        assert!(err.contains("cannot detect import format"));
        assert_eq!(
            resolve_import_format(
                ExchangeFormat::Auto,
                Some(ExchangeFormat::Actf),
                None,
                input,
                false
            )
            .unwrap(),
            ImportFormatResolution::Format(ExchangeFormat::Actf)
        );
    }

    #[test]
    fn suggested_actf_rejects_incompatible_shape() {
        let input = r#"{"error":"boom","message":"no pe"}"#;
        assert!(!suggested_format_compatible(
            ExchangeFormat::Actf,
            None,
            input
        ));
        let err = resolve_import_format(
            ExchangeFormat::Auto,
            Some(ExchangeFormat::Actf),
            None,
            input,
            false,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("--suggested-format did not match"));
    }
}
