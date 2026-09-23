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
                        class: "section",
                        id: "capability",
                        style: "padding-top:0",
                        aria_labelledby: "capability-title",
                        div { class: "section-head section-head--split",
                            div {
                                p { class: "section-kicker", "Capabilities" }
                                h2 { id: "capability-title",
                                    "一条运行链，"
                                    br {}
                                    "在线与离线两段能力。"
                                }
                            }
                            p { class: "section-intro",
                                "同一份轨迹数据，在线侧负责\"看得见\"，离线侧负责\"用得上\"。两侧共用一套采集规范与数据口径，不需要为分析再做一次对接。"
                            }
                        }

                        div { class: "cap-group",
                            div { class: "cap-group-head",
                                span { class: "cap-tag online",
                                    i {}
                                    "ONLINE"
                                }
                                h3 { "在线：运行过程中的观测与回放" }
                                p { "面向正在发生的运行，先看见，再定位。" }
                            }
                            div { class: "cap-grid",
                                article { class: "cap-card",
                                    span { class: "cap-ico",
                                        svg {
                                            view_box: "0 0 16 16",
                                            fill: "none",
                                            path {
                                                d: "M2 11l3-4 3 2.5L14 3",
                                                stroke: "currentColor",
                                                stroke_width: "1.6",
                                                stroke_linecap: "round",
                                                stroke_linejoin: "round",
                                            }
                                        }
                                    }
                                    h4 { "实时轨迹采集" }
                                    p { "以事件流捕获模型、检索、工具与外部 API 调用，串起 session、Agent 图、延迟与成本上下文。" }
                                    span { class: "cap-meta", "streaming · session" }
                                }
                                article { class: "cap-card",
                                    span { class: "cap-ico",
                                        svg {
                                            view_box: "0 0 16 16",
                                            fill: "none",
                                            circle { cx: "8", cy: "8", r: "5.6", stroke: "currentColor", stroke_width: "1.6" }
                                            path {
                                                d: "M8 5.2V8l2 1.4",
                                                stroke: "currentColor",
                                                stroke_width: "1.6",
                                                stroke_linecap: "round",
                                            }
                                        }
                                    }
                                    h4 { "轨迹回放" }
                                    p { "按会话、任务与分支逐步重建执行路径，从输入到输出逐节点查看，随时定位失败与异常发生的位置。" }
                                    span { class: "cap-meta", "replay · step-through" }
                                }
                                article { class: "cap-card",
                                    span { class: "cap-ico",
                                        svg {
                                            view_box: "0 0 16 16",
                                            fill: "none",
                                            path {
                                                d: "M2.5 12.5h3v-4h-3zM6.5 12.5h3v-7h-3zM10.5 12.5h3v-2.5h-3z",
                                                stroke: "currentColor",
                                                stroke_width: "1.4",
                                            }
                                        }
                                    }
                                    h4 { "实时监控视图" }
                                    p { "会话与任务维度的运行状态汇总，把高频失败、异常耗时和激增的成本集中在一个视图里。" }
                                    span { class: "cap-meta", "live view" }
                                }
                            }
                        }

                        div { class: "cap-group cap-group--offline",
                            div { class: "cap-group-head",
                                span { class: "cap-tag offline",
                                    i {}
                                    "OFFLINE"
                                }
                                h3 { "离线：数据集上的存储、检索与导出" }
                                p { "面向已经发生的运行，先沉淀，再用起来。" }
                            }
                            div { class: "cap-grid",
                                article { class: "cap-card",
                                    span { class: "cap-ico",
                                        svg {
                                            view_box: "0 0 16 16",
                                            fill: "none",
                                            ellipse { cx: "8", cy: "4", rx: "5", ry: "2.1", stroke: "currentColor", stroke_width: "1.5" }
                                            path {
                                                d: "M3 4v8c0 1.16 2.24 2.1 5 2.1s5-.94 5-2.1V4",
                                                stroke: "currentColor",
                                                stroke_width: "1.5",
                                            }
                                        }
                                    }
                                    h4 { "高性能存储" }
                                    p { "轨迹按列式结构组织并建立索引，支持大规模数据集的长期保存与快速读取，写入不阻塞线上运行。" }
                                    span { class: "cap-meta", "columnar · indexed" }
                                }
                                article { class: "cap-card",
                                    span { class: "cap-ico",
                                        svg {
                                            view_box: "0 0 16 16",
                                            fill: "none",
                                            path {
                                                d: "M8 2v7m0 0l-2.6-2.6M8 9l2.6-2.6",
                                                stroke: "currentColor",
                                                stroke_width: "1.6",
                                                stroke_linecap: "round",
                                                stroke_linejoin: "round",
                                            }
                                            path {
                                                d: "M2.6 11.4v1.4c0 .55.45 1 1 1h8.8c.55 0 1-.45 1-1v-1.4",
                                                stroke: "currentColor",
                                                stroke_width: "1.5",
                                                stroke_linecap: "round",
                                            }
                                        }
                                    }
                                    h4 { "数据导入" }
                                    p { "支持已有历史轨迹与外部数据源批量导入，自动完成字段映射与 schema 校验，历史数据同样可检索。" }
                                    span { class: "cap-meta", "ingest · schema check" }
                                }
                                article { class: "cap-card",
                                    span { class: "cap-ico",
                                        svg {
                                            view_box: "0 0 16 16",
                                            fill: "none",
                                            circle { cx: "7", cy: "7", r: "4.4", stroke: "currentColor", stroke_width: "1.6" }
                                            path {
                                                d: "M10.4 10.4 14 14",
                                                stroke: "currentColor",
                                                stroke_width: "1.6",
                                                stroke_linecap: "round",
                                            }
                                        }
                                    }
                                    h4 { "检索与筛选" }
                                    p { "按任务、会话、时间、状态与标签组合过滤，也可用查询语言与 API 深入定位具体事件与字段。" }
                                    span { class: "cap-meta", "filter · SQL / API" }
                                }
                                article { class: "cap-card",
                                    span { class: "cap-ico",
                                        svg {
                                            view_box: "0 0 16 16",
                                            fill: "none",
                                            path {
                                                d: "M8 13.4V6.6m0 0L5.4 9.2M8 6.6l2.6 2.6",
                                                stroke: "currentColor",
                                                stroke_width: "1.6",
                                                stroke_linecap: "round",
                                                stroke_linejoin: "round",
                                            }
                                            path {
                                                d: "M2.6 4.6V3.2c0-.55.45-1 1-1h8.8c.55 0 1 .45 1 1v1.4",
                                                stroke: "currentColor",
                                                stroke_width: "1.5",
                                                stroke_linecap: "round",
                                            }
                                        }
                                    }
                                    h4 { "数据集导出" }
                                    p { "按筛选条件导出为训练与评测可用的数据集格式，支持字段裁剪与脱敏，导出任务异步执行不占用前台。" }
                                    span { class: "cap-meta", "export · async job" }
                                }
                                article { class: "cap-card",
                                    span { class: "cap-ico",
                                        svg {
                                            view_box: "0 0 16 16",
                                            fill: "none",
                                            path {
                                                d: "M8 2.2 13.6 5v6L8 13.8 2.4 11V5z",
                                                stroke: "currentColor",
                                                stroke_width: "1.5",
                                                stroke_linejoin: "round",
                                            }
                                            path {
                                                d: "M8 8v5.8M8 8 2.4 5m5.6 3 5.6-3",
                                                stroke: "currentColor",
                                                stroke_width: "1.3",
                                            }
                                        }
                                    }
                                    h4 { "只读数据服务" }
                                    p { "向下游分析、评测与训练任务提供稳定的只读查询接口，统一数据口径，避免各团队重复采集。" }
                                    span { class: "cap-meta", "read-only · shared" }
                                }
                                article { class: "cap-card",
                                    span { class: "cap-ico",
                                        svg {
                                            view_box: "0 0 16 16",
                                            fill: "none",
                                            path {
                                                d: "M2.5 8.2 6 11.6l7.5-7.4",
                                                stroke: "currentColor",
                                                stroke_width: "1.8",
                                                stroke_linecap: "round",
                                                stroke_linejoin: "round",
                                            }
                                        }
                                    }
                                    h4 { "数据质量校验" }
                                    p { "入库前后校验字段完整性与格式一致性，标注异常轨迹，保证下游拿到的是可用、可比的数据。" }
                                    span { class: "cap-meta", "validation" }
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

                    section {
                        class: "section",
                        id: "deploy",
                        style: "padding-top:0",
                        aria_labelledby: "deploy-title",
                        div { class: "section-head",
                            p { class: "section-kicker", "Deployment & Security" }
                            h2 { id: "deploy-title",
                                "部署在实验室内部，"
                                br {}
                                "数据不出内网。"
                            }
                        }
                        div { class: "deploy-grid",
                            article { class: "deploy-card",
                                h3 { "内网部署" }
                                p { "整套平台部署在实验室内部环境，轨迹数据不出内网，满足内部研发数据的管理要求。" }
                                ul { class: "deploy-list",
                                    li {
                                        svg {
                                            view_box: "0 0 16 16",
                                            fill: "none",
                                            path {
                                                d: "M2.5 8.2 6 11.6l7.5-7.4",
                                                stroke: "currentColor",
                                                stroke_width: "2",
                                                stroke_linecap: "round",
                                                stroke_linejoin: "round",
                                            }
                                        }
                                        span { "数据全程在内网流转，不经过外部服务" }
                                    }
                                    li {
                                        svg {
                                            view_box: "0 0 16 16",
                                            fill: "none",
                                            path {
                                                d: "M2.5 8.2 6 11.6l7.5-7.4",
                                                stroke: "currentColor",
                                                stroke_width: "2",
                                                stroke_linecap: "round",
                                                stroke_linejoin: "round",
                                            }
                                        }
                                        span { "支持按团队与项目隔离数据集" }
                                    }
                                }
                            }
                            article { class: "deploy-card",
                                h3 { "权限与审计" }
                                p { "以账号与角色控制数据可见范围，关键操作留存记录，敏感字段在导出环节统一脱敏处理。" }
                                ul { class: "deploy-list",
                                    li {
                                        svg {
                                            view_box: "0 0 16 16",
                                            fill: "none",
                                            path {
                                                d: "M2.5 8.2 6 11.6l7.5-7.4",
                                                stroke: "currentColor",
                                                stroke_width: "2",
                                                stroke_linecap: "round",
                                                stroke_linejoin: "round",
                                            }
                                        }
                                        span { "角色化权限，按需授权数据集访问" }
                                    }
                                    li {
                                        svg {
                                            view_box: "0 0 16 16",
                                            fill: "none",
                                            path {
                                                d: "M2.5 8.2 6 11.6l7.5-7.4",
                                                stroke: "currentColor",
                                                stroke_width: "2",
                                                stroke_linecap: "round",
                                                stroke_linejoin: "round",
                                            }
                                        }
                                        span { "导出字段可裁剪，敏感内容脱敏后离开平台" }
                                    }
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
