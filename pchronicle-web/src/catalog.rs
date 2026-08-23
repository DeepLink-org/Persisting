use dioxus::prelude::*;

use crate::model::{CatalogTree, CatalogTreeChild};

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

#[component]
pub fn CatalogExplorer(
    tree: Option<CatalogTree>,
    loading: bool,
    on_open: EventHandler<(String, String)>,
    on_runs: EventHandler<(String, String)>,
) -> Element {
    let mut other_open = use_signal(|| false);
    let dataset = tree
        .as_ref()
        .and_then(|tree| tree.dataset.clone())
        .unwrap_or_default();
    let prefix = tree
        .as_ref()
        .map(|tree| tree.prefix.clone())
        .unwrap_or_default();
    let inside = !dataset.is_empty();
    rsx! {
        section { class: "pc-catalog",
            header { class: "pc-catalog-head",
                div { class: "pc-catalog-title",
                    p { class: "eyebrow", "pChronicle" }
                    CatalogBreadcrumb {
                        dataset: dataset.clone(),
                        prefix: prefix.clone(),
                        on_open,
                    }
                    p { "{catalog_subtitle(tree.as_ref())}" }
                }
                button {
                    class: "button",
                    onclick: move |_| on_runs.call((dataset.clone(), prefix.clone())),
                    "Open in Runs"
                }
            }
            if inside {
                CatalogStats { tree: tree.clone() }
            }
            div { class: "pc-catalog-mosaic",
                if loading && tree.is_none() {
                    div { class: "pc-catalog-empty", span { class: "spinner" } "Loading datasets…" }
                } else if tree.as_ref().is_none_or(|tree| tree.children.is_empty() && tree.run_count == 0) {
                    div { class: "pc-catalog-empty", strong { "No datasets" } span { "Mount a Dataset and refresh the local store." } }
                } else if tree.as_ref().is_some_and(|tree| tree.children.is_empty()) {
                    div { class: "pc-catalog-empty",
                        strong { "Source" }
                        span { "This prefix is a single source. Open it in Runs to inspect trajectories." }
                    }
                } else {
                    CatalogMosaic {
                        tree: tree.clone().unwrap(),
                        on_open,
                        on_runs,
                        on_other: move |_| other_open.set(!other_open()),
                    }
                }
            }
            if other_open() {
                if let Some(other) = tree.as_ref().and_then(other_child) {
                    ul { class: "pc-catalog-other",
                        for entry in other.entries.clone() {
                            OtherEntry {
                                key: "{entry.path}",
                                dataset: dataset.clone(),
                                entry,
                                on_open,
                                on_runs,
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
        h1 {
            button { class: "pc-catalog-crumb", onclick: move |_| on_open.call((String::new(), String::new())), "Datasets" }
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
fn CatalogStats(tree: Option<CatalogTree>) -> Element {
    let Some(tree) = tree else {
        return rsx! {};
    };
    let fail = if tree.run_count == 0 {
        "—".into()
    } else {
        format!(
            "{:.1}%",
            100.0 * tree.failed_count as f64 / tree.run_count as f64
        )
    };
    let errors = tree.error_sources.unwrap_or(0);
    rsx! {
        div { class: "pc-catalog-stats",
            div { span { "Runs" } strong { "{tree.run_count}" } }
            div { span { "Fail rate" } strong { "{fail}" } }
            div { span { "Duration" } strong { "{format_duration(tree.duration_ms)}" } }
            div { span { "Tokens" } strong { "{format_tokens(tree.total_tokens)}" } }
        }
        if errors > 0 {
            p { class: "pc-catalog-errors", "{errors} sources failed to project" }
        }
    }
}

#[component]
fn CatalogMosaic(
    tree: CatalogTree,
    on_open: EventHandler<(String, String)>,
    on_runs: EventHandler<(String, String)>,
    on_other: EventHandler<MouseEvent>,
) -> Element {
    let sizes = tree
        .children
        .iter()
        .map(|child| child.run_count.max(1) as f64)
        .collect::<Vec<_>>();
    let boxes = layout_treemap(&sizes, 100.0, 100.0);
    let dataset = tree.dataset.clone().unwrap_or_default();
    // A treemap with one or two children stretches a single tile across the
    // whole viewport. Render those as fixed-size cards instead.
    let compact = tree.children.len() <= 2;
    let tree_class = if compact {
        "pc-catalog-tree compact"
    } else {
        "pc-catalog-tree"
    };
    rsx! {
        div { class: "{tree_class}",
            for (index, child) in tree.children.iter().cloned().enumerate() {
                {
                    let tile = boxes.get(index).copied().unwrap_or(TileBox { x: 0.0, y: 0.0, w: 0.0, h: 0.0 });
                    let tone = index % 6;
                    rsx! {
                        CatalogTile {
                            key: "{child.kind}:{child.path}",
                            child,
                            dataset: dataset.clone(),
                            tile,
                            tone,
                            compact,
                            on_open,
                            on_runs,
                            on_other,
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn CatalogTile(
    child: CatalogTreeChild,
    dataset: String,
    tile: TileBox,
    tone: usize,
    compact: bool,
    on_open: EventHandler<(String, String)>,
    on_runs: EventHandler<(String, String)>,
    on_other: EventHandler<MouseEvent>,
) -> Element {
    let style = if compact {
        String::new()
    } else {
        format!(
            "left:{:.3}%;top:{:.3}%;width:{:.3}%;height:{:.3}%;",
            tile.x, tile.y, tile.w, tile.h
        )
    };
    let kind = child.kind.clone();
    let path = child.path.clone();
    let name = child.name.clone();
    rsx! {
        button {
            class: "pc-catalog-tile tone-{tone} kind-{kind}",
            style,
            title: "{name} · {child.run_count} runs",
            onclick: move |event| {
                match kind.as_str() {
                    "other" => on_other.call(event),
                    "file" => on_runs.call((dataset.clone(), path.clone())),
                    "dataset" => on_open.call((name.clone(), String::new())),
                    _ => on_open.call((dataset.clone(), path.clone())),
                }
            },
            strong { "{child.name}" }
            small { "{child.run_count}" }
        }
    }
}

#[component]
fn OtherEntry(
    dataset: String,
    entry: CatalogTreeChild,
    on_open: EventHandler<(String, String)>,
    on_runs: EventHandler<(String, String)>,
) -> Element {
    let kind = entry.kind.clone();
    let path = entry.path.clone();
    let name = entry.name.clone();
    rsx! {
        li {
            button {
                onclick: move |_| {
                    match kind.as_str() {
                        "file" => on_runs.call((dataset.clone(), path.clone())),
                        "dataset" => on_open.call((name.clone(), String::new())),
                        _ => on_open.call((dataset.clone(), path.clone())),
                    }
                },
                strong { "{entry.name}" }
                span { "{entry.run_count}" }
            }
        }
    }
}

fn other_child(tree: &CatalogTree) -> Option<&CatalogTreeChild> {
    tree.children.iter().find(|child| child.kind == "other")
}

fn catalog_subtitle(tree: Option<&CatalogTree>) -> String {
    let Some(tree) = tree else {
        return "Browse Datasets by captured run volume.".into();
    };
    if tree.dataset.is_none() {
        format!("{} datasets · {} runs", tree.children.len(), tree.run_count)
    } else if tree.prefix.is_empty() {
        "Folders follow Dataset _file_ paths.".into()
    } else {
        format!("Prefix {} · {} runs", tree.prefix, tree.run_count)
    }
}

fn format_duration(ms: Option<i64>) -> String {
    let Some(ms) = ms.filter(|value| *value >= 0) else {
        return "—".into();
    };
    if ms >= 3_600_000 {
        format!("{:.0}h", ms as f64 / 3_600_000.0)
    } else if ms >= 60_000 {
        format!("{:.0}m", ms as f64 / 60_000.0)
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
        format!("{:.0}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}k", tokens as f64 / 1_000.0)
    } else {
        tokens.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
