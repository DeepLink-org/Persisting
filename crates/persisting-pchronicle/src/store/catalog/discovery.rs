use super::*;
use crate::store::chronicle_manifest::{ManifestKind, try_load_manifest};
use crate::store::opendal_store::Store as OpendalStore;

#[derive(Debug)]
pub(super) enum Candidate {
    Storyline {
        file: String,
        uri: String,
        size_bytes: Option<u64>,
        last_modified: Option<String>,
    },
    Events {
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
            Self::Events {
                file,
                size_bytes,
                last_modified,
                ..
            } => (
                file.clone(),
                Some(DocumentFormat::CanonicalEvent.as_str().to_string()),
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
            projection_status: None,
            projection_generation: None,
            projection_candidates: 0,
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
        Candidate::Events { file, uri, .. } => {
            ensure_format_hint(mount, DocumentFormat::CanonicalEvent, &file)?;
            let snapshot = RawEventDataSource::pin_uri(&uri)
                .await
                .with_context(|| format!("pin canonical event source {uri}"))?;
            let fact = snapshot.fact_snapshot();
            source_row.revision = Some(CatalogSourceRevision::Events {
                fact_version: fact.fact_version,
                fact_rows: fact.fact_rows,
                layout_revision: fact.layout_revision,
            });
            Ok((
                source_row,
                Arc::new(LazySource::new(
                    file,
                    LazySourceSpec::Events {
                        uri,
                        snapshot,
                        projection: None,
                    },
                    options,
                    temporary_files,
                )),
            ))
        }
        Candidate::Compact { file, uri, .. } => {
            if let Some(manifest) =
                crate::store::chronicle_manifest::try_load_manifest(Path::new(&uri))
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

/// Collapse a canonical events source and its derived Storyline sidecar into
/// one Catalog identity. Fresh projections serve normalized tables; stale
/// projections remain hidden and the canonical events adapter is used instead.
pub(super) fn bind_canonical_storyline_projections(
    source_rows: &mut Vec<DiscoveredSource>,
    prepared_sources: &mut Vec<Arc<LazySource>>,
) -> Result<()> {
    struct Binding {
        projection_file: String,
        event_file: String,
        paths: StorylineTablePaths,
        fresh: bool,
        last_modified: Option<String>,
    }

    let mut bindings = BTreeMap::<String, Vec<Binding>>::new();
    for projection in prepared_sources.iter() {
        let LazySourceSpec::Storyline { paths } = &projection.spec else {
            continue;
        };
        let Some(lineage) = paths.projection.as_ref() else {
            continue;
        };
        let ProjectionSourceSnapshot::CanonicalEvents { source_uri, .. } = &lineage.source else {
            continue;
        };
        let Some(events) = prepared_sources.iter().find(|candidate| {
            matches!(
                &candidate.spec,
                LazySourceSpec::Events { uri, .. } if uri == source_uri
            )
        }) else {
            continue;
        };
        let LazySourceSpec::Events { snapshot, .. } = &events.spec else {
            anyhow::bail!(
                "catalog source '{}' matched canonical event URI but is not an events source",
                events.file
            )
        };
        let last_modified = source_rows
            .iter()
            .find(|source| source.file == projection.file)
            .and_then(|source| source.last_modified.clone());
        bindings
            .entry(events.file.clone())
            .or_default()
            .push(Binding {
                projection_file: projection.file.clone(),
                event_file: events.file.clone(),
                paths: paths.clone(),
                fresh: projection_lineage_is_fresh(&snapshot.fact_snapshot(), lineage),
                last_modified,
            });
    }

    let mut projection_files = HashSet::new();
    for candidates in bindings.values_mut() {
        candidates.sort_by(|left, right| {
            (
                left.fresh,
                left.last_modified.as_deref().unwrap_or(""),
                left.paths.generation.as_str(),
                left.projection_file.as_str(),
            )
                .cmp(&(
                    right.fresh,
                    right.last_modified.as_deref().unwrap_or(""),
                    right.paths.generation.as_str(),
                    right.projection_file.as_str(),
                ))
        });
        let binding = candidates
            .last()
            .context("projection binding group is empty")?;
        projection_files.extend(
            candidates
                .iter()
                .map(|candidate| candidate.projection_file.clone()),
        );
        let event_index = prepared_sources
            .iter()
            .position(|source| source.file == binding.event_file)
            .context("bound canonical event source disappeared")?;
        let event = &prepared_sources[event_index];
        let LazySourceSpec::Events { uri, snapshot, .. } = &event.spec else {
            anyhow::bail!("bound Catalog source is not canonical events");
        };
        prepared_sources[event_index] = Arc::new(LazySource::new(
            event.file.clone(),
            LazySourceSpec::Events {
                uri: uri.clone(),
                snapshot: snapshot.clone(),
                projection: binding.fresh.then(|| binding.paths.clone()),
            },
            event.options,
            event.temporary_files.clone(),
        ));
        let event_row = source_rows
            .iter_mut()
            .find(|source| source.file == binding.event_file)
            .context("bound canonical event source row disappeared")?;
        event_row.projection_status = Some(if binding.fresh {
            CatalogProjectionStatus::Fresh
        } else {
            CatalogProjectionStatus::Stale
        });
        event_row.projection_generation = Some(binding.paths.generation.clone());
        event_row.projection_candidates = candidates.len() as u64;
    }

    source_rows.retain(|source| !projection_files.contains(&source.file));
    prepared_sources.retain(|source| !projection_files.contains(&source.file));
    Ok(())
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

    fn token_strategy() -> impl Strategy<Value = String> {
        proptest::string::string_regex("[A-Za-z0-9._/-]{1,32}").unwrap()
    }

    proptest! {
        #[test]
        fn storyline_and_event_candidates_expose_stable_source_stubs(
            file in token_strategy(),
            uri in token_strategy(),
            size in prop::option::of(0u64..1_000_000),
            modified in prop::option::of(token_strategy()),
            is_events in any::<bool>(),
        ) {
            let candidate = if is_events {
                Candidate::Events { file: file.clone(), uri, size_bytes: size, last_modified: modified.clone() }
            } else {
                Candidate::Storyline { file: file.clone(), uri, size_bytes: size, last_modified: modified.clone() }
            };
            let source = candidate.source_stub();
            prop_assert_eq!(source.file, file);
            prop_assert_eq!(source.size_bytes, size);
            prop_assert_eq!(source.last_modified, modified);
            prop_assert_eq!(source.kind, CatalogSourceKind::Store);
            prop_assert_eq!(source.status, CatalogSourceStatus::Ready);
            prop_assert_eq!(source.format.as_deref(), Some(if is_events { DocumentFormat::CanonicalEvent.as_str() } else { DocumentFormat::StorylineLance.as_str() }));
        }

        #[test]
        fn source_format_hints_must_match_the_candidate_format(
            name in proptest::string::string_regex("[A-Za-z_][A-Za-z0-9_]{0,19}").unwrap(),
        ) {
            let mount = DatasetMount::new(name, "memory://catalog/source").unwrap();
            prop_assert!(ensure_format_hint(&mount, DocumentFormat::CanonicalEvent, "events.lance").is_ok());
            let hinted = mount.with_format_hint(DocumentFormat::StorylineLance);
            prop_assert!(ensure_format_hint(&hinted, DocumentFormat::StorylineLance, "storyline").is_ok());
            prop_assert!(ensure_format_hint(&hinted, DocumentFormat::CanonicalEvent, "events").is_err());
        }
    }
}

pub(super) async fn normalize_event_storylines(
    source: &RawEventDataSource,
    session_ids: Option<&BTreeSet<String>>,
    kind: CatalogTableKind,
    max_rows: usize,
    max_bytes: usize,
) -> Result<Arc<MemTable>> {
    let records = match session_ids {
        Some(session_ids) => {
            source
                .read_records_for_storylines_bounded(session_ids, max_rows, max_bytes)
                .await?
        }
        None => source.read_records_bounded(max_rows, max_bytes).await?,
    };
    normalize_event_records(records, kind, max_bytes)
}

fn normalize_event_records(
    records: Vec<EventRecord>,
    kind: CatalogTableKind,
    max_bytes: usize,
) -> Result<Arc<MemTable>> {
    let mut groups = BTreeMap::<String, Vec<EventRecord>>::new();
    for record in records {
        let key = event_storyline_key(&record)
            .context("canonical event cannot be projected without a Storyline identity")?;
        groups.entry(key.to_string()).or_default().push(record);
    }

    let stories = groups.into_iter().map(|(group_key, records)| {
        let story = project_event_records(&records)?;
        anyhow::ensure!(
            story.session_id == group_key,
            "projected Storyline identity changed"
        );
        Ok(story)
    });

    let (schema, batch) = match kind {
        CatalogTableKind::Runs => {
            let mut rows = Vec::<StoryRunRow>::new();
            for story in stories {
                rows.push(split_storyline(&story?)?.run);
            }
            (story_runs_arrow_schema(), story_runs_to_batch(&rows)?)
        }
        CatalogTableKind::Steps => {
            let mut rows = Vec::<StoryStepRow>::new();
            for story in stories {
                rows.extend(split_storyline(&story?)?.steps);
            }
            (story_steps_arrow_schema(), story_steps_to_batch(&rows)?)
        }
        CatalogTableKind::ToolCalls => {
            let mut rows = Vec::<StoryToolCallRow>::new();
            for story in stories {
                rows.extend(split_storyline(&story?)?.tool_calls);
            }
            (
                story_tool_calls_arrow_schema(),
                story_tool_calls_to_batch(&rows)?,
            )
        }
        CatalogTableKind::Events => {
            anyhow::bail!("canonical events do not require Storyline normalization")
        }
    };
    anyhow::ensure!(
        batch.get_array_memory_size() <= max_bytes,
        "normalized canonical event fallback exceeds max_event_fallback_bytes {max_bytes}; build or sync a Storyline projection"
    );
    Ok(Arc::new(MemTable::try_new(schema, vec![vec![batch]])?))
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

    if let Some(manifest) = try_load_manifest(root) {
        return collect_manifest_subtree(root, root, &manifest, options).await;
    }

    if root.join("CURRENT").is_file() {
        let metadata = fs::metadata(root.join("CURRENT"))?;
        return Ok(vec![Candidate::Storyline {
            file: ".".into(),
            uri: canonical_local_uri(root)?,
            size_bytes: Some(metadata.len()),
            last_modified: modified_string(&metadata),
        }]);
    }
    if root.join("_manifest.json").is_file()
        && root.file_name().is_some_and(|name| name == "events.lance")
    {
        let metadata = fs::metadata(root.join("_manifest.json"))?;
        return Ok(vec![Candidate::Events {
            file: ".".into(),
            uri: canonical_local_uri(root)?,
            size_bytes: Some(metadata.len()),
            last_modified: modified_string(&metadata),
        }]);
    }
    if is_lance_directory(root) && is_compact_jsonl_directory(root).await? {
        let metadata = fs::metadata(root)?;
        return Ok(vec![Candidate::Compact {
            file: ".".into(),
            uri: canonical_local_uri(root)?,
            size_bytes: Some(metadata.len()),
            last_modified: modified_string(&metadata),
        }]);
    }

    // Plain directory: only inspect immediate children for Dataset markers.
    // Loose files are not registered as sources (lazy Directory navigation).
    let mut candidates = Vec::new();
    let mut entries = fs::read_dir(root)
        .with_context(|| format!("read Dataset directory {}", root.display()))?
        .collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|entry| entry.path());
    for entry in entries {
        let file_type = entry.file_type()?;
        if file_type.is_symlink() || !file_type.is_dir() {
            continue;
        }
        anyhow::ensure!(
            candidates.len() < options.max_files,
            "Dataset manifest exceeds max_files limit of {}",
            options.max_files
        );
        let path = entry.path();
        if let Some(manifest) = try_load_manifest(&path) {
            let nested = collect_manifest_subtree(root, &path, &manifest, options).await?;
            candidates.extend(nested);
        } else if path.join("CURRENT").is_file() {
            let metadata = fs::metadata(path.join("CURRENT"))?;
            candidates.push(Candidate::Storyline {
                file: relative_catalog_path(root, &path, true)?,
                uri: canonical_local_uri(&path)?,
                size_bytes: Some(metadata.len()),
                last_modified: modified_string(&metadata),
            });
        } else if path.join("_manifest.json").is_file()
            && path.file_name().is_some_and(|name| name == "events.lance")
        {
            let metadata = fs::metadata(path.join("_manifest.json"))?;
            candidates.push(Candidate::Events {
                file: relative_catalog_path(root, &path, true)?,
                uri: canonical_local_uri(&path)?,
                size_bytes: Some(metadata.len()),
                last_modified: modified_string(&metadata),
            });
        } else if path.join("events.lance/_manifest.json").is_file() {
            let events = path.join("events.lance");
            let metadata = fs::metadata(events.join("_manifest.json"))?;
            candidates.push(Candidate::Events {
                file: relative_catalog_path(root, &events, true)?,
                uri: canonical_local_uri(&events)?,
                size_bytes: Some(metadata.len()),
                last_modified: modified_string(&metadata),
            });
        } else if is_lance_directory(&path) && is_compact_jsonl_directory(&path).await? {
            let metadata = fs::metadata(&path)?;
            candidates.push(Candidate::Compact {
                file: relative_catalog_path(root, &path, true)?,
                uri: canonical_local_uri(&path)?,
                size_bytes: Some(metadata.len()),
                last_modified: modified_string(&metadata),
            });
        }
    }
    candidates.sort_by(|left, right| left.source_stub().file.cmp(&right.source_stub().file));
    Ok(candidates)
}

async fn collect_manifest_subtree(
    mount_root: &Path,
    node: &Path,
    manifest: &crate::store::ChronicleManifest,
    options: LocalQueryManifestOptions,
) -> Result<Vec<Candidate>> {
    let mut candidates = Vec::new();
    let mut stack = vec![(node.to_path_buf(), manifest.clone())];
    while let Some((current, current_manifest)) = stack.pop() {
        match current_manifest.kind {
            ManifestKind::Leaf => {
                anyhow::ensure!(
                    current_manifest.is_compact_jsonl_leaf(),
                    "chronicle.manifest leaf format {:?} is not supported for discovery yet",
                    current_manifest.format
                );
                let metadata = fs::metadata(&current)?;
                let file = if current == mount_root {
                    ".".into()
                } else {
                    relative_catalog_path(mount_root, &current, true)?
                };
                candidates.push(Candidate::Compact {
                    file,
                    uri: canonical_local_uri(&current)?,
                    size_bytes: Some(metadata.len()),
                    last_modified: modified_string(&metadata),
                });
            }
            ManifestKind::Branch => {
                let mut entries = fs::read_dir(&current)
                    .with_context(|| {
                        format!("read chronicle.manifest branch {}", current.display())
                    })?
                    .collect::<std::io::Result<Vec<_>>>()?;
                entries.sort_by_key(|entry| entry.path());
                for entry in entries.into_iter().rev() {
                    anyhow::ensure!(
                        candidates.len() < options.max_files,
                        "Dataset manifest exceeds max_files limit of {}",
                        options.max_files
                    );
                    let file_type = entry.file_type()?;
                    if file_type.is_symlink() || !file_type.is_dir() {
                        continue;
                    }
                    let child = entry.path();
                    let Some(child_manifest) = try_load_manifest(&child) else {
                        continue;
                    };
                    stack.push((child, child_manifest));
                }
            }
        }
        anyhow::ensure!(
            candidates.len() <= options.max_files,
            "Dataset manifest exceeds max_files limit of {}",
            options.max_files
        );
    }
    candidates.sort_by(|left, right| left.source_stub().file.cmp(&right.source_stub().file));
    Ok(candidates)
}

async fn is_compact_jsonl_directory(path: &Path) -> Result<bool> {
    if let Some(manifest) = try_load_manifest(path) {
        return Ok(manifest.is_compact_jsonl_leaf());
    }
    let dataset = match lance::Dataset::open(path.to_string_lossy().as_ref()).await {
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

    // Prefer a Dataset root (chronicle.manifest / CURRENT / events) over a flat
    // recursive object walk. Plain prefixes navigate one directory level only.
    match probe_object_prefix(&store, uri, "", ".").await? {
        Some(ObjectProbe::Source(candidate)) => return Ok(vec![candidate]),
        Some(ObjectProbe::Branch) => {
            return collect_object_branch_children(&store, uri, "", options).await;
        }
        None => {}
    }

    let mut candidates = Vec::new();
    for child in object_child_names(&store, "").await? {
        anyhow::ensure!(
            candidates.len() < options.max_files,
            "Dataset manifest exceeds max_files limit of {}",
            options.max_files
        );
        match probe_object_prefix(&store, uri, &child, root_source_path(&child)).await? {
            Some(ObjectProbe::Source(candidate)) => candidates.push(candidate),
            Some(ObjectProbe::Branch) => {
                let nested = collect_object_branch_children(&store, uri, &child, options).await?;
                candidates.extend(nested);
            }
            None => {}
        }
    }

    candidates.sort_by(|left, right| left.source_stub().file.cmp(&right.source_stub().file));
    Ok(candidates)
}

enum ObjectProbe {
    Source(Candidate),
    Branch,
}

async fn object_child_names(store: &OpendalStore, relative: &str) -> Result<BTreeSet<String>> {
    let prefix = if relative.is_empty() {
        String::new()
    } else {
        format!("{}/", relative.trim_end_matches('/'))
    };
    let entries = store
        .list_shallow(&prefix)
        .await
        .with_context(|| format!("list object prefix '{prefix}'"))?;
    let mut child_names = BTreeSet::new();
    for entry in entries {
        let path = entry.path.trim_start_matches(&prefix).trim_matches('/');
        if path.is_empty() {
            continue;
        }
        let child = path.split('/').next().unwrap_or(path);
        if child.is_empty() {
            continue;
        }
        // Loose files at this level are ignored (Directory navigation only).
        if entry.mode == opendal::EntryMode::FILE && !path.contains('/') {
            continue;
        }
        child_names.insert(child.to_string());
    }
    Ok(child_names)
}

async fn collect_object_branch_children(
    store: &OpendalStore,
    root_uri: &str,
    relative: &str,
    options: LocalQueryManifestOptions,
) -> Result<Vec<Candidate>> {
    let mut candidates = Vec::new();
    let mut stack = vec![relative.to_string()];
    while let Some(current) = stack.pop() {
        for child in object_child_names(store, &current).await? {
            anyhow::ensure!(
                candidates.len() < options.max_files,
                "Dataset manifest exceeds max_files limit of {}",
                options.max_files
            );
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
                Some(ObjectProbe::Source(candidate)) => candidates.push(candidate),
                Some(ObjectProbe::Branch) => stack.push(child_relative),
                None => {}
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
                anyhow::ensure!(
                    manifest.is_compact_jsonl_leaf(),
                    "chronicle.manifest leaf format {:?} is not supported for discovery yet",
                    manifest.format
                );
                let meta = RemoteObjectMeta::from(entry);
                return Ok(Some(ObjectProbe::Source(Candidate::Compact {
                    file: source_file,
                    uri: child_uri(root_uri, relative),
                    size_bytes: Some(meta.size),
                    last_modified: Some(meta.last_modified),
                })));
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

    let events_manifest = if relative.is_empty() {
        "_manifest.json".to_string()
    } else if relative.trim_end_matches('/').ends_with("events.lance") {
        join("_manifest.json")
    } else {
        join("events.lance/_manifest.json")
    };
    if let Some(entry) = store.stat_file(&events_manifest).await? {
        let meta = RemoteObjectMeta::from(entry);
        let events_relative = if relative.is_empty() {
            if root_uri.trim_end_matches('/').ends_with("events.lance") {
                String::new()
            } else {
                "events.lance".to_string()
            }
        } else if relative.trim_end_matches('/').ends_with("events.lance") {
            relative.to_string()
        } else {
            format!("{}/events.lance", relative.trim_end_matches('/'))
        };
        return Ok(Some(ObjectProbe::Source(Candidate::Events {
            file: if events_relative.is_empty() {
                ".".into()
            } else {
                events_relative.clone()
            },
            uri: child_uri(root_uri, &events_relative),
            size_bytes: Some(meta.size),
            last_modified: Some(meta.last_modified),
        })));
    }

    Ok(None)
}
