use dioxus::prelude::*;
use wasm_bindgen::JsValue;

use crate::api;
use crate::notice::{WorkspaceNotice, workspace_notice};
use crate::model::{
    PhysicalBucket, PhysicalColumn, PhysicalFileLayout, PhysicalFragment, PhysicalLayout,
    PhysicalPagePreview, PhysicalSource, PhysicalTable,
};

const PHYSICAL_PREVIEW_LIMIT: usize = 32;

#[component]
pub fn PhysicalWorkspace() -> Element {
    let mut sources = use_signal(Vec::<PhysicalSource>::new);
    let mut layout = use_signal(|| None::<PhysicalLayout>);
    let mut file_layout = use_signal(|| None::<PhysicalFileLayout>);
    let mut preview = use_signal(|| None::<PhysicalPagePreview>);
    let mut error = use_signal(|| None::<WorkspaceNotice>);
    let mut loading_sources = use_signal(|| true);
    let mut loading_layout = use_signal(|| false);
    let mut loading_file = use_signal(|| false);
    let mut loading_preview = use_signal(|| false);
    let mut drawer_open = use_signal(|| false);
    let mut review_max = use_signal(|| false);

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
    let mut preview_offset = use_signal(|| {
        url_param("preview_offset")
            .and_then(|value| value.parse().ok())
            .unwrap_or(0usize)
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
                Err(failure) => error.set(Some(workspace_notice(&failure))),
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
                Err(failure) => {
                    layout.set(None);
                    error.set(Some(workspace_notice(&failure)));
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
                            preview_offset.set(0);
                        }
                    }
                    file_layout.set(Some(value));
                    error.set(None);
                }
                Err(failure) => {
                    file_layout.set(None);
                    error.set(Some(workspace_notice(&failure)));
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
        let preview_offset = preview_offset();
        if !drawer_open()
            || dataset.is_empty()
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
                preview_offset,
                PHYSICAL_PREVIEW_LIMIT,
            )
            .await
            {
                Ok(value) => {
                    preview.set(Some(value));
                    error.set(None);
                }
                Err(failure) => {
                    preview.set(None);
                    error.set(Some(workspace_notice(&failure)));
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
            preview_offset(),
        );
    });

    let selected_source = sources()
        .into_iter()
        .find(|source| source.dataset == dataset() && source.file == file());
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
    let selected_table_layout = layout().and_then(|current| {
        let name = table();
        current.tables.into_iter().find(|item| item.name == name)
    });
    let fragment_count = selected_table_layout
        .as_ref()
        .map(|item| item.fragments.len())
        .unwrap_or(0);

    rsx! {
        section { class: "physical-workspace", "aria-label": "Lance storage details",
            aside { class: "physical-tree",
                header {
                    strong { "Lance sources" }
                    span { "{sources().len()}" }
                }
                if loading_sources() {
                    div { class: "physical-empty", "Loading source files…" }
                } else if sources().is_empty() {
                    div { class: "physical-empty",
                        strong { "No Lance sources" }
                        span { "Storage details are available for Lance datasets. Browse JSON and other files under Datasets or Runs." }
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
                                            preview_offset.set(0);
                                            file_layout.set(None);
                                            preview.set(None);
                                            drawer_open.set(false);
                                            review_max.set(false);
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
                                        preview_offset.set(0);
                                        preview.set(None);
                                        drawer_open.set(false);
                                        review_max.set(false);
                                    },
                                }
                            }
                        }
                    } else if loading_layout() {
                        div { class: "physical-empty", "Reading data groups…" }
                    }
                }
            }
            div { class: "physical-detail",
                header { class: "physical-detail-head",
                    div {
                        p { class: "eyebrow", "Storage" }
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
                                "Select a source, data group, file, and column to inspect stored values."
                            }
                        }
                    }
                    div { class: "physical-chips",
                        if fragment_count > 0 {
                            span { class: "physical-chip", "{fragment_count} data groups" }
                        }
                        if let Some(current) = file_layout() {
                            span { class: "physical-chip", "{current.columns.len()} columns" }
                        }
                    }
                }
                if let Some(notice) = error() {
                    div { class: "pc2-workspace-notice", role: "alert",
                        div { class: "pc2-workspace-notice-copy",
                            strong { "{notice.title}" }
                            span { "{notice.summary}" }
                            if !notice.action.is_empty() {
                                span { "{notice.action}" }
                            }
                            if let Some(request_id) = notice.request_id.as_ref() {
                                p { class: "pc2-workspace-notice-request",
                                    "Request ID "
                                    code { "{request_id}" }
                                }
                            }
                            details { class: "pc2-workspace-notice-details",
                                summary { "Show technical details" }
                                pre { "{notice.detail}" }
                            }
                        }
                        button { aria_label: "Dismiss", onclick: move |_| error.set(None), "×" }
                    }
                }
                if let Some(current_table) = selected_table_layout.clone() {
                    PhysicalFragmentStrip {
                        table: current_table,
                        selected_fragment: fragment(),
                        on_file: move |(next_table, next_fragment, next_file): (String, u64, String)| {
                            table.set(next_table);
                            fragment.set(Some(next_fragment));
                            data_file.set(next_file);
                            column.set(String::new());
                            data_page.set(0);
                            preview_offset.set(0);
                            preview.set(None);
                            drawer_open.set(false);
                            review_max.set(false);
                        },
                    }
                }
                if let Some(current) = file_layout() {
                    div { class: "physical-layout",
                        div { class: "physical-layout-meta",
                            strong { "{current.table} · data group {current.fragment_id}" }
                            span { "{current.data_file}" }
                            span { "{file_size_label} · {file_rows_label}" }
                        }
                        for column_layout in current.columns.clone() {
                            PhysicalColumnCard {
                                column: column_layout,
                                selected_column: column(),
                                selected_page: data_page(),
                                on_open: move |(name, page): (String, u32)| {
                                    column.set(name);
                                    data_page.set(page);
                                    preview_offset.set(0);
                                    review_max.set(false);
                                    drawer_open.set(true);
                                },
                                on_review: move |(name, page): (String, u32)| {
                                    column.set(name);
                                    data_page.set(page);
                                    preview_offset.set(0);
                                    review_max.set(true);
                                    drawer_open.set(true);
                                },
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
                } else if selected_table_layout.is_none() {
                    div { class: "physical-empty",
                        strong { "Choose a data group" }
                        span { "The strip shows each data group's rows and size. Select a group or file, then select a column to inspect sample values." }
                    }
                }
                if drawer_open() {
                    PhysicalSampleDrawer {
                        column_name: column(),
                        stats: file_layout().and_then(|current| {
                            current.columns.into_iter().find(|item| item.name == column())
                        }),
                        preview: preview(),
                        loading: loading_preview(),
                        review_max: review_max(),
                        on_page: move |offset: usize| preview_offset.set(offset),
                        on_close: move |_| {
                            drawer_open.set(false);
                            review_max.set(false);
                        },
                    }
                }
            }
        }
    }
}

#[component]
fn PhysicalColumnCard(
    column: PhysicalColumn,
    selected_column: String,
    selected_page: u32,
    on_open: EventHandler<(String, u32)>,
    on_review: EventHandler<(String, u32)>,
) -> Element {
    let selected = selected_column == column.name;
    let total = column
        .pages
        .iter()
        .map(|page| page.size.max(1))
        .sum::<u64>()
        .max(1);
    let first_page = column.pages.first().map(|page| page.index).unwrap_or(0);
    let open_name = column.name.clone();
    let review_name = column.name.clone();
    rsx! {
        div { class: if selected { "physical-column selected" } else { "physical-column" },
            button {
                class: "physical-column-head",
                onclick: move |_| on_open.call((open_name.clone(), first_page)),
                div { class: "physical-column-label",
                    strong { "{column.name}" }
                    span { {column_type_label(&column)} }
                }
                span { class: "physical-column-counts", {column_count_label(&column)} }
            }
            div { class: "physical-column-stats",
                span { {storage_label(&column)} }
                if let Some(max_value) = column.max_value.clone() {
                    button {
                        class: "physical-max-link",
                        title: "{max_value.preview}",
                        onclick: move |_| on_review.call((review_name.clone(), first_page)),
                        "largest {format_bytes(Some(max_value.size_bytes))} · review"
                    }
                }
            }
            PhysicalDistributionStrip { label: "values", buckets: column.value_distribution.clone() }
            PhysicalDistributionStrip { label: "size", buckets: column.size_distribution.clone() }
            div { class: "physical-page-strip",
                for page in column.pages.clone() {
                    {
                        let name = column.name.clone();
                        let index = page.index;
                        let active = selected && selected_page == index;
                        let flex = ((page.size.max(1) * 100) / total).max(8);
                        rsx! {
                            button {
                                class: if active { "physical-page active" } else { "physical-page" },
                                style: "flex: {flex} 1 48px",
                                title: "{page.encoding} · {format_bytes(Some(page.size))}",
                                onclick: move |_| on_open.call((name.clone(), index)),
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

#[component]
fn PhysicalDistributionStrip(label: &'static str, buckets: Vec<PhysicalBucket>) -> Element {
    if buckets.is_empty() {
        return rsx! { span { class: "physical-dist-empty", "{label}: n/a" } };
    }
    let total = buckets
        .iter()
        .map(|bucket| bucket.weight.max(1))
        .sum::<u64>()
        .max(1);
    rsx! {
        div { class: "physical-dist",
            span { class: "physical-dist-label", "{label}" }
            div { class: "physical-dist-strip",
                for bucket in buckets {
                    {
                        let flex = ((bucket.weight.max(1) * 100) / total).max(6);
                        rsx! {
                            span {
                                class: "physical-dist-bar",
                                style: "flex: {flex} 1 12px",
                                title: "{bucket.label} · {bucket.count}",
                                "{bucket.label}"
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn PhysicalSampleDrawer(
    column_name: String,
    stats: Option<PhysicalColumn>,
    preview: Option<PhysicalPagePreview>,
    loading: bool,
    review_max: bool,
    on_page: EventHandler<usize>,
    on_close: EventHandler<()>,
) -> Element {
    let page_offset = preview.as_ref().map(|page| page.offset).unwrap_or(0);
    let page_limit = preview
        .as_ref()
        .map(|page| page.limit.max(1))
        .unwrap_or(PHYSICAL_PREVIEW_LIMIT);
    let total_rows = stats
        .as_ref()
        .and_then(|column| usize::try_from(column.row_count).ok());
    let page_number = page_offset / page_limit + 1;
    let page_count = total_rows
        .map(|total| total.div_ceil(page_limit).max(1));
    let can_previous = page_offset >= page_limit && !loading;
    let can_next = !loading
        && preview.as_ref().is_some_and(|page| {
            page.rows.len() >= page.limit.max(1)
                && total_rows
                    .map(|total| page_offset.saturating_add(page.rows.len()) < total)
                    .unwrap_or(true)
        });

    rsx! {
        button { class: "physical-drawer-mask", onclick: move |_| on_close.call(()) }
        aside { class: "physical-drawer", "aria-label": "Column sample",
            header {
                div {
                    strong { "{column_name}" }
                    if let Some(stats) = stats.as_ref() {
                        span { {column_count_label(stats)} }
                    }
                }
                button { class: "physical-drawer-close", onclick: move |_| on_close.call(()), "Close" }
            }
            if review_max {
                if let Some(max_value) = stats.as_ref().and_then(|column| column.max_value.clone()) {
                    div { class: "physical-max-card",
                        strong { "Largest stored value" }
                        span { "row {max_value.row_offset} · {format_bytes(Some(max_value.size_bytes))}" }
                        pre { "{max_value.preview}" }
                    }
                }
            }
            div { class: "physical-preview",
                header {
                    strong { "Page sample" }
                    if let Some(current) = preview.as_ref() {
                        span {
                            "offset {current.offset} · {current.rows.len()} rows"
                            if current.truncated { " · truncated" }
                        }
                    }
                    div { class: "physical-preview-controls",
                        button {
                            class: "physical-page-nav",
                            disabled: !can_previous,
                            onclick: move |_| on_page.call(page_offset.saturating_sub(page_limit)),
                            "Previous"
                        }
                        span { class: "physical-page-status",
                            if let Some(page_count) = page_count {
                                "page {page_number} / {page_count}"
                            } else {
                                "page {page_number}"
                            }
                        }
                        button {
                            class: "physical-page-nav",
                            disabled: !can_next,
                            onclick: move |_| on_page.call(page_offset.saturating_add(page_limit)),
                            "Next"
                        }
                    }
                }
                if loading {
                    div { class: "physical-empty", "Loading page values…" }
                } else if let Some(current) = preview {
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
                    div { class: "physical-empty", "No sample for this column." }
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
                span { {table_label(&table)} }
            }
            for fragment in table.fragments.clone() {
                div { class: "physical-branch",
                    {
                        let table_name = table.name.clone();
                        let fragment_id = fragment.id;
                        let first_path = fragment
                            .files
                            .first()
                            .map(|data| data.path.clone())
                            .unwrap_or_default();
                        let selected = selected_table == table.name
                            && selected_fragment == Some(fragment.id);
                        rsx! {
                            button {
                                class: if selected { "physical-tree-item nested active" } else { "physical-tree-item nested" },
                                onclick: move |_| on_file.call((table_name.clone(), fragment_id, first_path.clone())),
                                div {
                                    strong { "data group {fragment.id}" }
                                    span { {fragment_meta_label(&fragment)} }
                                }
                                code { {format_bytes(fragment.size_bytes)} }
                            }
                        }
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
                                    class: if active { "physical-tree-item nested deep active" } else { "physical-tree-item nested deep" },
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

#[component]
fn PhysicalFragmentStrip(
    table: PhysicalTable,
    selected_fragment: Option<u64>,
    on_file: EventHandler<(String, u64, String)>,
) -> Element {
    let total = table
        .fragments
        .iter()
        .map(fragment_weight)
        .sum::<u64>()
        .max(1);
    rsx! {
        div { class: "physical-layout",
            div { class: "physical-column",
                div { class: "physical-column-label",
                    strong { "Data group distribution" }
                    span { {table_label(&table)} }
                }
                div { class: "physical-page-strip physical-fragment-strip",
                    for fragment in table.fragments.clone() {
                        {
                            let table_name = table.name.clone();
                            let fragment_id = fragment.id;
                            let first_path = fragment
                                .files
                                .first()
                                .map(|data| data.path.clone())
                                .unwrap_or_default();
                            let active = selected_fragment == Some(fragment.id);
                            let flex = ((fragment_weight(&fragment) * 100) / total).max(8);
                            let title = format!(
                                "data group {fragment_id} · {}",
                                fragment_meta_label(&fragment)
                            );
                            rsx! {
                                button {
                                    class: if active { "physical-page active" } else { "physical-page" },
                                    style: "flex: {flex} 1 72px",
                                    title: "{title}",
                                    onclick: move |_| on_file.call((table_name.clone(), fragment_id, first_path.clone())),
                                    span { "g{fragment.id}" }
                                    small { "{fragment_rows_label(&fragment)}" }
                                    small { {format_bytes(fragment.size_bytes)} }
                                }
                            }
                        }
                    }
                }
                p { class: "physical-note",
                    "Bar width follows stored rows, or file size when the row count is unavailable."
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

fn table_size_bytes(table: &PhysicalTable) -> Option<u64> {
    sum_known_sizes(table.fragments.iter().map(|fragment| fragment.size_bytes))
}

fn table_label(table: &PhysicalTable) -> String {
    format!(
        "{} data groups · {} rows · {} · v{}",
        table.fragments.len(),
        table.num_rows,
        format_bytes(table_size_bytes(table)),
        table.version
    )
}

fn fragment_weight(fragment: &PhysicalFragment) -> u64 {
    fragment
        .physical_rows
        .or(fragment.size_bytes)
        .unwrap_or(1)
        .max(1)
}

fn fragment_rows_label(fragment: &PhysicalFragment) -> String {
    fragment
        .physical_rows
        .map(|rows| format!("{rows} rows"))
        .unwrap_or_else(|| "rows unknown".into())
}

fn fragment_meta_label(fragment: &PhysicalFragment) -> String {
    let mut parts = vec![
        fragment_rows_label(fragment),
        format_bytes(fragment.size_bytes),
        format!("{} files", fragment.files.len()),
    ];
    if fragment.deletion_file.is_some() {
        parts.push("deletions".into());
    }
    parts.join(" · ")
}

fn column_type_label(column: &PhysicalColumn) -> String {
    if column.data_type.is_empty() {
        format!("field {}", column.field_id)
    } else {
        format!("field {} · {}", column.field_id, column.data_type)
    }
}

fn column_count_label(column: &PhysicalColumn) -> String {
    format!(
        "{} rows · {} non-null",
        column.row_count, column.non_null_count
    )
}

fn storage_label(column: &PhysicalColumn) -> String {
    format!(
        "storage {} / {}",
        format_bytes(column.uncompressed_bytes),
        format_bytes(column.compressed_bytes)
    )
}

fn sum_known_sizes(sizes: impl IntoIterator<Item = Option<u64>>) -> Option<u64> {
    let sizes = sizes.into_iter().flatten().collect::<Vec<_>>();
    (!sizes.is_empty()).then_some(sizes.into_iter().sum())
}

fn format_bytes(bytes: Option<u64>) -> String {
    match bytes {
        None => "-".into(),
        Some(value) if value < 1024 => format!("{value} B"),
        Some(value) if value < 1024 * 1024 => format!("{:.1} KB", value as f64 / 1024.0),
        Some(value) if value < 1024 * 1024 * 1024 => {
            format!("{:.1} MB", value as f64 / (1024.0 * 1024.0))
        }
        Some(value) => format!("{:.1} GB", value as f64 / (1024.0 * 1024.0 * 1024.0)),
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
    physical_workspace_url_with_preview_offset(
        dataset,
        file,
        table,
        fragment,
        data_file,
        column,
        data_page,
        0,
    )
}

fn physical_workspace_url_with_preview_offset(
    dataset: &str,
    file: &str,
    table: &str,
    fragment: Option<u64>,
    data_file: &str,
    column: &str,
    data_page: u32,
    preview_offset: usize,
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
    if preview_offset > 0 {
        params.push(format!("preview_offset={preview_offset}"));
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
    preview_offset: usize,
) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let url = if preview_offset == 0 {
        physical_workspace_url(dataset, file, table, fragment, data_file, column, data_page)
    } else {
        physical_workspace_url_with_preview_offset(
            dataset,
            file,
            table,
            fragment,
            data_file,
            column,
            data_page,
            preview_offset,
        )
    };
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
                0,
            ),
            "/?page=physical&dataset=dataset&file=story&table=runs&fragment=1&data_file=data%2Fa.lance&column=session_id"
        );
        assert_eq!(
            physical_workspace_url_with_preview_offset(
                "dataset",
                "story",
                "runs",
                Some(1),
                "data/a.lance",
                "session_id",
                2,
                64,
            ),
            "/?page=physical&dataset=dataset&file=story&table=runs&fragment=1&data_file=data%2Fa.lance&column=session_id&data_page=2&preview_offset=64"
        );
    }

    fn sample_table() -> PhysicalTable {
        PhysicalTable {
            name: "runs".into(),
            uri: "file://runs".into(),
            version: 2,
            num_rows: 100,
            fragments: vec![
                PhysicalFragment {
                    id: 1,
                    physical_rows: Some(60),
                    size_bytes: Some(400),
                    deletion_file: None,
                    files: vec![],
                },
                PhysicalFragment {
                    id: 2,
                    physical_rows: Some(40),
                    size_bytes: Some(200),
                    deletion_file: Some("del".into()),
                    files: vec![],
                },
            ],
        }
    }

    #[test]
    fn table_label_includes_fragment_count_size_and_version() {
        assert_eq!(
            table_label(&sample_table()),
            "2 data groups · 100 rows · 600 B · v2"
        );
    }

    #[test]
    fn fragment_meta_includes_rows_size_files_and_deletions() {
        let table = sample_table();
        assert_eq!(fragment_weight(&table.fragments[0]), 60);
        assert_eq!(
            fragment_meta_label(&table.fragments[1]),
            "40 rows · 200 B · 0 files · deletions"
        );
    }

    #[test]
    fn fragment_weight_falls_back_to_size_then_one() {
        let sized = PhysicalFragment {
            id: 3,
            physical_rows: None,
            size_bytes: Some(128),
            deletion_file: None,
            files: vec![],
        };
        let unknown = PhysicalFragment {
            id: 4,
            physical_rows: None,
            size_bytes: None,
            deletion_file: None,
            files: vec![],
        };
        assert_eq!(fragment_weight(&sized), 128);
        assert_eq!(fragment_weight(&unknown), 1);
    }

    #[test]
    fn format_bytes_uses_gb_for_large_fragments() {
        assert_eq!(format_bytes(Some(3 * 1024 * 1024 * 1024)), "3.0 GB");
    }

    #[test]
    fn column_labels_include_counts_and_storage() {
        let column = PhysicalColumn {
            name: "session_id".into(),
            field_id: 3,
            data_type: "Utf8".into(),
            row_count: 28,
            null_count: 0,
            non_null_count: 28,
            compressed_bytes: Some(80),
            uncompressed_bytes: Some(200),
            max_value: None,
            value_distribution: vec![],
            size_distribution: vec![],
            pages: vec![],
        };
        assert_eq!(column_type_label(&column), "field 3 · Utf8");
        assert_eq!(column_count_label(&column), "28 rows · 28 non-null");
        assert_eq!(storage_label(&column), "storage 200 B / 80 B");
    }
}
