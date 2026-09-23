use dioxus::prelude::*;

use crate::components::WorkspaceIcon;
use crate::model::{CatalogTree, CatalogTreeChild};

#[component]
pub fn CatalogExplorer(
    tree: Option<CatalogTree>,
    loading: bool,
    on_open: EventHandler<(String, String)>,
    on_runs: EventHandler<(String, String)>,
    #[props(default = false)] auth_required: bool,
    on_settings: EventHandler<MouseEvent>,
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
                    if let Some(status) = tree.as_ref().and_then(|tree| tree.browse.as_ref()) {
                        p { role: "status",
                            if status.observed_at == 0 && status.refreshing {
                                {crate::strings::catalog::LOADING_CONTENTS}
                            } else if status.observed_at == 0 && status.state == "unavailable" {
                                {crate::strings::catalog::DIR_UNAVAILABLE_RETRY}
                            } else if status.last_error.is_some() {
                                {crate::strings::catalog::CACHED_REFRESH_FAILED}
                            } else if status.partial {
                                {crate::strings::catalog::PARTIAL_VIEW}
                            } else if status.refreshing {
                                {crate::strings::catalog::CACHED_REFRESHING}
                            } else if status.stale {
                                {crate::strings::catalog::CACHED_WAITING}
                            }
                        }
                    }
                }
                button {
                    class: "button pc-catalog-open",
                    onclick: move |_| on_runs.call((dataset.clone(), prefix.clone())),
                    WorkspaceIcon { name: "runs" }
                    {crate::strings::catalog::OPEN_IN_RUNS}
                }
            }
            if inside {
                CatalogStats { tree: tree.clone() }
            }
            div { class: "pc-catalog-mosaic",
                if (loading && tree.is_none()) || tree.as_ref().and_then(|tree| tree.browse.as_ref())
                    .is_some_and(|status| status.observed_at == 0 && status.refreshing) {
                    div { class: "pc-catalog-empty", span { class: "spinner" } {crate::strings::analysis::LOADING_DATASETS} }
                } else if auth_required {
                    div { class: "pc-catalog-empty",
                        strong { "Catalog identity required" }
                        span { {crate::strings::catalog::ADD_KEYS_HINT} }
                        button { class: "button primary", onclick: on_settings, {crate::strings::catalog::OPEN_KEYS} }
                    }
                } else if tree.as_ref().and_then(|tree| tree.browse.as_ref())
                    .is_some_and(|status| status.observed_at == 0 && (status.state == "unavailable" || status.last_error.is_some())) {
                    div { class: "pc-catalog-empty", strong { "Directory unavailable" } span { {crate::strings::catalog::DIR_UNAVAILABLE_HINT} } }
                } else if tree.as_ref().is_none_or(|tree| tree.children.is_empty() && tree.run_count == 0) {
                    div { class: "pc-catalog-empty", strong { "No datasets" } span { {crate::strings::catalog::NO_DATASETS_HINT} } }
                } else if tree.as_ref().is_some_and(|tree| tree.children.is_empty()) {
                    div { class: "pc-catalog-empty",
                        strong { "Source file" }
                        span { {crate::strings::catalog::SOURCE_FILE_HINT} }
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
            div { WorkspaceIcon { name: "folder" } div { span { "Datasets" } strong { "{tree.dataset_count.unwrap_or_else(|| tree.children.len())}" } } }
            div { WorkspaceIcon { name: "analysis" } div { span { {crate::strings::catalog::TRAJECTORIES} } strong { "{tree.trajectory_count.unwrap_or(tree.run_count)}" } } }
            div { WorkspaceIcon { name: "warning" } div { span { {crate::strings::status::FAILED} } strong { "{tree.failed_count}" } } }
        }
        if errors > 0 {
            p { class: "pc-catalog-errors", {crate::strings::catalog::SOURCE_LOAD_ERRORS_FMT} }
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
    let trajectory_count = child.trajectory_count.unwrap_or(child.run_count);
    let icon = if kind == "file" { "file" } else { "folder" };
    rsx! {
        button {
            class: "pc-catalog-folder type-{data_type} kind-{kind}",
            title: if is_dir {
                format!("{name} · directory")
            } else if trajectory_count > 0 {
                format!("{name} · {trajectory_count} trajectories")
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
                    if let Some(dataset_count) = child.dataset_count {
                        span {
                            "{dataset_count} datasets · {child.trajectory_count.unwrap_or(0)} trajectories"
                        }
                    } else {
                        span { {crate::strings::catalog::DIRECTORY} }
                    }
                } else if trajectory_count > 0 {
                    span { "{trajectory_count} trajectories" }
                } else {
                    span { {crate::strings::catalog::SOURCE} }
                }
                WorkspaceIcon { name: "chevron" }
            }
        }
    }
}

fn catalog_subtitle(tree: Option<&CatalogTree>) -> String {
    let Some(tree) = tree else {
        return {crate::strings::catalog::BROWSE_LIKE_LS}.into();
    };
    if tree.dataset.is_none() {
        format!("{} mounts", tree.children.len())
    } else if tree.prefix.is_empty() {
        format!("{} items", tree.children.len())
    } else {
        format!("Prefix {} · {} items", tree.prefix, tree.children.len())
    }
}
