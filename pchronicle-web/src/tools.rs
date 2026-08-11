use dioxus::prelude::*;

use crate::api;
use crate::components::DataTable;
use crate::model::{QueryCatalog, QueryEvidence};

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
pub fn ToolsWorkspace(
    catalog: Option<QueryCatalog>,
    mut selected_table: Signal<String>,
) -> Element {
    let mut sql_text = use_signal(String::new);
    let mut applied_table = use_signal(String::new);
    let mut path_filter = use_signal(String::new);
    let mut path_match = use_signal(|| "like".to_string());
    let mut output = use_signal(|| None::<Result<QueryEvidence, String>>);
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
            output.set(None);
        }
    });
    rsx! { div { class: "tools-workspace",
        div { class: "workspace-header",
            div { class: "title-block", div { class: "breadcrumb", "pChronicle / Analyze / {database}" } h2 { "Directory query workspace" } div { class: "header-meta", if let Some(catalog) = &catalog { code { "{catalog.storage_path}" } } else { span { "Loading query catalog…" } } } }
        }
        div { class: "tools-grid",
            aside { class: "schema-panel",
                if let Some(catalog) = &catalog {
                    div { class: "schema-table-heading", div { span { "Virtual tables" } strong { "{catalog.tables.len()}" } } p { "Select a table to inspect its queryable columns." } }
                    nav { class: "schema-table-list", aria_label: "Queryable virtual tables",
                        for candidate in &catalog.tables {
                            button { class: if candidate.name == selected { "active" } else { "" }, aria_current: if candidate.name == selected { "true" } else { "false" }, onclick: { let name = candidate.name.clone(); move |_| selected_table.set(name.clone()) },
                                div { code { "{database}.{candidate.name}" } span { "{candidate.fields.len()} fields" } }
                                p { "{candidate.grain}" }
                            }
                        }
                    }
                }
                if let Some(table) = &table {
                    div { class: "schema-panel-heading selected-schema", span { "Selected schema" } h3 { "{database}.{table.name}" } p { "{table.description}" } div { span { "Grain" } strong { "{table.grain}" } } }
                    div { class: "schema-field-heading", "Fields · {table.fields.len()}" }
                    div { class: "schema-field-list", for field in &table.fields { div { class: "schema-field", div { code { "{field.name}" } span { "{field.data_type}" } } p { "{field.description}" } } } }
                } else { ToolEmpty { title: "Loading schema", detail: "Choose a virtual table from the catalog." } }
            }
            section { class: "tool-surface",
                if catalog.is_none() { ToolEmpty { title: "Query catalog unavailable", detail: "The directory schema could not be loaded." } }
                else {
                    div { class: "tool-heading", h3 { "Read-only SQL" } p { "Use qualified tables such as {database}.runs, {database}.steps, and {database}.tool_calls." } }
                    div { class: "path-filter-card",
                        div { strong { "Path filter" } span { "Uses the virtual _file_ column" } }
                        div { class: "path-filter-controls",
                            select { value: "{path_match}", aria_label: "Path match type", onchange: move |event| path_match.set(event.value()), option { value: "like", "Wildcard (LIKE)" } option { value: "exact", "Exact path" } }
                            input { value: "{path_filter}", placeholder: "cybergym_*.json or batch/%", aria_label: "Source path filter", oninput: move |event| path_filter.set(event.value()) }
                            button { class: "button", disabled: path_filter().trim().is_empty(), onclick: { let database = database.clone(); move |_| sql_text.set(path_filter_sql(&database, &selected_table(), path_filter().trim(), path_match() == "exact")) }, "Apply" }
                            button { class: "button", onclick: { let database = database.clone(); move |_| { path_filter.set(String::new()); sql_text.set(format!("SELECT * FROM {}.{} LIMIT 100", database, selected_table())); } }, "Clear" }
                        }
                    }
                    textarea { class: "sql-editor", value: "{sql_text}", oninput: move |event| sql_text.set(event.value()) }
                    div { class: "query-actions",
                        button { class: "button primary", disabled: busy(), onclick: move |_| { let query = sql_text(); busy.set(true); spawn(async move { output.set(Some(api::query_evidence_interactive(&query).await)); busy.set(false); }); }, if busy() { "Running…" } else { "Run query" } }
                        span { "SELECT, WITH, and EXPLAIN only · bounded structured results" }
                    }
                    div { class: "tool-output pc2-tool-result", div { span { "Output" } button { class: "icon-button", aria_label: "Clear output", onclick: move |_| output.set(None), "×" } }
                        if let Some(result) = output() {
                            match result {
                                Ok(evidence) => rsx! { DataTable { evidence, title: Some("SQL query result".into()) } },
                                Err(message) => rsx! { div { class: "pc2-query-error", "{message}" } },
                            }
                        } else { div { class: "pc2-query-placeholder", "Run the prepared query to load a bounded table preview." } }
                    }
                }
            }
        }
    } }
}

#[component]
fn ToolEmpty(title: &'static str, detail: &'static str) -> Element {
    rsx! { div { class: "empty-state", div { class: "empty-icon", "◇" } strong { "{title}" } p { "{detail}" } } }
}

#[cfg(test)]
mod tests {
    use super::*;

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
