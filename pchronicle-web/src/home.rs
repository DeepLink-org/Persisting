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
                    nav { class: "topbar",
                        div { class: "topbar-inner",
                            a {
                                class: "brand",
                                href: "#top",
                                aria_label: "DeepTrace 首页",
                                onclick: move |event| {
                                    event.prevent_default();
                                },
                                img {
                                    class: "brand-mark-img",
                                    src: "/assets/home/brand-mark-v9.png?v=2",
                                    alt: "DeepTrace",
                                    width: "32",
                                    height: "32",
                                }
                                span { class: "brand-text", "DeepTrace" }
                            }
                            button {
                                class: "header-login",
                                onclick: move |_| go_litefuse(),
                                "进入控制台"
                            }
                        }
                    }

                    header { class: "hero-wrap", id: "top",
                        div { class: "hero",
                            div { class: "eyebrow", "实验室内部 · 数据全程不出内网" }
                            h1 {
                                span { class: "grad-text", "Agent 观测与优化 · 轨迹平台" }
                            }
                            p { class: "hero-copy",
                                "面向 Agent 运行轨迹的统一数据底座，将每次运行对话、模型调用、工具调用与输入输出统一沉淀为结构化 Dataset，让运行记录可存储、可查询、可分析，为不同团队提供一套共同的数据基础。"
                            }
                            div { class: "hero-actions",
                                button {
                                    class: "secondary-btn",
                                    onclick: move |_| go_litefuse(),
                                    "进入控制台"
                                }
                                a {
                                    class: "ghost-btn",
                                    href: DOCS,
                                    target: "_blank",
                                    rel: "noreferrer",
                                    "查看文档 →"
                                }
                            }
                            div { class: "hero-points",
                                span { class: "hero-point",
                                    i { class: "dot indigo" }
                                    "内网部署 · 不经任何外部服务"
                                }
                                span { class: "hero-point",
                                    i { class: "dot violet" }
                                    "一次采集 · 观测分析导出复用"
                                }
                                span { class: "hero-point",
                                    i { class: "dot blue" }
                                    "角色权限 · 操作留痕可审计"
                                }
                            }
                        }
                    }

                    section { class: "section", id: "capabilities",
                        div { class: "section-center",
                            h2 { "平台能力" }
                            p { class: "section-sub", "四层能力覆盖 Agent 运行的完整生命周期" }
                        }
                        div { class: "card-grid card-grid-4",
                            article { class: "glass-card",
                                div { class: "card-ico indigo",
                                    svg {
                                        view_box: "0 0 24 24",
                                        fill: "none",
                                        stroke: "currentColor",
                                        stroke_width: "2",
                                        path {
                                            d: "M13 10V3L4 14h7v7l9-11h-7z",
                                            stroke_linecap: "round",
                                            stroke_linejoin: "round",
                                        }
                                    }
                                }
                                h3 {
                                    "实时追踪 "
                                    span { class: "card-en indigo", "Tracing" }
                                }
                                p { "以事件流捕获模型、检索、工具与外部 API 调用，串起 Session、Agent 图、延迟与成本上下文，运行现场始终可见。" }
                                div { class: "card-foot", "会话与任务状态汇总 · 高频失败 / 异常耗时 / 成本激增集中呈现" }
                            }
                            article { class: "glass-card",
                                div { class: "card-ico violet",
                                    svg {
                                        view_box: "0 0 24 24",
                                        fill: "none",
                                        stroke: "currentColor",
                                        stroke_width: "2",
                                        path {
                                            d: "M14.752 11.168l-3.197-2.132A1 1 0 0010 9.87v4.263a1 1 0 001.555.832l3.197-2.132a1 1 0 000-1.664z",
                                            stroke_linecap: "round",
                                            stroke_linejoin: "round",
                                        }
                                        circle { cx: "12", cy: "12", r: "9" }
                                    }
                                }
                                h3 {
                                    "轨迹回放 "
                                    span { class: "card-en violet", "Replay" }
                                }
                                p { "按会话、任务与分支逐步重建执行路径，逐节点查看输入、输出与中间结果；失败发生在哪一步，一眼可见。" }
                                div { class: "card-foot", "任意节点定位 · 时间线对照延迟与 Token 分布" }
                            }
                            article { class: "glass-card",
                                div { class: "card-ico blue",
                                    svg {
                                        view_box: "0 0 24 24",
                                        fill: "none",
                                        stroke: "currentColor",
                                        stroke_width: "2",
                                        ellipse { cx: "12", cy: "6", rx: "8", ry: "3" }
                                        path {
                                            d: "M4 6v12c0 1.66 3.58 3 8 3s8-1.34 8-3V6",
                                            stroke_linecap: "round",
                                            stroke_linejoin: "round",
                                        }
                                        path {
                                            d: "M4 12c0 1.66 3.58 3 8 3s8-1.34 8-3",
                                            stroke_linecap: "round",
                                            stroke_linejoin: "round",
                                        }
                                    }
                                }
                                h3 {
                                    "数据集 "
                                    span { class: "card-en blue", "Datasets" }
                                }
                                p { "轨迹按列式结构存储并建立索引，支持批量导入、组合检索与中文全文检索；筛选、脱敏、导出为训练/评测数据集。" }
                                div { class: "card-foot", "统一只读服务 · 各组不重复采集 · 入库即校验" }
                            }
                            article { class: "glass-card",
                                div { class: "card-ico fuchsia",
                                    svg {
                                        view_box: "0 0 24 24",
                                        fill: "none",
                                        stroke: "currentColor",
                                        stroke_width: "2",
                                        path {
                                            d: "M8 10h.01M12 10h.01M16 10h.01M9 16H5a2 2 0 01-2-2V6a2 2 0 012-2h14a2 2 0 012 2v8a2 2 0 01-2 2h-5l-5 5v-5z",
                                            stroke_linecap: "round",
                                            stroke_linejoin: "round",
                                        }
                                    }
                                }
                                h3 {
                                    "AI 分析 "
                                    span { class: "card-en fuchsia", "Copilot" }
                                }
                                p { "自然语言提问，自动生成分析计划与只读 SQL 并解读结果；内置助手可检查运行，回答「为什么慢 / 错在哪一步」。" }
                                div { class: "card-foot", "计划 → SQL → 执行 → 解读，全程透明可复查" }
                            }
                        }
                    }

                    section { class: "section section-tight",
                        div { class: "section-center",
                            h2 { "一条运行链，在线与离线两段能力" }
                            p { class: "section-sub wide",
                                "同一份轨迹数据，在线侧负责"
                                b { "看得见" }
                                "，离线侧负责"
                                b { "用得上" }
                                "。"
                                br {}
                                "两侧共用一套采集规范与数据口径，不需要为分析再做一次对接。"
                            }
                        }
                        div { class: "split-grid",
                            article { class: "glass-panel",
                                div { class: "panel-bar indigo" }
                                span { class: "tag indigo", "Online · 正在发生的运行" }
                                h3 { "在线：观测与回放" }
                                p { class: "panel-lead", "先看见，再定位。" }
                                ul { class: "panel-list indigo",
                                    li {
                                        span { class: "bullet", "·" }
                                        div {
                                            b { "实时轨迹采集" }
                                            span { "事件流捕获模型、检索、工具与外部 API 调用，串起 Session、Agent 图、延迟与成本上下文。" }
                                        }
                                    }
                                    li {
                                        span { class: "bullet", "·" }
                                        div {
                                            b { "轨迹回放" }
                                            span { "按会话、任务与分支逐步重建执行路径，从输入到输出逐节点查看，随时定位失败与异常的位置。" }
                                        }
                                    }
                                    li {
                                        span { class: "bullet", "·" }
                                        div {
                                            b { "实时监控视图" }
                                            span { "会话与任务维度的运行状态汇总，高频失败、异常耗时和激增的成本集中在一个视图里。" }
                                        }
                                    }
                                }
                            }
                            article { class: "glass-panel",
                                div { class: "panel-bar cyan" }
                                span { class: "tag blue", "Offline · 已经发生的运行" }
                                h3 { "离线：存储、检索与导出" }
                                p { class: "panel-lead", "先沉淀，再用起来。" }
                                ul { class: "panel-list blue",
                                    li {
                                        span { class: "bullet", "·" }
                                        div {
                                            b { "高性能存储与导入" }
                                            span { "列式结构 + 索引，大规模数据集长期保存、快速读取；历史轨迹批量导入，自动 schema 校验。" }
                                        }
                                    }
                                    li {
                                        span { class: "bullet", "·" }
                                        div {
                                            b { "检索与筛选" }
                                            span { "按任务、会话、时间、状态与标签组合过滤，查询语言与 API 深入定位事件与字段。" }
                                        }
                                    }
                                    li {
                                        span { class: "bullet", "·" }
                                        div {
                                            b { "导出与只读服务" }
                                            span { "筛选导出为训练/评测格式，支持字段裁剪与脱敏；下游经统一只读接口取数，各组不重复采集。" }
                                        }
                                    }
                                }
                            }
                        }
                    }

                    section { class: "section section-tight", id: "scenarios",
                        div { class: "section-center",
                            h2 { "面向实验室的三类高频场景" }
                        }
                        div { class: "card-grid card-grid-3",
                            article { class: "glass-card",
                                span { class: "tag indigo", "场景 · 复盘" }
                                h3 { "Agent调用失败复盘" }
                                p { "任务报错，不用逐台机器翻日志——打开运行回放，沿分支找到第一个失败节点，直接查看该步的输入输出与工具调用。" }
                                div { class: "card-foot", "轨迹回放 · 逐节点定位 · 失败步上下文" }
                            }
                            article { class: "glass-card",
                                span { class: "tag violet", "场景 · 对比" }
                                h3 { "Agent 版本对比" }
                                p { "两组 Prompt / 模型改动谁更优？在同一数据集上对比延迟、成本、错误率与 Token 归因，用结构化结果代替争论。" }
                                div { class: "card-foot", "数据集对比 · 延迟 P95 · Token 按来源/模型归因" }
                            }
                            article { class: "glass-card",
                                span { class: "tag blue", "场景 · 沉淀" }
                                h3 { "训练与评测数据沉淀" }
                                p { "把不良 Case 按条件筛出、脱敏、导出为训练/评测格式；入库即校验，下游拿到的都是可比、可用的数据。" }
                                div { class: "card-foot", "组合筛选 · 字段脱敏 · 异步导出" }
                            }
                        }
                    }

                    section { class: "section section-tight", id: "security",
                        div { class: "security-panel",
                            div { class: "panel-bar indigo" }
                            h2 { "部署在实验室内部，数据不出内网" }
                            p { class: "security-lead",
                                "数据全程在内网流转，"
                                b { "不经过任何外部服务" }
                                "。"
                            }
                            div { class: "security-grid",
                                div {
                                    div { class: "security-head",
                                        svg {
                                            view_box: "0 0 24 24",
                                            fill: "none",
                                            stroke: "currentColor",
                                            stroke_width: "2",
                                            path {
                                                d: "M3 12l2 2m0-4l4-4m0 0l6-2m-6 2l-2 4m0 6l-4-4m18-2l-6 6m0 0l-4 2m4-2l2 2",
                                                stroke_linecap: "round",
                                                stroke_linejoin: "round",
                                            }
                                        }
                                        h3 { "内网部署" }
                                    }
                                    ul { class: "panel-list indigo",
                                        li {
                                            span { class: "bullet", "·" }
                                            div { span { "整套平台部署在实验室内部环境，满足内部研发数据管理要求" } }
                                        }
                                        li {
                                            span { class: "bullet", "·" }
                                            div { span { "支持按团队与项目隔离数据集" } }
                                        }
                                    }
                                }
                                div {
                                    div { class: "security-head violet",
                                        svg {
                                            view_box: "0 0 24 24",
                                            fill: "none",
                                            stroke: "currentColor",
                                            stroke_width: "2",
                                            path {
                                                d: "M9 12l2 2 4-4m5.618-4.016A11.955 11.955 0 0112 2.944a11.955 11.955 0 01-8.618 3.04A12.02 12.02 0 003 9c0 5.591 3.824 10.29 9 11.622 5.176-1.332 9-6.03 9-11.622 0-1.042-.133-2.052-.382-3.016z",
                                                stroke_linecap: "round",
                                                stroke_linejoin: "round",
                                            }
                                        }
                                        h3 { "权限与审计" }
                                    }
                                    ul { class: "panel-list violet",
                                        li {
                                            span { class: "bullet", "·" }
                                            div { span { "以账号与角色控制数据可见范围，关键操作留存记录" } }
                                        }
                                        li {
                                            span { class: "bullet", "·" }
                                            div { span { "导出字段可裁剪，敏感内容脱敏后才离开平台" } }
                                        }
                                    }
                                }
                            }
                        }
                    }

                    section { class: "section", id: "quickstart",
                        div { class: "section-center",
                            h2 { "三步接入，轨迹自动开始沉淀" }
                            p { class: "section-sub", "一次配置，无需改造业务代码；具体命令以使用文档为准" }
                        }
                        div { class: "card-grid card-grid-3",
                            article { class: "glass-card",
                                div { class: "num-dot", "1" }
                                h3 { "配置密钥" }
                                p { "在设置中创建访问密钥；密钥仅保存在本地浏览器，只发送到内网端点。" }
                            }
                            article { class: "glass-card",
                                div { class: "num-dot", "2" }
                                h3 { "挂载采集" }
                                p { "在 Agent 运行侧挂载事件流采集，同一套采集规范同时服务在线观测与离线数据集。" }
                            }
                            article { class: "glass-card",
                                div { class: "num-dot", "3" }
                                h3 { "打开控制台" }
                                p { "进入控制台查看运行与状态汇总，回放轨迹、检索数据集、向助手发起分析。" }
                            }
                        }
                    }

                    section { class: "section section-cta",
                        div { class: "cta-banner",
                            h2 { "把每一次运行，变成实验室的复用资产" }
                            p { "在线看得见，离线用得上——数据全程不出内网。" }
                            div { class: "cta-actions",
                                button {
                                    class: "cta-secondary",
                                    onclick: move |_| go_litefuse(),
                                    "进入控制台"
                                }
                            }
                        }
                    }

                    footer { class: "footer",
                        span { "DeepTrace · pChronicle × LiteFuse — 实验室统一的 Agent 轨迹平台" }
                        span { "Internal workspace" }
                    }
                }
            }
        }
    }
}
