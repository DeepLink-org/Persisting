// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright The Lance Authors
//
// 实现改编自 Apache-2.0 许可的 `lance-tools/src/reorder.rs`（仅保留 remap 模式、无二次排序 /
// 无 full-rebuild / 无 FSST / 无临时 pivot）。Persisting 仅需该流水线以服务 `SearchIndexReorderRequest`。

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use futures::{stream, StreamExt, TryStreamExt};
use lance::dataset::fragment::write::FragmentCreateBuilder;
use lance::dataset::index::DatasetIndexRemapperOptions;
use lance::dataset::optimize::{IndexRemapperOptions, RemappedIndex};
use lance::dataset::transaction::{Operation, RewriteGroup, RewrittenIndex, Transaction};
use lance::dataset::write::{CommitBuilder, WriteDestination, WriteParams};
use lance::dataset::ROW_ID;
use lance::deps::arrow_array::cast::AsArray;
use lance::deps::arrow_array::types::UInt64Type;
use lance::deps::arrow_array::RecordBatch;
use lance::deps::arrow_schema::Schema as ArrowSchema;
use lance::deps::datafusion::error::DataFusionError;
use lance::deps::datafusion::physical_plan::stream::RecordBatchStreamAdapter;
use lance::deps::datafusion::physical_plan::SendableRecordBatchStream;
use lance::index::DatasetIndexExt;
use lance::Dataset;
use lance_core::datatypes::Schema as LanceSchema;
use lance_core::utils::address::RowAddress;
use lance_core::{Error, Result};
use lance_index::frag_reuse::FRAG_REUSE_INDEX_NAME;
use lance_table::format::Fragment;
use uuid::Uuid;

/// Persisting 侧 IVF 物理重排参数（对齐历史上传给 `lance-tools::reorder::ReorderArgs` 的字段）。
pub(crate) struct IvfPhysicalReorderParams {
    pub source: String,
    pub target: Option<String>,
    pub in_place: bool,
    pub pivot_index: String,
    pub batch_size: usize,
}

pub(crate) async fn run_physical_reorder(
    args: &IvfPhysicalReorderParams,
    mut writer: impl Write,
) -> Result<()> {
    validate_destination(args)?;

    let source = Dataset::open(&args.source).await?;
    writeln!(
        &mut writer,
        "Opened source {} (version {}, {} rows)",
        args.source,
        source.version().version,
        source.count_rows(None).await?,
    )
    .map_err(|e| Error::io(e.to_string()))?;

    if source.manifest().uses_stable_row_ids() {
        return Err(Error::not_supported(
            "IVF physical reorder does not support datasets with stable row ids",
        ));
    }

    let pivot_column = resolve_pivot_column(&source, &args.pivot_index).await?;

    let (mut working, working_uri) = match (&args.target, args.in_place) {
        (Some(target_uri), false) => {
            writeln!(
                &mut writer,
                "Deep-cloning source to target {} (files + indices copied)",
                target_uri
            )
            .map_err(|e| Error::io(e.to_string()))?;
            let mut src_mut = source.clone();
            let version = src_mut.version().version;
            let cloned = src_mut.deep_clone(target_uri, version, None).await?;
            (cloned, target_uri.clone())
        }
        (None, true) => (source, args.source.clone()),
        _ => unreachable!("validate_destination ensures exactly one of target/in_place"),
    };

    writeln!(
        &mut writer,
        "Pivot index '{}' on column '{}'. Working dataset: {}",
        args.pivot_index, pivot_column, working_uri
    )
    .map_err(|e| Error::io(e.to_string()))?;

    let (permutation, partition_boundaries) =
        collect_permutation_natural(&working, &args.pivot_index, &mut writer).await?;
    writeln!(
        &mut writer,
        "Collected permutation of {} row_ids across {} IVF partitions",
        permutation.len(),
        partition_boundaries.len(),
    )
    .map_err(|e| Error::io(e.to_string()))?;

    let old_fragments: Vec<Fragment> = working
        .get_fragments()
        .into_iter()
        .map(|f| f.metadata().clone())
        .collect();
    let old_frag_ids: Vec<u64> = old_fragments.iter().map(|f| f.id).collect();

    let mut new_fragments = write_reordered_fragments_simple(
        &working,
        &working_uri,
        &permutation,
        args.batch_size,
        &mut writer,
    )
    .await?;

    reserve_fragment_ids(&mut working, &mut new_fragments, &mut writer).await?;

    let row_id_map = build_row_id_map(&permutation, &new_fragments);
    writeln!(
        &mut writer,
        "Built row_id_map with {} entries ({} new fragments)",
        row_id_map.len(),
        new_fragments.len(),
    )
    .map_err(|e| Error::io(e.to_string()))?;

    let rewritten_indices =
        remap_indices(&working, &row_id_map, &old_frag_ids, &[], &mut writer).await?;

    let read_version = working.manifest().version;
    let txn = Transaction::new(
        read_version,
        Operation::Rewrite {
            groups: vec![RewriteGroup {
                old_fragments,
                new_fragments,
            }],
            rewritten_indices,
            frag_reuse_index: None,
        },
        None,
    );
    let new_ds = CommitBuilder::new(WriteDestination::Dataset(Arc::new(working)))
        .execute(txn)
        .await?;
    working = new_ds;

    writeln!(
        &mut writer,
        "Committed Rewrite transaction. New version: {}",
        working.manifest().version,
    )
    .map_err(|e| Error::io(e.to_string()))?;

    writeln!(
        &mut writer,
        "Reorder complete. Dataset at {} (version {})",
        working_uri,
        working.manifest().version,
    )
    .map_err(|e| Error::io(e.to_string()))?;

    Ok(())
}

fn validate_destination(args: &IvfPhysicalReorderParams) -> Result<()> {
    match (&args.target, args.in_place) {
        (Some(uri), false) => {
            if uri == &args.source {
                return Err(Error::invalid_input(
                    "target equals source; use in_place to overwrite the source".to_string(),
                ));
            }
            Ok(())
        }
        (None, true) => Ok(()),
        (None, false) => Err(Error::invalid_input(
            "must specify either target URI or in_place".to_string(),
        )),
        (Some(_), true) => Err(Error::invalid_input(
            "target and in_place are mutually exclusive".to_string(),
        )),
    }
}

async fn resolve_pivot_column(source: &Dataset, pivot_name: &str) -> Result<String> {
    let indices = source.load_indices_by_name(pivot_name).await?;
    if indices.is_empty() {
        return Err(Error::invalid_input(format!(
            "pivot index '{pivot_name}' not found on dataset",
        )));
    }
    let field_id = *indices[0].fields.first().ok_or_else(|| {
        Error::invalid_input(format!(
            "pivot index '{pivot_name}' has no fields; cannot determine pivot column",
        ))
    })?;
    let field = source.schema().field_by_id(field_id).ok_or_else(|| {
        Error::invalid_input(format!(
            "pivot index '{pivot_name}' references unknown field id {field_id}",
        ))
    })?;
    Ok(field.name.clone())
}

async fn partition_count(source: &Dataset, pivot_name: &str) -> Result<usize> {
    let stats_json = source.index_statistics(pivot_name).await?;
    let stats: serde_json::Value = serde_json::from_str(&stats_json).map_err(|e| {
        Error::index(format!(
            "failed to parse index statistics for '{pivot_name}': {e}",
        ))
    })?;

    if let Some(n) = stats
        .get("num_partitions")
        .and_then(serde_json::Value::as_u64)
    {
        return Ok(n as usize);
    }
    for group_key in ["indices", "segments"] {
        if let Some(arr) = stats.get(group_key).and_then(serde_json::Value::as_array) {
            let mut total = 0usize;
            for entry in arr {
                if let Some(n) = entry
                    .get("num_partitions")
                    .and_then(serde_json::Value::as_u64)
                {
                    total = total.saturating_add(n as usize);
                }
            }
            if total > 0 {
                return Ok(total);
            }
        }
    }
    Err(Error::index(format!(
        "index '{pivot_name}' does not expose num_partitions; statistics: {stats}",
    )))
}

/// `read_index_partition(..., with_vector=false)` 的自然分区扫描顺序（无 PQ/SimHash 二次排序）。
async fn collect_permutation_natural(
    source: &Dataset,
    pivot_name: &str,
    mut writer: impl Write,
) -> Result<(Vec<u64>, Vec<usize>)> {
    let num_partitions = partition_count(source, pivot_name).await?;
    let approx_rows = source.count_rows(None).await?;
    let mut permutation = Vec::with_capacity(approx_rows);
    let mut partition_boundaries = Vec::with_capacity(num_partitions);

    for part_id in 0..num_partitions {
        let mut stream = source
            .read_index_partition(pivot_name, part_id, false)
            .await?;

        while let Some(batch) = stream.try_next().await? {
            let row_ids = batch.column_by_name(ROW_ID).ok_or_else(|| {
                Error::index(format!(
                    "partition {part_id} stream is missing row_id column",
                ))
            })?;
            let row_ids = row_ids.as_primitive_opt::<UInt64Type>().ok_or_else(|| {
                Error::index(format!("partition {part_id} row_id column is not UInt64",))
            })?;
            permutation.extend_from_slice(row_ids.values());
        }
        partition_boundaries.push(permutation.len());

        if part_id.is_multiple_of(64) || part_id + 1 == num_partitions {
            let _ = writeln!(
                &mut writer,
                "  read partition {}/{} (cumulative rows: {})",
                part_id + 1,
                num_partitions,
                permutation.len()
            );
        }
    }

    Ok((permutation, partition_boundaries))
}

async fn write_reordered_fragments_simple(
    source: &Dataset,
    target_uri: &str,
    permutation: &[u64],
    batch_size: usize,
    mut writer: impl Write,
) -> Result<Vec<Fragment>> {
    let full_lance_schema = source.schema().clone();
    let arrow_schema: Arc<ArrowSchema> = Arc::new((&full_lance_schema).into());
    let arrow_schema_for_lance = arrow_schema.clone();

    let total = permutation.len();
    let source_owned = source.clone();
    let perm = Arc::new(permutation.to_vec());

    let progress = Arc::new(AtomicUsize::new(0));
    let progress_cb = progress.clone();

    let chunks: Vec<(usize, usize)> = (0..total)
        .step_by(batch_size)
        .map(|start| {
            let end = (start + batch_size).min(total);
            (start, end)
        })
        .collect();

    let schema_for_adapter = arrow_schema.clone();
    let batch_stream = stream::iter(chunks).then(move |(start, end)| {
        let source = source_owned.clone();
        let perm = perm.clone();
        let lance_schema = full_lance_schema.clone();
        let plain_schema = arrow_schema.clone();
        async move {
            let batch = source
                .take_rows(&perm[start..end], lance_schema)
                .await
                .map_err(|e| DataFusionError::External(Box::new(e)))?;
            batch
                .with_schema(plain_schema)
                .map_err(|e| DataFusionError::External(Box::new(e)))
        }
    });
    let batch_stream = batch_stream.inspect(move |res: &std::result::Result<RecordBatch, _>| {
        if let Ok(batch) = res {
            let _ = progress_cb.fetch_add(batch.num_rows(), Ordering::Relaxed);
        }
    });

    let adapter = RecordBatchStreamAdapter::new(schema_for_adapter.clone(), batch_stream);
    let stream: SendableRecordBatchStream = Box::pin(adapter);

    writeln!(&mut writer, "Streaming {total} rows to {target_uri}",)
        .map_err(|e| Error::io(e.to_string()))?;

    let source_schema = LanceSchema::try_from(arrow_schema_for_lance.as_ref())?;
    let write_params = WriteParams::default();
    let fragments = FragmentCreateBuilder::new(target_uri)
        .schema(&source_schema)
        .write_params(&write_params)
        .write_fragments(stream)
        .await?;

    let copied = progress.load(Ordering::Relaxed);
    if copied != total {
        return Err(Error::internal(format!(
            "expected to stream {total} rows but write observed {copied}",
        )));
    }
    writeln!(
        &mut writer,
        "  wrote {} fragments ({copied} rows) to {target_uri}",
        fragments.len(),
    )
    .map_err(|e| Error::io(e.to_string()))?;
    Ok(fragments)
}

async fn reserve_fragment_ids(
    working: &mut Dataset,
    fragments: &mut [Fragment],
    mut writer: impl Write,
) -> Result<()> {
    let num = fragments.len() as u32;
    if num == 0 {
        return Ok(());
    }
    let txn = Transaction::new(
        working.manifest().version,
        Operation::ReserveFragments { num_fragments: num },
        None,
    );
    let new_ds = CommitBuilder::new(WriteDestination::Dataset(Arc::new(working.clone())))
        .execute(txn)
        .await?;

    let max_inclusive = new_ds.manifest().max_fragment_id.unwrap_or(0);
    let new_max_exclusive = max_inclusive + 1;
    let reserved_start = new_max_exclusive - num;
    for (fragment, new_id) in fragments.iter_mut().zip(reserved_start..new_max_exclusive) {
        fragment.id = new_id as u64;
    }
    *working = new_ds;
    writeln!(
        &mut writer,
        "Reserved fragment ids [{reserved_start}, {new_max_exclusive}) ({num} fragments)",
    )
    .map_err(|e| Error::io(e.to_string()))?;
    Ok(())
}

fn build_row_id_map(permutation: &[u64], new_fragments: &[Fragment]) -> HashMap<u64, Option<u64>> {
    let mut mapping = HashMap::with_capacity(permutation.len());
    let mut cursor = 0usize;
    for frag in new_fragments {
        let frag_id = frag.id as u32;
        let rows = frag.physical_rows.unwrap_or(0);
        for offset in 0..rows {
            let old_addr = permutation[cursor];
            let new_addr = u64::from(RowAddress::new_from_parts(frag_id, offset as u32));
            mapping.insert(old_addr, Some(new_addr));
            cursor += 1;
        }
    }
    mapping
}

async fn remap_indices(
    working: &Dataset,
    row_id_map: &HashMap<u64, Option<u64>>,
    old_frag_ids: &[u64],
    skip_names: &[String],
    mut writer: impl Write,
) -> Result<Vec<RewrittenIndex>> {
    let skip: HashSet<&str> = skip_names.iter().map(String::as_str).collect();
    let all_indices = working.load_indices().await?;

    let skipped_uuids: HashSet<Uuid> = all_indices
        .iter()
        .filter(|i| skip.contains(i.name.as_str()) || i.name == FRAG_REUSE_INDEX_NAME)
        .map(|i| i.uuid)
        .collect();

    if !skipped_uuids.is_empty() {
        let skipped_names: Vec<String> = all_indices
            .iter()
            .filter(|i| skipped_uuids.contains(&i.uuid))
            .map(|i| i.name.clone())
            .collect();
        writeln!(
            &mut writer,
            "Skipping remap for {} index(es): {:?}",
            skipped_names.len(),
            skipped_names,
        )
        .map_err(|e| Error::io(e.to_string()))?;
    }

    let remapper_opts = DatasetIndexRemapperOptions::default();
    let remapper = remapper_opts.create_remapper(working)?;
    let remapped: Vec<RemappedIndex> = remapper
        .remap_indices(row_id_map.clone(), old_frag_ids)
        .await?;

    let rewritten: Vec<RewrittenIndex> = remapped
        .into_iter()
        .filter(|r| !skipped_uuids.contains(&r.old_id))
        .map(|r| RewrittenIndex {
            old_id: r.old_id,
            new_id: r.new_id,
            new_index_details: r.index_details,
            new_index_version: r.index_version,
            new_index_files: r.files,
        })
        .collect();

    writeln!(
        &mut writer,
        "Remapped {} index(es) onto the new row-id space",
        rewritten.len(),
    )
    .map_err(|e| Error::io(e.to_string()))?;
    Ok(rewritten)
}
