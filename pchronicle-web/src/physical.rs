use dioxus::prelude::*;
use wasm_bindgen::JsValue;

use crate::api;
use crate::model::{
    PhysicalFileLayout, PhysicalLayout, PhysicalPagePreview, PhysicalSource, PhysicalTable,
};

#[component]
pub fn PhysicalWorkspace() -> Element {
    let mut sources = use_signal(Vec::<PhysicalSource>::new);
    let mut layout = use_signal(|| None::<PhysicalLayout>);
    let mut file_layout = use_signal(|| None::<PhysicalFileLayout>);
    let mut preview = use_signal(|| None::<PhysicalPagePreview>);
    let mut error = use_signal(|| None::<String>);
    let mut loading_sources = use_signal(|| true);
    let mut loading_layout = use_signal(|| false);
    let mut loading_file = use_signal(|| false);
    let mut loading_preview = use_signal(|| false);

    let mut dataset = use_signal(|| url_param("dataset").unwrap_or_default());
    let mut file = use_signal(|| url_param("file").unwrap_or_default());
    let mut table = use_signal(|| url_param("table").unwrap_or_default());
    let mut fragment = use_signal(|| url_param("fragment").and_then(|value| value.parse().ok()));
    let mut data_file = use_signal(|| url_param("data_file").unwrap_or_default());
    let mut column = use_signal(|| url_param("column").unwrap_or_default());
    let mut data_page = use_signal(|| {
        url_param("data_page")
            .and_then(|value| value.parse().ok())
            .unwrap_or(0u32)
    });

    use_effect(move || {
        spawn(async move {
            match api::physical_sources().await {
                Ok(value) => {
                    if dataset().is_empty() {
                        if let Some(source) = value.first() {
                            dataset.set(source.dataset.clone());
                            file.set(source.file.clone());
                        }
                    }
                    sources.set(value);
                    error.set(None);
                }
                Err(message) => error.set(Some(message)),
            }
            loading_sources.set(false);
        });
    });

    use_effect(move || {
        let dataset = dataset();
        let file = file();
        if dataset.is_empty() || file.is_empty() {
            layout.set(None);
            return;
        }
        loading_layout.set(true);
        spawn(async move {
            match api::physical_layout(&dataset, &file).await {
                Ok(value) => {
                    if table().is_empty() {
                        if let Some((next_table, next_fragment, next_file)) =
                            first_data_file(&value.tables)
                        {
                            table.set(next_table);
                            fragment.set(Some(next_fragment));
                            data_file.set(next_file);
                        }
                    }
                    layout.set(Some(value));
                    error.set(None);
                }
                Err(message) => {
                    layout.set(None);
                    error.set(Some(message));
                }
            }
            loading_layout.set(false);
        });
    });

    use_effect(move || {
        let dataset = dataset();
        let file = file();
        let table = table();
        let Some(fragment_id) = fragment() else {
            file_layout.set(None);
            return;
        };
        let data_file = data_file();
        if dataset.is_empty() || file.is_empty() || table.is_empty() || data_file.is_empty() {
            file_layout.set(None);
            return;
        }
        loading_file.set(true);
        spawn(async move {
            match api::physical_file(&dataset, &file, &table, fragment_id, &data_file).await {
                Ok(value) => {
                    if column().is_empty() {
                        if let Some(next) = value.columns.first() {
                            column.set(next.name.clone());
                            data_page.set(next.pages.first().map(|page| page.index).unwrap_or(0));
                        }
                    }
                    file_layout.set(Some(value));
                    error.set(None);
                }
                Err(message) => {
                    file_layout.set(None);
                    error.set(Some(message));
                }
            }
            loading_file.set(false);
        });
    });

    use_effect(move || {
        let dataset = dataset();
        let file = file();
        let table = table();
        let Some(fragment_id) = fragment() else {
            preview.set(None);
            return;
        };
        let data_file = data_file();
        let column = column();
        if dataset.is_empty()
            || file.is_empty()
            || table.is_empty()
            || data_file.is_empty()
            || column.is_empty()
        {
            preview.set(None);
            return;
        }
        loading_preview.set(true);
        spawn(async move {
            match api::physical_page(
                &dataset,
                &file,
                &table,
                fragment_id,
                &data_file,
                Some(&column),
                0,
                32,
            )
            .await
            {
                Ok(value) => {
                    preview.set(Some(value));
                    error.set(None);
                }
                Err(message) => {
                    preview.set(None);
                    error.set(Some(message));
                }
            }
            loading_preview.set(false);
        });
    });

    use_effect(move || {
        sync_physical_url(
            &dataset(),
            &file(),
            &table(),
            fragment(),
            &data_file(),
            &column(),
            data_page(),
        );
    });

    let selected_source = sources().into_iter().find(|source| {
        source.dataset == dataset() && source.file == file()
    });
    let file_size_label = file_layout()
        .as_ref()
        .map(|current| format_bytes(current.file_size_bytes))
        .unwrap_or_default();
    let file_rows_label = file_layout()
        .as_ref()
        .map(|current| {
            current
                .num_rows
                .map(|rows| format!("{rows} rows"))
                .unwrap_or_else(|| "rows unknown".to_string())
        })
        .unwrap_or_default();

    rsx! {
        section { class: "physical-workspace", "aria-label": "Physical Lance inspector",
            aside { class: "physical-tree",
                header {
                    strong { "Lance sources" }
                    span { "{sources().len()}" }
                }
                if loading_sources() {
                    div { class: "physical-empty", "Loading Catalog sources…" }
                } else if sources().is_empty() {
                    div { class: "physical-empty",
                        strong { "No Lance sources" }
                        span { "Physical inspects Catalog Lance datasets. JSON and other files stay in Data / Runs." }
                    }
                } else {
                    div { class: "physical-tree-list",
                        for source in sources() {
                            {
                                let selected = source.dataset == dataset() && source.file == file();
                                let source_dataset = source.dataset.clone();
                                let source_file = source.file.clone();
                                rsx! {
                                    button {
                                        class: if selected { "physical-tree-item active" } else { "physical-tree-item" },
                                        onclick: move |_| {
                                            dataset.set(source_dataset.clone());
                                            file.set(source_file.clone());
                                            table.set(String::new());
                                            fragment.set(None);
                                            data_file.set(String::new());
                                            column.set(String::new());
                                            data_page.set(0);
                                            file_layout.set(None);
                                            preview.set(None);
                                        },
                                        div {
                                            strong { "{source.dataset}/{source.file}" }
                                            span { "{source.format}" }
                                        }
                                        code { {format_bytes(source.size_bytes)} }
                                    }
                                }
                            }
                        }
                    }
                    if let Some(current) = layout() {
                        div { class: "physical-tree-list nested",
                            for table_layout in current.tables.clone() {
                                PhysicalTableBranch {
                                    table: table_layout,
                                    selected_table: table(),
                                    selected_fragment: fragment(),
                                    selected_data_file: data_file(),
                                    on_file: move |(next_table, next_fragment, next_file): (String, u64, String)| {
                                        table.set(next_table);
                                        fragment.set(Some(next_fragment));
                                        data_file.set(next_file);
                                        column.set(String::new());
                                        data_page.set(0);
                                        preview.set(None);
                                    },
                                }
                            }
                        }
                    } else if loading_layout() {
                        div { class: "physical-empty", "Reading table fragments…" }
                    }
                }
            }
            div { class: "physical-detail",
                header { class: "physical-detail-head",
                    div {
                        p { class: "eyebrow", "Physical" }
                        h1 {
                            if let Some(source) = selected_source.as_ref() {
                                "{source.dataset}/{source.file}"
                            } else {
                                "Select a Lance source"
                            }
                        }
                        p {
                            if let Some(source) = selected_source.as_ref() {
                                "{source.uri}"
                            } else {
                                "Source → fragment → data file, then sample a column page."
                            }
                        }
                    }
                    if let Some(current) = file_layout() {
                        span { class: "physical-chip", "{current.columns.len()} columns" }
                    }
                }
                if let Some(message) = error() {
                    div { class: "physical-error", "{message}" }
                }
                if let Some(current) = file_layout() {
                    div { class: "physical-layout",
                        div { class: "physical-layout-meta",
                            strong { "{current.table} · fragment {current.fragment_id}" }
                            span { "{current.data_file}" }
                            span { "{file_size_label} · {file_rows_label}" }
                        }
                        for column_layout in current.columns.clone() {
                            {
                                let selected_column = column();
                                let selected_page = data_page();
                                let total = column_layout.pages.iter().map(|page| page.size.max(1)).sum::<u64>().max(1);
                                rsx! {
                                    div { class: "physical-column",
                                        div { class: "physical-column-label",
                                            strong { "{column_layout.name}" }
                                            span { "field {column_layout.field_id}" }
                                        }
                                        div { class: "physical-page-strip",
                                            for page in column_layout.pages.clone() {
                                                {
                                                    let name = column_layout.name.clone();
                                                    let index = page.index;
                                                    let active = selected_column == name && selected_page == index;
                                                    let flex = ((page.size.max(1) * 100) / total).max(8);
                                                    rsx! {
                                                        button {
                                                            class: if active { "physical-page active" } else { "physical-page" },
                                                            style: "flex: {flex} 1 48px",
                                                            title: "{page.encoding} · {format_bytes(Some(page.size))}",
                                                            onclick: move |_| {
                                                                column.set(name.clone());
                                                                data_page.set(index);
                                                            },
                                                            span { "p{page.index}" }
                                                            small { "{format_bytes(Some(page.size))}" }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        if current.remaining_columns > 0 {
                            p { class: "physical-note", "{current.remaining_columns} more columns are hidden in this first-page inspector." }
                        }
                    }
                } else if loading_file() {
                    div { class: "physical-empty", "Reading data file layout…" }
                } else if !data_file().is_empty() {
                    div { class: "physical-empty", "No layout for this data file." }
                } else {
                    div { class: "physical-empty",
                        strong { "Choose a data file" }
                        span { "The strip shows each column as pages. Click a page to sample values." }
                    }
                }
                div { class: "physical-preview",
                    header {
                        strong { "Page sample" }
                        if let Some(current) = preview() {
                            span {
                                "offset {current.offset} · {current.rows.len()} rows"
                                if current.truncated { " · truncated" }
                            }
                        }
                    }
                    if loading_preview() {
                        div { class: "physical-empty", "Loading page values…" }
                    } else if let Some(current) = preview() {
                        div { class: "physical-preview-table-wrap",
                            table { class: "physical-preview-table",
                                thead {
                                    tr {
                                        for name in current.columns.clone() {
                                            th { "{name}" }
                                        }
                                    }
                                }
                                tbody {
                                    for row in current.rows.clone() {
                                        tr {
                                            for cell in row {
                                                td { "{cell}" }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    } else {
                        div { class: "physical-empty", "Select a column page to preview values." }
                    }
                }
            }
        }
    }
}

#[component]
fn PhysicalTableBranch(
    table: PhysicalTable,
    selected_table: String,
    selected_fragment: Option<u64>,
    selected_data_file: String,
    on_file: EventHandler<(String, u64, String)>,
) -> Element {
    rsx! {
        div { class: "physical-branch",
            div { class: "physical-branch-label",
                strong { "{table.name}" }
                span { "{table.num_rows} rows · v{table.version}" }
            }
            for fragment in table.fragments.clone() {
                div { class: "physical-branch",
                    div { class: "physical-branch-label muted",
                        span { "fragment {fragment.id}" }
                        span { {fragment.physical_rows.map(|rows| format!("{rows} physical rows")).unwrap_or_else(|| "rows unknown".into())} }
                    }
                    for data in fragment.files.clone() {
                        {
                            let active = selected_table == table.name
                                && selected_fragment == Some(fragment.id)
                                && selected_data_file == data.path;
                            let table_name = table.name.clone();
                            let fragment_id = fragment.id;
                            let path = data.path.clone();
                            rsx! {
                                button {
                                    class: if active { "physical-tree-item nested active" } else { "physical-tree-item nested" },
                                    onclick: move |_| on_file.call((table_name.clone(), fragment_id, path.clone())),
                                    div {
                                        strong { "{data.path}" }
                                        span { "{data.encoding} · {data.field_names.len()} fields" }
                                    }
                                    code { {format_bytes(data.size_bytes)} }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn first_data_file(tables: &[PhysicalTable]) -> Option<(String, u64, String)> {
    let table = tables.first()?;
    let fragment = table.fragments.first()?;
    let data = fragment.files.first()?;
    Some((table.name.clone(), fragment.id, data.path.clone()))
}

fn format_bytes(bytes: Option<u64>) -> String {
    match bytes {
        None => "-".into(),
        Some(value) if value < 1024 => format!("{value} B"),
        Some(value) if value < 1024 * 1024 => format!("{:.1} KB", value as f64 / 1024.0),
        Some(value) => format!("{:.1} MB", value as f64 / (1024.0 * 1024.0)),
    }
}

fn url_param(name: &str) -> Option<String> {
    let search = web_sys::window()?.location().search().ok()?;
    web_sys::UrlSearchParams::new_with_str(&search)
        .ok()?
        .get(name)
}

pub fn physical_workspace_url(
    dataset: &str,
    file: &str,
    table: &str,
    fragment: Option<u64>,
    data_file: &str,
    column: &str,
    data_page: u32,
) -> String {
    let mut params = vec!["page=physical".to_string()];
    if !dataset.is_empty() {
        params.push(format!("dataset={}", urlencoding::encode(dataset)));
    }
    if !file.is_empty() {
        params.push(format!("file={}", urlencoding::encode(file)));
    }
    if !table.is_empty() {
        params.push(format!("table={}", urlencoding::encode(table)));
    }
    if let Some(fragment) = fragment {
        params.push(format!("fragment={fragment}"));
    }
    if !data_file.is_empty() {
        params.push(format!("data_file={}", urlencoding::encode(data_file)));
    }
    if !column.is_empty() {
        params.push(format!("column={}", urlencoding::encode(column)));
    }
    if data_page > 0 {
        params.push(format!("data_page={data_page}"));
    }
    format!("/?{}", params.join("&"))
}

fn sync_physical_url(
    dataset: &str,
    file: &str,
    table: &str,
    fragment: Option<u64>,
    data_file: &str,
    column: &str,
    data_page: u32,
) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let url = physical_workspace_url(dataset, file, table, fragment, data_file, column, data_page);
    let _ = window
        .history()
        .and_then(|history| history.replace_state_with_url(&JsValue::NULL, "", Some(&url)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn physical_workspace_url_keeps_catalog_and_page_coordinates() {
        assert_eq!(
            physical_workspace_url(
                "dataset",
                "story",
                "runs",
                Some(1),
                "data/a.lance",
                "session_id",
                0
            ),
            "/?page=physical&dataset=dataset&file=story&table=runs&fragment=1&data_file=data%2Fa.lance&column=session_id"
        );
    }
}
