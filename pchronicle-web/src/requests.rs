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
            if e.transport == "running" {
                e.transport = "cancelled".into();
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
                transport: "running".into(),
                started: Instant::now(),
                snapshot: None,
                diagnostic_error: None,
            });
        }
        let _cancel = CancelOnDrop(id.clone());
        let result = self.send().await;
        update(&id, |e| {
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

pub fn use_request_polling() {
    use_future(|| async {
        loop {
            let pending = REQUESTS
                .peek()
                .iter()
                .filter(|e| {
                    e.diagnostic_error.is_none()
                        && e.snapshot.as_ref().is_none_or(|s| s.state == "running")
                })
                .map(|e| (e.id.clone(), e.started))
                .collect::<Vec<_>>();
            if let Some(token) = observer() {
                let queries=pending.into_iter().map(|(id,started)| { let token=token.clone(); async move {
                    let fetch=async {
                        let response=Request::get(&format!("/api/requests/{id}")).header(OBSERVER,&token).send().await.map_err(|_|"Diagnostics connection failed".to_string())?;
                        if response.status()==404 && started.elapsed().as_secs()<10 { return Ok(None); }
                        if !response.ok() { return Err("Diagnostics expired or are unavailable; the original request may still be running".into()); }
                        response.json::<Snapshot>().await.map(Some).map_err(|_|"Invalid diagnostics response".into())
                    };
                    let result= match futures_util::future::select(Box::pin(fetch),Box::pin(TimeoutFuture::new(3000))).await {
                        futures_util::future::Either::Left((r,_))=>r,
                        _=>Err("Diagnostics timed out; server status is unknown".into()),
                    };
                    update(&id,|e|match result { Ok(Some(s))=>{
                        if e.path == "Lookup" { e.method=s.method.clone(); e.path=s.path.clone(); }
                        e.snapshot=Some(s);
                    }, Ok(None)=>{}, Err(error)=>e.diagnostic_error=Some(error) });
                }});
                futures_util::future::join_all(queries).await;
            }
            TimeoutFuture::new(1000).await;
        }
    });
}
fn label(name: &str) -> &str {
    match name {
        "authentication" => "Check catalog identity",
        "worker_queue" => "Wait for worker",
        "worker_start" => "Start worker",
        "worker_execution" => "Execute in worker",
        "execution" => "Accept request",
        "browse_cache" => "Read directory cache",
        "manifest_summary" => "Read local manifest summaries",
        "directory_wait" => "Wait for directory listing",
        "query_queue" => "Wait for query slot / shared result",
        "source_metadata" => "Resolve source metadata",
        "storage_read" => "Read storage",
        "query" => "Build / execute query",
        "catalog_wait" => "Wait for catalog refresh",
        "response" => "Prepare response",
        _ => name,
    }
}
#[component]
pub fn RequestIndicator(on_open: EventHandler<()>) -> Element {
    let entries = REQUESTS.read();
    let running = entries
        .iter()
        .filter(|e| {
            e.transport == "running" || e.snapshot.as_ref().is_some_and(|s| s.state == "running")
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
            e.transport == "running" || e.snapshot.as_ref().is_some_and(|s| s.state == "running")
        })
        .and_then(|e| e.snapshot.as_ref())
        .and_then(|s| {
            let s = s.worker.as_deref().unwrap_or(s);
            s.phases
                .iter()
                .find(|p| p.state == "running")
                .map(|p| format!("{} · {} ms", label(&p.name), p.elapsed_ms))
        });
    rsx! { button { class:"request-indicator", onclick:move |_|on_open.call(()),
        if running>0 { span { class:"spinner" } "{running} running" if let Some(stage)=current { small { "{stage}" } } }
        else if failed>0 { "{failed} failed · View requests" }
        else { "Requests · Idle" }
    } }
}
#[component]
fn Phases(snapshot: Snapshot) -> Element {
    rsx! {
        if let Some(note)=&snapshot.note { p { role:"status", "{note}" } }
        table { class:"request-phases", caption { "Execution stages" }
            thead { tr { th { "Stage" } th { "Status" } th { "Elapsed" } } }
            tbody { for phase in &snapshot.phases { tr { key:"{phase.name}",
                td { "{label(&phase.name)}" } td { "{phase.state}" } td { "{phase.elapsed_ms} ms" }
            } } }
        }
        if let Some(worker)=snapshot.worker { h3 { "Worker execution" } Phases { snapshot:*worker } }
    }
}
#[component]
pub fn RequestsPanel() -> Element {
    let mut selected = use_signal(String::new);
    let mut lookup = use_signal(String::new);
    let entries = REQUESTS.read().clone();
    let active = if selected().is_empty() {
        entries.last()
    } else {
        entries.iter().find(|e| e.id == selected())
    }
    .cloned();
    rsx! { section { class:"requests-panel",
        header { h1 { "Requests" } p { "Inspect this browser’s recent requests, execution stages and failures. Server history is retained for up to 10 minutes." }
            form { onsubmit:move |event| {
                    event.prevent_default();
                    let id=lookup().trim().to_owned();
                    if id.is_empty() || id.len()>64 || !id.bytes().all(|b|b.is_ascii_alphanumeric() || b==b'-' || b==b'_') { return; }
                    selected.set(id.clone());
                    let mut entries=REQUESTS.write();
                    if let Some(entry)=entries.iter_mut().find(|e|e.id==id) { entry.diagnostic_error=None; }
                    else {
                        if entries.len()>=LIMIT { entries.remove(0); }
                        entries.push(Entry { id,method:"GET".into(),path:"Lookup".into(),transport:"unknown".into(),started:Instant::now()-std::time::Duration::from_secs(10),snapshot:None,diagnostic_error:None });
                    }
                },
                input { aria_label:"Request ID", placeholder:"Find by request ID", value:"{lookup}", oninput:move |e|lookup.set(e.value()) }
                button { class:"button", r#type:"submit", "Find" }
            }
        }
        if !selected().is_empty() && !entries.iter().any(|e|e.id==selected()) { p { role:"status", "This request is not in this browser’s recent history." } }
        div { class:"requests-grid",
            aside { aria_label:"Recent requests",
                for entry in entries.iter().rev() { button { class:if active.as_ref().is_some_and(|e|e.id==entry.id) {"request-row active"} else {"request-row"},
                    onclick:{let id=entry.id.clone();move |_|selected.set(id.clone())},
                    strong { "{entry.method} {entry.path}" } span { "{entry.transport} · {entry.id}" }
                } }
            }
            article {
                if let Some(entry)=active {
                    h2 { "{entry.method} {entry.path}" } code { "{entry.id}" }
                    p { "Browser request: {entry.transport}" }
                    if let Some(error)=entry.diagnostic_error { p { class:"request-error", role:"alert", "{error}" }
                        button { class:"button", onclick:{let id=entry.id.clone();move |_|update(&id,|e|e.diagnostic_error=None)}, "Retry diagnostics" }
                    }
                    if let Some(snapshot)=entry.snapshot {
                        p { role:"status", "Server: {snapshot.state} · {snapshot.elapsed_ms} ms" }
                        if let Some(status)=snapshot.status { p { "HTTP {status}" } }
                        if let Some(ref error)=snapshot.error { p { class:"request-error", role:"alert", "{error}" } }
                        Phases { snapshot }
                    } else { p { role:"status", "Waiting for server diagnostics…" } }
                } else { p { "Browse a dataset or open Runs to inspect a request." } }
            }
        }
    } }
}
