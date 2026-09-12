use dioxus::prelude::*;

use crate::api;
use crate::model::HomeNavLink;

const GITHUB: &str = "https://github.com/DeepLink-org/Persisting";
const DOCS: &str = "https://deeplink-org.github.io/Persisting/";
const QUICK_START: &str = "pip install persisting\npchronicle onboard";
const FROM_SOURCE: &str = "curl -fsSL https://raw.githubusercontent.com/DeepLink-org/Persisting/main/scripts/install-nightly.sh | bash";

async fn copy_text(text: &str) -> bool {
    if let Some(window) = web_sys::window() {
        return wasm_bindgen_futures::JsFuture::from(
            window.navigator().clipboard().write_text(text),
        )
        .await
        .is_ok();
    }
    false
}

fn assign_location(href: &str) {
    if let Some(window) = web_sys::window() {
        let _ = window.location().assign(href);
    }
}

#[component]
pub fn HomeLanding(on_open: EventHandler<String>) -> Element {
    let mut links = use_signal(Vec::<HomeNavLink>::new);
    let mut tab = use_signal(|| 0usize);
    let mut copied = use_signal(|| false);
    use_effect(move || {
        spawn(async move {
            if let Ok(config) = api::ui_config().await {
                links.set(config.links);
            }
        });
    });
    let command = if tab() == 0 { QUICK_START } else { FROM_SOURCE };
    rsx! {
        div { class: "pc-home",
            section { class: "pc-home-hero",
                div { class: "pc-home-aurora", aria_hidden: "true" }
                div { class: "pc-home-grid", aria_hidden: "true" }
                header { class: "pc-home-nav",
                    div { class: "pc-home-nav-left",
                        span { class: "pc-home-wordmark",
                            span { class: "pc-home-mark", "P" }
                            span { "Persisting Chronicle" }
                        }
                        button {
                            class: "pc-home-capsule",
                            onclick: move |_| on_open.call("catalog".into()),
                            "Warehouse"
                        }
                        for link in links() {
                            HomeLinkCapsule { key: "{link.href}", link }
                        }
                    }
                    div { class: "pc-home-nav-right",
                        a { class: "pc-home-nav-link", href: GITHUB, target: "_blank", rel: "noreferrer", "GitHub" }
                        a { class: "pc-home-nav-cta", href: DOCS, target: "_blank", rel: "noreferrer", "Docs" }
                    }
                }
                div { class: "pc-home-hero-grid",
                    div { class: "pc-home-copy",
                        p { class: "pc-home-kicker", "Persisting Chronicle" }
                        h1 { "Chronicled Experience" br {} "for the Agent Era" }
                        p { "Persisting Chronicle is now in developer preview for agent infrastructure developers worldwide — source code included." }
                        p { "Every capability of a run is recorded so it can be browsed, queried, and recomposed: prompts, tools, skills, sessions, sandboxes, storage, loops, scheduling, and the UI." }
                        div { class: "pc-home-actions",
                            button {
                                class: "pc-home-btn primary",
                                onclick: move |_| on_open.call("catalog".into()),
                                "Open Warehouse"
                            }
                            a { class: "pc-home-btn", href: GITHUB, target: "_blank", rel: "noreferrer", "View on GitHub" }
                            a { class: "pc-home-btn", href: DOCS, target: "_blank", rel: "noreferrer", "Developer docs" }
                        }
                    }
                    div { class: "pc-home-terminal-wrap",
                        div { class: "pc-home-tabs",
                            button {
                                class: if tab() == 0 { "active" } else { "" },
                                onclick: move |_| {
                                    tab.set(0);
                                    copied.set(false);
                                },
                                "Quick start"
                            }
                            button {
                                class: if tab() == 1 { "active" } else { "" },
                                onclick: move |_| {
                                    tab.set(1);
                                    copied.set(false);
                                },
                                "Install nightly"
                            }
                        }
                        div { class: "pc-home-terminal",
                            div { class: "pc-home-terminal-bar",
                                span { class: "dot red" }
                                span { class: "dot yellow" }
                                span { class: "dot green" }
                                button {
                                    class: "pc-home-copy-btn",
                                    onclick: move |_| {
                                        spawn(async move { if copy_text(command).await { copied.set(true); } });
                                    },
                                    if copied() { "Copied" } else { "Copy" }
                                }
                            }
                            pre { class: "pc-home-terminal-body",
                                for line in command.lines() {
                                    span { class: "pc-home-command-line",
                                        span { class: "pc-home-prompt", aria_hidden: "true", "$ " }
                                        "{line}"
                                    }
                                }
                            }
                        }
                    }
                }
            }
            section { class: "pc-home-value",
                p { class: "pc-home-badge", "Agent history = Dataset + query" }
                h2 { "Makes agents easier to understand and improve." }
                p { class: "pc-home-lede", "A harness keeps an agent working. Chronicle keeps the run as durable, queryable history so the next decision can see what actually happened." }
                div { class: "pc-home-cards",
                    article {
                        h3 { "Datasets" }
                        p { "Mount captured or imported Sources and see run counts before you drill in." }
                    }
                    article {
                        h3 { "Trajectory" }
                        p { "Reconstruct a complete run from one event stream: prompts, tool calls, results, and every context injection." }
                    }
                    article {
                        h3 { "Analysis" }
                        p { "Ask a question or run bounded SQL against the same Snapshot the warehouse is serving." }
                    }
                }
            }
            section { class: "pc-home-split",
                div { class: "pc-home-split-copy",
                    p { class: "pc-home-badge", "Design approach" }
                    h2 { "Every run is a Dataset. Every query is scoped." }
                    h3 { "Warehouse first" }
                    p { "Open the local warehouse to browse mounted Datasets, then enter Runs without leaving loopback. The API stays read-only." }
                    h3 { "Every run is traceable" }
                    p { "Inspect records by source in the trajectory view. Resume, search, and replay operate on the same event stream." }
                }
                div { class: "pc-home-split-media",
                    img {
                        class: "pc-home-shot",
                        src: "/assets/home/data-overview.jpg",
                        alt: "Datasets warehouse showing mounted trajectory Datasets and run counts"
                    }
                    img {
                        class: "pc-home-shot secondary",
                        src: "/assets/home/run-detail.jpg",
                        alt: "Run trajectory view reconstructing a complete Agent session"
                    }
                }
            }
            section { class: "pc-home-modes",
                h2 { "Warehouse surfaces" }
                div { class: "pc-home-mode-grid",
                    ModeCard {
                        title: "Datasets",
                        body: "See mounted Datasets and run counts, then enter the current scope.",
                        onclick: move |_| on_open.call("catalog".into()),
                    }
                    ModeCard {
                        title: "Runs",
                        body: "Filter by path, Dataset, status, or text and open one Run.",
                        onclick: move |_| on_open.call("runs".into()),
                    }
                    ModeCard {
                        title: "Analysis",
                        body: "Inspect available fields and analyze with a question or read-only SQL.",
                        onclick: move |_| on_open.call("tools".into()),
                    }
                    ModeCard {
                        title: "Storage",
                        body: "Inspect Lance tables, data groups, column distributions, and storage size.",
                        onclick: move |_| on_open.call("physical".into()),
                    }
                }
                img {
                    class: "pc-home-shot analysis",
                    src: "/assets/home/analysis-sql.jpg",
                    alt: "Analysis workspace with bounded SQL against a Dataset Snapshot"
                }
            }
            footer { class: "pc-home-footer",
                p { "Loopback only. The warehouse API is read-only and does not modify a mounted Dataset." }
                p { "Open source · Apache-2.0 · Persisting Chronicle" }
            }
        }
    }
}

#[component]
fn HomeLinkCapsule(link: HomeNavLink) -> Element {
    let href = link.href.clone();
    rsx! {
        a {
            class: "pc-home-capsule",
            href: "{link.href}",
            onclick: move |event| {
                event.prevent_default();
                assign_location(&href);
            },
            "{link.label}"
        }
    }
}

#[component]
fn ModeCard(title: String, body: String, onclick: EventHandler<()>) -> Element {
    rsx! {
        button { class: "pc-home-mode", onclick: move |_| onclick.call(()),
            strong { "{title}" }
            span { "{body}" }
        }
    }
}
