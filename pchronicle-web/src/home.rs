use dioxus::prelude::*;

const DOCS: &str = "https://aicarrier.feishu.cn/wiki/BkIVw48tWiRgDTkMZRZccuofnXE";

fn assign_location(href: &str) {
    if let Some(window) = web_sys::window() {
        let _ = window.location().assign(href);
    }
}

/// Skip Litefuse sign-in UI; kick off custom SSO (竹云/飞书) immediately.
fn litefuse_login_url() -> &'static str {
    "/litefuse/auth/sso-initiate?provider=custom"
}

fn go_litefuse() {
    assign_location(litefuse_login_url());
}

#[component]
pub fn HomeLanding(on_open: EventHandler<String>) -> Element {
    // Parent still passes on_open for Warehouse entry; header no longer exposes it.
    let _ = on_open;

    rsx! {
        div { class: "pc-home",
            div { class: "shell",
                main { class: "portal", id: "portalView",
                    header { class: "topbar",
                        a {
                            class: "brand",
                            href: "#top",
                            aria_label: "DeepTrace 首页",
                            onclick: move |event| {
                                event.prevent_default();
                            },
                            span { class: "brand-mark", aria_hidden: "true",
                                img {
                                    class: "brand-mark-img",
                                    src: "/assets/home/brand-mark-v9.png",
                                    alt: "",
                                    width: "32",
                                    height: "32",
                                }
                            }
                            "DeepTrace"
                        }
                        button {
                            class: "header-login",
                            onclick: move |_| go_litefuse(),
                            "登录控制台"
                        }
                    }

                    div { class: "hero-wrap", id: "top",
                        section { class: "hero",
                            div {
                                div { class: "eyebrow", "Agent observability platform" }
                                h1 { "为Agent时代提供" em { "可复现经验" } }
                                p { class: "hero-copy",
                                    "DeepTrace 为实验室内部 Agent 提供统一轨迹管理：实时掌握运行现场、离线回放关键路径，让每一个动作、判断与结果都清晰可见——把每一次运行沉淀为可检索、可复用的团队经验资产。"
                                }
                                div { class: "hero-actions",
                                    button {
                                        class: "primary-btn",
                                        onclick: move |_| go_litefuse(),
                                        "进入控制台 "
                                        svg {
                                            fill: "none",
                                            view_box: "0 0 16 16",
                                            path {
                                                d: "M3 8h9M8.5 4.5 12 8l-3.5 3.5",
                                                stroke: "currentColor",
                                                stroke_linecap: "round",
                                                stroke_linejoin: "round",
                                                stroke_width: "1.7",
                                            }
                                        }
                                    }
                                    a {
                                        class: "ghost-btn",
                                        href: DOCS,
                                        target: "_blank",
                                        rel: "noreferrer",
                                        "使用文档"
                                    }
                                }
                            }
                        }
                    }

                    section {
                        class: "section overview-shell",
                        id: "overview",
                        aria_labelledby: "overview-title",
                        p { class: "section-kicker", "Two ways to read a run" }
                        h2 { id: "overview-title",
                            "一条运行链，"
                            br {}
                            span { style: "color:var(--azure)", "两种工作方式。" }
                        }
                        p { class: "section-intro",
                            "DeepTrace 把现场观测和历史数据放在同一条路径上：实时轨迹帮助你看见正在发生什么，离线轨迹帮助你回到已经发生过的每一个关键节点。"
                        }
                        div { class: "mode-grid",
                            article { class: "mode-card live",
                                div { class: "mode-card-head",
                                    div {
                                        h3 { "实时轨迹" }
                                        p { class: "mode-card-copy",
                                            "以事件流捕获模型、检索、工具与外部 API 调用，串起 session、Agent 图、延迟与成本上下文，让运行现场保持可见。"
                                        }
                                    }
                                    span { class: "mode-badge",
                                        i {}
                                        "LIVE VIEW"
                                    }
                                }
                                div { class: "mode-rail", aria_label: "实时轨迹示例",
                                    span { small { "input" } }
                                    span { small { "tool" } }
                                    span { small { "model" } }
                                    span { small { "output" } }
                                    span {}
                                }
                            }
                            article { class: "mode-card archive",
                                div { class: "mode-card-head",
                                    div {
                                        h3 { "离线轨迹" }
                                        p { class: "mode-card-copy",
                                            "把已采集的运行记录沉淀为可查询的数据集，支持导入、检索、导出与只读服务，为复盘和评估提供稳定事实层。"
                                        }
                                    }
                                    span { class: "mode-badge",
                                        i {}
                                        "DATASET"
                                    }
                                }
                                div { class: "dataset-slab",
                                    span { "trace dataset" }
                                    b { "run_2048" }
                                    span { "events indexed" }
                                    b { "12,482" }
                                    span { "query surface" }
                                    b { "SQL / API" }
                                }
                            }
                        }
                    }

                    section {
                        class: "section",
                        id: "capabilities",
                        aria_labelledby: "capabilities-title",
                        div { class: "ability-header",
                            div {
                                p { class: "section-kicker", "Core capabilities" }
                                h2 { id: "capabilities-title",
                                    "对每条轨迹，"
                                    br {}
                                    "做两件有用的事。"
                                }
                            }
                            p { class: "section-intro",
                                "先重建上下文，再用结构化信号找出变化。每一步都对应真实的工程动作。"
                            }
                        }
                        div { class: "ability-grid",
                            article { class: "ability-card replay",
                                h3 { "轨迹回放" }
                                p {
                                    "将一次 Agent Run 固化为可复查的事件序列，按会话、任务和分支逐步重建输入、输出、中间结果与失败点，并定位到任意节点。"
                                }
                                div { class: "ability-timeline", aria_label: "轨迹回放时间线",
                                    span { small { "start" } }
                                    span { small { "tool.call" } }
                                    span { small { "decision" } }
                                    span { small { "result" } }
                                    span {}
                                }
                            }
                            article { class: "ability-card analysis",
                                h3 { "轨迹分析" }
                                p {
                                    "在历史轨迹数据集上做过滤、聚合与查询，对比时延、成本、错误、重试和质量信号，定位高频失败与性能瓶颈。"
                                }
                                div { class: "analysis-grid",
                                    div { class: "analysis-list",
                                        div { class: "analysis-row",
                                            b { "latency" }
                                            em { style: "width:82%" }
                                            span { "1.82s" }
                                        }
                                        div { class: "analysis-row",
                                            b { "tool error" }
                                            em {}
                                            span { "3.4%" }
                                        }
                                        div { class: "analysis-row",
                                            b { "retry rate" }
                                            em {}
                                            span { "6.1%" }
                                        }
                                        div { class: "analysis-row",
                                            b { "quality" }
                                            em {}
                                            span { "0.94" }
                                        }
                                    }
                                    svg {
                                        height: "80",
                                        width: "90",
                                        view_box: "0 0 90 80",
                                        path {
                                            d: "M2 70 C18 63 22 44 36 50 S48 61 56 42 S70 35 88 12",
                                            fill: "none",
                                            stroke: "#2a83eb",
                                            stroke_linecap: "round",
                                            stroke_width: "2.5",
                                        }
                                        circle { cx: "88", cy: "12", fill: "#6ddbd0", r: "4" }
                                    }
                                }
                            }
                        }
                    }

                    section {
                        class: "section",
                        id: "security",
                        style: "padding-top:0",
                        aria_labelledby: "workflow-title",
                        div { class: "workflow-panel",
                            div {
                                p { class: "section-kicker", "Trace to action" }
                                h2 { id: "workflow-title",
                                    "让轨迹进入工作流，"
                                    br {}
                                    "而不止停在日志里。"
                                }
                                p { "把一次运行转成可解释、可比较、可复用的工程资产。" }
                            }
                            div { class: "workflow-steps",
                                div { class: "workflow-step",
                                    b { "01" }
                                    strong { "捕获" }
                                    span { "保留事件与上下文" }
                                }
                                div { class: "workflow-step",
                                    b { "02" }
                                    strong { "回放" }
                                    span { "重建关键执行路径" }
                                }
                                div { class: "workflow-step",
                                    b { "03" }
                                    strong { "分析" }
                                    span { "定位趋势与异常" }
                                }
                            }
                        }
                    }

                    footer { class: "footer",
                        span {
                            strong { "DeepTrace" }
                            " · Agent 轨迹平台"
                        }
                        span { "Internal workspace · 2024—2025" }
                    }
                }
            }
        }
    }
}
