use dioxus::prelude::*;

use crate::analysis::AnalysisWorkspace;
use crate::analysis_session::AnalysisScope;
use crate::model::QueryCatalog;

#[component]
pub fn ToolsWorkspace(catalog: Option<QueryCatalog>, selected_table: Signal<String>) -> Element {
    let _selected_table = selected_table();
    let initial_scope = catalog.as_ref().map(AnalysisScope::from_catalog);
    rsx! {
        AnalysisWorkspace {
            catalog,
            initial_scope,
            requested_session_id: None,
            on_session_change: move |_session_id: String| {},
        }
    }
}
