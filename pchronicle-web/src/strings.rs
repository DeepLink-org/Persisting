//! User-facing UI chrome strings for the pChronicle workbench.
//!
//! This branch defaults to Chinese. English originals live in each domain's
//! nested [`en`] module (mirroring [`crate::terminology`]) for a future locale
//! switch — there is no runtime language switch here.
//!
//! Prefer these constants (or helpers) over inline Chinese in `rsx!` so English
//! can return later. Product names (`pChronicle`, `Persisting`, `DeepTrace`,
//! `AgenticMD`, `FTS`, `Jieba`, `SQL`, `LLM`) stay untranslated. Status *values*
//! (`active` / `completed` / `failed`) stay English; use [`status::label`] for
//! visible Chinese labels. Keep `aria-label` attributes in English.

// English catalogs are retained for a future locale switch and may be unused
// while this branch defaults to Chinese.
#![allow(dead_code)]


/// User-facing strings for the `common` workbench surface.
pub mod common {
    /// English originals for a future locale switch.
    pub mod en {
        pub const LOADING: &str = "Loading…";
        pub const RETRY: &str = "Retry";
        pub const CANCEL: &str = "Cancel";
        pub const CLOSE: &str = "Close";
        pub const CLEAR: &str = "Clear";
        pub const SAVE: &str = "Save";
        pub const FIND: &str = "Find";
        pub const NEXT: &str = "Next →";
        pub const PREVIOUS: &str = "Previous";
        pub const NEXT_SIMPLE: &str = "Next";
        pub const COPY: &str = "Copy";
        pub const OPEN: &str = "Open";
        pub const DELETE: &str = "Delete";
        pub const ALL: &str = "All";
        pub const PAGE: &str = "Page";
        pub const SKIP_TO_MAIN: &str = "Skip to main content";
        pub const PUBLIC_ACCESS: &str = "Public access";
        pub const CATALOG: &str = "Catalog";
        pub const LOCAL_PREFIX: &str = "Local";
        pub const WORKING: &str = "Working…";
        pub const CHARACTERS: &str = "characters";
        pub const ROWS: &str = "rows";
        pub const COLUMNS: &str = "columns";
        pub const EVENTS: &str = "events";
        pub const TRUNCATED: &str = "truncated";
        pub const N_A: &str = "n/a";
        pub const JUST_NOW: &str = "just now";
    }

    pub const LOADING: &str = "加载中…";
    pub const RETRY: &str = "重试";
    pub const CANCEL: &str = "取消";
    pub const CLOSE: &str = "关闭";
    pub const CLEAR: &str = "清除";
    pub const SAVE: &str = "保存";
    pub const FIND: &str = "查找";
    pub const NEXT: &str = "下一页 →";
    pub const PREVIOUS: &str = "上一页";
    pub const NEXT_SIMPLE: &str = "下一页";
    pub const COPY: &str = "复制";
    pub const OPEN: &str = "打开";
    pub const DELETE: &str = "删除";
    pub const ALL: &str = "全部";
    pub const PAGE: &str = "页";
    pub const SKIP_TO_MAIN: &str = "跳到主要内容";
    pub const PUBLIC_ACCESS: &str = "公开访问";
    pub const CATALOG: &str = "目录";
    pub const LOCAL_PREFIX: &str = "本地";
    pub const WORKING: &str = "处理中…";
    pub const CHARACTERS: &str = "字符";
    pub const ROWS: &str = "行";
    pub const COLUMNS: &str = "列";
    pub const EVENTS: &str = "事件";
    pub const TRUNCATED: &str = "已截断";
    pub const N_A: &str = "无";
    pub const JUST_NOW: &str = "刚刚";
}

/// User-facing strings for the `status` workbench surface.
pub mod status {
    /// English originals for a future locale switch.
    pub mod en {
        pub const ALL_STATUSES: &str = "All statuses";
        pub const ACTIVE: &str = "Active";
        pub const COMPLETED: &str = "Completed";
        pub const FAILED: &str = "Failed";
        pub const CANCELLED: &str = "Cancelled";
        pub const RUNNING: &str = "Running";
        pub const UNKNOWN: &str = "Unknown";
        pub const IDLE: &str = "Idle";
    }

    pub const ALL_STATUSES: &str = "全部状态";
    pub const ACTIVE: &str = "进行中";
    pub const COMPLETED: &str = "已完成";
    pub const FAILED: &str = "失败";
    pub const CANCELLED: &str = "已取消";
    pub const RUNNING: &str = "运行中";
    pub const UNKNOWN: &str = "未知";
    pub const IDLE: &str = "空闲";

    /// Visible Chinese label for a status *value* (value itself stays English).
    pub fn label(value: &str) -> String {
        match value {
            "completed" | "ok" | "pass" => COMPLETED.to_string(),
            "failed" | "error" | "fail" => FAILED.to_string(),
            "active" => ACTIVE.to_string(),
            "cancelled" | "canceled" => CANCELLED.to_string(),
            "running" => RUNNING.to_string(),
            "all" => ALL_STATUSES.to_string(),
            other => other.to_string(),
        }
    }
}

/// User-facing strings for the `runs` workbench surface.
pub mod runs {
    /// English originals for a future locale switch.
    pub mod en {
        pub const PAGE_TITLE: &str = "Runs";
        pub const PAGE_SUBTITLE: &str = "Inspect agent execution, latency, tool use, and failures.";
        pub const RUN_PATHS: &str = "Run paths";
        pub const SEARCH_RESULTS_ON_PAGE: &str = "Search results on this page";
        pub const ALL_RUNS_IN_DATASET: &str = "All runs in this dataset";
        pub const TREE_BY_IMPORT_PATH: &str = "Tree by import path";
        pub const FLAT: &str = "Flat";
        pub const TREE: &str = "Tree";
        pub const SEARCH_RESULTS: &str = "Search results";
        pub const ALL_RUNS: &str = "All runs";
        pub const LOADING_PATHS: &str = "Loading paths…";
        pub const NO_CAPTURED_PATHS: &str = "No captured run paths.";
        pub const SHOWING_SEARCH_PAGE: &str = "Showing the current search page.";
        pub const SHOWING_ALL_RUNS: &str = "Showing all runs in this dataset.";
        pub const TREE_FOLLOWS_IMPORT: &str = "Tree follows the imported path.";
        pub const SEARCH_PLACEHOLDER: &str = "Search message body · Enter to search";
        pub const SEARCH_UNAVAILABLE: &str = "Search unavailable for this Dataset";
        pub const ALL_DATASETS: &str = "All Datasets";
        pub const SESSION: &str = "Session";
        pub const AGENT_MODEL: &str = "Agent / model";
        pub const STATUS_COL: &str = "Status";
        pub const EVENTS_COL: &str = "Events";
        pub const ROOT_COL: &str = "Root";
        pub const SORT_SESSION: &str = "Session";
        pub const SORT_EVENTS: &str = "Events";
        pub const SORT_STATUS: &str = "Status";
        pub const SORT_AGENT: &str = "Agent";
        pub const ASC: &str = "↑ Asc";
        pub const DESC: &str = "↓ Desc";
        pub const NO_MATCHING_RUNS: &str = "No matching runs";
        pub const NO_MATCHING_HINT: &str = "Adjust the filters or refresh the datasets.";
        pub const MATCH_PREFIX: &str = "Match: ";
        pub const COMPACT_JSONL: &str = "Compact JSONL";
        pub const RECORD_PREFIX: &str = "Record · ";
        pub const CAPTURED_ROWS: &str = "captured rows";
        pub const JSON_RECORD_ONE: &str = "1 JSON record";
        pub const MEMORY_FALLBACK: &str = "Memory fallback";
        pub const MEMORY_FILTER: &str = "Memory filter";
        pub const LOADING_RUN_DETAILS: &str = "Loading run details…";
        pub const OPEN_ASSISTANT_CHAT: &str = "Open Assistant chat for this run";
        pub const STEPS_TIMEOUT: &str = "Loading steps timed out. Open Requests to inspect server progress, then retry.";
        pub const STATS_TIMEOUT: &str = "Run statistics timed out. Steps remain available; open Requests to inspect progress.";
    }

    pub const PAGE_TITLE: &str = "运行";
    pub const PAGE_SUBTITLE: &str = "查看 Agent 执行、延迟、工具调用与失败。";
    pub const RUN_PATHS: &str = "运行路径";
    pub const SEARCH_RESULTS_ON_PAGE: &str = "本页搜索结果";
    pub const ALL_RUNS_IN_DATASET: &str = "此数据集中的全部运行";
    pub const TREE_BY_IMPORT_PATH: &str = "按导入路径树形展示";
    pub const FLAT: &str = "平铺";
    pub const TREE: &str = "树形";
    pub const SEARCH_RESULTS: &str = "搜索结果";
    pub const ALL_RUNS: &str = "全部运行";
    pub const LOADING_PATHS: &str = "正在加载路径…";
    pub const NO_CAPTURED_PATHS: &str = "暂无已捕获的运行路径。";
    pub const SHOWING_SEARCH_PAGE: &str = "显示当前搜索页。";
    pub const SHOWING_ALL_RUNS: &str = "显示此数据集中的全部运行。";
    pub const TREE_FOLLOWS_IMPORT: &str = "树形视图跟随导入路径。";
    pub const SEARCH_PLACEHOLDER: &str = "搜索消息正文 · 按 Enter 搜索";
    pub const SEARCH_UNAVAILABLE: &str = "此数据集不可用搜索";
    pub const ALL_DATASETS: &str = "全部数据集";
    pub const SESSION: &str = "会话";
    pub const AGENT_MODEL: &str = "Agent / 模型";
    pub const STATUS_COL: &str = "状态";
    pub const EVENTS_COL: &str = "事件";
    pub const ROOT_COL: &str = "根会话";
    pub const SORT_SESSION: &str = "会话";
    pub const SORT_EVENTS: &str = "事件";
    pub const SORT_STATUS: &str = "状态";
    pub const SORT_AGENT: &str = "Agent";
    pub const ASC: &str = "↑ 升序";
    pub const DESC: &str = "↓ 降序";
    pub const NO_MATCHING_RUNS: &str = "没有匹配的运行";
    pub const NO_MATCHING_HINT: &str = "调整筛选条件或刷新数据集。";
    pub const MATCH_PREFIX: &str = "匹配：";
    pub const COMPACT_JSONL: &str = "紧凑 JSONL";
    pub const RECORD_PREFIX: &str = "记录 · ";
    pub const CAPTURED_ROWS: &str = "已捕获行";
    pub const JSON_RECORD_ONE: &str = "1 条 JSON 记录";
    pub const MEMORY_FALLBACK: &str = "内存回退";
    pub const MEMORY_FILTER: &str = "内存筛选";
    pub const LOADING_RUN_DETAILS: &str = "正在加载运行详情…";
    pub const OPEN_ASSISTANT_CHAT: &str = "打开此运行的助手对话";
    pub const STEPS_TIMEOUT: &str = "加载步骤超时。打开「请求」查看服务端进度，然后重试。";
    pub const STATS_TIMEOUT: &str = "运行统计超时。步骤仍可用；打开「请求」查看进度。";
}

/// User-facing strings for the `detail` workbench surface.
pub mod detail {
    /// English originals for a future locale switch.
    pub mod en {
        pub const BACK_TO_RUNS: &str = "← Runs";
        pub const ASK_ASSISTANT: &str = "◇ Ask Assistant";
        pub const ANALYZE_THIS_RUN: &str = "Analyze this run";
        pub const JSON_RECORD: &str = "JSON record";
        pub const COMPACT_NO_SEMANTICS: &str = "Compact JSONL · no inferred step semantics";
        pub const LOADING_RECORD: &str = "Loading record…";
        pub const RECORD_UNAVAILABLE: &str = "Record unavailable";
        pub const LOADING_STATS: &str = "Loading run statistics…";
        pub const STATS_FAILED: &str = "Run statistics could not be loaded. Steps remain available.";
        pub const STATS_UNAVAILABLE: &str = "Run statistics are not available yet.";
        pub const TAB_TRACE: &str = "Timeline";
        pub const TAB_ANALYSIS: &str = "Analysis";
        pub const STEPS: &str = "Steps";
        pub const CONVERSATIONS: &str = "Conversations";
        pub const TIMELINE_HINT: &str = "Bars show each step's place in the run · colors show type · sequence is not wall-clock time · expand a row for details";
        pub const ALL_ROLES: &str = "All roles";
        pub const ROLE_USER: &str = "User";
        pub const ROLE_AGENT: &str = "Agent";
        pub const ROLE_SYSTEM: &str = "System";
        pub const SEARCH_STEPS_FTS: &str = "Search steps · FTS available (Jieba)";
        pub const SEARCH_STEPS_MEMORY: &str = "Search steps · memory filter";
        pub const LOADING_STEPS: &str = "Loading steps…";
        pub const NO_VISIBLE_STEPS: &str = "No visible steps";
        pub const NO_LOADED_STEPS_MATCH: &str = "No loaded steps match this filter.";
        pub const METRIC_STEPS: &str = "Steps";
        pub const METRIC_TOOLS: &str = "Tools";
        pub const METRIC_EXPLICIT_ERRORS: &str = "Explicit errors";
        pub const METRIC_TOKENS: &str = "Tokens";
        pub const METRIC_LATENCY_P95: &str = "Latency P95";
        pub const CAPTURED_SIGNALS_ONLY: &str = "Captured signals only";
        pub const TOOL_NAMES_SUFFIX: &str = "tool names";
        pub const COMPOSITION: &str = "Composition";
        pub const BEHAVIOR: &str = "Behavior";
        pub const MODELS: &str = "Models";
        pub const COVERAGE: &str = "Coverage";
        pub const NO_CAPTURED_VALUES: &str = "No captured values";
        pub const OVERVIEW: &str = "Overview";
        pub const PERFORMANCE: &str = "Performance";
        pub const TOKENS_TAB: &str = "Tokens";
        pub const TOOLS_TAB: &str = "Tools";
        pub const NO_CAPTURED_TIMESTAMPS: &str = "No captured timestamps";
        pub const EXECUTION_COMPOSITION: &str = "Execution composition";
        pub const EXECUTION_COMPOSITION_SUB: &str = "Steps grouped by recorded role";
        pub const BEHAVIOR_MIX: &str = "Behavior mix";
        pub const BEHAVIOR_MIX_SUB: &str = "Recorded step types";
        pub const MODEL_MIX: &str = "Model mix";
        pub const MODEL_MIX_SUB: &str = "Model usage and coverage by step";
        pub const DATA_COVERAGE: &str = "Data coverage";
        pub const DATA_COVERAGE_SUB: &str = "Available measurements and missing values";
        pub const LATENCY: &str = "Latency";
        pub const TIMESTAMP: &str = "Timestamp";
        pub const TOKEN_USAGE: &str = "Token usage";
        pub const CAPTURED_RUN_SPAN: &str = "Captured run span";
        pub const CAPTURED_RUN_SPAN_SUB: &str = "Lexically ordered source timestamps; unavailable values remain explicit";
        pub const LATENCY_DISTRIBUTION: &str = "Latency distribution";
        pub const LATENCY_DISTRIBUTION_SUB: &str = "Fixed buckets keep runs directly comparable";
        pub const PERCENTILE_PROFILE: &str = "Percentile profile";
        pub const PERCENTILE_PROFILE_SUB: &str = "Observed samples only";
        pub const STEP_DECODE_FAILED_TITLE: &str = "Step details could not be decoded";
        pub const STEP_FORMAT_MISMATCH: &str = "The step data did not match the expected format";
        pub const OPEN_ANALYSIS_CHART_TITLE: &str = "Open Analysis for the full chart";
        pub const LATENCY_BY_STEP: &str = "Latency by step";
        pub const LATENCY_BY_STEP_SUB: &str = "Ordered by sequence and normalized to the slowest loaded step · select a bar for details";
        pub const SLOWEST_STEPS: &str = "Slowest steps";
        pub const SLOWEST_STEPS_SUB: &str = "Highest observed end-to-end latency among the loaded steps";
        pub const NO_LATENCY_SAMPLES: &str = "No latency samples by step";
        pub const TOKEN_COMPOSITION: &str = "Token composition";
        pub const TOKEN_COMPOSITION_SUB: &str = "Captured prompt and completion usage across the run";
        pub const TOTAL_SUFFIX: &str = "total";
        pub const PROMPT: &str = "Prompt";
        pub const COMPLETION: &str = "Completion";
        pub const TOKENS_BY_STEP: &str = "Tokens by step";
        pub const TOKENS_BY_STEP_SUB: &str = "Prompt and completion tokens · select a bar for details";
        pub const TOKENS_BY_SOURCE: &str = "Tokens by source";
        pub const TOKENS_BY_SOURCE_SUB: &str = "Full-run aggregate by captured source";
        pub const TOKENS_BY_MODEL: &str = "Tokens by model";
        pub const TOKENS_BY_MODEL_SUB: &str = "Full-run aggregate by attributed model";
        pub const TOOL_PERFORMANCE: &str = "Tool performance";
        pub const TOOL_PERFORMANCE_SUB: &str = "Frequency, observed duration, and association with steps that reported errors";
        pub const TOOL_COL: &str = "Tool";
        pub const CALLS_COL: &str = "Calls";
        pub const OBSERVED_DURATION: &str = "Observed duration";
        pub const AVERAGE: &str = "Average";
        pub const MAX: &str = "Max";
        pub const ERROR_LINKED: &str = "Error-linked";
        pub const NO_TOOL_CALLS: &str = "No tool calls captured";
        pub const NO_VALUES: &str = "No values captured";
        pub const NO_TOKEN_ATTRIBUTION: &str = "No token attribution captured";
        pub const ACROSS_STEPS_FMT: &str = "Across {n} steps";
        pub const NO_TOKEN_SAMPLES: &str = "No token samples by step";
        pub const SAMPLE_NOTE_FMT: &str = "Showing an even sample of 120 from {n} loaded steps";
        pub const CLEAR_SEARCH: &str = "Clear search";
        pub const CLEAR_FILTER: &str = "Clear filter";
        pub const SEARCH_BODY_TITLE: &str = "Search message body. Use #all(...) for all fields or an explicit field/JSON filter. Press Enter to search";
        pub const ACTIVE_CATALOG_PROFILE: &str = "Active catalog profile";
    }

    pub const BACK_TO_RUNS: &str = "← 运行";
    pub const ASK_ASSISTANT: &str = "◇ 询问助手";
    pub const ANALYZE_THIS_RUN: &str = "分析此运行";
    pub const JSON_RECORD: &str = "JSON 记录";
    pub const COMPACT_NO_SEMANTICS: &str = "紧凑 JSONL · 无推断步骤语义";
    pub const LOADING_RECORD: &str = "正在加载记录…";
    pub const RECORD_UNAVAILABLE: &str = "记录不可用";
    pub const LOADING_STATS: &str = "正在加载运行统计…";
    pub const STATS_FAILED: &str = "运行统计加载失败。步骤仍可用。";
    pub const STATS_UNAVAILABLE: &str = "运行统计尚未可用。";
    pub const TAB_TRACE: &str = "时间线";
    pub const TAB_ANALYSIS: &str = "分析";
    pub const STEPS: &str = "步骤";
    pub const CONVERSATIONS: &str = "对话";
    pub const TIMELINE_HINT: &str = "条形显示各步骤在运行中的位置 · 颜色表示类型 · 序列非挂钟时间 · 展开行查看详情";
    pub const ALL_ROLES: &str = "全部角色";
    pub const ROLE_USER: &str = "用户";
    pub const ROLE_AGENT: &str = "Agent";
    pub const ROLE_SYSTEM: &str = "系统";
    pub const SEARCH_STEPS_FTS: &str = "搜索步骤 · FTS 可用（Jieba）";
    pub const SEARCH_STEPS_MEMORY: &str = "搜索步骤 · 内存筛选";
    pub const LOADING_STEPS: &str = "正在加载步骤…";
    pub const NO_VISIBLE_STEPS: &str = "没有可见步骤";
    pub const NO_LOADED_STEPS_MATCH: &str = "没有已加载的步骤匹配此筛选。";
    pub const METRIC_STEPS: &str = "步骤";
    pub const METRIC_TOOLS: &str = "工具";
    pub const METRIC_EXPLICIT_ERRORS: &str = "显式错误";
    pub const METRIC_TOKENS: &str = "Token";
    pub const METRIC_LATENCY_P95: &str = "延迟 P95";
    pub const CAPTURED_SIGNALS_ONLY: &str = "仅已捕获信号";
    pub const TOOL_NAMES_SUFFIX: &str = "个工具名";
    pub const COMPOSITION: &str = "构成";
    pub const BEHAVIOR: &str = "行为";
    pub const MODELS: &str = "模型";
    pub const COVERAGE: &str = "覆盖";
    pub const NO_CAPTURED_VALUES: &str = "无已捕获值";
    pub const OVERVIEW: &str = "概览";
    pub const PERFORMANCE: &str = "性能";
    pub const TOKENS_TAB: &str = "Token";
    pub const TOOLS_TAB: &str = "工具";
    pub const NO_CAPTURED_TIMESTAMPS: &str = "无已捕获时间戳";
    pub const EXECUTION_COMPOSITION: &str = "执行构成";
    pub const EXECUTION_COMPOSITION_SUB: &str = "按已记录角色分组的步骤";
    pub const BEHAVIOR_MIX: &str = "行为构成";
    pub const BEHAVIOR_MIX_SUB: &str = "已记录的步骤类型";
    pub const MODEL_MIX: &str = "模型构成";
    pub const MODEL_MIX_SUB: &str = "按步骤的模型使用与覆盖";
    pub const DATA_COVERAGE: &str = "数据覆盖";
    pub const DATA_COVERAGE_SUB: &str = "可用测量值与缺失值";
    pub const LATENCY: &str = "延迟";
    pub const TIMESTAMP: &str = "时间戳";
    pub const TOKEN_USAGE: &str = "Token 用量";
    pub const CAPTURED_RUN_SPAN: &str = "已捕获运行跨度";
    pub const CAPTURED_RUN_SPAN_SUB: &str = "按字典序排列的源时间戳；不可用值保持显式";
    pub const LATENCY_DISTRIBUTION: &str = "延迟分布";
    pub const LATENCY_DISTRIBUTION_SUB: &str = "固定分桶便于跨运行直接对比";
    pub const PERCENTILE_PROFILE: &str = "百分位概况";
    pub const PERCENTILE_PROFILE_SUB: &str = "仅观测样本";
    pub const STEP_DECODE_FAILED_TITLE: &str = "步骤详情无法解码";
    pub const STEP_FORMAT_MISMATCH: &str = "步骤数据与预期格式不符";
    pub const OPEN_ANALYSIS_CHART_TITLE: &str = "在分析中打开完整图表";
    pub const LATENCY_BY_STEP: &str = "按步骤的延迟";
    pub const LATENCY_BY_STEP_SUB: &str = "按序列排序并相对最慢已加载步骤归一化 · 选择条形查看详情";
    pub const SLOWEST_STEPS: &str = "最慢步骤";
    pub const SLOWEST_STEPS_SUB: &str = "已加载步骤中观测到的最高端到端延迟";
    pub const NO_LATENCY_SAMPLES: &str = "无按步骤的延迟样本";
    pub const TOKEN_COMPOSITION: &str = "Token 构成";
    pub const TOKEN_COMPOSITION_SUB: &str = "整个运行中捕获的提示与补全用量";
    pub const TOTAL_SUFFIX: &str = "合计";
    pub const PROMPT: &str = "提示";
    pub const COMPLETION: &str = "补全";
    pub const TOKENS_BY_STEP: &str = "按步骤的 Token";
    pub const TOKENS_BY_STEP_SUB: &str = "提示与补全 Token · 选择条形查看详情";
    pub const TOKENS_BY_SOURCE: &str = "按来源的 Token";
    pub const TOKENS_BY_SOURCE_SUB: &str = "按捕获来源的全运行汇总";
    pub const TOKENS_BY_MODEL: &str = "按模型的 Token";
    pub const TOKENS_BY_MODEL_SUB: &str = "按归因模型的全运行汇总";
    pub const TOOL_PERFORMANCE: &str = "工具性能";
    pub const TOOL_PERFORMANCE_SUB: &str = "频次、观测时长，以及与报错步骤的关联";
    pub const TOOL_COL: &str = "工具";
    pub const CALLS_COL: &str = "调用";
    pub const OBSERVED_DURATION: &str = "观测时长";
    pub const AVERAGE: &str = "平均";
    pub const MAX: &str = "最大";
    pub const ERROR_LINKED: &str = "关联错误";
    pub const NO_TOOL_CALLS: &str = "未捕获工具调用";
    pub const NO_VALUES: &str = "未捕获数值";
    pub const NO_TOKEN_ATTRIBUTION: &str = "未捕获 Token 归因";
    pub const ACROSS_STEPS_FMT: &str = "跨 {n} 个步骤";
    pub const NO_TOKEN_SAMPLES: &str = "无按步骤的 Token 样本";
    pub const SAMPLE_NOTE_FMT: &str = "从 {n} 个已加载步骤中均匀抽样显示 120 个";
    pub const CLEAR_SEARCH: &str = "清除搜索";
    pub const CLEAR_FILTER: &str = "清除筛选";
    pub const SEARCH_BODY_TITLE: &str = "搜索消息正文。使用 #all(...) 搜索全部字段，或使用显式字段/JSON 筛选。按 Enter 搜索";
    pub const ACTIVE_CATALOG_PROFILE: &str = "当前目录配置";

    pub fn across_steps(n: usize) -> String { format!("跨 {n} 个步骤") }
    pub fn sample_note(n: usize) -> String { format!("从 {n} 个已加载步骤中均匀抽样显示 120 个") }

    pub fn expected_received(expected: &str, received: &str) -> String {
        format!("期望 {expected}，实际收到 {received}")
    }

    pub fn step_decode_summary(turn_id: i64, detail: &str) -> String {
        format!("步骤 #{turn_id} · {detail}")
    }
}

/// User-facing strings for the `analysis` workbench surface.
pub mod analysis {
    /// English originals for a future locale switch.
    pub mod en {
        pub const ANALYZE: &str = "Analyze";
        pub const RUN: &str = "Run";
        pub const ASK: &str = "Ask";
        pub const WRITE_SQL: &str = "Write SQL";
        pub const COMPILED_SQL: &str = "Compiled SQL";
        pub const MANUAL_SQL: &str = "Manual SQL";
        pub const NEW_ANALYSIS: &str = "New analysis";
        pub const SQL_TABLES: &str = "SQL tables";
        pub const DATASETS_LOADING: &str = "Datasets are still loading.";
        pub const ASK_PLAIN_OR_SQL: &str = "Ask in plain language, or write SQL. Analysis creates a plan, generates a read-only query, and returns limited results.";
        pub const RUN_EXECUTES_QUERY: &str = "Run executes this query. Manual SQL is not repaired automatically.";
        pub const RECENT: &str = "Recent";
        pub const CLEAR_HISTORY_CONFIRM: &str = "Clear analysis history for these datasets?";
        pub const CLEAR_HISTORY: &str = "Clear history";
        pub const MODEL_SETTINGS: &str = "Model settings";
        pub const MANUALLY_EDITED: &str = "Manually edited";
        pub const DRAFT: &str = "Draft";
        pub const DATASETS_READY: &str = "Datasets ready";
        pub const LOADING_DATASETS: &str = "Loading datasets…";
        pub const READ_ONLY: &str = "Read-only";
        pub const QUESTION: &str = "Question";
        pub const QUESTION_PLACEHOLDER: &str = "Ask about runs, errors, latency, tool use, or model behavior…";
        pub const TRY_STARTING_POINT: &str = "Try a starting point";
        pub const CONNECT_MODEL: &str = "Connect a model for Analysis";
        pub const DRAFT_STAYS: &str = "Your draft stays here while you configure the endpoint.";
        pub const OPEN_MODEL_SETTINGS: &str = "Open model settings";
        pub const QUESTION_UNCHANGED: &str = "Your question is unchanged. Adjust it or Analyze again.";
        pub const PLAN_CREATE_FAILED: &str = "The analysis plan could not be created";
        pub const PLAN_INVALID: &str = "The model did not return a valid analysis plan.";
        pub const PLAN_FOR_PREVIOUS: &str = "This plan is for the previous question";
        pub const ANALYZE_AGAIN_OR_RESTORE: &str = "Analyze again for the current question, or restore the reviewed question.";
        pub const PLAN_FLOW_HINT: &str = "Analysis creates a plan, generates a read-only query, and returns limited results.";
        pub const FIX_SQL_AND_RUN: &str = "Fix the SQL and Run. Analyze will not repair a handwritten query.";
        pub const ANALYSIS_COULD_NOT_RUN: &str = "Analysis could not run";
        pub const RERUN_TO_RESTORE: &str = "Rerun to restore rows";
        pub const ROWS_NOT_STORED: &str = "Saved summaries remain visible, but result rows are never stored in browser history.";
        pub const PROCESS_TITLE: &str = "Analysis process";
        pub const PROCESS_SUB: &str = "Plan, generated SQL, query execution, and summary.";
        pub const PROCESS_EMPTY: &str = "Analyze or Run to view each processing step.";
        pub const PLAN_TITLE: &str = "Analysis plan";
        pub const PLAN_SUB: &str = "The generated SQL comes from this plan. Analyze repairs the plan when needed.";
        pub const INTENT: &str = "Intent";
        pub const ONE_ROW_PER: &str = "One row per";
        pub const MEASURE: &str = "Measure";
        pub const GROUP_BY: &str = "Group by";
        pub const OUTPUT: &str = "Output";
        pub const ASSUMPTIONS: &str = "Assumptions";
        pub const REVIEW_PLAN: &str = "Review the analysis plan";
        pub const PLAN_OUT_OF_DATE: &str = "This saved SQL is out of date. Analyze again to create a new plan.";
        pub const SCOPE: &str = "Scope";
        pub const FILTERS: &str = "Filters";
        pub const GROUPING: &str = "Grouping";
        pub const MEASURES: &str = "Measures";
        pub const RESULTS_TITLE: &str = "Analysis results";
        pub const RESULTS_SUB: &str = "Limited results returned by the confirmed query.";
        pub const NO_ROWS_MATCHED: &str = "No rows matched this plan";
        pub const REWRITE_OR_BROADEN: &str = "Rewrite the question or broaden the plan before trying again.";
        pub const HISTORY_CLEARED: &str = "Analysis history cleared for these datasets.";
        pub const INTERPRETATION_REMAINS: &str = "This saved interpretation remains available. Rerun to restore rows in Result Explorer.";
        pub const PLAN_FROM_DRAFT_ONLY: &str = "An analysis plan can only be generated from a draft, error, or stale version.";
        pub const NOT_WAITING_COMPILED_PLAN: &str = "This version is not waiting for a compiled analysis plan.";
        pub const NOT_WAITING_GENERATED_PLAN: &str = "This version is not waiting for a generated analysis plan.";
        pub const REVISE_PLAN_NOT_REPLAY: &str = "Revise the analysis plan instead of replaying the failed SQL.";
        pub const REVIEW_PLAN_BEFORE_RUN: &str = "Review a ready analysis plan before running this analysis.";
        pub const COMPILED_SQL_REQUIRED: &str = "Compiled SQL is required before running this analysis.";
        pub const SQL_RUN_REVIEWED_ONLY: &str = "SQL can only be run from a reviewed or completed version.";
        pub const SQL_REQUIRED: &str = "SQL is required before running this analysis.";
        pub const NOT_WAITING_QUERY_RESULTS: &str = "This version is not waiting for query results.";
        pub const NOT_WAITING_RESULT_SUMMARY: &str = "This version is not waiting for a result summary.";
        pub const RETRY_INTERP_ONLY_AFTER_ERROR: &str = "Interpretation can only be retried after an interpretation error.";
        pub const NO_LONGER_WAITING_RESULT: &str = "This version is no longer waiting for that result.";
        pub const FOLLOW_UP_REQUIRED: &str = "A follow-up question is required.";
        pub const ACTIVE_VERSION_UNAVAILABLE: &str = "The active analysis version is unavailable.";
        pub const SCOPE_LOCKED_RUNNING: &str = "Analysis scope cannot change while an operation is running.";
        pub const SCOPE_LOCKED_STATE: &str = "Analysis scope cannot change in this version state.";
        pub const INTERP_INTERRUPTED: &str = "Interpretation was interrupted when this analysis was left.";
        pub const SELECTED_VERSION_UNAVAILABLE: &str = "The selected analysis version is unavailable.";
        pub const PREPARE_SESSION_STORAGE: &str = "Could not prepare the analysis session for storage";
        pub const RESTORE_SESSIONS_STORAGE: &str = "Could not restore local analysis sessions from browser storage.";
        pub const RESTORE_SESSIONS_INVALID: &str = "Could not restore local analysis sessions because the saved data is invalid.";
        pub const SAVE_SESSIONS_FAILED: &str = "Could not save local analysis sessions; they will not survive a refresh.";
        pub const CLEAR_SESSIONS_FAILED: &str = "Could not clear local analysis sessions from browser storage.";
        pub const RESTORE_SESSION_INVALID: &str = "Could not restore the local analysis session because the saved data is invalid.";
        pub const LINK_NO_SCOPE: &str = "The Analysis link has no scope.";
        pub const LINK_INVALID_SCOPE: &str = "The Analysis link has an invalid scope.";
        pub const LINK_INCOMPLETE_SCOPE: &str = "The Analysis link has an incomplete scope.";
        pub const PREPARE_SESSIONS_STORAGE: &str = "Could not prepare analysis sessions for storage";
        pub const STEP_RESTORED_EARLY: &str = "Session restored before this step finished.";
        pub const HANDWRITTEN_SQL_PLAN: &str = "This session stored a handwritten SQL plan. Analyze again to create a compatible analysis plan.";
        pub const SESSION_EXCEEDS_BUDGET: &str = "Analysis session exceeds the local storage budget and could not be compacted.";
        pub const SESSIONS_UNAVAILABLE_OUTSIDE_BROWSER: &str = "Local analysis sessions are unavailable outside a browser.";
        pub const ACCESS_BROWSER_STORAGE_FAILED: &str = "Could not access browser storage for analysis sessions.";
        pub const BROWSER_STORAGE_UNAVAILABLE: &str = "Browser storage is unavailable for analysis sessions.";

        pub const SQL_EDIT_WHILE_RUNNING: &str = "SQL cannot be edited while an operation is running.";
        pub const SQL_EDIT_DRAFT_ONLY: &str = "SQL can only be edited in a draft or reviewed version.";
        pub const STARTER_1: &str = "Compare step counts per run by agent model";
        pub const STARTER_2: &str = "Show the distribution of step latency and the slowest 20 steps";
        pub const STARTER_3: &str = "Count tool calls by function name and drill into the busiest runs";
        pub const REMOVE_SCOPE: &str = "Remove scope";
        pub const RESTORE_REVIEWED: &str = "Restore reviewed question";
        pub const FIELDS_SUFFIX: &str = "fields";
        pub const NO_OBSERVATIONS: &str = "No direct observations were returned.";
        pub const NO_INFERENCE: &str = "No inference was offered from these results.";
        pub const NO_LIMITATIONS: &str = "No additional limitations were reported.";
        pub const NO_FOLLOWUPS: &str = "No follow-up questions were suggested.";
        pub const REWRITE_QUESTION: &str = "Rewrite question";
        pub const SUMMARIZING: &str = "Summarizing the returned results…";
        pub const SUMMARIZING_HINT: &str = "Results remain available while the model prepares a summary tied to the returned rows.";
        pub const RESULTS_NOT_SUMMARIZED: &str = "The results could not be summarized";
        pub const RETRY_PRESERVES_ROWS: &str = "Returned rows and profiles are preserved. Retrying does not rerun SQL.";
        pub const RETRY_INTERPRETATION: &str = "Retry interpretation";
        pub const SAVED_INTERPRETATION: &str = "Saved interpretation";
        pub const SAVED_INTERPRETATION_SUB: &str = "The summary was restored from this analysis session.";
        pub const ROWS_NOT_IN_BROWSER: &str = "Returned rows are not stored in the browser";
        pub const CREATING_PLAN: &str = "Creating plan";
        pub const PLAN_READY: &str = "Plan ready";
        pub const EXECUTING: &str = "Executing";
        pub const INTERPRETING: &str = "Interpreting";
        pub const COMPLETE: &str = "Complete";
        pub const PLAN_ERROR: &str = "Plan error";
        pub const RERUN_REQUIRED: &str = "Rerun required";
        pub const INTERPRETATION_ERROR: &str = "Interpretation error";
        pub const STALE: &str = "Stale";
        pub const CREATING_PLAN_ELLIPSIS: &str = "Creating plan…";
        pub const REPAIRING_PLAN_ELLIPSIS: &str = "Repairing plan…";
        pub const COMPILING_SQL_ELLIPSIS: &str = "Compiling SQL…";
        pub const EXECUTING_ELLIPSIS: &str = "Executing…";
        pub const INTERPRETING_ELLIPSIS: &str = "Interpreting…";
        pub const ANALYZING_ELLIPSIS: &str = "Analyzing…";
        pub const IN_PROGRESS: &str = "In progress";
        pub const TRACE_KIND_CREATE_PLAN: &str = "Create plan";
        pub const TRACE_KIND_COMPILE_SQL: &str = "Compile SQL";
        pub const TRACE_KIND_REPAIR_PLAN: &str = "Repair plan";
        pub const TRACE_KIND_EXECUTE: &str = "Execute";
        pub const TRACE_KIND_RUN_QUERY: &str = "Run query";
        pub const TRACE_KIND_INTERPRET: &str = "Interpret results";
        pub const TRACE_PENDING: &str = "pending";
        pub const TRACE_RUNNING: &str = "running";
        pub const TRACE_OK: &str = "ok";
        pub const TRACE_ERROR: &str = "error";
        pub const SEQUENCE: &str = "Sequence";
        pub const PROMPT: &str = "Prompt";
        pub const RESULT: &str = "Result";
        pub const ERROR: &str = "Error";
        pub const STEP_STILL_RUNNING: &str = "This step is still running.";
        pub const NOT_RUN: &str = "Not run";
        pub const NONE: &str = "None";
        pub const OBSERVED_IN_RESULT: &str = "Observed in this result";
        pub const POSSIBLE_EXPLANATION: &str = "Possible explanation";
        pub const COVERAGE_LIMITATIONS: &str = "Coverage and limitations";
        pub const CONTINUE_INVESTIGATING: &str = "Continue investigating";
        pub const FOLLOWUP_PAUSED: &str = "Follow-up planning is paused because the draft question changed. Restore the reviewed question or generate the edited draft.";
        pub const EDIT_QUESTION: &str = "Edit question";
        pub const STEP: &str = "Step";
        pub const DATASET_PREFIX: &str = "Dataset · ";
        pub const ROOT_PREFIX: &str = "Root · ";
        pub const RUN_PREFIX: &str = "Run · ";
        pub const WAIT_OPERATION: &str = "Wait for the current plan or query operation to finish";
        pub const SCOPE_REQUIRED: &str = "At least one explicit scope is required";
        pub const WAIT_DATASETS: &str = "Wait for the datasets to load";
        pub const PLAN_REQUIRED: &str = "An analysis plan is required before summarizing query results.";
        pub const QUERY_RESULTS_REQUIRED: &str = "Query results are required before creating a summary.";
        pub const RETRY_NO_INTERPRETATION: &str = "Retry did not prepare an interpretation operation.";
        pub const PLAN_UNAVAILABLE: &str = "The reviewed plan is unavailable for interpretation.";
        pub const QUERY_UNAVAILABLE_RERUN: &str = "Query results are unavailable; rerun the analysis first.";
        pub const MIN_AGO_FMT: &str = "{n} min ago";
        pub const HR_AGO_FMT: &str = "{n} hr ago";
        pub const DAYS_AGO_FMT: &str = "{n} days ago";
        pub const ROWS_COUNT_FMT: &str = "{n} rows";

    }

    pub const ANALYZE: &str = "分析";
    pub const RUN: &str = "运行";
    pub const ASK: &str = "提问";
    pub const WRITE_SQL: &str = "编写 SQL";
    pub const COMPILED_SQL: &str = "已编译 SQL";
    pub const MANUAL_SQL: &str = "手动 SQL";
    pub const NEW_ANALYSIS: &str = "新分析";
    pub const SQL_TABLES: &str = "SQL 表";
    pub const DATASETS_LOADING: &str = "数据集仍在加载。";
    pub const ASK_PLAIN_OR_SQL: &str = "可用自然语言提问，或手写 SQL。分析会生成计划与只读查询，并返回有限结果。";
    pub const RUN_EXECUTES_QUERY: &str = "「运行」会执行此查询。手动 SQL 不会自动修复。";
    pub const RECENT: &str = "最近";
    pub const CLEAR_HISTORY_CONFIRM: &str = "清除这些数据集的分析历史？";
    pub const CLEAR_HISTORY: &str = "清除历史";
    pub const MODEL_SETTINGS: &str = "模型设置";
    pub const MANUALLY_EDITED: &str = "已手动编辑";
    pub const DRAFT: &str = "草稿";
    pub const DATASETS_READY: &str = "数据集就绪";
    pub const LOADING_DATASETS: &str = "正在加载数据集…";
    pub const READ_ONLY: &str = "只读";
    pub const QUESTION: &str = "问题";
    pub const QUESTION_PLACEHOLDER: &str = "询问运行、错误、延迟、工具使用或模型行为…";
    pub const TRY_STARTING_POINT: &str = "试试这些起点";
    pub const CONNECT_MODEL: &str = "为分析连接模型";
    pub const DRAFT_STAYS: &str = "配置端点时，草稿会保留在此。";
    pub const OPEN_MODEL_SETTINGS: &str = "打开模型设置";
    pub const QUESTION_UNCHANGED: &str = "问题未更改。可调整后再次分析。";
    pub const PLAN_CREATE_FAILED: &str = "无法创建分析计划";
    pub const PLAN_INVALID: &str = "模型未返回有效的分析计划。";
    pub const PLAN_FOR_PREVIOUS: &str = "此计划对应上一个问题";
    pub const ANALYZE_AGAIN_OR_RESTORE: &str = "对当前问题再次分析，或恢复已审阅的问题。";
    pub const PLAN_FLOW_HINT: &str = "分析会生成计划与只读查询，并返回有限结果。";
    pub const FIX_SQL_AND_RUN: &str = "请修复 SQL 后运行。分析不会修复手写查询。";
    pub const ANALYSIS_COULD_NOT_RUN: &str = "分析无法运行";
    pub const RERUN_TO_RESTORE: &str = "重新运行以恢复行";
    pub const ROWS_NOT_STORED: &str = "已保存的摘要仍可见，但结果行不会存入浏览器历史。";
    pub const PROCESS_TITLE: &str = "分析过程";
    pub const PROCESS_SUB: &str = "计划、生成的 SQL、查询执行与摘要。";
    pub const PROCESS_EMPTY: &str = "进行分析或运行后可查看各处理步骤。";
    pub const PLAN_TITLE: &str = "分析计划";
    pub const PLAN_SUB: &str = "生成的 SQL 来自此计划。需要时分析会修复计划。";
    pub const INTENT: &str = "意图";
    pub const ONE_ROW_PER: &str = "每行对应";
    pub const MEASURE: &str = "度量";
    pub const GROUP_BY: &str = "分组";
    pub const OUTPUT: &str = "输出";
    pub const ASSUMPTIONS: &str = "假设";
    pub const REVIEW_PLAN: &str = "审阅分析计划";
    pub const PLAN_OUT_OF_DATE: &str = "已保存的 SQL 已过期。请再次分析以创建新计划。";
    pub const SCOPE: &str = "范围";
    pub const FILTERS: &str = "筛选";
    pub const GROUPING: &str = "分组";
    pub const MEASURES: &str = "度量";
    pub const RESULTS_TITLE: &str = "分析结果";
    pub const RESULTS_SUB: &str = "已确认查询返回的有限结果。";
    pub const NO_ROWS_MATCHED: &str = "没有行匹配该计划";
    pub const REWRITE_OR_BROADEN: &str = "请改写问题或放宽计划后再试。";
    pub const HISTORY_CLEARED: &str = "已清除这些数据集的分析历史。";
    pub const INTERPRETATION_REMAINS: &str = "已保存的解读仍然可用。重新运行可在结果探索器中恢复行。";
    pub const PLAN_FROM_DRAFT_ONLY: &str = "只能从草稿、错误或过期版本生成分析计划。";
    pub const NOT_WAITING_COMPILED_PLAN: &str = "此版本未在等待已编译的分析计划。";
    pub const NOT_WAITING_GENERATED_PLAN: &str = "此版本未在等待已生成的分析计划。";
    pub const REVISE_PLAN_NOT_REPLAY: &str = "请修订分析计划，而不是重放失败的 SQL。";
    pub const REVIEW_PLAN_BEFORE_RUN: &str = "运行分析前请先审阅就绪的分析计划。";
    pub const COMPILED_SQL_REQUIRED: &str = "运行分析前需要已编译的 SQL。";
    pub const SQL_RUN_REVIEWED_ONLY: &str = "只能从已审阅或已完成的版本运行 SQL。";
    pub const SQL_REQUIRED: &str = "运行分析前需要 SQL。";
    pub const NOT_WAITING_QUERY_RESULTS: &str = "此版本未在等待查询结果。";
    pub const NOT_WAITING_RESULT_SUMMARY: &str = "此版本未在等待结果摘要。";
    pub const RETRY_INTERP_ONLY_AFTER_ERROR: &str = "仅在解读出错后才能重试解读。";
    pub const NO_LONGER_WAITING_RESULT: &str = "此版本已不再等待该结果。";
    pub const FOLLOW_UP_REQUIRED: &str = "需要后续问题。";
    pub const ACTIVE_VERSION_UNAVAILABLE: &str = "当前分析版本不可用。";
    pub const SCOPE_LOCKED_RUNNING: &str = "操作进行中时无法更改分析范围。";
    pub const SCOPE_LOCKED_STATE: &str = "当前版本状态下无法更改分析范围。";
    pub const INTERP_INTERRUPTED: &str = "离开此分析时解读被中断。";
    pub const SELECTED_VERSION_UNAVAILABLE: &str = "所选分析版本不可用。";
    pub const PREPARE_SESSION_STORAGE: &str = "无法准备分析会话以写入存储";
    pub const RESTORE_SESSIONS_STORAGE: &str = "无法从浏览器存储恢复本地分析会话。";
    pub const RESTORE_SESSIONS_INVALID: &str = "无法恢复本地分析会话：保存的数据无效。";
    pub const SAVE_SESSIONS_FAILED: &str = "无法保存本地分析会话；刷新后将丢失。";
    pub const CLEAR_SESSIONS_FAILED: &str = "无法从浏览器存储清除本地分析会话。";
    pub const RESTORE_SESSION_INVALID: &str = "无法恢复本地分析会话：保存的数据无效。";
    pub const LINK_NO_SCOPE: &str = "分析链接没有范围。";
    pub const LINK_INVALID_SCOPE: &str = "分析链接的范围无效。";
    pub const LINK_INCOMPLETE_SCOPE: &str = "分析链接的范围不完整。";
    pub const PREPARE_SESSIONS_STORAGE: &str = "无法准备分析会话以写入存储";
    pub const STEP_RESTORED_EARLY: &str = "会话在此步骤完成前已恢复。";
    pub const HANDWRITTEN_SQL_PLAN: &str = "此会话保存了手写 SQL 计划。请再次分析以创建兼容的分析计划。";
    pub const SESSION_EXCEEDS_BUDGET: &str = "分析会话超出本地存储预算，无法压缩。";
    pub const SESSIONS_UNAVAILABLE_OUTSIDE_BROWSER: &str = "浏览器外无法使用本地分析会话。";
    pub const ACCESS_BROWSER_STORAGE_FAILED: &str = "无法访问分析会话的浏览器存储。";
    pub const BROWSER_STORAGE_UNAVAILABLE: &str = "分析会话的浏览器存储不可用。";

    pub const SQL_EDIT_WHILE_RUNNING: &str = "操作进行中时无法编辑 SQL。";
    pub const SQL_EDIT_DRAFT_ONLY: &str = "仅可在草稿或已审阅版本中编辑 SQL。";
    pub const STARTER_1: &str = "按 Agent 模型比较每次运行的步骤数";
    pub const STARTER_2: &str = "展示步骤延迟分布与最慢的 20 个步骤";
    pub const STARTER_3: &str = "按函数名统计工具调用并下钻到最繁忙的运行";
    pub const REMOVE_SCOPE: &str = "移除范围";
    pub const RESTORE_REVIEWED: &str = "恢复已审阅问题";
    pub const FIELDS_SUFFIX: &str = "字段";
    pub const NO_OBSERVATIONS: &str = "未返回直接观察结果。";
    pub const NO_INFERENCE: &str = "未从这些结果中给出推断。";
    pub const NO_LIMITATIONS: &str = "未报告其他限制。";
    pub const NO_FOLLOWUPS: &str = "未建议后续问题。";
    pub const REWRITE_QUESTION: &str = "改写问题";
    pub const SUMMARIZING: &str = "正在汇总已返回结果…";
    pub const SUMMARIZING_HINT: &str = "模型正在基于已返回行准备摘要时，结果仍可查看。";
    pub const RESULTS_NOT_SUMMARIZED: &str = "无法汇总这些结果";
    pub const RETRY_PRESERVES_ROWS: &str = "已返回行与概况仍保留。重试不会重新执行 SQL。";
    pub const RETRY_INTERPRETATION: &str = "重试解读";
    pub const SAVED_INTERPRETATION: &str = "已保存的解读";
    pub const SAVED_INTERPRETATION_SUB: &str = "摘要已从此分析会话恢复。";
    pub const ROWS_NOT_IN_BROWSER: &str = "已返回行不会存储在浏览器中";
    pub const CREATING_PLAN: &str = "正在创建计划";
    pub const PLAN_READY: &str = "计划就绪";
    pub const EXECUTING: &str = "正在执行";
    pub const INTERPRETING: &str = "正在解读";
    pub const COMPLETE: &str = "已完成";
    pub const PLAN_ERROR: &str = "计划错误";
    pub const RERUN_REQUIRED: &str = "需要重新运行";
    pub const INTERPRETATION_ERROR: &str = "解读错误";
    pub const STALE: &str = "已过期";
    pub const CREATING_PLAN_ELLIPSIS: &str = "正在创建计划…";
    pub const REPAIRING_PLAN_ELLIPSIS: &str = "正在修复计划…";
    pub const COMPILING_SQL_ELLIPSIS: &str = "正在编译 SQL…";
    pub const EXECUTING_ELLIPSIS: &str = "正在执行…";
    pub const INTERPRETING_ELLIPSIS: &str = "正在解读…";
    pub const ANALYZING_ELLIPSIS: &str = "正在分析…";
    pub const IN_PROGRESS: &str = "进行中";
    pub const TRACE_KIND_CREATE_PLAN: &str = "创建计划";
    pub const TRACE_KIND_COMPILE_SQL: &str = "编译 SQL";
    pub const TRACE_KIND_REPAIR_PLAN: &str = "修复计划";
    pub const TRACE_KIND_EXECUTE: &str = "执行";
    pub const TRACE_KIND_RUN_QUERY: &str = "运行查询";
    pub const TRACE_KIND_INTERPRET: &str = "解读结果";
    pub const TRACE_PENDING: &str = "等待中";
    pub const TRACE_RUNNING: &str = "运行中";
    pub const TRACE_OK: &str = "成功";
    pub const TRACE_ERROR: &str = "错误";
    pub const SEQUENCE: &str = "序列";
    pub const PROMPT: &str = "提示";
    pub const RESULT: &str = "结果";
    pub const ERROR: &str = "错误";
    pub const STEP_STILL_RUNNING: &str = "此步骤仍在运行。";
    pub const NOT_RUN: &str = "未运行";
    pub const NONE: &str = "无";
    pub const OBSERVED_IN_RESULT: &str = "本结果中的观察";
    pub const POSSIBLE_EXPLANATION: &str = "可能的解释";
    pub const COVERAGE_LIMITATIONS: &str = "覆盖范围与限制";
    pub const CONTINUE_INVESTIGATING: &str = "继续调查";
    pub const FOLLOWUP_PAUSED: &str = "因草稿问题已更改，后续规划已暂停。请恢复已审阅问题，或生成编辑后的草稿。";
    pub const EDIT_QUESTION: &str = "编辑问题";
    pub const STEP: &str = "步骤";
    pub const DATASET_PREFIX: &str = "数据集 · ";
    pub const ROOT_PREFIX: &str = "根 · ";
    pub const RUN_PREFIX: &str = "运行 · ";
    pub const WAIT_OPERATION: &str = "请等待当前计划或查询操作完成";
    pub const SCOPE_REQUIRED: &str = "至少需要一个显式范围";
    pub const WAIT_DATASETS: &str = "请等待数据集加载完成";
    pub const PLAN_REQUIRED: &str = "汇总查询结果前需要分析计划。";
    pub const QUERY_RESULTS_REQUIRED: &str = "创建摘要前需要查询结果。";
    pub const RETRY_NO_INTERPRETATION: &str = "重试未能准备解读操作。";
    pub const PLAN_UNAVAILABLE: &str = "已审阅计划不可用于解读。";
    pub const QUERY_UNAVAILABLE_RERUN: &str = "查询结果不可用；请先重新运行分析。";
    pub const MIN_AGO_FMT: &str = "{n} 分钟前";
    pub const HR_AGO_FMT: &str = "{n} 小时前";
    pub const DAYS_AGO_FMT: &str = "{n} 天前";
    pub const ROWS_COUNT_FMT: &str = "{n} 行";

    pub fn min_ago(n: u64) -> String { format!("{n} 分钟前") }
    pub fn hr_ago(n: u64) -> String { format!("{n} 小时前") }
    pub fn days_ago(n: u64) -> String { format!("{n} 天前") }
    pub fn rows_count(n: usize) -> String { format!("{n} 行") }

}

/// User-facing strings for the `notice` workbench surface.
pub mod notice {
    /// English originals for a future locale switch.
    pub mod en {
        pub const INVALID_REQUEST: &str = "This request isn't valid";
        pub const NOTHING_MATCHED: &str = "Nothing matched";
        pub const VIEW_OUT_OF_DATE: &str = "This view is out of date";
        pub const REFRESH_CATALOG: &str = "Refresh the catalog and try again";
        pub const NOT_SUPPORTED: &str = "This isn't supported";
        pub const RESULT_TOO_LARGE: &str = "The result is too large";
        pub const NARROW_QUERY: &str = "Narrow the query or lower the row limit";
        pub const SERVER_UNREACHABLE: &str = "The server isn't reachable";
        pub const CHECK_SERVE: &str = "Check that pchronicle serve is still running";
        pub const SOMETHING_WRONG: &str = "Something went wrong";
        pub const SERVER_LOG_CAUSE: &str = "The server log for this request ID has the cause";
        pub const REQUEST_FAILED: &str = "Request failed";
        pub const REQUEST_ID_PREFIX: &str = "Request ID ";
        pub const SHOW_TECHNICAL: &str = "Show technical details";
        pub const STEP_DECODE_SUMMARY_FMT: &str = "Step #{turn_id} · {detail}";
        pub const EXPECTED_RECEIVED_FMT: &str = "Expected {expected}, received {received}";
    }

    pub const INVALID_REQUEST: &str = "此请求无效";
    pub const NOTHING_MATCHED: &str = "没有匹配项";
    pub const VIEW_OUT_OF_DATE: &str = "此视图已过期";
    pub const REFRESH_CATALOG: &str = "请刷新目录后重试";
    pub const NOT_SUPPORTED: &str = "不支持此操作";
    pub const RESULT_TOO_LARGE: &str = "结果过大";
    pub const NARROW_QUERY: &str = "请缩小查询范围或降低行数限制";
    pub const SERVER_UNREACHABLE: &str = "无法连接服务器";
    pub const CHECK_SERVE: &str = "请确认 pchronicle serve 仍在运行";
    pub const SOMETHING_WRONG: &str = "出了点问题";
    pub const SERVER_LOG_CAUSE: &str = "请在此请求 ID 对应的服务端日志中查看原因";
    pub const REQUEST_FAILED: &str = "请求失败";
    pub const REQUEST_ID_PREFIX: &str = "请求 ID ";
    pub const SHOW_TECHNICAL: &str = "显示技术细节";
    pub const STEP_DECODE_SUMMARY_FMT: &str = "步骤 #{turn_id} · {detail}";
    pub const EXPECTED_RECEIVED_FMT: &str = "期望 {expected}，实际收到 {received}";
}

/// User-facing strings for the `catalog` workbench surface.
pub mod catalog {
    /// English originals for a future locale switch.
    pub mod en {
        pub const LOADING_CONTENTS: &str = "Loading directory contents…";
        pub const DIR_UNAVAILABLE_RETRY: &str = "Directory unavailable · Retrying automatically";
        pub const CACHED_REFRESH_FAILED: &str = "Showing cached view · Refresh failed; retrying automatically";
        pub const PARTIAL_VIEW: &str = "Partial view · Some directories have not been loaded";
        pub const CACHED_REFRESHING: &str = "Showing cached view · Refreshing in background";
        pub const CACHED_WAITING: &str = "Showing cached view · Waiting for refresh";
        pub const OPEN_IN_RUNS: &str = "Open in Runs";
        pub const LOADING_DATASETS: &str = "Loading datasets…";
        pub const IDENTITY_REQUIRED: &str = "Catalog identity required";
        pub const ADD_KEYS_HINT: &str = "Add an access key and secret key to browse this catalog.";
        pub const OPEN_KEYS: &str = "Open Keys";
        pub const DIR_UNAVAILABLE: &str = "Directory unavailable";
        pub const DIR_UNAVAILABLE_HINT: &str = "Retrying automatically. Check the storage connection if this persists.";
        pub const NO_DATASETS: &str = "No datasets";
        pub const NO_DATASETS_HINT: &str = "Add a dataset, then refresh this page.";
        pub const SOURCE_FILE: &str = "Source file";
        pub const SOURCE_FILE_HINT: &str = "This path contains one source file. Open it in Runs to inspect its runs.";
        pub const TRAJECTORIES: &str = "Trajectories";
        pub const DIRECTORY: &str = "Directory";
        pub const SOURCE: &str = "Source";
        pub const BROWSE_LIKE_LS: &str = "Browse the current path, like ls.";
        pub const SOURCE_LOAD_ERRORS_FMT: &str = "{errors} source files could not be loaded";
        pub const PREFIX_ITEMS_FMT: &str = "Prefix {prefix} · {count} items";
    }

    pub const LOADING_CONTENTS: &str = "正在加载目录内容…";
    pub const DIR_UNAVAILABLE_RETRY: &str = "目录不可用 · 正在自动重试";
    pub const CACHED_REFRESH_FAILED: &str = "显示缓存视图 · 刷新失败；正在自动重试";
    pub const PARTIAL_VIEW: &str = "部分视图 · 部分目录尚未加载";
    pub const CACHED_REFRESHING: &str = "显示缓存视图 · 正在后台刷新";
    pub const CACHED_WAITING: &str = "显示缓存视图 · 等待刷新";
    pub const OPEN_IN_RUNS: &str = "在运行中打开";
    pub const LOADING_DATASETS: &str = "正在加载数据集…";
    pub const IDENTITY_REQUIRED: &str = "需要目录身份";
    pub const ADD_KEYS_HINT: &str = "添加访问密钥与私密密钥以浏览此目录。";
    pub const OPEN_KEYS: &str = "打开密钥";
    pub const DIR_UNAVAILABLE: &str = "目录不可用";
    pub const DIR_UNAVAILABLE_HINT: &str = "正在自动重试。若持续如此，请检查存储连接。";
    pub const NO_DATASETS: &str = "暂无数据集";
    pub const NO_DATASETS_HINT: &str = "添加数据集后刷新此页。";
    pub const SOURCE_FILE: &str = "源文件";
    pub const SOURCE_FILE_HINT: &str = "此路径包含一个源文件。在运行中打开以检查其运行。";
    pub const TRAJECTORIES: &str = "轨迹";
    pub const DIRECTORY: &str = "目录";
    pub const SOURCE: &str = "源";
    pub const BROWSE_LIKE_LS: &str = "浏览当前路径，类似 ls。";
    pub const SOURCE_LOAD_ERRORS_FMT: &str = "{errors} 个源文件无法加载";
    pub const PREFIX_ITEMS_FMT: &str = "前缀 {prefix} · {count} 项";

    pub fn source_load_errors(errors: usize) -> String {
        format!("{errors} 个源文件无法加载")
    }

    pub fn prefix_items(prefix: &str, count: usize) -> String {
        format!("前缀 {prefix} · {count} 项")
    }
}

/// User-facing strings for the `physical` workbench surface.
pub mod physical {
    /// English originals for a future locale switch.
    pub mod en {
        pub const LANCE_SOURCES: &str = "Lance sources";
        pub const LOADING_SOURCES: &str = "Loading source files…";
        pub const NO_LANCE: &str = "No Lance sources";
        pub const NO_LANCE_HINT: &str = "Storage details are available for Lance datasets. Browse JSON and other files under Datasets or Runs.";
        pub const READING_GROUPS: &str = "Reading data groups…";
        pub const SELECT_SOURCE: &str = "Select a Lance source";
        pub const SELECT_HINT: &str = "Select a source, data group, file, and column to inspect stored values.";
        pub const DATA_GROUPS_FMT: &str = "{count} data groups";
        pub const COLUMNS_FMT: &str = "{count} columns";
        pub const MORE_COLUMNS_HIDDEN: &str = "more columns are hidden in this first-page inspector.";
        pub const READING_LAYOUT: &str = "Reading data file layout…";
        pub const NO_LAYOUT: &str = "No layout for this data file.";
        pub const CHOOSE_GROUP: &str = "Choose a data group";
        pub const CHOOSE_GROUP_HINT: &str = "The strip shows each data group's rows and size. Select a group or file, then select a column to inspect sample values.";
        pub const ROWS_UNKNOWN: &str = "rows unknown";
        pub const LARGEST_REVIEW: &str = "largest";
        pub const REVIEW: &str = "review";
        pub const VALUES: &str = "values";
        pub const SIZE: &str = "size";
        pub const LARGEST_STORED: &str = "Largest stored value";
        pub const PAGE_SAMPLE: &str = "Page sample";
        pub const LOADING_PAGE_VALUES: &str = "Loading page values…";
        pub const NO_SAMPLE: &str = "No sample for this column.";
        pub const DATA_GROUP: &str = "data group";
        pub const DATA_GROUP_DIST: &str = "Data group distribution";
        pub const BAR_WIDTH_NOTE: &str = "Bar width follows stored rows, or file size when the row count is unavailable.";
        pub const DELETIONS: &str = "deletions";
        pub const FIELD: &str = "field";
        pub const STORAGE: &str = "storage";
        pub const NEXT: &str = "Next";
        pub const PREV: &str = "Previous";

    }

    pub const LANCE_SOURCES: &str = "Lance 源";
    pub const LOADING_SOURCES: &str = "正在加载源文件…";
    pub const NO_LANCE: &str = "暂无 Lance 源";
    pub const NO_LANCE_HINT: &str = "存储详情适用于 Lance 数据集。请在数据集或运行中浏览 JSON 及其他文件。";
    pub const READING_GROUPS: &str = "正在读取数据组…";
    pub const SELECT_SOURCE: &str = "选择 Lance 源";
    pub const SELECT_HINT: &str = "选择源、数据组、文件与列以检查存储值。";
    pub const DATA_GROUPS_FMT: &str = "{count} 个数据组";
    pub const COLUMNS_FMT: &str = "{count} 列";
    pub const MORE_COLUMNS_HIDDEN: &str = "列在此首页检查器中被隐藏。";
    pub const READING_LAYOUT: &str = "正在读取数据文件布局…";
    pub const NO_LAYOUT: &str = "此数据文件无布局。";
    pub const CHOOSE_GROUP: &str = "选择数据组";
    pub const CHOOSE_GROUP_HINT: &str = "条带显示各数据组的行数与大小。选择组或文件，再选择列以检查样本值。";
    pub const ROWS_UNKNOWN: &str = "行数未知";
    pub const LARGEST_REVIEW: &str = "最大";
    pub const REVIEW: &str = "查看";
    pub const VALUES: &str = "值";
    pub const SIZE: &str = "大小";
    pub const LARGEST_STORED: &str = "最大存储值";
    pub const PAGE_SAMPLE: &str = "页样本";
    pub const LOADING_PAGE_VALUES: &str = "正在加载页值…";
    pub const NO_SAMPLE: &str = "此列无样本。";
    pub const DATA_GROUP: &str = "数据组";
    pub const DATA_GROUP_DIST: &str = "数据组分布";
    pub const BAR_WIDTH_NOTE: &str = "条宽按存储行数，行数不可用时按文件大小。";
    pub const DELETIONS: &str = "删除";
    pub const FIELD: &str = "字段";
    pub const STORAGE: &str = "存储";

    pub fn data_groups(count: usize) -> String {
        format!("{count} 个数据组")
    }

    pub fn columns(count: usize) -> String {
        format!("{count} 列")
    }
}

/// User-facing strings for the `requests` workbench surface.
pub mod requests {
    /// English originals for a future locale switch.
    pub mod en {
        pub const PAGE_TITLE: &str = "Requests";
        pub const PAGE_SUBTITLE: &str = "Inspect this browser’s recent requests, execution stages and failures. Server history is retained for up to 10 minutes.";
        pub const RUNNING_FMT: &str = "{n} running";
        pub const IDLE: &str = "Requests · Idle";
        pub const EXECUTION_STAGES: &str = "Execution stages";
        pub const STAGE: &str = "Stage";
        pub const STATUS: &str = "Status";
        pub const ELAPSED: &str = "Elapsed";
        pub const WORKER_EXECUTION: &str = "Worker execution";
        pub const FIND_PLACEHOLDER: &str = "Find by request ID";
        pub const NOT_IN_HISTORY: &str = "This request is not in this browser’s recent history.";
        pub const BROWSER_REQUEST: &str = "Browser request:";
        pub const RETRY_DIAGNOSTICS: &str = "Retry diagnostics";
        pub const SERVER_STATUS_FMT: &str = "Server: {state} · {ms} ms";
        pub const WAITING_DIAGNOSTICS: &str = "Waiting for server diagnostics…";
        pub const BROWSE_HINT: &str = "Browse a dataset or open Runs to inspect a request.";
        pub const COULD_NOT_REACH: &str = "Could not reach the server:";
        pub const DIAG_CONN_FAILED: &str = "Diagnostics connection failed";
        pub const DIAG_EXPIRED: &str = "Diagnostics expired or are unavailable; the original request may still be running";
        pub const DIAG_INVALID: &str = "Invalid diagnostics response";
        pub const DIAG_TIMEOUT: &str = "Diagnostics timed out; server status is unknown";
        pub const STAGE_AUTHENTICATION: &str = "Check catalog identity";
        pub const STAGE_WORKER_QUEUE: &str = "Wait for worker";
        pub const STAGE_WORKER_START: &str = "Start worker";
        pub const STAGE_WORKER_EXECUTION: &str = "Execute in worker";
        pub const STAGE_EXECUTION: &str = "Accept request";
        pub const STAGE_BROWSE_CACHE: &str = "Read directory cache";
        pub const STAGE_MANIFEST_SUMMARY: &str = "Read local manifest summaries";
        pub const STAGE_DIRECTORY_WAIT: &str = "Wait for directory listing";
        pub const STAGE_QUERY_QUEUE: &str = "Wait for query slot / shared result";
        pub const STAGE_SOURCE_METADATA: &str = "Resolve source metadata";
        pub const STAGE_STORAGE_READ: &str = "Read storage";
        pub const STAGE_QUERY: &str = "Build / execute query";
        pub const STAGE_CATALOG_WAIT: &str = "Wait for catalog refresh";
        pub const STAGE_RESPONSE: &str = "Prepare response";
    }

    pub const PAGE_TITLE: &str = "请求";
    pub const PAGE_SUBTITLE: &str = "检查此浏览器的近期请求、执行阶段与失败。服务端历史最多保留 10 分钟。";
    pub const RUNNING_FMT: &str = "{n} 个运行中";
    pub const IDLE: &str = "请求 · 空闲";
    pub const EXECUTION_STAGES: &str = "执行阶段";
    pub const STAGE: &str = "阶段";
    pub const STATUS: &str = "状态";
    pub const ELAPSED: &str = "耗时";
    pub const WORKER_EXECUTION: &str = "Worker 执行";
    pub const FIND_PLACEHOLDER: &str = "按请求 ID 查找";
    pub const NOT_IN_HISTORY: &str = "此请求不在本浏览器的近期历史中。";
    pub const BROWSER_REQUEST: &str = "浏览器请求：";
    pub const RETRY_DIAGNOSTICS: &str = "重试诊断";
    pub const SERVER_STATUS_FMT: &str = "服务端：{state} · {ms} ms";
    pub const WAITING_DIAGNOSTICS: &str = "正在等待服务端诊断…";
    pub const BROWSE_HINT: &str = "浏览数据集或打开运行以检查请求。";
    pub const COULD_NOT_REACH: &str = "无法连接服务器：";
    pub const DIAG_CONN_FAILED: &str = "诊断连接失败";
    pub const DIAG_EXPIRED: &str = "诊断已过期或不可用；原始请求可能仍在运行";
    pub const DIAG_INVALID: &str = "无效的诊断响应";
    pub const DIAG_TIMEOUT: &str = "诊断超时；服务端状态未知";
    pub const STAGE_AUTHENTICATION: &str = "检查目录身份";
    pub const STAGE_WORKER_QUEUE: &str = "等待 Worker";
    pub const STAGE_WORKER_START: &str = "启动 Worker";
    pub const STAGE_WORKER_EXECUTION: &str = "在 Worker 中执行";
    pub const STAGE_EXECUTION: &str = "接受请求";
    pub const STAGE_BROWSE_CACHE: &str = "读取目录缓存";
    pub const STAGE_MANIFEST_SUMMARY: &str = "读取本地清单摘要";
    pub const STAGE_DIRECTORY_WAIT: &str = "等待目录列表";
    pub const STAGE_QUERY_QUEUE: &str = "等待查询槽位 / 共享结果";
    pub const STAGE_SOURCE_METADATA: &str = "解析源元数据";
    pub const STAGE_STORAGE_READ: &str = "读取存储";
    pub const STAGE_QUERY: &str = "构建 / 执行查询";
    pub const STAGE_CATALOG_WAIT: &str = "等待目录刷新";
    pub const STAGE_RESPONSE: &str = "准备响应";

    pub fn running_count(n: usize) -> String {
        format!("{n} 个运行中")
    }

    pub fn stage_label(stage: &str) -> &str {
        match stage {
            "authentication" => STAGE_AUTHENTICATION,
            "worker_queue" => STAGE_WORKER_QUEUE,
            "worker_start" => STAGE_WORKER_START,
            "worker_execution" => STAGE_WORKER_EXECUTION,
            "execution" => STAGE_EXECUTION,
            "browse_cache" => STAGE_BROWSE_CACHE,
            "manifest_summary" => STAGE_MANIFEST_SUMMARY,
            "directory_wait" => STAGE_DIRECTORY_WAIT,
            "query_queue" => STAGE_QUERY_QUEUE,
            "source_metadata" => STAGE_SOURCE_METADATA,
            "storage_read" => STAGE_STORAGE_READ,
            "query" => STAGE_QUERY,
            "catalog_wait" => STAGE_CATALOG_WAIT,
            "response" => STAGE_RESPONSE,
            other => other,
        }
    }
}

/// User-facing strings for the `assistant` workbench surface.
pub mod assistant {
    /// English originals for a future locale switch.
    pub mod en {
        pub const OPEN_RUN_TO_CHAT: &str = "Open a run to start a chat.";
        pub const NEW_CHAT: &str = "New chat";
        pub const ASK_ASSISTANT: &str = "Ask Assistant";
        pub const OPEN_RUN_OR_HISTORY: &str = "Open a run, or select a previous chat from history.";
        pub const CAN_INSPECT: &str = "Assistant can inspect this analysis, examine a step, or run read-only SQL.";
        pub const PLACEHOLDER_NO_RUN: &str = "Open a run to start a chat…";
        pub const PLACEHOLDER_LOADING: &str = "Loading run details…";
        pub const PLACEHOLDER_ASK: &str = "Ask about this run…";
        pub const SEND: &str = "Send";
        pub const CONFIGURE_MODEL: &str = "Configure an OpenAI-compatible model in Settings before asking Assistant.";
        pub const MINUTES_AGO_FMT: &str = "{n}m ago";
        pub const HOURS_AGO_FMT: &str = "{n}h ago";
        pub const DAYS_AGO_FMT: &str = "{n}d ago";
        pub const READ_ONLY_SELECTED: &str = "Read-only · selected run data";
        pub const CHAT_HISTORY: &str = "Chat history";
        pub const NO_SAVED_CHATS: &str = "No saved chats yet.";
        pub const RESTORE_WIDTH: &str = "Restore Assistant width";
        pub const EXPAND_WIDTH: &str = "Expand Assistant to two-thirds of the screen";
        pub const STOP_FOLLOWING: &str = "Stop following new output";
        pub const JUMP_FOLLOW: &str = "Jump to latest output and follow";
        pub const RUNNING_ACTION: &str = "Running";
        pub const TOOL_OUTPUT: &str = "Tool output";
        pub const REFS_OUTSIDE: &str = "Referenced steps are outside the loaded data window.";
        pub const EXECUTED_SQL: &str = "Executed read-only SQL";
        pub const DATA_TRUNCATED: &str = "The available data was limited or truncated; conclusions may be incomplete.";
        pub const STEP_IN_CONTEXT: &str = "Step {id} in context";
        pub const CURRENT_RUN_CONTEXT: &str = "Current run in context";
        pub const UNABLE_ANALYSIS: &str = "Unable to complete analysis:";
    }

    pub const OPEN_RUN_TO_CHAT: &str = "打开一次运行以开始对话。";
    pub const NEW_CHAT: &str = "新对话";
    pub const ASK_ASSISTANT: &str = "询问助手";
    pub const OPEN_RUN_OR_HISTORY: &str = "打开一次运行，或从历史中选择之前的对话。";
    pub const CAN_INSPECT: &str = "助手可检查此分析、查看步骤或运行只读 SQL。";
    pub const PLACEHOLDER_NO_RUN: &str = "打开一次运行以开始对话…";
    pub const PLACEHOLDER_LOADING: &str = "正在加载运行详情…";
    pub const PLACEHOLDER_ASK: &str = "询问此次运行…";
    pub const SEND: &str = "发送";
    pub const CONFIGURE_MODEL: &str = "询问助手前，请先在设置中配置 OpenAI 兼容模型。";
    pub const MINUTES_AGO_FMT: &str = "{n} 分钟前";
    pub const HOURS_AGO_FMT: &str = "{n} 小时前";
    pub const DAYS_AGO_FMT: &str = "{n} 天前";
    pub const READ_ONLY_SELECTED: &str = "只读 · 已选运行数据";
    pub const CHAT_HISTORY: &str = "聊天历史";
    pub const NO_SAVED_CHATS: &str = "尚无已保存的对话。";
    pub const RESTORE_WIDTH: &str = "恢复助手宽度";
    pub const EXPAND_WIDTH: &str = "将助手扩展到屏幕三分之二宽";
    pub const STOP_FOLLOWING: &str = "停止跟随新输出";
    pub const JUMP_FOLLOW: &str = "跳到最新输出并跟随";
    pub const RUNNING_ACTION: &str = "正在运行";
    pub const TOOL_OUTPUT: &str = "工具输出";
    pub const REFS_OUTSIDE: &str = "引用的步骤不在已加载数据窗口内。";
    pub const EXECUTED_SQL: &str = "已执行的只读 SQL";
    pub const DATA_TRUNCATED: &str = "可用数据受限或已截断；结论可能不完整。";
    pub const STEP_IN_CONTEXT: &str = "上下文中的步骤 {id}";
    pub const CURRENT_RUN_CONTEXT: &str = "当前运行在上下文中";
    pub const UNABLE_ANALYSIS: &str = "无法完成分析：";

    pub fn step_in_context(id: i64) -> String {
        format!("上下文中的步骤 {id}")
    }

    pub fn running_action(action: &str) -> String {
        format!("正在运行 {action} ›")
    }

    pub fn relative_time(now: i64, then: i64) -> String {
        let delta = now.saturating_sub(then).max(0);
        if delta < 60_000 {
            crate::strings::common::JUST_NOW.into()
        } else if delta < 3_600_000 {
            format!("{} 分钟前", delta / 60_000)
        } else if delta < 86_400_000 {
            format!("{} 小时前", delta / 3_600_000)
        } else {
            format!("{} 天前", delta / 86_400_000)
        }
    }
}

/// User-facing strings for the `llm` workbench surface.
pub mod llm {
    /// English originals for a future locale switch.
    pub mod en {
        pub const BROWSER_SETTINGS: &str = "Browser settings";
        pub const KEYS_TITLE: &str = "Keys";
        pub const SETTINGS_NOTE: &str = "Catalog keys are sent to this pChronicle server as request headers so it can authorize queries. Assistant keys stay in this browser and are sent only to the OpenAI-compatible endpoint.";
        pub const CATALOG_IDENTITY: &str = "Catalog identity";
        pub const PROFILE: &str = "Profile";
        pub const NEW_PROFILE: &str = "＋ New profile";
        pub const PROFILE_NAME: &str = "Profile name";
        pub const PROFILE_PLACEHOLDER: &str = "e.g. Production";
        pub const ACCESS_KEY: &str = "Access key";
        pub const SECRET_KEY: &str = "Secret key";
        pub const ASSISTANT_MODEL: &str = "Assistant model";
        pub const API_BASE: &str = "API base";
        pub const API_KEY: &str = "API key";
        pub const MODEL: &str = "Model";
        pub const DELETE_PROFILE: &str = "Delete profile";
        pub const SAVE_LOCALLY: &str = "Save locally";
    }

    pub const BROWSER_SETTINGS: &str = "浏览器设置";
    pub const KEYS_TITLE: &str = "密钥";
    pub const SETTINGS_NOTE: &str = "目录密钥会作为请求头发送到此 pChronicle 服务器以授权查询。助手密钥仅保存在本浏览器，并只发送到 OpenAI 兼容端点。";
    pub const CATALOG_IDENTITY: &str = "目录身份";
    pub const PROFILE: &str = "配置";
    pub const NEW_PROFILE: &str = "＋ 新建配置";
    pub const PROFILE_NAME: &str = "配置名称";
    pub const PROFILE_PLACEHOLDER: &str = "例如：生产环境";
    pub const ACCESS_KEY: &str = "访问密钥";
    pub const SECRET_KEY: &str = "私密密钥";
    pub const ASSISTANT_MODEL: &str = "助手模型";
    pub const API_BASE: &str = "API 基址";
    pub const API_KEY: &str = "API 密钥";
    pub const MODEL: &str = "模型";
    pub const DELETE_PROFILE: &str = "删除配置";
    pub const SAVE_LOCALLY: &str = "本地保存";
}

/// User-facing strings for the `components` workbench surface.
pub mod components {
    /// English originals for a future locale switch.
    pub mod en {
        pub const QUERY_RESULT: &str = "Query result";
        pub const NO_ROWS: &str = "The query returned no rows.";
        pub const LIMITED_TO: &str = "Limited to";
        pub const SERVER_TRUNCATED: &str = "The server truncated this result before rendering.";
        pub const FULL_CELL_VALUE: &str = "Full cell value";
        pub const CONVERSATION: &str = "Conversation";
        pub const SYSTEM: &str = "System";
        pub const USER: &str = "User";
        pub const AGENT: &str = "Agent";
        pub const TOOL: &str = "tool";
        pub const TOOLS: &str = "tools";
        pub const NO_USER_STEP: &str = "No user step";
        pub const NO_TEXT: &str = "No text";
        pub const CONVERSATION_N_FMT: &str = "Conversation {n}";
        pub const PROMPT_FOR_FMT: &str = "Prompt for #{id}";
        pub const NO_VISIBLE_NOUN: &str = "No visible";
        pub const NO_LOADED_MATCH: &str = "No loaded";
        pub const MATCH_THIS_FILTER: &str = "match this filter.";
        pub const STRUCTURE: &str = "Structure";
        pub const OVERVIEW: &str = "Overview";
        pub const SEQUENCE_COVERAGE: &str = "Sequence / coverage";
        pub const DETAILS: &str = "Details";
        pub const RUN: &str = "run";
        pub const EVENT_REFERENCES: &str = "event references";
        pub const SEQUENCE_WINDOW: &str = "Sequence window";
        pub const REASONING: &str = "Reasoning";
        pub const OBSERVATION: &str = "Observation";
        pub const METRICS: &str = "Metrics";
        pub const STEP_N_FMT: &str = "Step {id}";
        pub const MODEL_PREFIX: &str = "Model";
        pub const LATENCY_PREFIX: &str = "Latency";
        pub const TTFT_PREFIX: &str = "TTFT";
        pub const CALL_PREFIX: &str = "Call";
        pub const SEQ_RANGE_FMT: &str = "seq {first}–{last}";
        pub const OPEN_FULL_CELL: &str = "Open full cell value";
        pub const LIMITED_TO_FMT: &str = "Limited to";
        pub const COLUMNS_HIDDEN_FMT: &str = "columns hidden";
        pub const RUN_COVERAGE_FMT: &str = "Run coverage · {n} steps";
        pub const OPEN_RUN_AGENTICMD: &str = "Open run as AgenticMD";
        pub const OPEN_CONVERSATION_AGENTICMD: &str = "Open conversation as AgenticMD";
        pub const STEPS_VISIBLE: &str = "Steps visible in the current list";
        pub const EXPANDED_STEP: &str = "Expanded step";
        pub const LOADING_FULL_STEP: &str = "Loading full step…";
        pub const DETAILS_UNAVAILABLE: &str = "Details are unavailable for this step.";
        pub const TOKEN_SPLIT: &str = "Token split";
        pub const MESSAGE: &str = "Message";
        pub const EXTRA: &str = "Extra";
        pub const STEP_DETAILS: &str = "Step details";
        pub const LOADING_STEP: &str = "Loading step…";
        pub const COLLAPSE: &str = "Collapse";
        pub const EXPAND_TRUNCATED: &str = "Expand truncated content";
        pub const STEP_LABEL: &str = "Step";
        pub const ROLE_LABEL: &str = "Role";
        pub const TYPE_LABEL: &str = "Type";
        pub const INITIAL_PROMPT: &str = "initial prompt";
        pub const IGNORED_TOOL_CALLS_FMT: &str = "Ignored {n} tool call(s) attached to a {source} turn.";
        pub const STEPS_SUFFIX: &str = "steps";
        pub const EVENTS_SUFFIX: &str = "events";
        pub const IN_OUT_FMT: &str = "{prompt} in · {completion} out";
        pub const ROWS_COLUMNS_FMT: &str = "{rows} rows · {cols} columns";
    }

    pub const QUERY_RESULT: &str = "查询结果";
    pub const NO_ROWS: &str = "查询未返回任何行。";
    pub const LIMITED_TO: &str = "限制为";
    pub const SERVER_TRUNCATED: &str = "服务端在渲染前截断了此结果。";
    pub const FULL_CELL_VALUE: &str = "完整单元格值";
    pub const CONVERSATION: &str = "对话";
    pub const SYSTEM: &str = "系统";
    pub const USER: &str = "用户";
    pub const AGENT: &str = "Agent";
    pub const TOOL: &str = "工具";
    pub const TOOLS: &str = "工具";
    pub const NO_USER_STEP: &str = "无用户步骤";
    pub const NO_TEXT: &str = "无文本";
    pub const CONVERSATION_N_FMT: &str = "对话 {n}";
    pub const PROMPT_FOR_FMT: &str = "#{id} 的提示";
    pub const NO_VISIBLE_NOUN: &str = "没有可见";
    pub const NO_LOADED_MATCH: &str = "没有已加载的";
    pub const MATCH_THIS_FILTER: &str = "匹配此筛选。";
    pub const STRUCTURE: &str = "结构";
    pub const OVERVIEW: &str = "概览";
    pub const SEQUENCE_COVERAGE: &str = "序列 / 覆盖";
    pub const DETAILS: &str = "详情";
    pub const RUN: &str = "运行";
    pub const EVENT_REFERENCES: &str = "事件引用";
    pub const SEQUENCE_WINDOW: &str = "序列窗口";
    pub const REASONING: &str = "推理";
    pub const OBSERVATION: &str = "观察";
    pub const METRICS: &str = "指标";
    pub const STEP_N_FMT: &str = "步骤 {id}";
    pub const MODEL_PREFIX: &str = "模型";
    pub const LATENCY_PREFIX: &str = "延迟";
    pub const TTFT_PREFIX: &str = "TTFT";
    pub const CALL_PREFIX: &str = "调用";
    pub const SEQ_RANGE_FMT: &str = "序列 {first}–{last}";
    pub const OPEN_FULL_CELL: &str = "打开完整单元格值";
    pub const LIMITED_TO_FMT: &str = "限制为";
    pub const COLUMNS_HIDDEN_FMT: &str = "列已隐藏";
    pub const RUN_COVERAGE_FMT: &str = "运行覆盖 · {n} 个步骤";
    pub const OPEN_RUN_AGENTICMD: &str = "以 AgenticMD 打开运行";
    pub const OPEN_CONVERSATION_AGENTICMD: &str = "以 AgenticMD 打开对话";
    pub const STEPS_VISIBLE: &str = "当前列表中可见的步骤";
    pub const EXPANDED_STEP: &str = "已展开步骤";
    pub const LOADING_FULL_STEP: &str = "正在加载完整步骤…";
    pub const DETAILS_UNAVAILABLE: &str = "此步骤详情不可用。";
    pub const TOKEN_SPLIT: &str = "Token 拆分";
    pub const MESSAGE: &str = "消息";
    pub const EXTRA: &str = "额外";
    pub const STEP_DETAILS: &str = "步骤详情";
    pub const LOADING_STEP: &str = "正在加载步骤…";
    pub const COLLAPSE: &str = "折叠";
    pub const EXPAND_TRUNCATED: &str = "展开截断内容";
    pub const STEP_LABEL: &str = "步骤";
    pub const ROLE_LABEL: &str = "角色";
    pub const TYPE_LABEL: &str = "类型";
    pub const INITIAL_PROMPT: &str = "初始提示";
    pub const IGNORED_TOOL_CALLS_FMT: &str = "已忽略附加到 {source} 轮次的 {n} 个工具调用。";
    pub const STEPS_SUFFIX: &str = "个步骤";
    pub const EVENTS_SUFFIX: &str = "个事件";
    pub const IN_OUT_FMT: &str = "输入 {prompt} · 输出 {completion}";
    pub const ROWS_COLUMNS_FMT: &str = "{rows} 行 · {cols} 列";

    pub fn run_coverage(n: usize) -> String {
        format!("运行覆盖 · {n} 个步骤")
    }

    pub fn ignored_tool_calls(n: usize, source: &str) -> String {
        format!("已忽略附加到 {source} 轮次的 {n} 个工具调用。")
    }

    pub fn limited_to(rows: usize, budget: &str) -> String {
        format!("限制为 {rows} 行 / {budget}")
    }

    pub fn columns_hidden(n: usize) -> String {
        format!("+{n} 列已隐藏")
    }

    pub fn rows_cols(rows: usize, cols: usize) -> String {
        format!("{rows} 行 · {cols} 列")
    }

    pub fn in_out(prompt: &str, completion: &str) -> String {
        format!("输入 {prompt} · 输出 {completion}")
    }

    pub fn conversation_n(n: usize) -> String {
        format!("对话 {n}")
    }

    pub fn prompt_for(id: i64) -> String {
        format!("#{id} 的提示")
    }

    pub fn format_tool_count(count: usize) -> String {
        if count == 1 {
            format!("{count} {TOOL}")
        } else {
            format!("{count} {TOOLS}")
        }
    }

    pub fn kind_label(kind: &str) -> &'static str {
        match kind {
            "chat" => CONVERSATION,
            "system" => SYSTEM,
            "user" => USER,
            _ => AGENT,
        }
    }

    pub fn rows_columns(rows: usize, cols: usize) -> String {
        format!("{rows} 行 · {cols} 列")
    }

    pub fn no_visible(noun: &str) -> String {
        format!("{NO_VISIBLE_NOUN}{noun}")
    }

    pub fn no_loaded_match(noun: &str) -> String {
        format!("{NO_LOADED_MATCH}{noun}{MATCH_THIS_FILTER}")
    }
}

/// User-facing strings for the `result` workbench surface.
pub mod result {
    /// English originals for a future locale switch.
    pub mod en {
        pub const EXPLORER_TITLE: &str = "Result Explorer";
        pub const NO_DISTRIBUTION: &str = "No distribution · 0 returned rows";
        pub const PREVIEW_DISTRIBUTION: &str = "Preview distribution";
        pub const DIST_ALL_ROWS: &str = "Distribution of all returned rows";
        pub const REFINEMENT_PAUSED: &str = "Refinement planning is paused";
        pub const REFINEMENT_HINT: &str = "Regenerate for the edited question, or restore the reviewed question to prepare a refinement.";
        pub const STAGED_REFINEMENT: &str = "Staged refinement";
        pub const NO_QUERY_UNCHANGED: &str = "No query has run and the current SQL is unchanged.";
        pub const APPLY_THROUGH_ASSISTANT: &str = "Apply through Assistant";
        pub const RESULT_LIMIT: &str = "Result limit";
        pub const RETURNED_TRUNCATED: &str = "Returned rows only; the server truncated this result.";
        pub const SELECTED_COLUMN: &str = "Selected column";
        pub const PRESENT: &str = "Present";
        pub const UNIQUE: &str = "Unique";
        pub const MISSING: &str = "Missing";
        pub const NO_DIST_AVAILABLE: &str = "No returned rows; no distribution is available.";
        pub const STAGE_MISSING: &str = "Stage missing values";
        pub const CREATE_FULL_DIST: &str = "Create full-distribution query";
        pub const ASSISTANT_DRAFT_HINT: &str = "Assistant will draft an aggregate plan for review. It will not run automatically.";
        pub const REGENERATE_BEFORE: &str = "Regenerate or restore the reviewed question before preparing this query.";
        pub const MINIMUM: &str = "Minimum";
        pub const MAXIMUM: &str = "Maximum";
        pub const MEAN: &str = "Mean";
        pub const MEDIAN: &str = "Median";
        pub const MIN_LENGTH: &str = "Minimum length";
        pub const MAX_LENGTH: &str = "Maximum length";
        pub const MEAN_LENGTH: &str = "Mean length";
        pub const MEDIAN_LENGTH: &str = "Median length";
        pub const EARLIEST: &str = "Earliest";
        pub const LATEST: &str = "Latest";
    }

    pub const EXPLORER_TITLE: &str = "结果探索器";
    pub const NO_DISTRIBUTION: &str = "无分布 · 已返回 0 行";
    pub const PREVIEW_DISTRIBUTION: &str = "预览分布";
    pub const DIST_ALL_ROWS: &str = "全部已返回行的分布";
    pub const REFINEMENT_PAUSED: &str = "细化规划已暂停";
    pub const REFINEMENT_HINT: &str = "请为编辑后的问题重新生成，或恢复已审阅问题以准备细化。";
    pub const STAGED_REFINEMENT: &str = "已暂存细化";
    pub const NO_QUERY_UNCHANGED: &str = "尚未运行查询，且当前 SQL 未更改。";
    pub const APPLY_THROUGH_ASSISTANT: &str = "通过助手应用";
    pub const RESULT_LIMIT: &str = "结果限制";
    pub const RETURNED_TRUNCATED: &str = "仅已返回行；服务端截断了此结果。";
    pub const SELECTED_COLUMN: &str = "所选列";
    pub const PRESENT: &str = "存在";
    pub const UNIQUE: &str = "唯一";
    pub const MISSING: &str = "缺失";
    pub const NO_DIST_AVAILABLE: &str = "无已返回行；无可用分布。";
    pub const STAGE_MISSING: &str = "暂存缺失值";
    pub const CREATE_FULL_DIST: &str = "创建完整分布查询";
    pub const ASSISTANT_DRAFT_HINT: &str = "助手将起草聚合计划供审阅，不会自动运行。";
    pub const REGENERATE_BEFORE: &str = "准备此查询前，请重新生成或恢复已审阅问题。";
    pub const MINIMUM: &str = "最小";
    pub const MAXIMUM: &str = "最大";
    pub const MEAN: &str = "均值";
    pub const MEDIAN: &str = "中位数";
    pub const MIN_LENGTH: &str = "最小长度";
    pub const MAX_LENGTH: &str = "最大长度";
    pub const MEAN_LENGTH: &str = "平均长度";
    pub const MEDIAN_LENGTH: &str = "中位长度";
    pub const EARLIEST: &str = "最早";
    pub const LATEST: &str = "最晚";
}

