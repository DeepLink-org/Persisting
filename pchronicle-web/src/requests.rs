use dioxus::prelude::*;
use gloo_net::http::{Request, RequestBuilder, Response};
use gloo_timers::future::TimeoutFuture;
use serde::Deserialize;
use web_time::Instant;

const OBSERVER: &str = "x-pchronicle-observer";
const LIMIT: usize = 40;
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Phase {
    pub name: String,
    pub state: String,
    pub elapsed_ms: u64,
}
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Snapshot {
    pub request_id: String,
    pub method: String,
    pub path: String,
    pub state: String,
    pub elapsed_ms: u64,
    pub status: Option<u16>,
    pub error: Option<String>,
    pub note: Option<String>,
    pub phases: Vec<Phase>,
    pub worker: Option<Box<Snapshot>>,
}
#[derive(Clone, PartialEq)]
pub struct Entry {
    pub id: String,
    pub method: String,
    pub path: String,
    pub transport: String,
    pub started: Instant,
    pub finished_ms: Option<u64>,
    pub snapshot: Option<Snapshot>,
    pub diagnostic_error: Option<String>,
}
pub static REQUESTS: GlobalSignal<Vec<Entry>> = Signal::global(Vec::new);

fn random_id() -> Option<String> {
    let mut bytes = [0u8; 16];
    web_sys::window()?
        .crypto()
        .ok()?
        .get_random_values_with_u8_array(&mut bytes)
        .ok()?;
    Some(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
fn observer() -> Option<String> {
    let storage = web_sys::window()?.session_storage().ok()??;
    if let Ok(Some(token)) = storage.get_item("pchronicle.request_observer") {
        return Some(token);
    }
    let token = random_id()?;
    storage
        .set_item("pchronicle.request_observer", &token)
        .ok()?;
    Some(token)
}
fn update(id: &str, apply: impl FnOnce(&mut Entry)) {
    if let Some(entry) = REQUESTS.write().iter_mut().find(|e| e.id == id) {
        apply(entry);
    }
}
struct CancelOnDrop(String);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        update(&self.0, |e| {
            if e.transport == {crate::strings::analysis::TRACE_RUNNING} {
                e.transport = "cancelled".into();
                e.finished_ms = Some(e.started.elapsed().as_millis() as u64);
            }
        });
    }
}

pub trait TrackedSend {
    async fn send_tracked(self) -> Result<Response, gloo_net::Error>;
}
impl TrackedSend for RequestBuilder {
    async fn send_tracked(self) -> Result<Response, gloo_net::Error> {
        self.build()?.send_tracked().await
    }
}
impl TrackedSend for Request {
    async fn send_tracked(self) -> Result<Response, gloo_net::Error> {
        let (Some(token), Some(id)) = (observer(), random_id()) else {
            return self.send().await;
        };
        self.headers().set("x-request-id", &id);
        self.headers().set(OBSERVER, &token);
        let url = self.url();
        let path = format!(
            "/api/{}",
            url.split("/api/")
                .nth(1)
                .unwrap_or("")
                .split('?')
                .next()
                .unwrap_or("")
        );
        {
            let mut entries = REQUESTS.write();
            // Keep failures available while routine catalog polling continues.
            if entries.len() >= LIMIT {
                let index = entries
                    .iter()
                    .position(|e| e.transport == "completed")
                    .unwrap_or(0);
                entries.remove(index);
            }
            entries.push(Entry {
                id: id.clone(),
                method: self.method().to_string(),
                path,
                transport: {crate::strings::analysis::TRACE_RUNNING}.into(),
                started: Instant::now(),
                finished_ms: None,
                snapshot: None,
                diagnostic_error: None,
            });
        }
        let _cancel = CancelOnDrop(id.clone());
        let result = self.send().await;
        update(&id, |e| {
            e.finished_ms = Some(e.started.elapsed().as_millis() as u64);
            e.transport = match &result {
                Ok(r) if r.ok() => "completed",
                _ => "failed",
            }
            .into();
            if let Err(error) = &result {
                e.diagnostic_error = Some(format!("Could not reach the server: {error}"));
            }
        });
        result
    }
}

fn selected_entry<'a>(entries: &'a [Entry], selected: &str) -> Option<&'a Entry> {
    if selected.is_empty() {
        entries.last()
    } else {
        entries.iter().find(|entry| entry.id == selected)
    }
}

fn needs_diagnostics(entry: &Entry) -> bool {
    entry.diagnostic_error.is_none() && entry.snapshot.as_ref().is_none_or(|s| s.state == {crate::strings::analysis::TRACE_RUNNING})
}

fn use_request_polling(selected: Signal<String>) {
    use_future(move || async move {
        loop {
            // Only the visible inspector needs server progress. Transport status
            // for the sidebar is already maintained by send_tracked.
            if web_sys::window()
                .and_then(|window| window.document())
                .is_none_or(|document| document.hidden())
            {
                TimeoutFuture::new(1000).await;
                continue;
            }
            let pending = {
                let entries = REQUESTS.peek();
                let selected = selected.peek();
                selected_entry(&entries, &selected)
                    .filter(|entry| needs_diagnostics(entry))
                    .map(|entry| (entry.id.clone(), entry.started))
            };
            if let Some((id, started)) = pending
                && let Some(token) = observer()
            {
                let fetch = async {
                    let response = Request::get(&format!("/api/requests/{id}"))
                        .header(OBSERVER, &token)
                        .send()
                        .await
                        .map_err(|_| {crate::strings::requests::DIAG_CONN_FAILED}.to_string())?;
                    if response.status() == 404 && started.elapsed().as_secs() < 10 {
                        return Ok(None);
                    }
                    if !response.ok() {
                        return Err({crate::strings::requests::DIAG_EXPIRED}.into());
                    }
                    response
                        .json::<Snapshot>()
                        .await
                        .map(Some)
                        .map_err(|_| {crate::strings::requests::DIAG_INVALID}.into())
                };
                let result = match futures_util::future::select(
                    Box::pin(fetch),
                    Box::pin(TimeoutFuture::new(3000)),
                )
                .await
                {
                    futures_util::future::Either::Left((r, _)) => r,
                    _ => Err({crate::strings::requests::DIAG_TIMEOUT}.into()),
                };
                update(&id, |e| match result {
                    Ok(Some(s)) => {
                        if e.path == "Lookup" {
                            e.method = s.method.clone();
                            e.path = s.path.clone();
                        }
                        e.snapshot = Some(s);
                    }
                    Ok(None) => {}
                    Err(error) => e.diagnostic_error = Some(error),
                });
            }
            TimeoutFuture::new(1000).await;
        }
    });
}
fn label(name: &str) -> &str {
    match name {
        "authentication" => {crate::strings::requests::STAGE_AUTHENTICATION},
        "worker_queue" => {crate::strings::requests::STAGE_WORKER_QUEUE},
        "worker_start" => {crate::strings::requests::STAGE_WORKER_START},
        "worker_execution" => {crate::strings::requests::STAGE_WORKER_EXECUTION},
        "execution" => {crate::strings::requests::STAGE_EXECUTION},
        "browse_cache" => {crate::strings::requests::STAGE_BROWSE_CACHE},
        "manifest_summary" => {crate::strings::requests::STAGE_MANIFEST_SUMMARY},
        "directory_wait" => {crate::strings::requests::STAGE_DIRECTORY_WAIT},
        "query_queue" => {crate::strings::requests::STAGE_QUERY_QUEUE},
        "source_metadata" => {crate::strings::requests::STAGE_SOURCE_METADATA},
        "storage_read" => {crate::strings::requests::STAGE_STORAGE_READ},
        "query" => {crate::strings::requests::STAGE_QUERY},
        "catalog_wait" => {crate::strings::requests::STAGE_CATALOG_WAIT},
        "response" => {crate::strings::requests::STAGE_RESPONSE},
        _ => name,
    }
}

fn entry_elapsed_ms(entry: &Entry) -> u64 {
    entry.finished_ms.unwrap_or_else(|| {
        entry
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.elapsed_ms)
            .unwrap_or_else(|| entry.started.elapsed().as_millis() as u64)
    })
}

fn entry_summary(entry: &Entry) -> String {
    let mut parts = vec![entry.transport.clone()];
    if let Some(status) = entry.snapshot.as_ref().and_then(|snapshot| snapshot.status) {
        parts.push(format!("HTTP {status}"));
    }
    parts.push(format!("{} ms", entry_elapsed_ms(entry)));
    parts.join(" · ")
}

fn short_request_id(id: &str) -> String {
    if id.len() <= 16 {
        id.to_owned()
    } else {
        format!("{}…{}", &id[..8], &id[id.len().saturating_sub(4)..])
    }
}
#[component]
pub fn RequestIndicator(on_open: EventHandler<()>) -> Element {
    let entries = REQUESTS.read();
    let running = entries
        .iter()
        .filter(|e| {
            e.transport == {crate::strings::analysis::TRACE_RUNNING}
                || (e.transport == "unknown"
                    && e.snapshot.as_ref().is_some_and(|s| s.state == {crate::strings::analysis::TRACE_RUNNING}))
        })
        .count();
    let failed = entries
        .iter()
        .filter(|e| {
            e.transport == "failed" || e.snapshot.as_ref().is_some_and(|s| s.state == "failed")
        })
        .count();
    let current = entries
        .iter()
        .rev()
        .find(|e| {
            e.transport == {crate::strings::analysis::TRACE_RUNNING}
                || (e.transport == "unknown"
                    && e.snapshot.as_ref().is_some_and(|s| s.state == {crate::strings::analysis::TRACE_RUNNING}))
        })
        .and_then(|e| e.snapshot.as_ref())
        .and_then(|s| {
            let s = s.worker.as_deref().unwrap_or(s);
            s.phases
                .iter()
                .find(|p| p.state == {crate::strings::analysis::TRACE_RUNNING})
                .map(|p| format!("{} · {} ms", label(&p.name), p.elapsed_ms))
        });
    rsx! { button { class:"request-indicator", onclick:move |_|on_open.call(()),
        if running>0 { span { class:"spinner" } "{running} running" if let Some(stage)=current { small { "{stage}" } } }
        else if failed>0 { "{failed} failed · View requests" }
        else { {crate::strings::requests::IDLE} }
    } }
}
#[component]
fn Phases(snapshot: Snapshot) -> Element {
    rsx! {
        if let Some(note)=&snapshot.note { p { role:"status", "{note}" } }
        table { class:"request-phases", caption { {crate::strings::requests::EXECUTION_STAGES} }
            thead { tr { th { "Stage" } th { "Status" } th { {crate::strings::requests::ELAPSED} } } }
            tbody { for phase in &snapshot.phases { tr { key:"{phase.name}",
                td { "{label(&phase.name)}" } td { "{phase.state}" } td { "{phase.elapsed_ms} ms" }
            } } }
        }
        if let Some(worker)=snapshot.worker { h3 { {crate::strings::requests::WORKER_EXECUTION} } Phases { snapshot:*worker } }
    }
}
#[component]
fn RequestRow(entry: Entry, active: bool, on_select: EventHandler<String>) -> Element {
    let summary = entry_summary(&entry);
    let short_id = short_request_id(&entry.id);
    let id = entry.id.clone();
    rsx! {
        button {
            class: if active { "request-row active" } else { "request-row" },
            onclick: move |_| on_select.call(id.clone()),
            strong { "{entry.method} {entry.path}" }
            span { class: "request-row-summary", "{summary}" }
            span { class: "request-row-id", title: "{entry.id}", "{short_id}" }
        }
    }
}

#[component]
pub fn RequestsPanel() -> Element {
    let mut selected = use_signal(String::new);
    let mut lookup = use_signal(String::new);
    use_request_polling(selected);
    let entries = REQUESTS.read().clone();
    let active = selected_entry(&entries, &selected()).cloned();
    rsx! { section { class:"requests-panel",
        header { h1 { "Requests" } p { {crate::strings::requests::PAGE_SUBTITLE} }
            form { onsubmit:move |event| {
                    event.prevent_default();
                    let id=lookup().trim().to_owned();
                    if id.is_empty() || id.len()>64 || !id.bytes().all(|b|b.is_ascii_alphanumeric() || b==b'-' || b==b'_') { return; }
                    selected.set(id.clone());
                    let mut entries=REQUESTS.write();
                    if let Some(entry)=entries.iter_mut().find(|e|e.id==id) { entry.diagnostic_error=None; }
                    else {
                        if entries.len()>=LIMIT { entries.remove(0); }
                        entries.push(Entry { id,method:"GET".into(),path:"Lookup".into(),transport:"unknown".into(),started:Instant::now()-std::time::Duration::from_secs(10),finished_ms:None,snapshot:None,diagnostic_error:None });
                    }
                },
                input { aria_label:"Request ID", placeholder:{crate::strings::requests::FIND_PLACEHOLDER}, value:"{lookup}", oninput:move |e|lookup.set(e.value()) }
                button { class:"button", r#type:"submit", {crate::strings::common::FIND} }
            }
        }
        if !selected().is_empty() && !entries.iter().any(|e|e.id==selected()) { p { role:"status", {crate::strings::requests::NOT_IN_HISTORY} } }
        div { class:"requests-grid",
            aside { aria_label:"Recent requests",
                for entry in entries.iter().rev() {
                    RequestRow {
                        key: "{entry.id}",
                        entry: entry.clone(),
                        active: active.as_ref().is_some_and(|e| e.id == entry.id),
                        on_select: move |id| selected.set(id),
                    }
                }
            }
            article {
                if let Some(entry)=active {
                    h2 { "{entry.method} {entry.path}" } code { "{entry.id}" }
                    p { "Browser request: {entry.transport}" }
                    if let Some(error)=entry.diagnostic_error { p { class:"request-error", role:"alert", "{error}" }
                        button { class:"button", onclick:{let id=entry.id.clone();move |_|update(&id,|e|e.diagnostic_error=None)}, {crate::strings::requests::RETRY_DIAGNOSTICS} }
                    }
                    if let Some(snapshot)=entry.snapshot {
                        p { role:"status", "Server: {snapshot.state} · {snapshot.elapsed_ms} ms" }
                        if let Some(status)=snapshot.status { p { "HTTP {status}" } }
                        if let Some(ref error)=snapshot.error { p { class:"request-error", role:"alert", "{error}" } }
                        Phases { snapshot }
                    } else { p { role:"status", {crate::strings::requests::WAITING_DIAGNOSTICS} } }
                } else { p { {crate::strings::requests::BROWSE_HINT} } }
            }
        }
    } }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_stop_on_terminal_snapshot_or_error() {
        let mut entry = Entry {
            id: "test".into(),
            method: "GET".into(),
            path: "test".into(),
            transport: "completed".into(),
            started: Instant::now(),
            finished_ms: None,
            snapshot: None,
            diagnostic_error: None,
        };
        let entries = (0..40)
            .map(|id| Entry {
                id: id.to_string(),
                ..entry.clone()
            })
            .collect::<Vec<_>>();
        assert_eq!(selected_entry(&entries, "").unwrap().id, "39");
        assert_eq!(selected_entry(&entries, "5").unwrap().id, "5");
        assert!(selected_entry(&entries, "missing").is_none());
        assert!(selected_entry(&[], "").is_none());
        entry.finished_ms = Some(12);
        assert_eq!(entry_elapsed_ms(&entry), 12);
        // Opening the inspector can retrieve diagnostics for a completed request.
        assert!(needs_diagnostics(&entry));
        entry.snapshot = Some(Snapshot {
            request_id: "test".into(),
            method: "GET".into(),
            path: "test".into(),
            state: {crate::strings::analysis::TRACE_RUNNING}.into(),
            elapsed_ms: 0,
            status: None,
            error: None,
            note: None,
            phases: vec![],
            worker: None,
        });
        assert!(needs_diagnostics(&entry));
        for state in ["completed", "failed", "cancelled"] {
            entry.snapshot.as_mut().unwrap().state = state.into();
            assert!(!needs_diagnostics(&entry));
        }
        entry.snapshot = None;
        entry.diagnostic_error = Some("unavailable".into());
        assert!(!needs_diagnostics(&entry));
    }
}
