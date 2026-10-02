use super::*;
use crate::store::catalog::manifest::{ManifestKind, try_load_manifest};
use crate::store::opendal_store::Store as OpendalStore;

#[derive(Debug)]
pub(super) enum Candidate {
    Storyline {
        file: String,
        uri: String,
        size_bytes: Option<u64>,
        last_modified: Option<String>,
    },

    Compact {
        file: String,
        uri: String,
        size_bytes: Option<u64>,
        last_modified: Option<String>,
    },
    LocalFile {
        file: String,
        root: PathBuf,
        path: PathBuf,
        size_bytes: u64,
        last_modified: Option<String>,
    },
    RemoteFile {
        file: String,
        store: OpendalStore,
        meta: RemoteObjectMeta,
    },
}

impl Candidate {
    pub(super) fn source_stub(&self) -> DiscoveredSource {
        let (file, format, kind, size_bytes, last_modified, revision) = match self {
            Self::Storyline {
                file,
                size_bytes,
                last_modified,
                ..
            } => (
                file.clone(),
                Some(DocumentFormat::StorylineLance.as_str().to_string()),
                CatalogSourceKind::Store,
                *size_bytes,
                last_modified.clone(),
                None,
            ),
            Self::Compact {
                file,
                uri,
                size_bytes,
                last_modified,
            } => (
                file.clone(),
                Some("compact-jsonl/v1".into()),
                CatalogSourceKind::Store,
                *size_bytes,
                last_modified.clone(),
                Some(CatalogSourceRevision::LocalFile {
                    fingerprint: local_snapshot_ref(Path::new(uri)),
                }),
            ),
            Self::LocalFile {
                file,
                path,
                size_bytes,
                last_modified,
                ..
            } => (
                file.clone(),
                None,
                CatalogSourceKind::File,
                Some(*size_bytes),
                last_modified.clone(),
                Some(CatalogSourceRevision::LocalFile {
                    fingerprint: local_snapshot_ref(path),
                }),
            ),
            Self::RemoteFile { file, meta, .. } => (
                file.clone(),
                None,
                CatalogSourceKind::File,
                Some(meta.size),
                Some(meta.last_modified.clone()),
                Some(remote_source_revision(meta)),
            ),
        };
        DiscoveredSource {
            file,
            format,
            kind,
            revision,
            size_bytes,
            last_modified,
            status: CatalogSourceStatus::Ready,
            error: None,
            record_count: None,
            failed_count: None,
        }
    }
}

pub(super) async fn freeze_candidate(
    mount: &DatasetMount,
    candidate: Candidate,
    temporary_files: Arc<SnapshotTempDir>,
    options: CatalogSnapshotOptions,
) -> Result<(DiscoveredSource, Arc<LazySource>)> {
    let mut source_row = candidate.source_stub();
    match candidate {
        Candidate::Storyline { file, uri, .. } => {
            ensure_format_hint(mount, DocumentFormat::StorylineLance, &file)?;
            let paths = StorylineDataSource::pin_uri(&uri)
                .await
                .with_context(|| format!("pin Storyline source {uri}"))?;
            source_row.revision = Some(CatalogSourceRevision::Storyline {
                generation: paths.generation.clone(),
            });
            if let Ok(Some(manifest)) =
                crate::store::catalog::manifest::load_manifest_at_uri(&uri).await
                && manifest.is_storyline_leaf()
                && let Some(stats) = manifest.stats
            {
                source_row.record_count = Some(stats.record_count);
                source_row.failed_count = Some(stats.failed_count);
            }
            Ok((
                source_row,
                Arc::new(LazySource::new(
                    file,
                    LazySourceSpec::Storyline { paths },
                    options,
                    temporary_files,
                )),
            ))
        }

        Candidate::Compact { file, uri, .. } => {
            if let Some(manifest) =
                crate::store::catalog::manifest::try_load_manifest(Path::new(&uri))
                && let Some(stats) = manifest.stats
            {
                source_row.record_count = Some(stats.record_count);
                source_row.failed_count = Some(stats.failed_count);
            }
            Ok((
                source_row,
                Arc::new(LazySource::new(
                    file,
                    LazySourceSpec::Compact { uri },
                    options,
                    temporary_files,
                )),
            ))
        }
        Candidate::LocalFile {
            file,
            root,
            path,
            size_bytes,
            ..
        } => {
            anyhow::ensure!(
                size_bytes <= options.files.max_file_bytes,
                "trajectory query file {file} is {size_bytes} bytes, exceeding max_file_bytes {}",
                options.files.max_file_bytes
            );
            // Keep format detection behind LazySource::resolve so an exact
            // `_file_` predicate can prune unrelated malformed files before
            // any of their contents are opened.
            source_row.format = mount.format_hint.map(|format| format.as_str().to_string());
            let frozen_file = LocalQueryInputFile::freeze(path, file.clone())?;
            Ok((
                source_row,
                Arc::new(LazySource::new(
                    file,
                    LazySourceSpec::LocalFile {
                        root,
                        file: frozen_file,
                        format_hint: mount.format_hint,
                    },
                    options,
                    temporary_files,
                )),
            ))
        }
        Candidate::RemoteFile { file, store, meta } => {
            anyhow::ensure!(
                meta.size <= options.manifest.max_detection_bytes,
                "format detection input {file} is {} bytes, exceeding max_detection_bytes {}",
                meta.size,
                options.manifest.max_detection_bytes
            );
            anyhow::ensure!(
                meta.size <= options.files.max_file_bytes,
                "trajectory query file {file} is {} bytes, exceeding max_file_bytes {}",
                meta.size,
                options.files.max_file_bytes
            );
            source_row.format = mount.format_hint.map(|format| format.as_str().to_string());
            Ok((
                source_row,
                Arc::new(LazySource::new(
                    file,
                    LazySourceSpec::RemoteFile {
                        store,
                        meta,
                        format_hint: mount.format_hint,
                    },
                    options,
                    temporary_files,
                )),
            ))
        }
    }
}

fn ensure_format_hint(mount: &DatasetMount, actual: DocumentFormat, file: &str) -> Result<()> {
    if let Some(expected) = mount.format_hint {
        anyhow::ensure!(
            expected == actual,
            "Dataset source {file} is {actual}, but --source selected {expected}"
        );
    }
    Ok(())
}

#[cfg(all(test, feature = "proptest"))]
mod proptests {
    use proptest::prelude::*;

    use super::*;

    proptest! {
        #[test]
        fn source_format_hints_must_match_the_candidate_format(
            name in proptest::string::string_regex("[A-Za-z_][A-Za-z0-9_]{0,19}").unwrap(),
        ) {
            let mount = DatasetMount::new(name, "memory://catalog/source").unwrap();
            prop_assert!(ensure_format_hint(&mount, DocumentFormat::Atif, "data.json").is_ok());
            let hinted = mount.with_format_hint(DocumentFormat::StorylineLance);
            prop_assert!(ensure_format_hint(&hinted, DocumentFormat::StorylineLance, "storyline").is_ok());
            prop_assert!(ensure_format_hint(&hinted, DocumentFormat::Atif, "data.json").is_err());
        }
    }
}

pub(super) async fn discover_candidates(
    mount: &DatasetMount,
    options: LocalQueryManifestOptions,
) -> Result<Vec<Candidate>> {
    if let Some(path) = local_mount_path(&mount.uri) {
        discover_local_candidates(&mount.uri, &path, options).await
    } else {
        discover_object_candidates(&mount.uri, options).await
    }
}

pub(super) async fn discover_cached_candidates(
    mount: &DatasetMount,
    files: &[String],
    options: LocalQueryManifestOptions,
) -> Result<Vec<Candidate>> {
    if local_mount_path(&mount.uri).is_none() {
        // Remote discovery is mount-wide; do it once and filter the result.
        let wanted: BTreeSet<_> = files.iter().map(|file| file.trim_matches('/')).collect();
        return Ok(discover_candidates(mount, options)
            .await?
            .into_iter()
            .filter(|candidate| wanted.contains(candidate.source_stub().file.as_str()))
            .collect());
    }
    let mut candidates = Vec::new();
    for file in files {
        candidates.extend(discover_cached_candidate_at(mount, file, options).await?);
    }
    Ok(candidates)
}

pub(super) async fn discover_cached_candidate_at(
    mount: &DatasetMount,
    file: &str,
    options: LocalQueryManifestOptions,
) -> Result<Vec<Candidate>> {
    let file = file.trim().trim_matches('/');
    anyhow::ensure!(
        !file.is_empty()
            && !file
                .split('/')
                .any(|part| part.is_empty() || matches!(part, "." | "..") || part.contains('\\')),
        "invalid cached source path"
    );
    let Some(root) = local_mount_path(&mount.uri) else {
        // Remote cache entries are hints only until object metadata can be
        // validated without a full listing.
        return discover_candidates(mount, options).await;
    };
    let path = root.join(file);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).context("inspect cached source"),
    };
    if metadata.is_file() {
        anyhow::ensure!(
            is_json_candidate(&path),
            "cached source is not a supported JSON file"
        );
        return Ok(vec![Candidate::LocalFile {
            file: file.to_owned(),
            root: root.clone(),
            path,
            size_bytes: metadata.len(),
            last_modified: modified_string(&metadata),
        }]);
    }
    let candidates = match classify_local_dir(&root, &path).await? {
        LocalDirClass::Leaf(candidates) => candidates,
        LocalDirClass::Skip => Vec::new(),
        LocalDirClass::Recurse => {
            collect_local_virtual(&root, &path, DiscoveryBudget::new(options)).await?
        }
    };
    Ok(candidates)
}

pub(super) async fn discover_candidate_at(
    mount: &DatasetMount,
    file: &str,
    options: LocalQueryManifestOptions,
) -> Result<Vec<Candidate>> {
    let file = file.trim().trim_matches('/');
    anyhow::ensure!(
        !file.is_empty()
            && (file == "."
                || !file.split('/').any(|part| part.is_empty()
                    || matches!(part, "." | "..")
                    || part.contains('\\'))),
        "invalid source scope"
    );
    let Some(root) = local_mount_path(&mount.uri) else {
        return discover_object_candidate_at(&mount.uri, file, options).await;
    };
    let root_metadata = fs::metadata(&root).context("inspect scoped Dataset root")?;
    if file == "." || root_metadata.is_file() {
        return discover_candidates(mount, options).await;
    }
    let mut current = root.clone();
    let mut budget = DiscoveryBudget::new(options);
    let parts: Vec<_> = file.split('/').collect();
    for (index, part) in parts.iter().enumerate() {
        // Respect opaque Dataset ancestors. Directly jumping into a Lance
        // interior would change the source namespace.
        match classify_local_dir(&root, &current).await? {
            LocalDirClass::Recurse => {}
            _ => return discover_candidates(mount, options).await,
        }
        current.push(part);
        budget.observe_entry()?;
        let metadata = match fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error).context("inspect scoped source"),
        };
        if metadata.file_type().is_symlink() {
            return Ok(Vec::new());
        }
        if metadata.is_file() {
            if index + 1 != parts.len() || !is_json_candidate(&current) {
                return Ok(Vec::new());
            }
            budget.observe_source()?;
            return Ok(vec![Candidate::LocalFile {
                file: file.to_owned(),
                root,
                path: current,
                size_bytes: metadata.len(),
                last_modified: modified_string(&metadata),
            }]);
        }
    }
    let candidates = match classify_local_dir(&root, &current).await? {
        LocalDirClass::Leaf(candidates) => {
            anyhow::ensure!(
                candidates.len() <= options.max_files,
                "scoped discovery exceeds max_files"
            );
            candidates
        }
        LocalDirClass::Skip => Vec::new(),
        LocalDirClass::Recurse => collect_local_virtual(&root, &current, budget).await?,
    };
    Ok(candidates)
}

async fn discover_local_candidates(
    original_uri: &str,
    root: &Path,
    options: LocalQueryManifestOptions,
) -> Result<Vec<Candidate>> {
    anyhow::ensure!(
        options.max_entries > 0,
        "catalog max_entries must be positive"
    );
    anyhow::ensure!(options.max_files > 0, "catalog max_files must be positive");
    anyhow::ensure!(
        root.exists(),
        "Dataset input does not exist: {original_uri}"
    );
    if root.is_file() {
        anyhow::ensure!(
            is_json_candidate(root),
            "unsupported Dataset file: {original_uri}"
        );
        let metadata = fs::metadata(root)?;
        return Ok(vec![Candidate::LocalFile {
            file: root
                .file_name()
                .and_then(|name| name.to_str())
                .context("Dataset input filename is not UTF-8")?
                .to_string(),
            root: root
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .to_path_buf(),
            path: root.to_path_buf(),
            size_bytes: metadata.len(),
            last_modified: modified_string(&metadata),
        }]);
    }
    anyhow::ensure!(
        root.is_dir(),
        "Dataset input is not a directory: {original_uri}"
    );

    match classify_local_dir(root, root).await? {
        LocalDirClass::Leaf(candidates) => {
            anyhow::ensure!(
                candidates.len() <= options.max_files,
                "Dataset manifest exceeds max_files limit of {}",
                options.max_files
            );
            Ok(candidates)
        }
        LocalDirClass::Skip => Ok(Vec::new()),
        LocalDirClass::Recurse => {
            collect_local_virtual(root, root, DiscoveryBudget::new(options)).await
        }
    }
}

struct DiscoveryBudget {
    max_files: usize,
    max_entries: usize,
    files: usize,
    entries: usize,
}

impl DiscoveryBudget {
    fn new(options: LocalQueryManifestOptions) -> Self {
        Self {
            max_files: options.max_files,
            max_entries: options.max_entries,
            files: 0,
            entries: 0,
        }
    }

    fn observe_entry(&mut self) -> Result<()> {
        self.entries += 1;
        anyhow::ensure!(
            self.entries <= self.max_entries,
            "Dataset manifest exceeds max_entries limit of {}",
            self.max_entries
        );
        Ok(())
    }

    fn observe_source(&mut self) -> Result<()> {
        self.files += 1;
        anyhow::ensure!(
            self.files <= self.max_files,
            "Dataset manifest exceeds max_files limit of {}",
            self.max_files
        );
        Ok(())
    }
}

enum LocalDirClass {
    Leaf(Vec<Candidate>),
    Recurse,
    Skip,
}

fn catalog_file_name(mount_root: &Path, path: &Path) -> Result<String> {
    if path == mount_root {
        Ok(".".into())
    } else {
        relative_catalog_path(mount_root, path, true)
    }
}

fn candidate_from_local_leaf_manifest(
    mount_root: &Path,
    current: &Path,
    manifest: &crate::store::ChronicleManifest,
) -> Result<Candidate> {
    let file = catalog_file_name(mount_root, current)?;
    if manifest.is_compact_jsonl_leaf() {
        let metadata = fs::metadata(current)?;
        return Ok(Candidate::Compact {
            file,
            uri: canonical_local_uri(current)?,
            size_bytes: Some(metadata.len()),
            last_modified: modified_string(&metadata),
        });
    }
    if manifest.is_storyline_leaf() {
        anyhow::ensure!(
            current.join("CURRENT").is_file(),
            "storyline chronicle.manifest requires CURRENT at {}",
            current.display()
        );
        let current_meta = fs::metadata(current.join("CURRENT"))?;
        return Ok(Candidate::Storyline {
            file,
            uri: canonical_local_uri(current)?,
            size_bytes: Some(current_meta.len()),
            last_modified: modified_string(&current_meta),
        });
    }
    anyhow::bail!(
        "chronicle.manifest leaf format {:?} is not supported for discovery yet",
        manifest.format
    )
}

async fn classify_local_dir(mount_root: &Path, path: &Path) -> Result<LocalDirClass> {
    if let Some(manifest) = try_load_manifest(path) {
        return match manifest.kind {
            ManifestKind::Leaf => Ok(LocalDirClass::Leaf(vec![
                candidate_from_local_leaf_manifest(mount_root, path, &manifest)?,
            ])),
            ManifestKind::Branch => Ok(LocalDirClass::Recurse),
        };
    }
    if path.join("CURRENT").is_file() {
        let metadata = fs::metadata(path.join("CURRENT"))?;
        return Ok(LocalDirClass::Leaf(vec![Candidate::Storyline {
            file: catalog_file_name(mount_root, path)?,
            uri: canonical_local_uri(path)?,
            size_bytes: Some(metadata.len()),
            last_modified: modified_string(&metadata),
        }]));
    }
    if is_lance_directory(path) {
        if is_compact_jsonl_directory(path).await? {
            let metadata = fs::metadata(path)?;
            return Ok(LocalDirClass::Leaf(vec![Candidate::Compact {
                file: catalog_file_name(mount_root, path)?,
                uri: canonical_local_uri(path)?,
                size_bytes: Some(metadata.len()),
                last_modified: modified_string(&metadata),
            }]));
        }
        return Ok(LocalDirClass::Skip);
    }
    Ok(LocalDirClass::Recurse)
}

async fn collect_local_virtual(
    mount_root: &Path,
    start: &Path,
    mut budget: DiscoveryBudget,
) -> Result<Vec<Candidate>> {
    let mut candidates = Vec::new();
    let mut stack = vec![start.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let mut entries = fs::read_dir(&dir)
            .with_context(|| format!("read Dataset directory {}", dir.display()))?
            .collect::<std::io::Result<Vec<_>>>()?;
        entries.sort_by_key(|entry| entry.path());
        for entry in entries {
            let file_type = entry.file_type()?;
            if file_type.is_symlink() {
                continue;
            }
            budget.observe_entry()?;
            let path = entry.path();
            if file_type.is_dir() {
                match classify_local_dir(mount_root, &path).await? {
                    LocalDirClass::Leaf(sources) => {
                        for source in sources {
                            budget.observe_source()?;
                            candidates.push(source);
                        }
                    }
                    LocalDirClass::Recurse => stack.push(path),
                    LocalDirClass::Skip => {}
                }
            } else if file_type.is_file() && is_json_candidate(&path) {
                budget.observe_source()?;
                let metadata = entry.metadata()?;
                candidates.push(Candidate::LocalFile {
                    file: relative_catalog_path(mount_root, &path, false)?,
                    root: mount_root.to_path_buf(),
                    path,
                    size_bytes: metadata.len(),
                    last_modified: modified_string(&metadata),
                });
            }
        }
    }
    candidates.sort_by(|left, right| left.source_stub().file.cmp(&right.source_stub().file));
    Ok(candidates)
}

async fn is_compact_jsonl_directory(path: &Path) -> Result<bool> {
    if let Some(manifest) = try_load_manifest(path) {
        return Ok(manifest.is_compact_jsonl_leaf());
    }
    let dataset = match crate::storage::open_lance_dataset(path.to_string_lossy().as_ref()).await {
        Ok(dataset) => dataset,
        Err(_) => return Ok(false),
    };
    let is_compact = dataset
        .schema()
        .metadata
        .get("pchronicle.format")
        .is_some_and(|value| value == "compact-jsonl/v1");
    if is_compact {
        // Store-layer upgrade path for pre-manifest datasets: first discovery
        // that opens Lance also publishes chronicle.manifest.
        let _ = crate::store::CompactJsonlStore::ensure_manifest(path).await?;
    }
    Ok(is_compact)
}

async fn discover_object_candidates(
    uri: &str,
    options: LocalQueryManifestOptions,
) -> Result<Vec<Candidate>> {
    anyhow::ensure!(options.max_files > 0, "catalog max_files must be positive");
    let store = OpendalStore::from_uri(uri).await?;

    match probe_object_prefix(&store, uri, "", ".").await? {
        Some(ObjectProbe::Source(candidate)) => Ok(vec![candidate]),
        Some(ObjectProbe::Branch) | None => collect_object_virtual(&store, uri, "", options).await,
    }
}

// Follow only the requested ancestry. Opaque leaves keep their namespace
// semantics; unrelated siblings must never gate an exact read.
async fn discover_object_candidate_at(
    uri: &str,
    file: &str,
    mut options: LocalQueryManifestOptions,
) -> Result<Vec<Candidate>> {
    if file == "." {
        return discover_object_candidates(uri, options).await;
    }
    let store = OpendalStore::from_uri(uri).await?;
    let mut budget = DiscoveryBudget::new(options);
    let mut current = String::new();
    let mut parts = file.split('/');
    loop {
        budget.observe_entry()?;
        if current == file
            && is_json_candidate(Path::new(file))
            && let Some(entry) = store.stat_file(file).await?
        {
            budget.observe_source()?;
            return Ok(vec![Candidate::RemoteFile {
                file: file.into(),
                store: store.clone(),
                meta: RemoteObjectMeta::from(entry),
            }]);
        }
        match probe_object_prefix(&store, uri, &current, root_source_path(&current)).await? {
            Some(ObjectProbe::Source(candidate)) => {
                let candidates = vec![candidate];
                budget.observe_source()?;
                if !candidates.iter().any(|candidate| {
                    let source = candidate.source_stub().file;
                    source == file || source.starts_with(&format!("{file}/"))
                }) {
                    return Ok(Vec::new());
                }
                return Ok(candidates);
            }
            None if current.ends_with(".lance") => return Ok(Vec::new()),
            Some(ObjectProbe::Branch) | None => {}
        }
        let Some(part) = parts.next() else {
            options.max_entries = options.max_entries.saturating_sub(budget.entries);
            return collect_object_virtual(&store, uri, file, options).await;
        };
        if !current.is_empty() {
            current.push('/');
        }
        current.push_str(part);
    }
}

enum ObjectProbe {
    Source(Candidate),
    Branch,
}

async fn object_shallow_children(
    store: &OpendalStore,
    relative: &str,
) -> Result<(BTreeSet<String>, Vec<(String, RemoteObjectMeta)>)> {
    let prefix = if relative.is_empty() {
        String::new()
    } else {
        format!("{}/", relative.trim_end_matches('/'))
    };
    let entries = store
        .list_shallow(&prefix)
        .await
        .with_context(|| format!("list object prefix '{prefix}'"))?;
    let mut child_dirs = BTreeSet::new();
    let mut files = Vec::new();
    for entry in entries {
        let path = entry
            .path
            .strip_prefix(&prefix)
            .unwrap_or(&entry.path)
            .trim_matches('/');
        if path.is_empty() {
            continue;
        }
        let child = path.split('/').next().unwrap_or(path);
        if child.is_empty() {
            continue;
        }
        if entry.mode == opendal::EntryMode::FILE && !path.contains('/') {
            files.push((
                child.to_string(),
                RemoteObjectMeta {
                    location: entry.path.clone(),
                    size: entry.metadata.content_length(),
                    etag: entry.metadata.etag().map(ToOwned::to_owned),
                    version: entry.metadata.version().map(ToOwned::to_owned),
                    last_modified: entry
                        .metadata
                        .last_modified()
                        .map(|value| value.to_string())
                        .unwrap_or_default(),
                },
            ));
            continue;
        }
        child_dirs.insert(child.to_string());
    }
    Ok((child_dirs, files))
}

async fn collect_object_virtual(
    store: &OpendalStore,
    root_uri: &str,
    relative: &str,
    options: LocalQueryManifestOptions,
) -> Result<Vec<Candidate>> {
    let mut budget = DiscoveryBudget::new(options);
    let mut candidates = Vec::new();
    let mut stack = vec![relative.to_string()];
    while let Some(current) = stack.pop() {
        let (child_dirs, files) = object_shallow_children(store, &current).await?;
        for (name, meta) in files {
            budget.observe_entry()?;
            if !is_json_candidate(Path::new(&name)) {
                continue;
            }
            budget.observe_source()?;
            let file = if current.is_empty() {
                name
            } else {
                format!("{}/{name}", current.trim_end_matches('/'))
            };
            candidates.push(Candidate::RemoteFile {
                file: root_source_path(&file),
                store: store.clone(),
                meta,
            });
        }
        for child in child_dirs {
            budget.observe_entry()?;
            let child_relative = if current.is_empty() {
                child.clone()
            } else {
                format!("{}/{}", current.trim_end_matches('/'), child)
            };
            match probe_object_prefix(
                store,
                root_uri,
                &child_relative,
                root_source_path(&child_relative),
            )
            .await?
            {
                Some(ObjectProbe::Source(candidate)) => {
                    budget.observe_source()?;
                    candidates.push(candidate);
                }
                Some(ObjectProbe::Branch) => stack.push(child_relative),
                None if child.ends_with(".lance") => {}
                None => stack.push(child_relative),
            }
        }
    }
    candidates.sort_by(|left, right| left.source_stub().file.cmp(&right.source_stub().file));
    Ok(candidates)
}

async fn probe_object_prefix(
    store: &OpendalStore,
    root_uri: &str,
    relative: &str,
    source_file: impl Into<String>,
) -> Result<Option<ObjectProbe>> {
    let source_file = source_file.into();
    let prefix = if relative.is_empty() {
        String::new()
    } else {
        format!("{}/", relative.trim_end_matches('/'))
    };
    let join = |name: &str| {
        if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{prefix}{name}")
        }
    };

    if let Some(entry) = store
        .stat_file(&join(crate::store::CHRONICLE_MANIFEST_FILE))
        .await?
    {
        let bytes = store
            .read(&entry.path)
            .await?
            .map(|(bytes, _)| bytes)
            .unwrap_or_default();
        let text = std::str::from_utf8(&bytes).context("chronicle.manifest must be UTF-8")?;
        let manifest: crate::store::ChronicleManifest =
            toml::from_str(text).context("parse chronicle.manifest")?;
        manifest.validate()?;
        match manifest.kind {
            ManifestKind::Leaf => {
                let meta = RemoteObjectMeta::from(entry);
                if manifest.is_compact_jsonl_leaf() {
                    return Ok(Some(ObjectProbe::Source(Candidate::Compact {
                        file: source_file,
                        uri: child_uri(root_uri, relative),
                        size_bytes: Some(meta.size),
                        last_modified: Some(meta.last_modified),
                    })));
                }
                if manifest.is_storyline_leaf() {
                    let current = store.stat_file(&join("CURRENT")).await?.ok_or_else(|| {
                        anyhow::anyhow!(
                            "storyline chronicle.manifest requires CURRENT under {relative}"
                        )
                    })?;
                    let current_meta = RemoteObjectMeta::from(current);
                    return Ok(Some(ObjectProbe::Source(Candidate::Storyline {
                        file: source_file,
                        uri: child_uri(root_uri, relative),
                        size_bytes: Some(current_meta.size),
                        last_modified: Some(current_meta.last_modified),
                    })));
                }
                anyhow::bail!(
                    "chronicle.manifest leaf format {:?} is not supported for discovery yet",
                    manifest.format
                );
            }
            ManifestKind::Branch => return Ok(Some(ObjectProbe::Branch)),
        }
    }

    if let Some(entry) = store.stat_file(&join("CURRENT")).await? {
        let meta = RemoteObjectMeta::from(entry);
        return Ok(Some(ObjectProbe::Source(Candidate::Storyline {
            file: source_file,
            uri: child_uri(root_uri, relative),
            size_bytes: Some(meta.size),
            last_modified: Some(meta.last_modified),
        })));
    }

    Ok(None)
}
