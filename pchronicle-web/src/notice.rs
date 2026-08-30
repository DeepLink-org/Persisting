use dioxus::prelude::*;

use crate::api::ApiFailure;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct WorkspaceNotice {
    pub title: String,
    pub summary: String,
    pub action: String,
    pub detail: String,
    pub engine_detail: Option<String>,
    pub request_id: Option<String>,
    pub turn_id: Option<i64>,
}

pub(crate) fn workspace_notice(failure: &ApiFailure) -> WorkspaceNotice {
    let (title, action) = match failure.code.as_str() {
        "invalid_request" => ("This request isn't valid", String::new()),
        "not_found" => ("Nothing matched", String::new()),
        "conflict" => (
            "This view is out of date",
            "Refresh the catalog and try again".into(),
        ),
        "unsupported" | "unplannable" => ("This isn't supported", String::new()),
        "resource_exhausted" => (
            "The result is too large",
            "Narrow the query or lower the row limit".into(),
        ),
        "unavailable" => (
            "The server isn't reachable",
            "Check that pchronicle serve is still running".into(),
        ),
        "internal" => (
            "Something went wrong",
            "The server log for this request ID has the cause".into(),
        ),
        _ => ("Request failed", String::new()),
    };
    let summary = if failure.message.is_empty() {
        title.to_string()
    } else {
        failure.message.clone()
    };
    WorkspaceNotice {
        title: title.into(),
        summary,
        action,
        detail: failure.raw.clone(),
        engine_detail: failure.engine_detail.clone(),
        request_id: failure.request_id.clone(),
        turn_id: None,
    }
}

pub(crate) fn copy_request_id(text: &str) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let _ = window.navigator().clipboard().write_text(text);
}

#[component]
pub(crate) fn ErrorNotice(
    notice: WorkspaceNotice,
    on_dismiss: Option<EventHandler<()>>,
) -> Element {
    let request_id = notice.request_id.clone();
    let engine_detail = notice.engine_detail.clone();
    rsx! {
        div { class: "pc2-workspace-notice", role: "alert",
            div { class: "pc2-workspace-notice-copy",
                strong { "{notice.title}" }
                span { "{notice.summary}" }
                if !notice.action.is_empty() {
                    span { "{notice.action}" }
                }
                if let Some(request_id) = request_id {
                    p { class: "pc2-workspace-notice-request",
                        "Request ID "
                        code { "{request_id}" }
                        button {
                            class: "pc2-workspace-notice-copy-id",
                            r#type: "button",
                            aria_label: "Copy request ID",
                            onclick: move |_| copy_request_id(&request_id),
                            "Copy"
                        }
                    }
                }
                details { class: "pc2-workspace-notice-details",
                    summary { "Show technical details" }
                    pre { "{notice.detail}" }
                    if let Some(engine_detail) = engine_detail {
                        pre { "{engine_detail}" }
                    }
                }
            }
            if let Some(on_dismiss) = on_dismiss {
                button {
                    class: "pc2-workspace-notice-dismiss",
                    r#type: "button",
                    aria_label: "Dismiss",
                    onclick: move |_| on_dismiss.call(()),
                    "×"
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_notice_uses_request_id_and_hides_secret() {
        let failure = crate::api::parse_api_failure(
            500,
            r#"{"code":"internal","message":"internal server error","request_id":"deadbeefdeadbeef"}"#,
        );
        let notice = workspace_notice(&failure);
        assert_eq!(notice.title, "Something went wrong");
        assert_eq!(
            notice.request_id.as_deref(),
            Some("deadbeefdeadbeef")
        );
        assert!(!notice.detail.contains("secret"));
        assert!(!notice.summary.contains("secret"));
        assert!(notice.engine_detail.is_none());
    }

    #[test]
    fn resource_exhausted_notice_asks_to_narrow_the_query() {
        let failure = crate::api::parse_api_failure(
            429,
            r#"{"code":"resource_exhausted","message":"find result exceeds row limit of 51","request_id":"rid"}"#,
        );
        let notice = workspace_notice(&failure);
        assert_eq!(notice.title, "The result is too large");
        assert!(notice.action.contains("Narrow the query"));
    }

    #[test]
    fn unplannable_notice_keeps_engine_detail() {
        let failure = crate::api::parse_api_failure(
            422,
            r#"{"code":"unplannable","message":"compiled SQL could not be planned against the live catalog","request_id":"rid","engine_detail":"column x not found"}"#,
        );
        let notice = workspace_notice(&failure);
        assert_eq!(notice.title, "This isn't supported");
        assert_eq!(notice.engine_detail.as_deref(), Some("column x not found"));
        assert!(notice.detail.contains("unplannable"));
        assert_ne!(notice.summary, notice.engine_detail.clone().unwrap());
    }
}
