use super::*;

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
    LocalFile {
        file: String,
        root: PathBuf,
        path: PathBuf,
        size_bytes: u64,
        last_modified: Option<String>,
    },
    RemoteFile {
        file: String,
        store: Arc<LanceObjectStore>,
        meta: ObjectMeta,
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
                Some(DocumentFormat::Storyline.as_str().to_string()),
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
                Some(meta.last_modified.to_rfc3339()),
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
            ensure_format_hint(mount, DocumentFormat::Storyline, &file)?;
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
        Candidate::LocalFile {
            file, root, path, ..
        } => {
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
        discover_local_candidates(&mount.uri, &path, options)
    } else {
        discover_object_candidates(&mount.uri, options).await
    }
}

fn discover_local_candidates(
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

    let mut candidates = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    let mut visited = 0usize;
    while let Some(directory) = pending.pop() {
        let mut entries = fs::read_dir(&directory)
            .with_context(|| format!("read Dataset directory {}", directory.display()))?
            .collect::<std::io::Result<Vec<_>>>()?;
        entries.sort_by_key(|entry| entry.path());
        for entry in entries {
            visited = visited.saturating_add(1);
            anyhow::ensure!(
                visited <= options.max_entries,
                "Dataset traversal exceeds max_entries limit of {}",
                options.max_entries
            );
            let file_type = entry.file_type()?;
            if file_type.is_symlink() {
                continue;
            }
            let path = entry.path();
            if file_type.is_dir() {
                if path.join("CURRENT").is_file() {
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
                } else if is_lance_directory(&path) {
                    // Derived Lance datasets are sidecars of a canonical Run,
                    // not trajectory sources. Never descend into their internal
                    // metadata and register it as an outer file source.
                } else {
                    pending.push(path);
                }
            } else if file_type.is_file() && is_json_candidate(&path) {
                let metadata = entry.metadata()?;
                candidates.push(Candidate::LocalFile {
                    file: relative_catalog_path(root, &path, false)?,
                    root: root.to_path_buf(),
                    path,
                    size_bytes: metadata.len(),
                    last_modified: modified_string(&metadata),
                });
            }
            anyhow::ensure!(
                candidates.len() <= options.max_files,
                "Dataset manifest exceeds max_files limit of {}",
                options.max_files
            );
        }
    }
    candidates.sort_by(|left, right| left.source_stub().file.cmp(&right.source_stub().file));
    Ok(candidates)
}

async fn discover_object_candidates(
    uri: &str,
    options: LocalQueryManifestOptions,
) -> Result<Vec<Candidate>> {
    let (store, root) = LanceObjectStore::from_uri(uri)
        .await
        .with_context(|| format!("open Dataset object store {uri}"))?;
    let store = Arc::clone(&store);
    let mut listing = store.inner.list(Some(&root));
    let mut metas = Vec::new();
    while let Some(meta) = listing
        .try_next()
        .await
        .with_context(|| format!("list Dataset object prefix {uri}"))?
    {
        anyhow::ensure!(
            metas.len() < options.max_entries,
            "Dataset traversal exceeds max_entries limit of {}",
            options.max_entries
        );
        metas.push(meta);
    }
    metas.sort_by(|left, right| left.location.cmp(&right.location));

    let root_is_events = root.as_ref().ends_with("events.lance");
    let mut storyline_roots = BTreeMap::<String, ObjectMeta>::new();
    let mut event_roots = BTreeMap::<String, ObjectMeta>::new();
    let mut relative_metas = Vec::with_capacity(metas.len());
    for meta in metas {
        let relative = relative_object_path(&root, &meta.location)?;
        if relative == "CURRENT" || relative.ends_with("/CURRENT") {
            storyline_roots.insert(parent_relative_path(&relative, "CURRENT"), meta.clone());
        }
        if (relative == "_manifest.json" && root_is_events)
            || relative.ends_with("/events.lance/_manifest.json")
        {
            event_roots.insert(
                parent_relative_path(&relative, "_manifest.json"),
                meta.clone(),
            );
        }
        relative_metas.push((relative, meta));
    }

    let mut candidates = Vec::new();
    for (relative, meta) in &storyline_roots {
        candidates.push(Candidate::Storyline {
            file: root_source_path(relative),
            uri: child_uri(uri, relative),
            size_bytes: Some(meta.size),
            last_modified: Some(meta.last_modified.to_rfc3339()),
        });
    }
    for (relative, meta) in &event_roots {
        if is_nested_in_any(relative, storyline_roots.keys()) {
            continue;
        }
        candidates.push(Candidate::Events {
            file: root_source_path(relative),
            uri: child_uri(uri, relative),
            size_bytes: Some(meta.size),
            last_modified: Some(meta.last_modified.to_rfc3339()),
        });
    }

    let composite_roots = storyline_roots
        .keys()
        .chain(event_roots.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    for (relative, meta) in relative_metas {
        if is_nested_in_any(&relative, composite_roots.iter())
            || path_is_inside_lance_directory(&relative)
        {
            continue;
        }
        let candidate_path = if relative.is_empty() {
            Path::new(root.as_ref())
        } else {
            Path::new(&relative)
        };
        if is_json_candidate(candidate_path) {
            let file = if relative.is_empty() {
                root.as_ref()
                    .rsplit('/')
                    .next()
                    .unwrap_or("dataset.json")
                    .to_string()
            } else {
                relative
            };
            candidates.push(Candidate::RemoteFile {
                file,
                store: Arc::clone(&store),
                meta,
            });
        }
    }
    anyhow::ensure!(
        candidates.len() <= options.max_files,
        "Dataset manifest exceeds max_files limit of {}",
        options.max_files
    );
    candidates.sort_by(|left, right| left.source_stub().file.cmp(&right.source_stub().file));
    Ok(candidates)
}
