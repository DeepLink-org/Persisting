use dioxus::prelude::*;

use crate::components::WorkspaceIcon;
use crate::model::{CatalogTree, CatalogTreeChild};

#[component]
pub fn CatalogExplorer(
    tree: Option<CatalogTree>,
    loading: bool,
    on_open: EventHandler<(String, String)>,
    on_runs: EventHandler<(String, String)>,
) -> Element {
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
                    class: "button pc-catalog-open",
                    onclick: move |_| on_runs.call((dataset.clone(), prefix.clone())),
                    WorkspaceIcon { name: "runs" }
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
                    div { class: "pc-catalog-empty", strong { "No datasets" } span { "Add a dataset, then refresh this page." } }
                } else if tree.as_ref().is_some_and(|tree| tree.children.is_empty()) {
                    div { class: "pc-catalog-empty",
                        strong { "Source file" }
                        span { "This path contains one source file. Open it in Runs to inspect its runs." }
                    }
                } else {
                    CatalogFolders {
                        tree: tree.clone().unwrap(),
                        on_open,
                        on_runs,
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
                span { class: "pc-catalog-separator", "/" }
                button {
                    class: "pc-catalog-crumb",
                    onclick: move |_| on_open.call((root_dataset.clone(), String::new())),
                    "{dataset}"
                }
            }
            for (index, segment) in segments.iter().enumerate() {
                span { class: "pc-catalog-separator", "/" }
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
    let errors = tree.error_sources.unwrap_or(0);
    rsx! {
        div { class: "pc-catalog-stats",
            div { WorkspaceIcon { name: "folder" } div { span { "Items" } strong { "{tree.children.len()}" } } }
            div { WorkspaceIcon { name: "analysis" } div { span { "Trajectories" } strong { "{tree.run_count}" } } }
            div { WorkspaceIcon { name: "warning" } div { span { "Failed" } strong { "{tree.failed_count}" } } }
        }
        if errors > 0 {
            p { class: "pc-catalog-errors", "{errors} source files could not be loaded" }
        }
    }
}

#[component]
fn CatalogFolders(
    tree: CatalogTree,
    on_open: EventHandler<(String, String)>,
    on_runs: EventHandler<(String, String)>,
) -> Element {
    let dataset = tree.dataset.clone().unwrap_or_default();
    rsx! {
        div { class: "pc-catalog-folders",
            for child in tree.children.iter().cloned() {
                CatalogFolder {
                    key: "{child.kind}:{child.path}",
                    child,
                    dataset: dataset.clone(),
                    on_open,
                    on_runs,
                }
            }
        }
    }
}

#[component]
fn CatalogFolder(
    child: CatalogTreeChild,
    dataset: String,
    on_open: EventHandler<(String, String)>,
    on_runs: EventHandler<(String, String)>,
) -> Element {
    let kind = child.kind.clone();
    let path = child.path.clone();
    let name = child.name.clone();
    let data_type = child.data_type.clone();
    let is_dir = kind == "dir";
    let icon = if kind == "file" { "file" } else { "folder" };
    rsx! {
        button {
            class: "pc-catalog-folder type-{data_type} kind-{kind}",
            title: if is_dir {
                format!("{name} · directory")
            } else if child.run_count > 0 {
                format!("{name} · {} trajectories", child.run_count)
            } else {
                name.clone()
            },
            onclick: move |_| {
                match kind.as_str() {
                    "file" => on_runs.call((dataset.clone(), path.clone())),
                    "dataset" => on_open.call((name.clone(), String::new())),
                    _ => on_open.call((dataset.clone(), path.clone())),
                }
            },
            div { class: "pc-catalog-folder-title",
                span { class: "pc-catalog-folder-icon", WorkspaceIcon { name: icon } }
                strong { "{child.name}" }
            }
            span { class: "pc-catalog-folder-path", title: "{child.path}", "{child.path}" }
            span { class: "pc-catalog-folder-type", "{data_type}" }
            div { class: "pc-catalog-folder-meta",
                if is_dir {
                    span { "Directory" }
                } else if child.run_count > 0 {
                    span { "{child.run_count} trajectories" }
                } else {
                    span { "Source" }
                }
                WorkspaceIcon { name: "chevron" }
            }
        }
    }
}

fn catalog_subtitle(tree: Option<&CatalogTree>) -> String {
    let Some(tree) = tree else {
        return "Browse the current path, like ls.".into();
    };
    if tree.dataset.is_none() {
        format!("{} mounts", tree.children.len())
    } else if tree.prefix.is_empty() {
        format!("{} items", tree.children.len())
    } else {
        format!("Prefix {} · {} items", tree.prefix, tree.children.len())
    }
}
