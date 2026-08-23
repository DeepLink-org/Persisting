use dioxus::prelude::*;

use crate::model::{WarehouseSource, WarehouseSources};

pub const MAX_TREEMAP_TILES: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TileBox {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

pub fn layout_treemap(sizes: &[f64], width: f64, height: f64) -> Vec<TileBox> {
    if sizes.is_empty() || width <= 0.0 || height <= 0.0 {
        return Vec::new();
    }
    let total: f64 = sizes.iter().copied().sum();
    if total <= 0.0 {
        return sizes
            .iter()
            .map(|_| TileBox {
                x: 0.0,
                y: 0.0,
                w: 0.0,
                h: 0.0,
            })
            .collect();
    }
    split(sizes, 0.0, 0.0, width, height)
}

fn split(areas: &[f64], x: f64, y: f64, w: f64, h: f64) -> Vec<TileBox> {
    match areas {
        [] => Vec::new(),
        [_] => vec![TileBox { x, y, w, h }],
        _ => {
            let total: f64 = areas.iter().sum();
            let mut acc = 0.0;
            let mut cut = 1;
            for (index, area) in areas.iter().enumerate() {
                acc += area;
                cut = index + 1;
                if acc >= total / 2.0 {
                    break;
                }
            }
            cut = cut.clamp(1, areas.len() - 1);
            let left_sum: f64 = areas[..cut].iter().sum();
            let frac = left_sum / total;
            if w >= h {
                let left = w * frac;
                let mut tiles = split(&areas[..cut], x, y, left, h);
                tiles.extend(split(&areas[cut..], x + left, y, w - left, h));
                tiles
            } else {
                let top = h * frac;
                let mut tiles = split(&areas[..cut], x, y, w, top);
                tiles.extend(split(&areas[cut..], x, y + top, w, h - top));
                tiles
            }
        }
    }
}

pub fn selected_source<'a>(
    sources: &'a [WarehouseSource],
    selected: Option<&(String, String)>,
) -> Option<&'a WarehouseSource> {
    selected.and_then(|(dataset, file)| {
        sources
            .iter()
            .find(|source| source.dataset == *dataset && source.file == *file)
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CatalogNodeKind {
    Dir,
    Source,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CatalogNode {
    pub name: String,
    pub dataset: String,
    pub path: String,
    pub kind: CatalogNodeKind,
    pub run_count: usize,
    pub size_bytes: Option<u64>,
    pub error_count: usize,
    pub source: Option<WarehouseSource>,
}

pub fn catalog_children(
    sources: &[WarehouseSource],
    dataset: Option<&str>,
    prefix: &str,
) -> Vec<CatalogNode> {
    let prefix = prefix.trim().trim_matches('/');
    match dataset {
        None => group_datasets(sources),
        Some(dataset) => group_prefix(sources, dataset, prefix),
    }
}

struct NodeAcc {
    run_count: usize,
    size_bytes: Option<u64>,
    error_count: usize,
    has_deeper: bool,
    members: Vec<WarehouseSource>,
}

fn group_datasets(sources: &[WarehouseSource]) -> Vec<CatalogNode> {
    let mut groups = std::collections::BTreeMap::<String, NodeAcc>::new();
    for source in sources {
        let entry = groups
            .entry(source.dataset.clone())
            .or_insert_with(NodeAcc::default);
        add_source(entry, source, true);
    }
    finish_groups(groups, |name, acc| CatalogNode {
        dataset: name.clone(),
        path: String::new(),
        name,
        kind: CatalogNodeKind::Dir,
        run_count: acc.run_count,
        size_bytes: acc.size_bytes,
        error_count: acc.error_count,
        source: None,
    })
}

fn group_prefix(sources: &[WarehouseSource], dataset: &str, prefix: &str) -> Vec<CatalogNode> {
    let mut groups = std::collections::BTreeMap::<String, NodeAcc>::new();
    for source in sources {
        let Some(rest) = source_rest(source, dataset, prefix) else {
            continue;
        };
        if rest.is_empty() {
            continue;
        }
        let (name, has_deeper) = match rest.split_once('/') {
            Some((name, _)) => (name, true),
            None => (rest, false),
        };
        if name.is_empty() {
            continue;
        }
        let entry = groups
            .entry(name.to_string())
            .or_insert_with(NodeAcc::default);
        add_source(entry, source, has_deeper);
    }
    finish_groups(groups, |name, acc| {
        let path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let leaf = !acc.has_deeper && acc.members.len() == 1;
        CatalogNode {
            dataset: dataset.to_string(),
            path,
            name,
            kind: if leaf {
                CatalogNodeKind::Source
            } else {
                CatalogNodeKind::Dir
            },
            run_count: acc.run_count,
            size_bytes: acc.size_bytes,
            error_count: acc.error_count,
            source: leaf.then(|| acc.members[0].clone()),
        }
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderStats {
    pub run_count: usize,
    pub source_count: usize,
    pub error_sources: usize,
}

pub fn folder_stats(
    sources: &[WarehouseSource],
    dataset: Option<&str>,
    prefix: &str,
) -> FolderStats {
    let mut stats = FolderStats {
        run_count: 0,
        source_count: 0,
        error_sources: 0,
    };
    for source in sources {
        if !source_in_folder(source, dataset, prefix) {
            continue;
        }
        stats.run_count += source.run_count;
        stats.source_count += 1;
        if source.status != "ready" {
            stats.error_sources += 1;
        }
    }
    stats
}

fn source_in_folder(source: &WarehouseSource, dataset: Option<&str>, prefix: &str) -> bool {
    match dataset {
        None => true,
        Some(dataset) if source.dataset != dataset => false,
        Some(_) if prefix.is_empty() => true,
        Some(_) => source.file == prefix || source.file.starts_with(&format!("{prefix}/")),
    }
}

fn source_rest<'a>(source: &'a WarehouseSource, dataset: &str, prefix: &str) -> Option<&'a str> {
    if source.dataset != dataset {
        return None;
    }
    if prefix.is_empty() {
        return Some(source.file.as_str());
    }
    if source.file == prefix {
        return None;
    }
    source.file.strip_prefix(&format!("{prefix}/"))
}

fn add_source(entry: &mut NodeAcc, source: &WarehouseSource, has_deeper: bool) {
    entry.run_count += source.run_count;
    entry.size_bytes = match (entry.size_bytes, source.size_bytes) {
        (Some(left), Some(right)) => Some(left + right),
        (None, right) => right,
        (left, None) => left,
    };
    if source.status != "ready" {
        entry.error_count += 1;
    }
    entry.has_deeper |= has_deeper;
    entry.members.push(source.clone());
}

fn finish_groups(
    groups: std::collections::BTreeMap<String, NodeAcc>,
    build: impl Fn(String, NodeAcc) -> CatalogNode,
) -> Vec<CatalogNode> {
    let mut nodes = groups
        .into_iter()
        .map(|(name, acc)| build(name, acc))
        .collect::<Vec<_>>();
    nodes.sort_by(|left, right| {
        right
            .run_count
            .cmp(&left.run_count)
            .then(left.name.cmp(&right.name))
    });
    nodes
}

impl Default for NodeAcc {
    fn default() -> Self {
        Self {
            run_count: 0,
            size_bytes: None,
            error_count: 0,
            has_deeper: false,
            members: Vec::new(),
        }
    }
}

pub fn treemap_head<T>(items: &[T]) -> &[T] {
    let end = items.len().min(MAX_TREEMAP_TILES);
    &items[..end]
}

#[component]
pub fn CatalogExplorer(
    page: Option<WarehouseSources>,
    loading: bool,
    dataset: String,
    prefix: String,
    selected: Option<(String, String)>,
    on_open: EventHandler<(String, String)>,
    on_select: EventHandler<(String, String)>,
    on_open_traces: EventHandler<()>,
    on_analyze: EventHandler<()>,
    on_source_traces: EventHandler<(String, String)>,
    on_source_analyze: EventHandler<(String, String)>,
) -> Element {
    let sources = page
        .as_ref()
        .map(|page| page.sources.as_slice())
        .unwrap_or_default();
    let nodes = catalog_children(
        sources,
        (!dataset.is_empty()).then_some(dataset.as_str()),
        &prefix,
    );
    let active = selected_source(sources, selected.as_ref()).cloned();
    rsx! {
        section { class: "pc-catalog",
            header { class: "pc-catalog-head",
                div { class: "pc-catalog-title",
                    p { class: "eyebrow", "Data" }
                    h1 { "Dataset explorer" }
                    p { "Understand where trajectory evidence comes from and whether it is queryable." }
                }
                div { class: "pc-catalog-actions",
                    button { class: "pc-catalog-btn", onclick: move |_| on_open_traces.call(()), "Open traces" }
                    button { class: "pc-catalog-btn primary", onclick: move |_| on_analyze.call(()), "Analyze dataset" }
                }
            }
            CatalogStats {
                page: page.clone(),
                dataset: dataset.clone(),
                prefix: prefix.clone(),
            }
            CatalogTreemap {
                dataset: dataset.clone(),
                prefix: prefix.clone(),
                nodes: treemap_head(&nodes).to_vec(),
                selected: selected.clone(),
                loading,
                empty: page.as_ref().is_some_and(|page| page.sources.is_empty()),
                on_open,
                on_select,
            }
            if let Some(source) = active.clone() {
                SelectedSource {
                    source,
                    on_source_traces,
                    on_source_analyze,
                }
            }
            if !nodes.is_empty() {
                SourceInventory {
                    nodes,
                    selected,
                    on_open,
                    on_select,
                }
            }
        }
    }
}

fn format_source_summary(source_count: usize, error_sources: usize) -> String {
    if error_sources > 0 {
        format!("{source_count} discovered · {error_sources} error")
    } else {
        format!("{source_count} discovered")
    }
}

#[component]
fn CatalogStats(page: Option<WarehouseSources>, dataset: String, prefix: String) -> Element {
    let warehouse_root = dataset.is_empty();
    let scoped = page.as_ref().map(|page| {
        folder_stats(
            &page.sources,
            (!dataset.is_empty()).then_some(dataset.as_str()),
            &prefix,
        )
    });
    let runs = if warehouse_root {
        page.as_ref().map(|page| page.run_count).unwrap_or(0)
    } else {
        scoped.as_ref().map(|stats| stats.run_count).unwrap_or(0)
    };
    let sources = if warehouse_root {
        page.as_ref()
            .map(|page| format_source_summary(page.source_count, page.error_sources))
            .unwrap_or_else(|| "—".into())
    } else {
        scoped
            .as_ref()
            .map(|stats| format_source_summary(stats.source_count, stats.error_sources))
            .unwrap_or_else(|| "—".into())
    };
    let duration = if warehouse_root {
        page.as_ref()
            .map(|page| format_duration(page.duration_ms))
            .unwrap_or_else(|| "—".into())
    } else {
        "—".into()
    };
    let tokens = if warehouse_root {
        page.as_ref()
            .map(|page| format_tokens(page.total_tokens))
            .unwrap_or_else(|| "—".into())
    } else {
        "—".into()
    };
    rsx! {
        div { class: "pc-catalog-stats",
            div { span { "Runs" } strong { "{format_count(runs)}" } }
            div { span { "Sources" } strong { "{sources}" } }
            div { span { "Captured duration" } strong { "{duration}" } }
            div { span { "Tokens" } strong { "{tokens}" } }
        }
    }
}

#[component]
fn CatalogTreemap(
    dataset: String,
    prefix: String,
    nodes: Vec<CatalogNode>,
    selected: Option<(String, String)>,
    loading: bool,
    empty: bool,
    on_open: EventHandler<(String, String)>,
    on_select: EventHandler<(String, String)>,
) -> Element {
    let sizes = nodes
        .iter()
        .map(|node| node.run_count.max(1) as f64)
        .collect::<Vec<_>>();
    let boxes = layout_treemap(&sizes, 100.0, 100.0);
    rsx! {
        section { class: "pc-catalog-card pc-catalog-mosaic",
            div { class: "pc-catalog-card-head",
                div {
                    CatalogBreadcrumb { dataset: dataset.clone(), prefix: prefix.clone(), on_open }
                    h2 { "Sources by trace volume" }
                }
                span { class: "pc-catalog-view", "Treemap" }
            }
            div { class: "pc-catalog-tree",
                if loading && nodes.is_empty() && !empty {
                    div { class: "pc-catalog-empty", span { class: "spinner" } "Loading sources…" }
                } else if empty {
                    div { class: "pc-catalog-empty",
                        strong { "No sources" }
                        span { "Mount a Dataset and refresh the local store." }
                    }
                } else if nodes.is_empty() {
                    div { class: "pc-catalog-empty",
                        strong { "Empty folder" }
                        span { "This prefix has no nested sources." }
                    }
                } else {
                    for (index, node) in nodes.iter().cloned().enumerate() {
                        {
                            let tile = boxes.get(index).copied().unwrap_or(TileBox { x: 0.0, y: 0.0, w: 0.0, h: 0.0 });
                            let tone = index % 6;
                            let active = node_is_selected(&node, selected.as_ref());
                            let style = format!(
                                "left:{:.3}%;top:{:.3}%;width:{:.3}%;height:{:.3}%;",
                                tile.x, tile.y, tile.w, tile.h
                            );
                            let class_name = format!(
                                "pc-catalog-tile tone-{tone}{}",
                                if active { " selected" } else { "" }
                            );
                            let click_node = node.clone();
                            rsx! {
                                button {
                                    class: "{class_name}",
                                    style,
                                    title: "{node.name}",
                                    onclick: move |_| activate_node(&click_node, on_open, on_select),
                                    strong { "{node.name}" }
                                    small { "{node_caption(&node)}" }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn CatalogBreadcrumb(
    dataset: String,
    prefix: String,
    on_open: EventHandler<(String, String)>,
) -> Element {
    let segments = prefix
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    let root_dataset = dataset.clone();
    rsx! {
        p { class: "eyebrow",
            button {
                class: "pc-catalog-crumb",
                onclick: move |_| on_open.call((String::new(), String::new())),
                "Warehouse"
            }
            if !dataset.is_empty() {
                span { " / " }
                button {
                    class: "pc-catalog-crumb",
                    onclick: move |_| on_open.call((root_dataset.clone(), String::new())),
                    "{dataset}"
                }
            }
            for (index, segment) in segments.iter().enumerate() {
                span { " / " }
                {
                    let dataset = dataset.clone();
                    let path = segments[..=index].join("/");
                    let label = segment.clone();
                    rsx! {
                        button {
                            class: "pc-catalog-crumb",
                            onclick: move |_| on_open.call((dataset.clone(), path.clone())),
                            "{label}"
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn SelectedSource(
    source: WarehouseSource,
    on_source_traces: EventHandler<(String, String)>,
    on_source_analyze: EventHandler<(String, String)>,
) -> Element {
    let key = (source.dataset.clone(), source.file.clone());
    let analyze_key = key.clone();
    let ready = source.status == "ready";
    rsx! {
        section { class: "pc-catalog-card pc-catalog-selected",
            div { class: "pc-catalog-card-head",
                div {
                    p { class: "eyebrow", "Selected source" }
                    h2 { "{source.dataset} / {source.file}" }
                }
                span { class: if ready { "pc-catalog-badge ready" } else { "pc-catalog-badge error" },
                    if ready { "Ready" } else { "Error" }
                }
            }
            dl { class: "pc-catalog-meta",
                div { dt { "Format" } dd { "{source.format.as_deref().unwrap_or(\"—\")}" } }
                div { dt { "Revision" } dd { "{source.snapshot_ref.as_deref().unwrap_or(\"—\")}" } }
                div { dt { "Projection" } dd { "{projection_label(&source)}" } }
                div { dt { "Modified" } dd { "{source.last_modified.as_deref().unwrap_or(\"—\")}" } }
                div { dt { "Identity" } dd { "{source.dataset} / {source.file}" } }
            }
            if let Some(error) = source.error.clone() {
                p { class: "pc-catalog-errors", "{error}" }
            }
            div { class: "pc-catalog-selected-actions",
                button { class: "pc-catalog-btn", onclick: move |_| on_source_traces.call(key.clone()), "Open traces" }
                button { class: "pc-catalog-btn primary", onclick: move |_| on_source_analyze.call(analyze_key.clone()), "Analyze" }
            }
        }
    }
}

#[component]
fn SourceInventory(
    nodes: Vec<CatalogNode>,
    selected: Option<(String, String)>,
    on_open: EventHandler<(String, String)>,
    on_select: EventHandler<(String, String)>,
) -> Element {
    rsx! {
        section { class: "pc-catalog-card pc-catalog-inventory-card",
            div { class: "pc-catalog-card-head",
                div {
                    p { class: "eyebrow", "Current folder" }
                    h2 { "Source inventory" }
                }
            }
            ul { class: "pc-catalog-inventory",
                for node in nodes {
                    {
                        let click_node = node.clone();
                        let active = node_is_selected(&node, selected.as_ref());
                        rsx! {
                            li {
                                button {
                                    class: if active { "selected" } else { "" },
                                    onclick: move |_| activate_node(&click_node, on_open, on_select),
                                    span {
                                        strong { "{node.name}" }
                                        small { "{node_caption(&node)}" }
                                    }
                                    if node.kind == CatalogNodeKind::Source {
                                        span { class: if node.source.as_ref().is_some_and(|source| source.status == "ready") { "pc-catalog-badge ready" } else { "pc-catalog-badge error" },
                                            if node.source.as_ref().is_some_and(|source| source.status == "ready") { "Ready" } else { "Error" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn activate_node(
    node: &CatalogNode,
    on_open: EventHandler<(String, String)>,
    on_select: EventHandler<(String, String)>,
) {
    match node.kind {
        CatalogNodeKind::Dir => on_open.call((node.dataset.clone(), node.path.clone())),
        CatalogNodeKind::Source => {
            if let Some(source) = &node.source {
                on_select.call((source.dataset.clone(), source.file.clone()));
            }
        }
    }
}

fn node_is_selected(node: &CatalogNode, selected: Option<&(String, String)>) -> bool {
    node.source.as_ref().is_some_and(|source| {
        selected.is_some_and(|(dataset, file)| dataset == &source.dataset && file == &source.file)
    })
}

fn node_caption(node: &CatalogNode) -> String {
    if let Some(source) = &node.source {
        return tile_caption(source);
    }
    let traces = format!(
        "{} {}",
        format_count(node.run_count),
        if node.run_count == 1 {
            "trace"
        } else {
            "traces"
        }
    );
    let mut parts = vec![traces];
    if let Some(size) = node.size_bytes {
        parts.push(format_bytes(size));
    }
    if node.error_count > 0 {
        parts.push(format!(
            "{} source issue{}",
            node.error_count,
            if node.error_count == 1 { "" } else { "s" }
        ));
    }
    parts.join(" · ")
}

fn tile_caption(source: &WarehouseSource) -> String {
    let traces = format!(
        "{} {}",
        format_count(source.run_count),
        if source.run_count == 1 {
            "trace"
        } else {
            "traces"
        }
    );
    let mut parts = vec![traces];
    if let Some(size) = source.size_bytes {
        parts.push(format_bytes(size));
    }
    if source.status != "ready" {
        parts.push("1 source issue".into());
    }
    parts.join(" · ")
}

fn projection_label(source: &WarehouseSource) -> String {
    match (
        source.projection_status.as_deref(),
        source.projection_generation.as_deref(),
    ) {
        (Some(status), Some(generation)) => format!("{status} · generation {generation}"),
        (Some(status), None) => status.to_string(),
        (None, Some(generation)) => format!("generation {generation}"),
        (None, None) => "—".into(),
    }
}

fn format_count(value: usize) -> String {
    let text = value.to_string();
    let mut grouped = String::new();
    for (index, character) in text.chars().rev().enumerate() {
        if index > 0 && index % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(character);
    }
    grouped.chars().rev().collect()
}

fn format_duration(ms: Option<i64>) -> String {
    let Some(ms) = ms.filter(|value| *value >= 0) else {
        return "—".into();
    };
    if ms >= 3_600_000 {
        let hours = ms / 3_600_000;
        let minutes = (ms % 3_600_000) / 60_000;
        if minutes == 0 {
            format!("{hours}h")
        } else {
            format!("{hours}h {minutes}m")
        }
    } else if ms >= 60_000 {
        format!("{}m", ms / 60_000)
    } else if ms >= 1_000 {
        format!("{:.1}s", ms as f64 / 1_000.0)
    } else {
        format!("{ms}ms")
    }
}

fn format_tokens(tokens: Option<u64>) -> String {
    let Some(tokens) = tokens else {
        return "—".into();
    };
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}k", tokens as f64 / 1_000.0)
    } else {
        tokens.to_string()
    }
}

fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1000.0;
    const MB: f64 = KB * 1000.0;
    const GB: f64 = MB * 1000.0;
    if bytes as f64 >= GB {
        format!("{:.1} GB", bytes as f64 / GB)
    } else if bytes as f64 >= MB {
        format!("{:.1} MB", bytes as f64 / MB)
    } else if bytes as f64 >= KB {
        format!("{:.1} kB", bytes as f64 / KB)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(dataset: &str, file: &str, runs: usize) -> WarehouseSource {
        WarehouseSource {
            dataset: dataset.into(),
            file: file.into(),
            format: Some("canonical-event".into()),
            kind: "store".into(),
            snapshot_ref: None,
            projection_status: Some("fresh".into()),
            projection_generation: Some("42".into()),
            size_bytes: Some(2_400_000_000),
            last_modified: None,
            status: "ready".into(),
            error: None,
            run_count: runs,
            failed_count: 0,
        }
    }

    #[test]
    fn treemap_covers_the_canvas_and_keeps_area_proportional() {
        let boxes = layout_treemap(&[2.0, 1.0, 1.0], 100.0, 100.0);
        assert_eq!(boxes.len(), 3);
        let area: f64 = boxes.iter().map(|tile| tile.w * tile.h).sum();
        assert!((area - 10_000.0).abs() < 0.01);
        assert!((boxes[0].w * boxes[0].h - 5_000.0).abs() < 0.01);
    }

    #[test]
    fn empty_sizes_yield_no_tiles() {
        assert!(layout_treemap(&[], 100.0, 100.0).is_empty());
    }

    #[test]
    fn selected_source_keeps_explicit_choice_and_ignores_missing_keys() {
        let sources = vec![
            source("evals", "gateway/capture", 12),
            source("archive", "old.json", 1),
        ];
        let selected =
            selected_source(&sources, Some(&("archive".into(), "old.json".into()))).unwrap();
        assert_eq!(selected.file, "old.json");
        assert!(selected_source(&sources, None).is_none());
        assert!(selected_source(&sources, Some(&("missing".into(), "x".into()))).is_none());
    }

    #[test]
    fn warehouse_root_groups_sources_by_dataset() {
        let sources = vec![
            source("evals", "gateway/capture", 10),
            source("evals", "experiments/v4", 3),
            source("archive", "old.json", 1),
        ];
        let nodes = catalog_children(&sources, None, "");
        assert_eq!(
            nodes
                .iter()
                .map(|node| (node.name.as_str(), node.kind, node.run_count))
                .collect::<Vec<_>>(),
            vec![
                ("evals", CatalogNodeKind::Dir, 13),
                ("archive", CatalogNodeKind::Dir, 1),
            ]
        );
    }

    #[test]
    fn dataset_root_groups_the_next_file_segment() {
        let sources = vec![
            source("evals", "gateway/capture", 10),
            source("evals", "experiments/v4", 3),
            source("evals", "experiments/v3", 2),
            source("archive", "old.json", 1),
        ];
        let nodes = catalog_children(&sources, Some("evals"), "");
        assert_eq!(
            nodes
                .iter()
                .map(|node| (node.name.as_str(), node.kind, node.path.as_str(), node.run_count))
                .collect::<Vec<_>>(),
            vec![
                ("gateway", CatalogNodeKind::Dir, "gateway", 10),
                ("experiments", CatalogNodeKind::Dir, "experiments", 5),
            ]
        );
    }

    #[test]
    fn a_file_with_no_deeper_path_is_a_leaf() {
        let sources = vec![source("archive", "old.json", 1)];
        let nodes = catalog_children(&sources, Some("archive"), "");
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].kind, CatalogNodeKind::Source);
        assert_eq!(nodes[0].name, "old.json");
        assert_eq!(nodes[0].source.as_ref().unwrap().file, "old.json");
    }

    #[test]
    fn nested_prefix_exposes_the_leaf_source() {
        let sources = vec![source("evals", "gateway/capture", 10)];
        let nodes = catalog_children(&sources, Some("evals"), "gateway");
        assert_eq!(nodes[0].name, "capture");
        assert_eq!(nodes[0].kind, CatalogNodeKind::Source);
        assert_eq!(nodes[0].path, "gateway/capture");
    }

    #[test]
    fn treemap_head_does_not_drop_inventory_rows() {
        let sources = (0..20)
            .map(|index| source("evals", &format!("s{index}"), 20 - index))
            .collect::<Vec<_>>();
        assert_eq!(treemap_head(&sources).len(), 16);
        assert_eq!(sources.len(), 20);
    }

    #[test]
    fn folder_stats_follow_the_current_dataset_and_prefix() {
        let mut nested = source("data", "caiyuxuan/debug", 2);
        nested.status = "ready".into();
        let mut other = source("data", "other/file", 10);
        other.status = "error".into();
        let sources = vec![
            nested,
            other,
            source("archive", "old.json", 42),
        ];
        assert_eq!(
            folder_stats(&sources, None, ""),
            FolderStats {
                run_count: 54,
                source_count: 3,
                error_sources: 1,
            }
        );
        assert_eq!(
            folder_stats(&sources, Some("data"), ""),
            FolderStats {
                run_count: 12,
                source_count: 2,
                error_sources: 1,
            }
        );
        assert_eq!(
            folder_stats(&sources, Some("data"), "caiyuxuan"),
            FolderStats {
                run_count: 2,
                source_count: 1,
                error_sources: 0,
            }
        );
    }

    #[test]
    fn format_bytes_uses_decimal_units() {
        assert_eq!(format_bytes(2_400_000_000), "2.4 GB");
    }
}
