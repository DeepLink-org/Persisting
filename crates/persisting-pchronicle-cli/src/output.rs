use super::*;

pub(super) struct LimitedBuffer {
    pub(super) bytes: Vec<u8>,
    max_bytes: usize,
    limit_exceeded: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum QueryOutputBudgetOutcome {
    Complete(Vec<u8>),
    RowLimitExceeded,
    ByteLimitExceeded,
}

impl LimitedBuffer {
    pub(super) fn new(max_bytes: usize) -> Self {
        Self {
            bytes: Vec::new(),
            max_bytes,
            limit_exceeded: false,
        }
    }

    pub(super) fn finish(
        self,
        write_result: Result<persisting_pchronicle::query::QueryWriteOutcome>,
    ) -> Result<QueryOutputBudgetOutcome> {
        if self.limit_exceeded {
            return Ok(QueryOutputBudgetOutcome::ByteLimitExceeded);
        }
        match write_result {
            Ok(persisting_pchronicle::query::QueryWriteOutcome::Complete) => {
                Ok(QueryOutputBudgetOutcome::Complete(self.bytes))
            }
            Ok(persisting_pchronicle::query::QueryWriteOutcome::LimitExceeded) => {
                Ok(QueryOutputBudgetOutcome::RowLimitExceeded)
            }
            Err(error) => Err(error),
        }
    }
}

impl Write for LimitedBuffer {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let next_size = self.bytes.len().saturating_add(buffer.len());
        if next_size > self.max_bytes {
            self.limit_exceeded = true;
            return Err(IoError::other(format!(
                "SQL result exceeds max_output_bytes limit of {}",
                self.max_bytes
            )));
        }
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub(super) struct QueryRows {
    columns: Vec<String>,
    pub(super) values: Vec<serde_json::Map<String, serde_json::Value>>,
}

pub(super) fn parse_jsonl_rows(jsonl: &str) -> Result<QueryRows> {
    use serde::de::{MapAccess, Visitor};
    use serde::Deserializer as _;

    struct OrderedObjectVisitor;

    impl<'de> Visitor<'de> for OrderedObjectVisitor {
        type Value = Vec<(String, serde_json::Value)>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a JSON object")
        }

        fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
        where
            A: MapAccess<'de>,
        {
            let mut values = Vec::new();
            while let Some(entry) = map.next_entry()? {
                values.push(entry);
            }
            Ok(values)
        }
    }

    let mut columns = Vec::new();
    let mut values = Vec::new();
    for line in jsonl.lines().filter(|line| !line.trim().is_empty()) {
        let mut deserializer = serde_json::Deserializer::from_str(line);
        let entries = deserializer
            .deserialize_map(OrderedObjectVisitor)
            .context("decode pChronicle query row")?;
        deserializer.end().context("decode pChronicle query row")?;
        let mut row = serde_json::Map::new();
        for (column, value) in entries {
            if !columns.contains(&column) {
                columns.push(column.clone());
            }
            row.insert(column, value);
        }
        values.push(row);
    }
    Ok(QueryRows { columns, values })
}

pub(super) fn encode_query_csv(rows: &QueryRows) -> Vec<u8> {
    if rows.columns.is_empty() {
        return Vec::new();
    }
    let mut output = String::new();
    write_csv_row(&mut output, rows.columns.iter().cloned());
    for row in &rows.values {
        write_csv_row(
            &mut output,
            rows.columns
                .iter()
                .map(|column| query_value(row.get(column))),
        );
    }
    output.into_bytes()
}

pub(super) fn write_csv_row(output: &mut String, values: impl IntoIterator<Item = String>) {
    for (index, value) in values.into_iter().enumerate() {
        if index > 0 {
            output.push(',');
        }
        if value.contains([',', '"', '\n', '\r']) {
            output.push('"');
            output.push_str(&value.replace('"', "\"\""));
            output.push('"');
        } else {
            output.push_str(&value);
        }
    }
    output.push('\n');
}

pub(super) fn encode_query_table(rows: &QueryRows) -> Result<Vec<u8>> {
    if rows.columns.is_empty() {
        return Ok(b"(0 rows)\n".to_vec());
    }
    let mut grid = Vec::with_capacity(rows.values.len() + 1);
    grid.push(rows.columns.clone());
    grid.extend(rows.values.iter().map(|row| {
        rows.columns
            .iter()
            .map(|column| truncate(&query_value(row.get(column)), 80))
            .collect()
    }));
    let mut output = Vec::new();
    write_grid(&mut output, &grid, "write pChronicle query table")?;
    Ok(output)
}

pub(super) fn query_value(value: Option<&serde_json::Value>) -> String {
    match value {
        None | Some(serde_json::Value::Null) => String::new(),
        Some(serde_json::Value::String(value)) => value.clone(),
        Some(value) => value.to_string(),
    }
}

pub(super) fn write_query_output(path: &str, output: &[u8], stdout: &mut dyn Write) -> Result<()> {
    if path == "-" {
        stdout.write_all(output).context("write query output")?;
        return Ok(());
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("create query output file {path}"))?;
    file.write_all(output)
        .with_context(|| format!("write query output file {path}"))?;
    file.flush()
        .with_context(|| format!("flush query output file {path}"))
}

pub(super) fn query_format_name(format: QueryOutputFormat) -> &'static str {
    match format {
        QueryOutputFormat::Table => "table",
        QueryOutputFormat::Jsonl => "jsonl",
        QueryOutputFormat::Csv => "csv",
        QueryOutputFormat::Auto => "auto",
    }
}

pub(super) fn write_status_table(stdout: &mut dyn Write, response: &StatusResponse) -> Result<()> {
    writeln!(stdout, "FIELD         VALUE       ACCURACY")?;
    writeln!(stdout, "status        {}", response.status)?;
    writeln!(
        stdout,
        "sources       {}          exact",
        response.sources.total
    )?;
    writeln!(
        stdout,
        "ready_sources {}          exact",
        response.sources.ready
    )?;
    writeln!(
        stdout,
        "error_sources {}          exact",
        response.sources.error
    )?;
    let accuracy = if response.counts_complete {
        "exact"
    } else {
        "partial"
    };
    writeln!(
        stdout,
        "runs          {}          {accuracy}",
        response.counts.runs
    )?;
    writeln!(
        stdout,
        "trajectories  {}          {accuracy}",
        response.counts.trajectories
    )?;
    writeln!(
        stdout,
        "steps         {}          {accuracy}",
        response.counts.steps
    )?;
    writeln!(
        stdout,
        "tool_calls    {}          {accuracy}",
        response.counts.tool_calls
    )?;
    writeln!(
        stdout,
        "events        {}          {accuracy}",
        response.counts.events
    )?;
    if !response.projections.is_empty() {
        writeln!(stdout)?;
        writeln!(
            stdout,
            "PROJECTION                         STATUS   FACT_VERSION FACT_ROWS GENERATION"
        )?;
        for projection in &response.projections {
            let path = format!(
                "{} -> {}",
                projection.source_path, projection.projection_path
            );
            writeln!(
                stdout,
                "{:<34} {:<8} {:<12} {:<9} {}",
                truncate(&path, 34),
                projection.status.as_str(),
                projection
                    .fact_version
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
                projection
                    .fact_rows
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
                projection.generation.as_deref().unwrap_or_default(),
            )?;
        }
    }
    for error in &response.source_errors {
        writeln!(
            stdout,
            "source_error  {}: {}",
            truncate(&error.source_path, 48),
            truncate(&error.error, 80)
        )?;
    }
    Ok(())
}

pub(super) fn sql_string(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

pub(super) fn expand_dataset_alias(input: &str) -> Result<String> {
    let input = input.trim();
    if !input.starts_with('@') {
        return Ok(input.to_string());
    }
    anyhow::ensure!(
        !input[1..].contains("://"),
        "dataset alias must not contain a URI scheme"
    );
    let rest = &input[1..];
    let (name, suffix) = rest.split_once('/').unwrap_or((rest, ""));
    anyhow::ensure!(
        !name.is_empty(),
        "dataset alias must include a name after '@'"
    );
    let remainder = suffix.trim_start_matches('/');
    if !remainder.is_empty() {
        for component in remainder.split('/') {
            anyhow::ensure!(
                !component.is_empty() && component != "..",
                "dataset alias path must not contain empty or parent segments"
            );
        }
    }
    let root = match name {
        "codex" => alias_root("CODEX_HOME", ".codex", "sessions", "@codex")?,
        "claude" => alias_root("CLAUDE_CONFIG_DIR", ".claude", "projects", "@claude")?,
        "claude-code" => alias_root("CLAUDE_CONFIG_DIR", ".claude", "projects", "@claude-code")?,
        other => anyhow::bail!("unknown dataset alias '@{other}'; expected @codex or @claude"),
    };
    if remainder.is_empty() {
        return Ok(root.to_string_lossy().into_owned());
    }
    Ok(root.join(remainder).to_string_lossy().into_owned())
}

fn alias_root(env_key: &str, home_subdir: &str, leaf: &str, label: &str) -> Result<PathBuf> {
    let configured = std::env::var_os(env_key).filter(|value| !value.is_empty());
    let base = match configured {
        Some(value) => {
            let path = PathBuf::from(value);
            if path.is_absolute() {
                path
            } else {
                std::env::current_dir()
                    .with_context(|| {
                        format!("cannot resolve {label}: current directory is unknown")
                    })?
                    .join(path)
            }
        }
        None => dirs::home_dir()
            .ok_or_else(|| anyhow!("cannot resolve {label}: home directory is unknown"))?
            .join(home_subdir),
    };
    Ok(base.join(leaf))
}

pub(super) fn normalize_and_validate_dataset_uri(input: &str) -> Result<String> {
    let input = expand_dataset_alias(input)?;
    anyhow::ensure!(!input.is_empty(), "Dataset URI must not be empty");
    if !input.contains("://") {
        return Ok(std::fs::canonicalize(input)
            .with_context(|| "canonicalize local Dataset path")?
            .to_string_lossy()
            .into_owned());
    }

    let url = Url::parse(&input).context("parse Dataset URI")?;
    anyhow::ensure!(
        matches!(url.scheme(), "local" | "file" | "s3" | "az" | "gs"),
        "unsupported Dataset URI scheme '{}'",
        url.scheme()
    );
    anyhow::ensure!(
        url.username().is_empty() && url.password().is_none(),
        "Dataset URI must not contain embedded credentials"
    );
    anyhow::ensure!(
        url.query().is_none(),
        "Dataset URI must not contain a query string or signed credentials"
    );
    anyhow::ensure!(
        url.fragment().is_none(),
        "Dataset URI must not contain a fragment"
    );
    if matches!(url.scheme(), "s3" | "az" | "gs") {
        anyhow::ensure!(
            url.host_str().is_some(),
            "object-store URI must name a bucket"
        );
    } else {
        anyhow::ensure!(
            url.host_str().is_none(),
            "local Dataset URI must not contain a host"
        );
    }
    let minimum_length = input.find("://").map_or(1, |index| {
        index
            + if matches!(url.scheme(), "local" | "file") {
                4
            } else {
                3
            }
    });
    let mut normalized = input.to_string();
    while normalized.len() > minimum_length && normalized.ends_with('/') {
        normalized.pop();
    }
    Ok(normalized)
}

pub(super) fn write_table(
    stdout: &mut dyn Write,
    sources: &[SourceResponse],
    physical: bool,
) -> Result<()> {
    let mut rows = Vec::with_capacity(sources.len() + 1);
    let mut header = vec!["SOURCE", "FORMAT", "KIND", "STATUS"];
    if physical {
        header.extend(["SIZE", "LAST MODIFIED", "SNAPSHOT"]);
    }
    header.push("ERROR");
    rows.push(header.into_iter().map(str::to_string).collect::<Vec<_>>());

    for source in sources {
        let mut row = vec![
            truncate(&source.source_path, 64),
            source.format.as_deref().unwrap_or("-").to_string(),
            enum_json(source.kind),
            enum_json(source.status),
        ];
        if physical {
            row.extend([
                source
                    .size_bytes
                    .map(format_bytes)
                    .unwrap_or_else(|| "-".into()),
                source.last_modified.as_deref().unwrap_or("-").to_string(),
                truncate(source.snapshot_ref.as_deref().unwrap_or("-"), 40),
            ]);
        }
        row.push(truncate(source.error.as_deref().unwrap_or("-"), 80));
        rows.push(row);
    }

    write_grid(stdout, &rows, "write pChronicle ls table")
}

pub(super) fn write_grid(
    stdout: &mut dyn Write,
    rows: &[Vec<String>],
    context: &'static str,
) -> Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let widths = (0..rows[0].len())
        .map(|column| {
            rows.iter()
                .map(|row| row[column].chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect::<Vec<_>>();
    for row in rows {
        let mut line = String::new();
        for (column, cell) in row.iter().enumerate() {
            if column > 0 {
                line.push_str("  ");
            }
            let padding = widths[column].saturating_sub(cell.chars().count());
            write!(line, "{cell}{}", " ".repeat(padding))?;
        }
        writeln!(stdout, "{}", line.trim_end()).context(context)?;
    }
    Ok(())
}

pub(super) fn enum_json<T: Serialize>(value: T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| "unknown".into())
}

pub(super) fn truncate(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let mut output = value
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    output.push('…');
    output
}

pub(super) fn format_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}
