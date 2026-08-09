#![allow(non_snake_case)]

mod api;
mod model;

use dioxus::document;
use dioxus::prelude::*;
use futures_util::StreamExt;
use gloo_net::eventsource::futures::EventSource;
use model::{EventRecord, QueryCatalog, RunSummary, StreamSnapshot, TrajectoryView, TurnView};
use serde_json::Value;
use wasm_bindgen::JsValue;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ToolTone {
    Shell,
    Code,
    File,
    Web,
    Reasoning,
    Generic,
}

impl ToolTone {
    fn class(self) -> &'static str {
        match self {
            Self::Shell => "shell",
            Self::Code => "code",
            Self::File => "file",
            Self::Web => "web",
            Self::Reasoning => "reasoning-tool",
            Self::Generic => "generic",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Shell => "Shell",
            Self::Code => "Code",
            Self::File => "File",
            Self::Web => "Web",
            Self::Reasoning => "Reasoning",
            Self::Generic => "Tool",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Self::Shell => "$",
            Self::Code => ">_",
            Self::File => "F",
            Self::Web => "↗",
            Self::Reasoning => "◇",
            Self::Generic => "⌁",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct DisplayToolCall {
    id: Option<String>,
    name: String,
    arguments: Value,
}

#[derive(Clone, PartialEq)]
struct SpanGroup {
    key: String,
    label: String,
    call_id: Option<String>,
    entries: Vec<(usize, TurnView)>,
    first_seq: u64,
    last_seq: u64,
    tool_calls: usize,
}

fn main() {
    launch(App);
}

fn format_json(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

fn pretty_event(event: &EventRecord) -> String {
    serde_json::to_string_pretty(event).unwrap_or_else(|_| "Unable to serialize event".into())
}

fn short_id(value: &str) -> String {
    if value.chars().count() > 18 {
        format!("{}…", value.chars().take(17).collect::<String>())
    } else {
        value.to_string()
    }
}

fn url_param(name: &str) -> Option<String> {
    let search = web_sys::window()?.location().search().ok()?;
    web_sys::UrlSearchParams::new_with_str(&search)
        .ok()?
        .get(name)
}

fn sync_url(run: &RunSummary, page: &str, source: &str, filter: &str) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let url = format!(
        "/?{}&page={}&source={}&q={}",
        run.query(),
        urlencoding::encode(page),
        urlencoding::encode(source),
        urlencoding::encode(filter)
    );
    let _ = window
        .history()
        .and_then(|history| history.replace_state_with_url(&JsValue::NULL, "", Some(&url)));
}

fn run_from_url(runs: &[RunSummary]) -> Option<RunSummary> {
    let search = web_sys::window()?.location().search().ok()?;
    let params = web_sys::UrlSearchParams::new_with_str(&search).ok()?;
    let agent = params.get("agent_id")?;
    let session = params.get("session_id")?;
    let root = params.get("root_session_id");
    runs.iter()
        .find(|run| {
            run.agent_id == agent && run.session_id == session && run.root_session_id == root
        })
        .cloned()
}

fn load_run(
    run: RunSummary,
    mut view: Signal<Option<TrajectoryView>>,
    mut events: Signal<Vec<EventRecord>>,
    mut loading: Signal<bool>,
    mut error: Signal<Option<String>>,
    mut selected_turn: Signal<Option<usize>>,
    scroll: bool,
) {
    loading.set(true);
    error.set(None);
    spawn(async move {
        let (next_view, next_events) =
            futures_util::join!(api::trajectory(&run), api::all_events(&run));
        match (next_view, next_events) {
            (Ok(next_view), Ok(next_events)) => {
                view.set(Some(next_view));
                events.set(next_events);
                if selected_turn().is_some_and(|index| {
                    view.read()
                        .as_ref()
                        .is_none_or(|value| index >= value.turns.len())
                }) {
                    selected_turn.set(None);
                }
                if scroll {
                    let _ = document::eval(
                        "requestAnimationFrame(() => document.getElementById('trajectory-end')?.scrollIntoView({block:'end'}))",
                    );
                }
            }
            (Err(message), _) | (_, Err(message)) => error.set(Some(message)),
        }
        loading.set(false);
    });
}

#[component]
fn App() -> Element {
    let mut runs = use_signal(Vec::<RunSummary>::new);
    let mut runs_loading = use_signal(|| true);
    let mut selected = use_signal(|| None::<RunSummary>);
    let mut view = use_signal(|| None::<TrajectoryView>);
    let events = use_signal(Vec::<EventRecord>::new);
    let loading = use_signal(|| false);
    let mut error = use_signal(|| None::<String>);
    let mut selected_turn = use_signal(|| None::<usize>);
    let mut run_filter = use_signal(String::new);
    let mut turn_filter = use_signal(|| url_param("q").unwrap_or_default());
    let mut source_filter = use_signal(|| {
        url_param("source")
            .filter(|value| matches!(value.as_str(), "all" | "user" | "agent" | "system"))
            .unwrap_or_else(|| "all".into())
    });
    let mut follow = use_signal(|| true);
    let mut new_events = use_signal(|| 0usize);
    let mut inspector_open = use_signal(|| true);
    let mut expand_spans = use_signal(|| false);
    let mut page = use_signal(|| {
        url_param("page")
            .filter(|value| matches!(value.as_str(), "runs" | "tools"))
            .unwrap_or_else(|| "runs".into())
    });
    let mut query_catalog = use_signal(|| None::<QueryCatalog>);
    let mut query_tables_loading = use_signal(|| false);
    let mut query_tables_error = use_signal(|| None::<String>);
    let mut selected_query_table = use_signal(String::new);

    use_effect(move || {
        spawn(async move {
            match api::runs().await {
                Ok(mut values) => {
                    values.sort_by(|a, b| {
                        a.agent_id
                            .cmp(&b.agent_id)
                            .then(a.session_id.cmp(&b.session_id))
                    });
                    let initial = run_from_url(&values).or_else(|| values.first().cloned());
                    runs.set(values);
                    if let Some(run) = initial {
                        selected.set(Some(run.clone()));
                        load_run(run, view, events, loading, error, selected_turn, false);
                    }
                }
                Err(message) => error.set(Some(message)),
            }
            runs_loading.set(false);
        });
    });

    use_effect(move || {
        if let Some(run) = selected() {
            sync_url(&run, &page(), &source_filter(), &turn_filter());
        }
    });

    use_effect(move || {
        if page() != "tools" {
            return;
        }
        query_tables_loading.set(true);
        query_tables_error.set(None);
        spawn(async move {
            match api::query_catalog().await {
                Ok(catalog) => {
                    if !catalog
                        .tables
                        .iter()
                        .any(|table| table.name == selected_query_table())
                    {
                        selected_query_table.set(
                            catalog
                                .tables
                                .first()
                                .map(|table| table.name.clone())
                                .unwrap_or_default(),
                        );
                    }
                    query_catalog.set(Some(catalog));
                }
                Err(message) => query_tables_error.set(Some(message)),
            }
            query_tables_loading.set(false);
        });
    });

    let stream_run = selected();
    use_effect(move || {
        let Some(run) = stream_run.clone() else {
            return;
        };
        spawn(async move {
            let url = format!("/api/v1/stream?{}", run.query());
            let Ok(mut source) = EventSource::new(&url) else {
                return;
            };
            let Ok(mut subscription) = source.subscribe("snapshot") else {
                return;
            };
            while let Some(message) = subscription.next().await {
                if selected.read().as_ref() != Some(&run) {
                    break;
                }
                let Ok((_, message)) = message else { continue };
                let Some(payload) = message.data().as_string() else {
                    continue;
                };
                let Ok(snapshot) = serde_json::from_str::<StreamSnapshot>(&payload) else {
                    continue;
                };
                if let Some(message) = snapshot.error {
                    error.set(Some(message));
                    continue;
                }
                let Some(row_count) = snapshot.row_count else {
                    continue;
                };
                if let Some(status) = snapshot.status {
                    if let Some(current_view) = view.write().as_mut() {
                        current_view.run.status = status;
                    }
                }
                let current = view
                    .read()
                    .as_ref()
                    .map(|value| value.run.row_count)
                    .unwrap_or(0);
                if row_count > current {
                    if follow() {
                        new_events.set(0);
                        load_run(
                            run.clone(),
                            view,
                            events,
                            loading,
                            error,
                            selected_turn,
                            true,
                        );
                    } else {
                        new_events.set(row_count - current);
                    }
                }
            }
            source.close();
        });
    });

    let selected_value = selected();
    let view_value = view();
    let catalog_value = query_catalog();
    let query = turn_filter().to_ascii_lowercase();
    let source = source_filter();
    let filtered_turns = view_value
        .as_ref()
        .map(|value| {
            value
                .turns
                .iter()
                .enumerate()
                .filter(|(_, item)| {
                    (source == "all" || item.turn.source == source)
                        && (query.is_empty() || item.turn.searchable_text().contains(&query))
                })
                .map(|(index, item)| (index, item.clone()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let run_query = run_filter().to_ascii_lowercase();
    let visible_runs = runs
        .read()
        .iter()
        .filter(|run| run_query.is_empty() || run.search_text().contains(&run_query))
        .cloned()
        .collect::<Vec<_>>();

    rsx! {
        div { class: "app-shell",
            a { class: "skip-link", href: "#workspace", "Skip to trajectory" }
            nav { class: "rail", aria_label: "Workspace",
                div { class: "brand-mark", title: "pChronicle", "pC" }
                button {
                    class: if page() == "runs" { "rail-button active" } else { "rail-button" },
                    aria_label: "Trajectory runs",
                    aria_current: if page() == "runs" { "page" } else { "false" },
                    onclick: move |_| page.set("runs".into()),
                    span { class: "rail-icon", "◫" } span { "Runs" }
                }
                button {
                    class: if page() == "tools" { "rail-button active" } else { "rail-button" },
                    aria_label: "Analysis tools",
                    aria_current: if page() == "tools" { "page" } else { "false" },
                    onclick: move |_| page.set("tools".into()),
                    span { class: "rail-icon", "⌁" } span { "Tools" }
                }
                div { class: "rail-spacer" }
                div { class: "rail-status", title: "Loopback-only local workspace", span { class: "live-dot" } "Local" }
            }

            aside { class: "run-sidebar",
                if page() == "tools" {
                    div { class: "sidebar-heading table-sidebar-heading",
                        div { p { class: "eyebrow", "Query catalog" } h1 { "Available tables" } }
                    }
                    div { class: "table-source-summary",
                        if let Some(catalog) = &catalog_value {
                            span { "Directory database" }
                            strong { title: "{catalog.storage_path}", "{catalog.database}" }
                        } else {
                            span { "Inspecting data path" }
                        }
                    }
                    div { class: "run-count", "{catalog_value.as_ref().map_or(0, |catalog| catalog.tables.len())} virtual tables" }
                    div { class: "query-table-list",
                        if query_tables_loading() {
                            for _ in 0..3 { div { class: "run-skeleton table-skeleton" } }
                        } else if let Some(message) = query_tables_error() {
                            EmptyState { title: "Unable to inspect schema", detail: message }
                        } else if catalog_value.is_none() {
                            EmptyState { title: "No queryable tables", detail: "The data path did not expose a query catalog." }
                        } else {
                            div { class: "database-node",
                                span { class: "database-glyph", "◫" }
                                div { strong { "{catalog_value.as_ref().unwrap().database}" } code { "{catalog_value.as_ref().unwrap().storage_path}" } }
                            }
                            for table in catalog_value.as_ref().unwrap().tables.iter() {
                                {
                                    let name = table.name.clone();
                                    let active = selected_query_table() == name;
                                    rsx! { button {
                                        key: "table-{table.name}",
                                        class: if active { "query-table-item selected" } else { "query-table-item" },
                                        aria_pressed: active,
                                        onclick: move |_| selected_query_table.set(name.clone()),
                                        div { class: "query-table-name", span { class: "table-glyph", "▦" } strong { "{catalog_value.as_ref().unwrap().database}.{table.name}" } span { class: "table-source-pill", "{table.grain}" } }
                                        p { "{table.description}" }
                                    } }
                                }
                            }
                        }
                    }
                    div { class: "table-sidebar-help", strong { "Read-only workspace" } span { "Select a table to prepare a SQL query." } }
                } else {
                div { class: "sidebar-heading",
                    div { p { class: "eyebrow", "pChronicle" } h1 { "Trajectory runs" } }
                    button {
                        class: "icon-button",
                        aria_label: "Refresh runs",
                        onclick: move |_| {
                            runs_loading.set(true);
                            spawn(async move {
                                match api::runs().await {
                                    Ok(values) => runs.set(values),
                                    Err(message) => error.set(Some(message)),
                                }
                                runs_loading.set(false);
                            });
                        },
                        "↻"
                    }
                }
                label { class: "search-field",
                    span { "⌕" }
                    input {
                        value: "{run_filter}",
                        placeholder: "Filter agent or session",
                        aria_label: "Filter runs",
                        oninput: move |event| run_filter.set(event.value()),
                    }
                    kbd { "/" }
                }
                div { class: "run-count", "{visible_runs.len()} runs" }
                div { class: "run-list",
                    if runs_loading() {
                        for _ in 0..5 { div { class: "run-skeleton" } }
                    } else if visible_runs.is_empty() {
                        if let Some(message) = error() {
                            EmptyState { title: "Unable to load runs", detail: message }
                        } else {
                            EmptyState { title: "No matching runs", detail: "Try a broader filter or refresh the store." }
                        }
                    } else {
                        for run in visible_runs {
                            {
                                let active = selected_value.as_ref() == Some(&run);
                                let item = run.clone();
                                rsx! { button {
                                    key: "{run.agent_id}/{run.session_id}/{run.root_session_id:?}",
                                    class: if active { "run-item selected" } else { "run-item" },
                                    aria_pressed: active,
                                    onclick: move |_| {
                                            sync_url(&item, "runs", &source_filter(), &turn_filter());
                                        selected.set(Some(item.clone()));
                                        selected_turn.set(None);
                                        new_events.set(0);
                                        follow.set(true);
                                        page.set("runs".into());
                                        load_run(item.clone(), view, events, loading, error, selected_turn, false);
                                    },
                                    div { class: "run-item-top", strong { "{run.agent_id}" } StatusPill { status: run.status.clone() } }
                                    div { class: "run-session", title: "{run.session_id}", "{run.session_id}" }
                                    div { class: "run-meta",
                                        span { "{run.row_count} events" }
                                        if run.duplicate_event_ids > 0 { span { class: "warning-text", "{run.duplicate_event_ids} duplicates" } }
                                        else if let Some(root) = &run.root_session_id {
                                            if root == &run.session_id { span { "root session" } } else { span { "child session" } }
                                        }
                                    }
                                } }
                            }
                        }
                    }
                }
                }
            }

            main { id: "workspace", class: "workspace", tabindex: "-1",
                if page() == "tools" {
                    ToolsWorkspace { catalog: catalog_value.clone(), selected_table: selected_query_table }
                } else if selected_value.is_none() {
                    WelcomeState {}
                } else {
                    div { class: "workspace-header",
                        div { class: "title-block",
                            div { class: "breadcrumb", "Runs / {selected_value.as_ref().unwrap().agent_id}" }
                            h2 { "{selected_value.as_ref().unwrap().session_id}" }
                            div { class: "header-meta",
                                StatusPill { status: selected_value.as_ref().unwrap().status.clone() }
                                code { "{selected_value.as_ref().unwrap().agent_id}" }
                                if let Some(root) = &selected_value.as_ref().unwrap().root_session_id { span { "root {short_id(root)}" } }
                            }
                        }
                        div { class: "header-actions",
                            button {
                                class: if follow() { "button active-follow" } else { "button" },
                                aria_pressed: follow(),
                                onclick: move |_| {
                                    follow.set(!follow());
                                    if follow() && new_events() > 0 {
                                        new_events.set(0);
                                        if let Some(run) = selected() { load_run(run, view, events, loading, error, selected_turn, true); }
                                    }
                                },
                                span { class: "live-dot" } if follow() { "Following" } else { "Paused" }
                            }
                            a { class: "button", href: "/api/v1/export/har?{selected_value.as_ref().unwrap().query()}", download: "{selected_value.as_ref().unwrap().session_id}.har", "Export HAR" }
                            a { class: "button", href: "/api/v1/export/otlp?{selected_value.as_ref().unwrap().query()}", download: "{selected_value.as_ref().unwrap().session_id}.otlp.json", "OTLP" }
                        }
                    }

                    if let Some(message) = error() {
                        div { class: "notice error-notice", role: "alert", strong { "Unable to load evidence" } span { "{message}" } }
                    }
                    if new_events() > 0 {
                        button {
                            class: "new-events",
                            onclick: move |_| {
                                follow.set(true);
                                new_events.set(0);
                                if let Some(run) = selected() { load_run(run, view, events, loading, error, selected_turn, true); }
                            },
                            "{new_events} new events · resume following"
                        }
                    }
                    if let Some(value) = &view_value {
                        MetricStrip { view: value.clone() }
                    }
                    div { class: if inspector_open() { "evidence-layout" } else { "evidence-layout inspector-hidden" },
                        section { class: "evidence-surface",
                            div { class: "surface-toolbar",
                                div { div { class: "surface-title", "Trace hierarchy" } div { class: "surface-subtitle", "Calls grouped as spans over canonical event sequence" } }
                                div { class: "filters",
                                    button { class: "compact-action", onclick: move |_| expand_spans.set(true), "Expand all" }
                                    button { class: "compact-action", onclick: move |_| expand_spans.set(false), "Collapse all" }
                                    select {
                                        aria_label: "Filter by source",
                                        value: "{source_filter}",
                                        onchange: move |event| source_filter.set(event.value()),
                                        option { value: "all", "All roles" }
                                        option { value: "user", "User" }
                                        option { value: "agent", "Agent" }
                                        option { value: "system", "System" }
                                    }
                                    input {
                                        value: "{turn_filter}",
                                        aria_label: "Filter storyline",
                                        placeholder: "Filter loaded evidence",
                                        oninput: move |event| turn_filter.set(event.value()),
                                    }
                                    button { class: "icon-button", aria_label: "Toggle inspector", onclick: move |_| inspector_open.set(!inspector_open()), "◧" }
                                }
                            }
                            div {
                                class: "trajectory-scroll",
                                onwheel: move |_| if follow() { follow.set(false) },
                                if loading() && view_value.is_none() {
                                    div { class: "loading-panel", span { class: "spinner" } "Loading canonical events…" }
                                } else if filtered_turns.is_empty() {
                                    EmptyState { title: "No visible turns", detail: "The projection is empty or the current filter removed every turn." }
                                } else {
                                    SpanTimeline {
                                        turns: filtered_turns,
                                        total_events: view_value.as_ref().map(|value| value.run.row_count).unwrap_or(0),
                                        expanded: expand_spans(),
                                        selected: selected_turn(),
                                        on_select: move |next| { selected_turn.set(Some(next)); inspector_open.set(true); },
                                    }
                                    div { id: "trajectory-end" }
                                }
                            }
                        }
                        if inspector_open() {
                            Inspector {
                                item: selected_turn().and_then(|index| view_value.as_ref().and_then(|value| value.turns.get(index).cloned())),
                                events: events(),
                                on_close: move |_| inspector_open.set(false),
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn StatusPill(status: String) -> Element {
    let class = if status.eq_ignore_ascii_case("ok") || status.eq_ignore_ascii_case("active") {
        "status-pill good"
    } else {
        "status-pill neutral"
    };
    rsx! { span { class, span { class: "status-dot" } "{status}" } }
}

#[component]
fn EmptyState(title: &'static str, detail: String) -> Element {
    rsx! { div { class: "empty-state", div { class: "empty-icon", "◇" } strong { "{title}" } p { "{detail}" } } }
}

#[component]
fn WelcomeState() -> Element {
    rsx! { div { class: "welcome-state",
        div { class: "welcome-orbit", div { class: "orbit-dot" } span { "pC" } }
        p { class: "eyebrow", "Wire truth · local analysis" }
        h2 { "Select a trajectory to inspect" }
        p { "Choose a run from the left. Storyline keeps the dialogue readable while every turn remains linked to its canonical events." }
        div { class: "welcome-keys", span { kbd { "/" } " Filter runs" } span { kbd { "↑↓" } " Move through evidence" } }
    } }
}

#[component]
fn MetricStrip(view: TrajectoryView) -> Element {
    let kinds = view.event_kind_counts.len();
    let duplicates = view.run.duplicate_event_ids;
    rsx! { section { class: "metric-strip", aria_label: "Trajectory summary",
        Metric { label: "Events", value: view.run.row_count.to_string(), detail: format!("{kinds} canonical kinds") }
        Metric { label: "Turns", value: view.turns.len().to_string(), detail: "Storyline projection" }
        Metric { label: "Tool calls", value: view.tool_call_count.to_string(), detail: "Observed in wire payloads" }
        Metric { label: "Duplicate IDs", value: duplicates.to_string(), detail: if duplicates == 0 { String::from("No duplicates reported") } else { String::from("Review canonical identity") } }
    } }
}

#[component]
fn Metric(label: String, value: String, detail: String) -> Element {
    rsx! { div { class: "metric", span { "{label}" } strong { "{value}" } small { "{detail}" } } }
}

fn span_groups(turns: Vec<(usize, TurnView)>) -> Vec<SpanGroup> {
    let mut groups = Vec::<SpanGroup>::new();
    for (index, item) in turns {
        let key = item
            .call_id
            .clone()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| format!("turn-{}", item.turn.id));
        let first_seq = item
            .event_seqs
            .iter()
            .copied()
            .min()
            .unwrap_or(index as u64);
        let last_seq = item.event_seqs.iter().copied().max().unwrap_or(first_seq);
        let tool_calls = item
            .turn
            .tool_calls
            .as_ref()
            .map_or(item.wire_tool_calls.len(), Vec::len);
        let action_name = item
            .turn
            .tool_calls
            .as_ref()
            .and_then(|calls| calls.first())
            .map(|call| call.function_name.clone())
            .or_else(|| item.wire_tool_calls.first().map(|call| call.name.clone()));
        let step_number = (item.turn.id.max(1) + 1) / 2;
        if let Some(group) = groups.last_mut().filter(|group| group.key == key) {
            group.first_seq = group.first_seq.min(first_seq);
            group.last_seq = group.last_seq.max(last_seq);
            group.tool_calls += tool_calls;
            if let Some(action_name) = action_name {
                group.label = format!("Step {step_number} · {action_name}");
            }
            group.entries.push((index, item));
            continue;
        }
        let label = if let Some(action_name) = action_name {
            format!("Step {step_number} · {action_name}")
        } else if tool_calls > 0 {
            format!("Agent step #{}", item.turn.id)
        } else {
            format!("Step {step_number}")
        };
        groups.push(SpanGroup {
            key,
            label,
            call_id: item.call_id.clone(),
            entries: vec![(index, item)],
            first_seq,
            last_seq,
            tool_calls,
        });
    }
    groups
}

fn compact_preview(value: &str, limit: usize) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() > limit {
        format!("{}…", normalized.chars().take(limit).collect::<String>())
    } else if normalized.is_empty() {
        "No text response".to_string()
    } else {
        normalized
    }
}

fn tool_tone(name: &str) -> ToolTone {
    let name = name.to_ascii_lowercase();
    if ["ipython", "python", "notebook", "code_cell"]
        .iter()
        .any(|token| name.contains(token))
    {
        ToolTone::Code
    } else if ["bash", "shell", "terminal", "command", "execute"]
        .iter()
        .any(|token| name.contains(token))
    {
        ToolTone::Shell
    } else if ["read", "write", "edit", "file", "replace", "patch"]
        .iter()
        .any(|token| name.contains(token))
    {
        ToolTone::File
    } else if ["browser", "web", "http", "url", "fetch", "navigate"]
        .iter()
        .any(|token| name.contains(token))
    {
        ToolTone::Web
    } else if ["think", "reason", "reflect"]
        .iter()
        .any(|token| name.contains(token))
    {
        ToolTone::Reasoning
    } else {
        ToolTone::Generic
    }
}

fn tool_narrative(content: &str) -> String {
    let boundary = ["<tool_call>", "<function="]
        .iter()
        .filter_map(|marker| content.find(marker))
        .min()
        .unwrap_or(content.len());
    content[..boundary]
        .trim()
        .trim_end_matches("</think>")
        .trim()
        .to_string()
}

fn display_tool_calls(item: &TurnView) -> Vec<DisplayToolCall> {
    if let Some(calls) = item
        .turn
        .tool_calls
        .as_ref()
        .filter(|calls| !calls.is_empty())
    {
        calls
            .iter()
            .map(|call| DisplayToolCall {
                id: (!call.tool_call_id.is_empty()).then(|| call.tool_call_id.clone()),
                name: call.function_name.clone(),
                arguments: call.arguments.clone(),
            })
            .collect()
    } else {
        item.wire_tool_calls
            .iter()
            .map(|call| DisplayToolCall {
                id: call.id.clone(),
                name: call.name.clone(),
                arguments: call.arguments.clone(),
            })
            .collect()
    }
}

fn argument_text(arguments: &Value, key: &str) -> Option<String> {
    arguments.get(key).and_then(|value| match value {
        Value::String(value) if !value.trim().is_empty() => Some(value.clone()),
        Value::Null => None,
        value => Some(value.to_string()),
    })
}

fn tool_preview(call: &DisplayToolCall, limit: usize) -> String {
    let detail = [
        "command",
        "cmd",
        "code",
        "path",
        "file_path",
        "url",
        "query",
        "thought",
    ]
    .iter()
    .find_map(|key| argument_text(&call.arguments, key));
    detail.map_or_else(
        || call.name.clone(),
        |detail| compact_preview(&format!("{} · {detail}", call.name), limit),
    )
}

#[component]
fn SpanTimeline(
    turns: Vec<(usize, TurnView)>,
    total_events: usize,
    expanded: bool,
    selected: Option<usize>,
    on_select: EventHandler<usize>,
) -> Element {
    let groups = span_groups(turns);
    let total_refs = groups
        .iter()
        .map(|group| {
            group
                .entries
                .iter()
                .map(|(_, item)| item.event_seqs.len())
                .sum::<usize>()
        })
        .sum::<usize>();
    rsx! {
        div { class: "span-summary",
            span { strong { "{groups.len()} spans" } " · {total_refs} event references" }
            span { "Sequence window 0 — {total_events.saturating_sub(1)}" }
        }
        div { class: "span-table", role: "tree", aria_label: "Trajectory span hierarchy",
            div { class: "span-table-head",
                div { "Structure" }
                div { "Summary" }
                div { class: "span-axis-head",
                    span { "Position / occupancy" }
                    div { class: "span-axis-ticks", span { "0" } span { "25%" } span { "50%" } span { "75%" } span { "{total_events.saturating_sub(1)}" } }
                }
                div { "Evidence" }
            }
            details { class: "trace-root", open: true,
                summary { class: "trace-root-summary",
                    div { class: "span-structure root", span { class: "disclosure" } strong { "trajectory" } span { "{groups.len()} spans" } }
                    div { class: "span-row-copy root-copy", "{total_refs} canonical references across the loaded run" }
                    div { class: "span-track", div { class: "span-bar root-bar", style: "left:0%;width:100%" } }
                    div { class: "span-evidence-count", strong { "{total_refs} ev" } span { "{groups.iter().map(|group| group.tool_calls).sum::<usize>()} tools" } }
                }
                div { class: "span-children",
                    for group in groups {
                        SpanRow {
                            key: "{group.key}",
                            group,
                            total_events,
                            expanded,
                            selected,
                            on_select,
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn SpanRow(
    group: SpanGroup,
    total_events: usize,
    expanded: bool,
    selected: Option<usize>,
    on_select: EventHandler<usize>,
) -> Element {
    let event_refs = group
        .entries
        .iter()
        .map(|(_, item)| item.event_seqs.len())
        .sum::<usize>();
    let roles = group
        .entries
        .iter()
        .map(|(_, item)| item.turn.source.as_str())
        .collect::<Vec<_>>()
        .join(" → ");
    let action = group.entries.iter().find_map(|(_, item)| {
        item.turn
            .tool_calls
            .as_ref()
            .and_then(|calls| calls.first())
            .map(|call| (call.function_name.clone(), call.arguments.clone()))
            .or_else(|| {
                item.wire_tool_calls
                    .first()
                    .map(|call| (call.name.clone(), call.arguments.clone()))
            })
    });
    let preview = action
        .as_ref()
        .map(|(name, arguments)| {
            let detail = arguments
                .get("command")
                .or_else(|| arguments.get("path"))
                .or_else(|| arguments.get("file_path"))
                .and_then(Value::as_str)
                .map(|value| compact_preview(value, 100))
                .unwrap_or_else(|| compact_preview(&format_json(arguments), 100));
            format!("{name} · {detail}")
        })
        .or_else(|| {
            group
                .entries
                .iter()
                .rev()
                .find(|(_, item)| {
                    item.turn.source == "agent" && !item.turn.text().trim().is_empty()
                })
                .or_else(|| {
                    group
                        .entries
                        .iter()
                        .rev()
                        .find(|(_, item)| !item.turn.text().trim().is_empty())
                })
                .map(|(_, item)| compact_preview(&item.turn.text(), 120))
        })
        .unwrap_or_else(|| "No response content".to_string());
    let model = group
        .entries
        .iter()
        .rev()
        .find_map(|(_, item)| item.turn.model_name.clone());
    let latency = group
        .entries
        .iter()
        .filter_map(|(_, item)| item.turn.latency_ms)
        .sum::<i64>();
    let denominator = total_events.max(1) as f64;
    let left = group.first_seq as f64 / denominator * 100.0;
    let width = (group.last_seq.saturating_sub(group.first_seq) + 1) as f64 / denominator * 100.0;
    let phase = if group.tool_calls > 0 {
        "tool"
    } else {
        "model"
    };
    rsx! {
        details { class: "span-row", open: expanded,
            summary { class: "span-row-summary",
                div { class: "span-structure",
                    span { class: "disclosure" }
                    span { class: "span-status {phase}" }
                    div { strong { title: "{group.label}", "{group.label}" } span { "{roles} · seq {group.first_seq}–{group.last_seq}" } }
                    span { class: "phase-badge {phase}", "{phase}" }
                }
                div { class: "span-row-copy",
                    strong { title: "{preview}", "{preview}" }
                    div { class: "span-copy-meta",
                        if let Some(model) = model { span { "{model}" } }
                        if latency > 0 { span { "{latency} ms" } }
                        if group.tool_calls > 0 { span { "{group.tool_calls} tool calls" } }
                    }
                }
                div { class: "span-track", title: "seq {group.first_seq} — {group.last_seq}",
                    div { class: "span-grid-lines" }
                    div { class: "span-bar {phase}", style: "left:{left:.4}%;width:max({width:.4}%,3px)" }
                }
                div { class: "span-evidence-count", strong { "{event_refs} ev" } span { "{group.tool_calls} tools" } }
            }
            div { class: "span-detail",
                div { class: "span-detail-meta",
                    code { "seq {group.first_seq}..{group.last_seq}" }
                    if let Some(call_id) = &group.call_id { code { "call {call_id}" } }
                }
                for (index, item) in group.entries {
                    TurnRow {
                        key: "turn-{item.turn.id}",
                        index,
                        item: item.clone(),
                        active: selected == Some(index),
                        on_select,
                    }
                }
            }
        }
    }
}

#[component]
fn TurnRow(index: usize, item: TurnView, active: bool, on_select: EventHandler<usize>) -> Element {
    let source_class = match item.turn.source.as_str() {
        "user" => "user",
        "agent" => "agent",
        "system" => "system",
        _ => "other",
    };
    let content = item.turn.text();
    let calls = display_tool_calls(&item);
    let narrative = tool_narrative(&content);
    let preview = calls
        .first()
        .map(|call| tool_preview(call, 180))
        .unwrap_or_else(|| compact_preview(&content, 180));
    let kind = item.turn.kind.clone().unwrap_or_else(|| "dialogue".into());
    let tool_count = calls.len();
    rsx! { details {
        class: if active { "compact-turn selected" } else { "compact-turn" },
        summary {
            aria_label: "Expand {item.turn.source} turn {item.turn.id}",
            span { class: "compact-turn-chevron" }
            span { class: "role-badge {source_class}", "{item.turn.source}" }
            code { "#{item.turn.id}" }
            span { class: "compact-kind", "{kind}" }
            span { class: "compact-preview", title: "{preview}", "{preview}" }
            span { class: "compact-turn-stats",
                if tool_count > 0 { span { "{tool_count} tools" } }
                span { "{item.event_seqs.len()} ev" }
            }
        }
        div { class: "compact-turn-body",
            if !narrative.is_empty() {
                div { class: "tool-narrative", "{narrative}" }
            } else if calls.is_empty() && content.trim().is_empty() {
                div { class: "empty-response", "No assistant text was returned for this step." }
            } else if calls.is_empty() {
                pre { class: "compact-content", "{content}" }
            }
            if let Some(reasoning) = &item.turn.reasoning_content { details { class: "reasoning", summary { "Reasoning preview" } pre { "{reasoning}" } } }
            if !calls.is_empty() {
                div { class: "tool-call-stack",
                    for call in calls {
                        ToolCallCard {
                            key: "tool-{call.id.clone().unwrap_or_else(|| call.name.clone())}",
                            call,
                        }
                    }
                }
            }
            div { class: "compact-turn-footer",
                if let Some(model) = &item.turn.model_name { span { "model {model}" } }
                if let Some(call_id) = &item.call_id { span { "call {short_id(call_id)}" } }
                if let Some(latency) = item.turn.latency_ms { span { "{latency} ms" } }
                button {
                    class: "inspect-turn",
                    onclick: move |event| { event.stop_propagation(); on_select.call(index); },
                    "Inspect evidence"
                }
            }
        }
    } }
}

#[component]
fn ToolCallCard(call: DisplayToolCall) -> Element {
    let tone = tool_tone(&call.name);
    let tone_class = tone.class();
    let command =
        argument_text(&call.arguments, "command").or_else(|| argument_text(&call.arguments, "cmd"));
    let code = argument_text(&call.arguments, "code");
    let path = argument_text(&call.arguments, "path")
        .or_else(|| argument_text(&call.arguments, "file_path"));
    let url = argument_text(&call.arguments, "url");
    let query = argument_text(&call.arguments, "query");
    let thought = argument_text(&call.arguments, "thought");
    let recognized = [
        "command",
        "cmd",
        "code",
        "path",
        "file_path",
        "url",
        "query",
        "thought",
    ];
    let remaining = call
        .arguments
        .as_object()
        .map(|arguments| {
            arguments
                .iter()
                .filter(|(key, _)| !recognized.contains(&key.as_str()))
                .map(|(key, value)| (key.clone(), format_json(value)))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let fallback = command.is_none()
        && code.is_none()
        && path.is_none()
        && url.is_none()
        && query.is_none()
        && thought.is_none()
        && remaining.is_empty();
    rsx! {
        section { class: "tool-call-card {tone_class}",
            header { class: "tool-call-header",
                span { class: "tool-call-icon", "{tone.icon()}" }
                div {
                    span { class: "tool-call-type", "{tone.label()} call" }
                    strong { "{call.name}" }
                }
                if let Some(id) = &call.id { code { title: "{id}", "{short_id(id)}" } }
            }
            if let Some(command) = command {
                pre { class: "tool-command", span { "$ " } "{command}" }
            }
            if let Some(code) = code {
                div { class: "tool-code-label", "Python code" }
                pre { class: "tool-code", "{code}" }
            }
            if let Some(path) = path {
                div { class: "tool-primary-arg", span { "Path" } code { "{path}" } }
            }
            if let Some(url) = url {
                div { class: "tool-primary-arg", span { "URL" } a { href: "{url}", target: "_blank", rel: "noreferrer", "{url}" } }
            }
            if let Some(query) = query {
                div { class: "tool-primary-arg", span { "Query" } code { "{query}" } }
            }
            if let Some(thought) = thought {
                div { class: "tool-thought", "{thought}" }
            }
            if !remaining.is_empty() {
                dl { class: "tool-arguments",
                    for (key, value) in remaining { div { dt { "{key}" } dd { pre { "{value}" } } } }
                }
            }
            if fallback {
                pre { class: "tool-command generic-arguments", "{format_json(&call.arguments)}" }
            }
        }
    }
}

#[component]
fn Inspector(
    item: Option<TurnView>,
    events: Vec<EventRecord>,
    on_close: EventHandler<()>,
) -> Element {
    let mut tab = use_signal(|| "summary".to_string());
    let related = item
        .as_ref()
        .map(|item| {
            events
                .iter()
                .filter(|event| item.event_seqs.contains(&event.seq))
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    rsx! { aside { class: "inspector",
        div { class: "inspector-header", div { p { class: "eyebrow", "Evidence inspector" } h3 { if let Some(item) = &item { "Turn #{item.turn.id}" } else { "Nothing selected" } } } button { class: "icon-button", aria_label: "Close inspector", onclick: move |_| on_close.call(()), "×" } }
        if let Some(item) = item {
            div { class: "inspector-tabs",
                button { class: if tab() == "summary" { "active" } else { "" }, onclick: move |_| tab.set("summary".into()), "Summary" }
                button { class: if tab() == "raw" { "active" } else { "" }, onclick: move |_| tab.set("raw".into()), "Raw events ({related.len()})" }
            }
            div { class: "inspector-body",
                if tab() == "summary" {
                    InspectorField { label: "Source", value: item.turn.source.clone() }
                    InspectorField { label: "Kind", value: item.turn.kind.clone().unwrap_or_else(|| "—".into()) }
                    InspectorField { label: "Timestamp", value: item.turn.timestamp.clone().unwrap_or_else(|| "Not captured".into()) }
                    InspectorField { label: "Call ID", value: item.call_id.clone().unwrap_or_else(|| "Not assigned".into()) }
                    InspectorField { label: "Event seqs", value: item.event_seqs.iter().map(u64::to_string).collect::<Vec<_>>().join(", ") }
                    if let Some(latency) = item.turn.latency_ms { InspectorField { label: "Latency", value: format!("{latency} ms") } }
                    if let Some(ttft) = item.turn.ttft_ms { InspectorField { label: "TTFT", value: format!("{ttft} ms") } }
                    if let Some(metrics) = &item.turn.metrics { div { class: "inspector-code", span { "Metrics" } pre { "{format_json(metrics)}" } } }
                    if let Some(observation) = &item.turn.observation { div { class: "inspector-code", span { "Observation" } pre { "{format_json(observation)}" } } }
                } else if related.is_empty() {
                    EmptyState { title: "No direct event reference", detail: "This derived turn did not expose a stable seq or call ID." }
                } else {
                    for event in related {
                        RawEvent { event }
                    }
                }
            }
        } else {
            div { class: "inspector-empty", "Select a Storyline turn to inspect its canonical event references." }
        }
    } }
}

#[component]
fn RawEvent(event: EventRecord) -> Element {
    let timestamp = event.timestamp.as_deref().unwrap_or("no timestamp");
    let body = pretty_event(&event);
    rsx! { details { class: "raw-event", open: true,
        summary { span { class: "kind-dot" } strong { "#{event.seq} {event.kind}" } span { "{timestamp}" } }
        pre { "{body}" }
    } }
}

#[component]
fn InspectorField(label: String, value: String) -> Element {
    rsx! { div { class: "inspector-field", span { "{label}" } code { title: "{value}", "{value}" } } }
}

fn sql_literal(value: &str) -> String {
    value.replace('\'', "''")
}

fn path_filter_sql(database: &str, table: &str, value: &str, exact: bool) -> String {
    let normalized = if exact {
        value.to_string()
    } else {
        value.replace('*', "%").replace('?', "_")
    };
    let operator = if exact { "=" } else { "LIKE" };
    format!(
        "SELECT * FROM {database}.{table}\nWHERE _file_ {operator} '{}'\nLIMIT 100",
        sql_literal(&normalized)
    )
}

#[component]
fn ToolsWorkspace(catalog: Option<QueryCatalog>, selected_table: Signal<String>) -> Element {
    let mut sql_text = use_signal(String::new);
    let mut applied_table = use_signal(String::new);
    let mut path_filter = use_signal(String::new);
    let mut path_match = use_signal(|| "like".to_string());
    let mut output = use_signal(|| "Run the prepared query to load rows.".to_string());
    let mut busy = use_signal(|| false);
    let database = catalog
        .as_ref()
        .map(|catalog| catalog.database.clone())
        .unwrap_or_else(|| "data".into());
    let selected = selected_table();
    let table = catalog
        .as_ref()
        .and_then(|catalog| catalog.tables.iter().find(|table| table.name == selected))
        .cloned();
    let effect_database = database.clone();
    use_effect(move || {
        let table = selected_table();
        let key = format!("{effect_database}.{table}");
        if !table.is_empty() && applied_table() != key {
            sql_text.set(format!("SELECT * FROM {effect_database}.{table} LIMIT 100"));
            applied_table.set(key);
            path_filter.set(String::new());
            output.set("Run the prepared query to load rows.".into());
        }
    });
    rsx! { div { class: "tools-workspace",
        div { class: "workspace-header",
            div { class: "title-block", div { class: "breadcrumb", "pChronicle / Tools / {database}" } h2 { "Directory query workspace" } div { class: "header-meta", if let Some(catalog) = &catalog { code { "{catalog.storage_path}" } if !selected_table().is_empty() { span { class: "selected-table-chip", "▦ {database}.{selected_table}" } } } else { span { "Loading query catalog…" } } } }
        }
        div { class: "tools-grid",
            aside { class: "schema-panel",
                if let Some(table) = &table {
                    div { class: "schema-panel-heading", span { "Virtual table" } h3 { "{database}.{table.name}" } p { "{table.description}" } div { span { "Grain" } strong { "{table.grain}" } } }
                    div { class: "schema-field-heading", "Fields · {table.fields.len()}" }
                    div { class: "schema-field-list",
                        for field in &table.fields {
                            div { class: "schema-field",
                                div { code { "{field.name}" } span { "{field.data_type}" } }
                                p { "{field.description}" }
                            }
                        }
                    }
                } else {
                    EmptyState { title: "Loading schema", detail: "Choose a virtual table from the catalog." }
                }
            }
            section { class: "tool-surface",
                if catalog.is_none() { EmptyState { title: "Query catalog unavailable", detail: "The directory schema could not be loaded." } }
                else {
                    div { class: "tool-heading", h3 { "Read-only SQL" } p { "The directory is exposed as one database. Use qualified names such as {database}.runs, {database}.steps, and {database}.tool_calls." } }
                    div { class: "path-filter-card",
                        div { strong { "Path filter" } span { "Uses the virtual _file_ column" } }
                        div { class: "path-filter-controls",
                            select { value: "{path_match}", aria_label: "Path match type", onchange: move |event| path_match.set(event.value()), option { value: "like", "Wildcard (LIKE)" } option { value: "exact", "Exact path" } }
                            input { value: "{path_filter}", placeholder: "cybergym_*.json or batch/%", aria_label: "Source path filter", oninput: move |event| path_filter.set(event.value()) }
                            button { class: "button", disabled: path_filter().trim().is_empty(), onclick: { let database = database.clone(); move |_| { sql_text.set(path_filter_sql(&database, &selected_table(), path_filter().trim(), path_match() == "exact")); } }, "Apply" }
                            button { class: "button", onclick: { let database = database.clone(); move |_| { path_filter.set(String::new()); sql_text.set(format!("SELECT * FROM {}.{} LIMIT 100", database, selected_table())); } }, "Clear" }
                        }
                        div { class: "path-filter-examples", span { "Examples" } button { onclick: move |_| path_filter.set("cybergym_*.json".into()), "cybergym_*.json" } button { onclick: move |_| path_filter.set("%scientific-computing.json".into()), "%scientific-computing.json" } }
                    }
                    textarea { class: "sql-editor", value: "{sql_text}", oninput: move |event| sql_text.set(event.value()) }
                    div { class: "query-actions",
                        button { class: "button primary", disabled: busy(), onclick: move |_| { let query = sql_text(); busy.set(true); spawn(async move { output.set(api::sql(&query).await.unwrap_or_else(|e| e)); busy.set(false); }); }, if busy() { "Running…" } else { "Run query" } }
                        span { "SELECT, WITH, and EXPLAIN only · results are NDJSON" }
                    }
                    div { class: "tool-output", div { span { "Output" } button { class: "icon-button", aria_label: "Clear output", onclick: move |_| output.set("Output cleared.".into()), "×" } } pre { "{output}" } }
                }
            }
        }
    } }
}

#[cfg(test)]
mod ui_tests {
    use super::{compact_preview, path_filter_sql, tool_narrative, tool_tone, ToolTone};

    #[test]
    fn preview_is_single_line_and_bounded() {
        assert_eq!(compact_preview(" hello\n  world ", 40), "hello world");
        assert_eq!(compact_preview("abcdefgh", 5), "abcde…");
        assert_eq!(compact_preview(" \n ", 5), "No text response");
    }

    #[test]
    fn tool_types_are_classified_by_action() {
        assert_eq!(tool_tone("execute_bash"), ToolTone::Shell);
        assert_eq!(tool_tone("execute_ipython_cell"), ToolTone::Code);
        assert_eq!(tool_tone("write_file"), ToolTone::File);
        assert_eq!(tool_tone("browser_navigate"), ToolTone::Web);
        assert_eq!(tool_tone("think"), ToolTone::Reasoning);
        assert_eq!(tool_tone("custom_lookup"), ToolTone::Generic);
    }

    #[test]
    fn narrative_hides_embedded_tool_protocol() {
        assert_eq!(
            tool_narrative("I will inspect it.\n<tool_call>execute_bash <parameter=command>ls"),
            "I will inspect it."
        );
        assert_eq!(tool_narrative("<function=execute_bash>{}</function>"), "");
    }

    #[test]
    fn path_filter_supports_shell_and_sql_wildcards() {
        assert_eq!(
            path_filter_sql("data", "steps", "batch/*.json", false),
            "SELECT * FROM data.steps\nWHERE _file_ LIKE 'batch/%.json'\nLIMIT 100"
        );
        assert!(
            path_filter_sql("data", "runs", "it's.json", true).contains("_file_ = 'it''s.json'")
        );
    }
}
