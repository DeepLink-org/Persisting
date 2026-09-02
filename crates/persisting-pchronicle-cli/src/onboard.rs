use std::fmt::Write as _;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Args, Subcommand};

use super::{
    AnalysisArgs, AnalysisCommand, AnalysisOptions, DefaultArgs, DefaultCommand, ErrorMode,
    ExchangeFormat, ExportArgs, ExportFormat, FindArgs, ImportArgs, ImportMode, ImportOutputFormat,
    ListArgs, OutputFormat, QueryArgs, QueryOutputFormat, StatusArgs, run_analysis, run_default,
    run_export, run_find, run_import, run_list, run_query, run_status,
};

const DEMO_ATIF: &str = include_str!("../assets/onboard/support-ticket.json");
const DEMO_ACTF: &str = include_str!("../assets/onboard/code-repair.actf.json");
const DEMO_OPENAI: &str = include_str!("../assets/onboard/training.json");
const MAX_FILES: usize = persisting_pchronicle::storage::DEFAULT_MAX_LOCAL_QUERY_FILES;
const MAX_ENTRIES: usize = persisting_pchronicle::storage::DEFAULT_MAX_LOCAL_QUERY_ENTRIES;
const STEP_SAMPLE_SQL: &str = r#"SELECT session_id, step_id, source, model_name, message_kind, message_value
FROM dataset.steps
ORDER BY session_id, step_id
LIMIT 10"#;
const TOOL_SAMPLE_SQL: &str = r#"SELECT session_id, step_id, function_name, arguments
FROM dataset.tool_calls
ORDER BY session_id, step_id, call_index
LIMIT 10"#;
const FIND_TARGET_SQL: &str = r#"SELECT session_id, step_id
FROM dataset.steps
ORDER BY session_id, step_id
LIMIT 1"#;
const CROSS_FORMAT_SQL: &str = r#"SELECT 'actf' AS dataset_name, session_id, COUNT(*) AS steps
FROM actf.steps
GROUP BY session_id
UNION ALL
SELECT 'atif' AS dataset_name, session_id, COUNT(*) AS steps
FROM atif.steps
GROUP BY session_id
UNION ALL
SELECT 'openai' AS dataset_name, session_id, COUNT(*) AS steps
FROM openai.steps
GROUP BY session_id
ORDER BY dataset_name, session_id"#;

#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub(crate) struct OnboardArgs {
    /// Print the complete walkthrough without waiting between steps.
    #[arg(long, global = true)]
    no_pause: bool,

    /// Explore this Dataset through the complete walkthrough.
    #[arg(value_name = "DATASET_URI")]
    dataset_uri: Option<String>,

    #[command(subcommand)]
    section: Option<OnboardSection>,
}

#[derive(Debug, Subcommand)]
enum OnboardSection {
    /// Run every onboarding section.
    All(DatasetSectionArgs),
    /// Learn the Dataset, Source, Snapshot, and normalized-table concepts.
    Concepts,
    /// Discover Sources and inspect Dataset health.
    Inspect(DatasetSectionArgs),
    /// Run overview and tool-usage analyses.
    Analyze(DatasetSectionArgs),
    /// Inspect the schema and run focused SQL queries.
    Query(DatasetSectionArgs),
    /// Query ATIF, ACTF, and OpenAI Messages together.
    Formats,
    /// Locate Steps with Source-local IDs, FTS, or JSONB expressions.
    Find(DatasetSectionArgs),
    /// Configure an isolated Warehouse and perform strict import/export.
    #[command(visible_alias = "import-export")]
    Exchange,
    /// Learn how to configure the read-only Web/API server.
    Serve,
}

#[derive(Debug, Args)]
struct DatasetSectionArgs {
    /// Explore this Dataset instead of the built-in temporary ATIF example.
    #[arg(value_name = "DATASET_URI")]
    dataset_uri: Option<String>,
}

enum Selection {
    All(Option<String>),
    Concepts,
    Inspect(Option<String>),
    Analyze(Option<String>),
    Query(Option<String>),
    Formats,
    Find(Option<String>),
    Exchange,
    Serve,
}

impl OnboardArgs {
    fn into_selection(self) -> Selection {
        match self.section {
            None => Selection::All(self.dataset_uri),
            Some(OnboardSection::All(args)) => Selection::All(args.dataset_uri),
            Some(OnboardSection::Concepts) => Selection::Concepts,
            Some(OnboardSection::Inspect(args)) => Selection::Inspect(args.dataset_uri),
            Some(OnboardSection::Analyze(args)) => Selection::Analyze(args.dataset_uri),
            Some(OnboardSection::Query(args)) => Selection::Query(args.dataset_uri),
            Some(OnboardSection::Formats) => Selection::Formats,
            Some(OnboardSection::Find(args)) => Selection::Find(args.dataset_uri),
            Some(OnboardSection::Exchange) => Selection::Exchange,
            Some(OnboardSection::Serve) => Selection::Serve,
        }
    }
}

pub(crate) async fn run(
    args: OnboardArgs,
    settings_override: Option<&Path>,
    stdin_is_terminal: bool,
    stdout_is_terminal: bool,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
) -> Result<()> {
    let no_pause = args.no_pause;
    let selection = args.into_selection();
    let interactive = matches!(&selection, Selection::All(_))
        && stdin_is_terminal
        && stdout_is_terminal
        && !no_pause;
    let demo = DemoWorkspace::create()?;
    let selected_dataset = match selection.dataset_uri() {
        Some(dataset) => super::resolve_dataset_uri(Some(dataset), settings_override)?,
        None => demo.atif_uri(),
    };
    let mut renderer =
        WalkthroughRenderer::for_output(stdout, stdout_is_terminal, interactive.then_some(stdin));

    match selection {
        Selection::All(_) => {
            render_concepts(&mut renderer, Some(&selected_dataset))?;
            if renderer.stopped() {
                return Ok(());
            }
            render_inspect(&mut renderer, &selected_dataset).await?;
            if renderer.stopped() {
                return Ok(());
            }
            render_analyze(&mut renderer, &selected_dataset).await?;
            if renderer.stopped() {
                return Ok(());
            }
            render_query(&mut renderer, &selected_dataset).await?;
            if renderer.stopped() {
                return Ok(());
            }
            render_formats(&mut renderer, &demo).await?;
            if renderer.stopped() {
                return Ok(());
            }
            render_find(&mut renderer, &selected_dataset).await?;
            if renderer.stopped() {
                return Ok(());
            }
            render_exchange(&mut renderer, &demo).await?;
            if renderer.stopped() {
                return Ok(());
            }
            render_serve(&mut renderer)?;
            if renderer.stopped() {
                return Ok(());
            }
            render_completion(&mut renderer)?;
        }
        Selection::Concepts => render_concepts(&mut renderer, None)?,
        Selection::Inspect(_) => {
            render_section_header(&mut renderer, "Inspect", &selected_dataset)?;
            render_inspect(&mut renderer, &selected_dataset).await?;
            render_section_footer(&mut renderer)?;
        }
        Selection::Analyze(_) => {
            render_section_header(&mut renderer, "Analyze", &selected_dataset)?;
            render_analyze(&mut renderer, &selected_dataset).await?;
            render_section_footer(&mut renderer)?;
        }
        Selection::Query(_) => {
            render_section_header(&mut renderer, "Query", &selected_dataset)?;
            render_query(&mut renderer, &selected_dataset).await?;
            render_section_footer(&mut renderer)?;
        }
        Selection::Formats => {
            render_section_header(&mut renderer, "Formats", "built-in examples")?;
            render_formats(&mut renderer, &demo).await?;
            render_section_footer(&mut renderer)?;
        }
        Selection::Find(_) => {
            render_section_header(&mut renderer, "Find", &selected_dataset)?;
            render_find(&mut renderer, &selected_dataset).await?;
            render_section_footer(&mut renderer)?;
        }
        Selection::Exchange => {
            render_section_header(&mut renderer, "Exchange", "isolated temporary Warehouse")?;
            render_exchange(&mut renderer, &demo).await?;
            render_section_footer(&mut renderer)?;
        }
        Selection::Serve => {
            render_section_header(&mut renderer, "Serve", "configuration walkthrough")?;
            render_serve(&mut renderer)?;
            render_section_footer(&mut renderer)?;
        }
    }
    Ok(())
}

impl Selection {
    fn dataset_uri(&self) -> Option<&str> {
        match self {
            Self::All(uri)
            | Self::Inspect(uri)
            | Self::Analyze(uri)
            | Self::Query(uri)
            | Self::Find(uri) => uri.as_deref(),
            Self::Concepts | Self::Formats | Self::Exchange | Self::Serve => None,
        }
    }
}

struct DemoWorkspace {
    temp: tempfile::TempDir,
}

impl DemoWorkspace {
    fn create() -> Result<Self> {
        let temp = tempfile::Builder::new()
            .prefix("pchronicle-onboard-")
            .tempdir()
            .context("create temporary onboard workspace")?;
        write_demo_source(temp.path(), "atif", "support-ticket.json", DEMO_ATIF)?;
        write_demo_source(temp.path(), "actf", "code-repair.actf.json", DEMO_ACTF)?;
        write_demo_source(temp.path(), "openai-messages", "training.json", DEMO_OPENAI)?;
        Ok(Self { temp })
    }

    fn root(&self) -> &Path {
        self.temp.path()
    }

    fn dataset(&self, name: &str) -> PathBuf {
        self.root().join(name)
    }

    fn atif_uri(&self) -> String {
        self.dataset("atif").to_string_lossy().into_owned()
    }

    fn atif_source(&self) -> PathBuf {
        self.dataset("atif").join("support-ticket.json")
    }
}

fn write_demo_source(root: &Path, dataset: &str, filename: &str, content: &str) -> Result<()> {
    let directory = root.join(dataset);
    std::fs::create_dir(&directory)
        .with_context(|| format!("create temporary onboard {dataset} Dataset"))?;
    std::fs::write(directory.join(filename), content)
        .with_context(|| format!("write temporary onboard {filename}"))?;
    Ok(())
}

fn render_concepts(
    renderer: &mut WalkthroughRenderer<'_>,
    dataset_uri: Option<&str>,
) -> Result<()> {
    let dataset = dataset_uri
        .map(|uri| format!("本次完整流程将实际读取 `{uri}`。"))
        .unwrap_or_default();
    renderer.render(&format!(
        r#"# pChronicle Onboard

pChronicle 把 ATIF、ACTF、OpenAI Messages 等轨迹格式投影为统一的 Dataset 关系表。{dataset}

- `Dataset` 是一个本地目录或对象存储 URI。
- `Source` 是 Dataset 中的一份逻辑轨迹数据源。
- `Snapshot` 固定一次命令所观察到的目录视图。
- `runs`、`steps`、`tool_calls` 等统一表屏蔽了交换格式差异；Storyline Lance
  Dataset 还可以为 Step 内容建立 FTS/Jieba 索引。

可以运行完整引导，也可以直接进入一个章节：

```console
pchronicle onboard
pchronicle onboard inspect ./my-trajectories
pchronicle onboard analyze ./my-trajectories
pchronicle onboard query ./my-trajectories
pchronicle onboard formats
pchronicle onboard find ./my-trajectories
pchronicle onboard exchange
pchronicle onboard serve
```

"#,
    ))?;
    renderer.pause()
}

fn render_section_header(
    renderer: &mut WalkthroughRenderer<'_>,
    name: &str,
    target: &str,
) -> Result<()> {
    renderer.render(&format!(
        "# pChronicle Onboard · {name}\n\n当前目标：`{target}`。\n\n"
    ))
}

fn render_section_footer(renderer: &mut WalkthroughRenderer<'_>) -> Result<()> {
    renderer.render(
        "其他章节可通过 `pchronicle onboard --help` 查看；运行 `pchronicle onboard` 可执行完整引导。\n",
    )
}

async fn render_inspect(renderer: &mut WalkthroughRenderer<'_>, dataset_uri: &str) -> Result<()> {
    let dataset = shell_quote(dataset_uri);
    let list = capture_list(dataset_uri.to_owned()).await?;
    renderer.render(&command_section(
        "Inspect · 发现 Source",
        "`ls` 展示 Dataset 中可供查询的逻辑 Source，而不是底层存储碎片。",
        &format!("pchronicle ls {dataset} --format table"),
        &list,
    ))?;
    renderer.pause()?;
    if renderer.stopped() {
        return Ok(());
    }
    let status = capture_status(dataset_uri.to_owned()).await?;
    renderer.render(&command_section(
        "Inspect · 检查健康状态",
        "`status` 汇总 Source 就绪情况以及轨迹、Step 和工具调用数量。",
        &format!("pchronicle status {dataset} --format table"),
        &status,
    ))?;
    renderer.pause()
}

async fn render_analyze(renderer: &mut WalkthroughRenderer<'_>, dataset_uri: &str) -> Result<()> {
    let dataset = shell_quote(dataset_uri);
    let overview = capture_analysis(dataset_uri.to_owned(), AnalysisKind::Overview).await?;
    renderer.render(&command_section(
        "Analyze · 总览",
        "先用稳定的内置分析确认数据规模和覆盖度。",
        &format!("pchronicle analysis overview {dataset} --format table"),
        &overview,
    ))?;
    renderer.pause()?;
    if renderer.stopped() {
        return Ok(());
    }
    let tools = capture_analysis(dataset_uri.to_owned(), AnalysisKind::Tools).await?;
    renderer.render(&command_section(
        "Analyze · 工具使用",
        "工具分析按统一函数名聚合调用次数、轨迹覆盖和耗时覆盖。",
        &format!("pchronicle analysis tools {dataset} --format table"),
        &tools,
    ))?;
    renderer.pause()
}

async fn render_query(renderer: &mut WalkthroughRenderer<'_>, dataset_uri: &str) -> Result<()> {
    let dataset = shell_quote(dataset_uri);
    let schema = capture_query(
        dataset_uri.to_owned(),
        "DESCRIBE dataset.steps",
        QueryOutputFormat::Table,
    )
    .await?;
    renderer.render(&command_section(
        "Query · 先看 Schema",
        "不要猜交换格式的物理字段；先查看统一表当前公开的列。",
        &format!("pchronicle query {dataset} --sql 'DESCRIBE dataset.steps' --format table"),
        &schema,
    ))?;
    renderer.pause()?;
    if renderer.stopped() {
        return Ok(());
    }
    let steps = capture_query(
        dataset_uri.to_owned(),
        STEP_SAMPLE_SQL,
        QueryOutputFormat::Table,
    )
    .await?;
    renderer.render(&command_section(
        "Query · 查看 Step",
        "`message_kind` 标识消息类型，`message_value` 保留原始消息值；Agent、Model 和时间字段则可直接参与 SQL。",
        &format!(
            "pchronicle query {dataset} --sql {} --format table",
            shell_quote(STEP_SAMPLE_SQL)
        ),
        &steps,
    ))?;
    renderer.pause()?;
    if renderer.stopped() {
        return Ok(());
    }
    let tools = capture_query(
        dataset_uri.to_owned(),
        TOOL_SAMPLE_SQL,
        QueryOutputFormat::Table,
    )
    .await?;
    renderer.render(&command_section(
        "Query · 查看工具调用",
        "工具调用在独立的统一表中，通过 Session 和 Step 坐标与轨迹关联。",
        &format!(
            "pchronicle query {dataset} --sql {} --format table",
            shell_quote(TOOL_SAMPLE_SQL)
        ),
        &tools,
    ))?;
    renderer.pause()?;
    if renderer.stopped() {
        return Ok(());
    }
    renderer.render(
        "需要把结果交给 Python、jq 或流水线时，改用 `--format jsonl`；每一行都是独立 JSON 对象。\n\n",
    )
}

async fn render_formats(
    renderer: &mut WalkthroughRenderer<'_>,
    demo: &DemoWorkspace,
) -> Result<()> {
    let output = capture_named_query(demo).await?;
    let command = format!(
        "pchronicle query --mount atif=./atif --mount actf=./actf --mount openai=./openai-messages --sql {} --format table",
        shell_quote(CROSS_FORMAT_SQL)
    );
    renderer.render(&command_section(
        "Formats · 跨格式查询",
        "命名挂载允许一个只读 SQL 同时查询 ATIF、ACTF 和 OpenAI Messages，无需预先转换物理格式。",
        &command,
        &output,
    ))?;
    renderer.pause()
}

async fn render_find(renderer: &mut WalkthroughRenderer<'_>, dataset_uri: &str) -> Result<()> {
    let target = discover_find_target(dataset_uri.to_owned()).await?;
    if let Some((session_id, step_id)) = target {
        let output = capture_find(dataset_uri.to_owned(), session_id.clone(), step_id).await?;
        renderer.render(&command_section(
            "Find · 按 Source-local ID 定位",
            "外部 ID 只保证在各自 Source 内有意义。`find` 返回 `source_path`，用于消除不同 Source 之间的歧义。",
            &format!(
                "pchronicle find {} --session-id {} --step-id {step_id} --format table",
                shell_quote(dataset_uri),
                shell_quote(&session_id)
            ),
            &output,
        ))?;
    } else {
        renderer.render(
            "## Find · 没有可定位的 Step\n\n当前 Dataset 的 `steps` 表为空。导入轨迹后再使用 `find`。\n",
        )?;
    }
    renderer.pause()?;
    if renderer.stopped() {
        return Ok(());
    }
    let dataset = shell_quote(dataset_uri);
    renderer.render(&format!(
        r#"## Find · FTS、字段限定与 JSONB

`--match` 是统一检索入口：普通文本走 Storyline Step 的 FTS/Jieba 索引，`#user(...)`、
`#system(...)` 等形式限定字段，`AND`、`OR`、`NOT` 和括号组合条件。JSONB 使用
`$.path=value`，或用 `#json.COLUMN("$.path")=value` 指定列。

```console
pchronicle find {dataset} --match "deployment"
pchronicle find {dataset} --match '#user("pending")'
pchronicle find {dataset} --match '$.tags="important"' --format json
pchronicle find {dataset} --match '(#user("timeout") OR #assistant("retry")) AND NOT #system("example")'
```

重复 `--match` 表示 AND。JSON 输出中的 `snapshot_id`、`search.mode`、`search.scope`、
`fts_available` 和 `preview` 用于自动化诊断；`source_path`、`session_id` 和 `step_id`
可以直接复制到下一次定位查询。FTS 只有在 Storyline Lance 索引可用时才会执行，索引
不可用不等价于零命中。

"#
    ))?;
    renderer.pause()
}

async fn render_exchange(
    renderer: &mut WalkthroughRenderer<'_>,
    demo: &DemoWorkspace,
) -> Result<()> {
    renderer.render(
        "## Exchange · 本地 Dataset\n\n默认 Dataset 只是用户配置，不是守护进程或隐藏数据库。下面的真实演练使用隔离 config 和临时目录，不修改用户配置。\n\n",
    )?;
    let exchange = capture_exchange(demo).await?;
    renderer.render(&command_section(
        "Exchange · 设置默认 Warehouse",
        "设置后，本地读命令可以省略 Dataset URI。",
        "pchronicle default set ./trajectory-data",
        &exchange.default_output,
    ))?;
    renderer.pause()?;
    if renderer.stopped() {
        return Ok(());
    }
    renderer.render(&command_section(
        "Exchange · 导入",
        "导入默认使用安全的 create 模式，来源和目标都显式写在命令中。保留布局适合严格往返和审计原始来源；已有 Storyline Dataset 可显式选择 append 或 replace。",
        "pchronicle import --from ./support-ticket.json --to ./trajectory-data/support-ticket --input-format atif",
        &exchange.import_output,
    ))?;
    renderer.pause()?;
    if renderer.stopped() {
        return Ok(());
    }
    renderer.render(&command_section(
        "Exchange · 构建 Storyline Lance",
        "`--output-format storyline` 将输入规范化为 Dataset 根部的 Storyline Lance Store，并为后续 FTS/Jieba 与 JSONB 查询准备统一布局。",
        "pchronicle import --from ./support-ticket.json --to ./trajectory-data/storyline-support-ticket --input-format atif --output-format storyline",
        &exchange.storyline_import_output,
    ))?;
    renderer.pause()?;
    if renderer.stopped() {
        return Ok(());
    }
    renderer.render(&command_section(
        "Exchange · 严格导出",
        "`--strict` 拒绝无法保留原交换文档的转换。本例导出的 JSON 与输入语义相等。",
        "pchronicle export --from ./trajectory-data/support-ticket --to ./restored.json --output-format atif --strict",
        &exchange.export_output,
    ))?;
    renderer.pause()
}

fn render_serve(renderer: &mut WalkthroughRenderer<'_>) -> Result<()> {
    renderer.render(
        r#"## Serve · 只读 Web/API

`serve` 使用一个或多个位置参数 `[NAME=]DATASET` 显式挂载，不读取默认 Dataset。

```console
pchronicle serve --listen 127.0.0.1:8080 --open evals=../data/atif
```

服务只允许 loopback 地址，因为这个本地表面不提供认证；Dataset API 和 Web UI 都是只读的。
Runs 页面检索使用与 `find --match` 相同的 FTS/JSONB 语义，命中的轨迹会展示上下文预览；
可以先用 CLI `find` 定位，再在 Web 中继续钻取。

"#,
    )?;
    renderer.pause()
}

fn render_completion(renderer: &mut WalkthroughRenderer<'_>) -> Result<()> {
    renderer.render(
        r#"# 完成

你已经走通 Dataset 发现、内置分析、Schema、SQL、FTS/JSONB 检索、跨格式查询、ID 定位、
Storyline Lance 导入导出和只读 Web/API 服务边界。

本引导创建的内置示例和隔离 Warehouse 将在命令退出时自动清理。把自己的 Dataset 接入相同流程：

```console
pchronicle onboard inspect ./my-trajectories
pchronicle onboard query ./my-trajectories
pchronicle find ./my-trajectories --match "timeout" --format json
```

完整参数以 `pchronicle --help` 为准。
"#,
    )
}

#[derive(Clone, Copy)]
enum AnalysisKind {
    Overview,
    Tools,
}

async fn capture_list(dataset_uri: String) -> Result<String> {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    run_list(
        ListArgs {
            dataset_uri: Some(dataset_uri),
            physical: false,
            format: OutputFormat::Table,
            errors: ErrorMode::Report,
            max_files: MAX_FILES,
            max_entries: MAX_ENTRIES,
        },
        None,
        true,
        &mut stdout,
        &mut stderr,
    )
    .await?;
    decode_output(stdout)
}

async fn capture_status(dataset_uri: String) -> Result<String> {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    run_status(
        StatusArgs {
            dataset_uri: Some(dataset_uri),
            format: OutputFormat::Table,
            errors: ErrorMode::Report,
            max_files: MAX_FILES,
            max_entries: MAX_ENTRIES,
            timeout_seconds: 30,
        },
        None,
        true,
        &mut stdout,
        &mut stderr,
    )
    .await?;
    decode_output(stdout)
}

async fn capture_analysis(dataset_uri: String, kind: AnalysisKind) -> Result<String> {
    let options = AnalysisOptions {
        dataset_uri: Some(dataset_uri),
        format: QueryOutputFormat::Table,
        limit: 100,
        max_output_bytes: 8 * 1024 * 1024,
        timeout_seconds: 30,
        max_files: MAX_FILES,
        max_entries: MAX_ENTRIES,
    };
    let command = match kind {
        AnalysisKind::Overview => AnalysisCommand::Overview(options),
        AnalysisKind::Tools => AnalysisCommand::Tools(options),
    };
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    run_analysis(
        AnalysisArgs { command },
        None,
        true,
        &mut stdout,
        &mut stderr,
    )
    .await?;
    decode_output(stdout)
}

async fn capture_query(
    dataset_uri: String,
    sql: &str,
    format: QueryOutputFormat,
) -> Result<String> {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    run_query(
        QueryArgs {
            dataset_uri: Some(dataset_uri),
            datasets: Vec::new(),
            sql: Some(sql.to_owned()),
            sql_option: None,
            file: None,
            format,
            output: "-".to_owned(),
            overwrite: false,
            max_output_rows: 100_000,
            max_output_bytes: 64 * 1024 * 1024,
            timeout_seconds: 30,
            max_files: MAX_FILES,
            max_entries: MAX_ENTRIES,
        },
        None,
        true,
        &mut std::io::empty(),
        &mut stdout,
        &mut stderr,
    )
    .await?;
    decode_output(stdout)
}

async fn capture_named_query(demo: &DemoWorkspace) -> Result<String> {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    run_query(
        QueryArgs {
            dataset_uri: Some(CROSS_FORMAT_SQL.to_owned()),
            datasets: vec![
                format!("atif={}", demo.dataset("atif").display()),
                format!("actf={}", demo.dataset("actf").display()),
                format!("openai={}", demo.dataset("openai-messages").display()),
            ],
            sql: None,
            sql_option: None,
            file: None,
            format: QueryOutputFormat::Table,
            output: "-".to_owned(),
            overwrite: false,
            max_output_rows: 100_000,
            max_output_bytes: 64 * 1024 * 1024,
            timeout_seconds: 30,
            max_files: MAX_FILES,
            max_entries: MAX_ENTRIES,
        },
        None,
        true,
        &mut std::io::empty(),
        &mut stdout,
        &mut stderr,
    )
    .await?;
    decode_output(stdout)
}

async fn discover_find_target(dataset_uri: String) -> Result<Option<(String, i64)>> {
    let output = capture_query(dataset_uri, FIND_TARGET_SQL, QueryOutputFormat::Jsonl).await?;
    let Some(line) = output.lines().find(|line| !line.trim().is_empty()) else {
        return Ok(None);
    };
    let value: serde_json::Value =
        serde_json::from_str(line).context("decode onboard find target")?;
    let session_id = value["session_id"]
        .as_str()
        .context("onboard find target has no session_id")?
        .to_owned();
    let step_id = value["step_id"]
        .as_i64()
        .context("onboard find target has no step_id")?;
    Ok(Some((session_id, step_id)))
}

async fn capture_find(dataset_uri: String, session_id: String, step_id: i64) -> Result<String> {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    run_find(
        FindArgs {
            dataset_uri: Some(dataset_uri),
            source: None,
            document_id: None,
            run_id: None,
            session_id: Some(session_id),
            step_id: Some(step_id),
            matches: Vec::new(),
            format: OutputFormat::Table,
            max_results: 100,
            max_output_bytes: 8 * 1024 * 1024,
            timeout_seconds: 30,
            max_files: MAX_FILES,
            max_entries: MAX_ENTRIES,
        },
        None,
        true,
        &mut stdout,
        &mut stderr,
    )
    .await?;
    decode_output(stdout)
}

struct ExchangeOutput {
    default_output: String,
    import_output: String,
    storyline_import_output: String,
    export_output: String,
}

async fn capture_exchange(demo: &DemoWorkspace) -> Result<ExchangeOutput> {
    let settings = demo.root().join("onboard-settings.toml");
    let warehouse = demo.root().join("warehouse");
    let mut default_stdout = Vec::new();
    let mut default_stderr = Vec::new();
    run_default(
        DefaultArgs {
            command: Some(DefaultCommand::Set {
                dataset: warehouse.to_string_lossy().into_owned(),
            }),
            legacy_directory: None,
        },
        Some(&settings),
        &mut default_stdout,
        &mut default_stderr,
    )?;

    let mut import_stdout = Vec::new();
    let mut import_stderr = Vec::new();
    let mut empty_stdin = std::io::empty();
    run_import(
        ImportArgs {
            from: demo.atif_source().to_string_lossy().into_owned(),
            output: Some(
                warehouse
                    .join("support-ticket")
                    .to_string_lossy()
                    .into_owned(),
            ),
            format: ExchangeFormat::Atif,
            output_format: Some(ImportOutputFormat::Preserve),
            mode: ImportMode::Create,
            on_duplicate: None,
            yes: false,
            stream: false,
            max_input_bytes: None,
        },
        Some(&settings),
        false,
        &mut empty_stdin,
        &mut import_stdout,
        &mut import_stderr,
    )
    .await?;
    let import_value: serde_json::Value =
        serde_json::from_slice(&import_stdout).context("decode onboard import result")?;
    let imported_dataset = import_value["dataset_uri"]
        .as_str()
        .context("onboard import result has no dataset_uri")?;
    let storyline_output = warehouse.join("storyline-support-ticket");
    let mut storyline_stdout = Vec::new();
    let mut storyline_stderr = Vec::new();
    run_import(
        ImportArgs {
            from: demo.atif_source().to_string_lossy().into_owned(),
            output: Some(storyline_output.to_string_lossy().into_owned()),
            format: ExchangeFormat::Atif,
            output_format: Some(ImportOutputFormat::Storyline),
            mode: ImportMode::Create,
            on_duplicate: None,
            yes: false,
            stream: false,
            max_input_bytes: None,
        },
        Some(&settings),
        false,
        &mut std::io::empty(),
        &mut storyline_stdout,
        &mut storyline_stderr,
    )
    .await?;
    let storyline_import_output = decode_output(storyline_stdout)?;
    let exported = demo.root().join("restored-support-ticket.json");
    let mut export_stdout = Vec::new();
    let mut export_stderr = Vec::new();
    run_export(
        ExportArgs {
            from: Some(imported_dataset.to_owned()),
            output: exported.to_string_lossy().into_owned(),
            format: ExportFormat::Atif,
            source: None,
            document_id: None,
            run_id: None,
            session_id: None,
            r#where: None,
            strict: true,
            overwrite: false,
            stream: false,
            max_trajectories: 10_000,
            max_output_bytes: 64 * 1024 * 1024,
            timeout_seconds: 30,
            max_files: MAX_FILES,
            max_entries: MAX_ENTRIES,
        },
        Some(&settings),
        &mut export_stdout,
        &mut export_stderr,
    )
    .await?;
    let original: serde_json::Value =
        serde_json::from_str(DEMO_ATIF).context("decode embedded onboard ATIF")?;
    let restored: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&exported).context("read onboard strict export")?)
            .context("decode onboard strict export")?;
    anyhow::ensure!(
        original == restored,
        "onboard strict ATIF round trip changed JSON"
    );

    let root = demo.root().to_string_lossy();
    let canonical_root = std::fs::canonicalize(demo.root())
        .context("canonicalize onboard temporary workspace")?
        .to_string_lossy()
        .into_owned();
    let sanitize = |output: String| {
        output
            .replace(&canonical_root, "<temporary-workspace>")
            .replace(root.as_ref(), "<temporary-workspace>")
    };
    Ok(ExchangeOutput {
        default_output: sanitize(decode_output(default_stdout)?),
        import_output: sanitize(decode_output(import_stdout)?),
        storyline_import_output: sanitize(storyline_import_output),
        export_output: format!(
            "{}严格往返校验：输入与输出 JSON 语义相等\n",
            sanitize(decode_output(export_stderr)?)
        ),
    })
}

fn decode_output(output: Vec<u8>) -> Result<String> {
    String::from_utf8(output).context("onboard command output is not UTF-8")
}

fn command_section(title: &str, explanation: &str, command: &str, output: &str) -> String {
    format!(
        "## {title}\n\n{explanation}\n\n```console\n$ {command}\n{}```\n\n",
        output.trim_end_matches('\n').to_owned() + "\n"
    )
}

fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"/._:-=".contains(&byte))
    {
        return value.to_owned();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[derive(Clone, Copy)]
enum RenderMode {
    Markdown,
    Terminal { ansi: bool },
}

struct WalkthroughRenderer<'a> {
    markdown: MarkdownRenderer<'a>,
    input: Option<&'a mut dyn Read>,
    stopped: bool,
}

impl<'a> WalkthroughRenderer<'a> {
    fn for_output(
        output: &'a mut dyn Write,
        is_terminal: bool,
        input: Option<&'a mut dyn Read>,
    ) -> Self {
        Self {
            markdown: MarkdownRenderer::for_output(output, is_terminal),
            input,
            stopped: false,
        }
    }

    fn render(&mut self, markdown: &str) -> Result<()> {
        self.markdown.render(markdown)
    }

    fn pause(&mut self) -> Result<()> {
        let Some(input) = self.input.as_deref_mut() else {
            return Ok(());
        };

        let prompt = "按 Enter 继续，输入 q 退出引导：";
        match self.markdown.mode {
            RenderMode::Terminal { ansi: true } => {
                write!(self.markdown.output, "\x1b[2m{prompt}\x1b[0m ")?;
            }
            RenderMode::Markdown | RenderMode::Terminal { ansi: false } => {
                write!(self.markdown.output, "{prompt} ")?;
            }
        }
        self.markdown
            .output
            .flush()
            .context("flush onboard prompt")?;

        let mut answer = Vec::new();
        let mut byte = [0_u8; 1];
        loop {
            match input.read(&mut byte).context("read onboard response")? {
                0 => {
                    self.stopped = true;
                    break;
                }
                _ if matches!(byte[0], b'\n' | b'\r') => break,
                _ => answer.push(byte[0]),
            }
        }

        if answer
            .iter()
            .copied()
            .find(|byte| !byte.is_ascii_whitespace())
            .is_some_and(|byte| matches!(byte, b'q' | b'Q'))
        {
            self.stopped = true;
        }
        if self.stopped {
            writeln!(self.markdown.output, "\n已退出引导；临时示例将自动清理。")?;
        } else {
            writeln!(self.markdown.output)?;
        }
        Ok(())
    }

    fn stopped(&self) -> bool {
        self.stopped
    }
}

struct MarkdownRenderer<'a> {
    output: &'a mut dyn Write,
    mode: RenderMode,
}

impl<'a> MarkdownRenderer<'a> {
    fn for_output(output: &'a mut dyn Write, is_terminal: bool) -> Self {
        let mode = if is_terminal {
            let ansi = std::env::var_os("NO_COLOR").is_none()
                && std::env::var("TERM").map_or(true, |term| term != "dumb");
            RenderMode::Terminal { ansi }
        } else {
            RenderMode::Markdown
        };
        Self { output, mode }
    }

    fn render(&mut self, markdown: &str) -> Result<()> {
        match self.mode {
            RenderMode::Markdown => {
                self.output
                    .write_all(markdown.as_bytes())
                    .context("write onboard Markdown")?;
                if !markdown.ends_with('\n') {
                    writeln!(self.output).context("finish onboard Markdown")?;
                }
            }
            RenderMode::Terminal { ansi } => render_terminal(markdown, ansi, self.output)?,
        }
        Ok(())
    }
}

fn render_terminal(markdown: &str, ansi: bool, output: &mut dyn Write) -> Result<()> {
    let mut in_code_block = false;
    for line in markdown.lines() {
        if line.starts_with("```") {
            in_code_block = !in_code_block;
            continue;
        }
        if in_code_block {
            if ansi {
                writeln!(output, "\x1b[90m  {line}\x1b[0m")?;
            } else {
                writeln!(output, "  {line}")?;
            }
            continue;
        }
        if let Some(heading) = line.strip_prefix("# ") {
            if ansi {
                writeln!(output, "\x1b[1;36m{heading}\x1b[0m")?;
            } else {
                writeln!(output, "{heading}")?;
            }
        } else if let Some(heading) = line.strip_prefix("## ") {
            if ansi {
                writeln!(output, "\x1b[1;34m{heading}\x1b[0m")?;
            } else {
                writeln!(output, "{heading}")?;
            }
        } else if let Some(item) = line.strip_prefix("- ") {
            writeln!(output, "  • {}", render_inline(item, ansi))?;
        } else if let Some(quote) = line.strip_prefix("> ") {
            if ansi {
                writeln!(output, "\x1b[2m  │ {}\x1b[0m", render_inline(quote, ansi))?;
            } else {
                writeln!(output, "  │ {}", render_inline(quote, ansi))?;
            }
        } else {
            writeln!(output, "{}", render_inline(line, ansi))?;
        }
    }
    Ok(())
}

fn render_inline(line: &str, ansi: bool) -> String {
    let mut rendered = String::new();
    let mut code = false;
    for part in line.split('`') {
        if code && ansi {
            let _ = write!(rendered, "\x1b[36m{part}\x1b[0m");
        } else {
            rendered.push_str(part);
        }
        code = !code;
    }
    rendered
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_mode_preserves_markdown() -> Result<()> {
        let mut output = Vec::new();
        let mut renderer = MarkdownRenderer {
            output: &mut output,
            mode: RenderMode::Markdown,
        };
        renderer.render("# Title\n\n- item\n")?;
        assert_eq!(String::from_utf8(output)?, "# Title\n\n- item\n");
        Ok(())
    }

    #[test]
    fn terminal_mode_renders_supported_markdown_without_ansi() -> Result<()> {
        let mut output = Vec::new();
        render_terminal(
            "# Title\n\n- use `status`\n\n```console\npchronicle status\n```\n",
            false,
            &mut output,
        )?;
        assert_eq!(
            String::from_utf8(output)?,
            "Title\n\n  • use status\n\n  pchronicle status\n"
        );
        Ok(())
    }

    #[test]
    fn terminal_mode_uses_ansi_only_when_enabled() -> Result<()> {
        let mut output = Vec::new();
        render_terminal("## Query\n\nRun `query`.\n", true, &mut output)?;
        let output = String::from_utf8(output)?;
        assert!(output.contains("\x1b[1;34mQuery\x1b[0m"));
        assert!(output.contains("\x1b[36mquery\x1b[0m"));
        Ok(())
    }

    #[test]
    fn interactive_walkthrough_waits_and_can_quit() -> Result<()> {
        let mut output = Vec::new();
        let mut input = std::io::Cursor::new(b"\nq\n");
        let mut renderer = WalkthroughRenderer::for_output(&mut output, true, Some(&mut input));

        renderer.render("## Inspect\n\nfirst result\n")?;
        renderer.pause()?;
        assert!(!renderer.stopped());
        renderer.render("## Query\n\nsecond result\n")?;
        renderer.pause()?;
        assert!(renderer.stopped());

        let output = String::from_utf8(output)?;
        assert_eq!(output.matches("按 Enter 继续").count(), 2);
        assert!(output.contains("first result"));
        assert!(output.contains("second result"));
        assert!(output.contains("已退出引导"));
        Ok(())
    }

    #[test]
    fn shell_quote_keeps_commands_copyable() {
        assert_eq!(shell_quote("./demo"), "./demo");
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }
}
