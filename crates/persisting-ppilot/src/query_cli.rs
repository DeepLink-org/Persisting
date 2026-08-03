//! Public pPilot SQL query command backed by pChronicle.

use std::collections::HashSet;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::{Args, ValueEnum};
use persisting_pchronicle::{ChronicleQueryEngine, ExternalTableFormat, ExternalTableSpec};

#[derive(Debug, Clone, Copy, Default, ValueEnum)]
pub enum QuerySource {
    #[default]
    Auto,
    Lance,
    Atif,
}

#[derive(Debug, Args)]
pub struct QueryArgs {
    /// Lance store path/S3 URI, or an ATIF JSON/JSONL file or directory.
    #[arg(value_name = "INPUT")]
    pub input: String,

    /// Input representation. `auto` treats a directory containing CURRENT as Lance.
    #[arg(long, value_enum, default_value_t = QuerySource::Auto)]
    pub source: QuerySource,

    /// Register an external table before querying. Repeat as needed.
    /// Formats: `NAME=csv:PATH`, `NAME=json:PATH`, `NAME=jsonl:PATH`.
    #[arg(long = "table", value_name = "NAME=FORMAT:PATH")]
    pub tables: Vec<String>,

    /// SQL to execute against runs, steps, and tool_calls.
    #[arg(
        long,
        short = 'q',
        value_name = "SQL",
        required_unless_present = "sql_file",
        conflicts_with = "sql_file"
    )]
    pub sql: Option<String>,

    /// Read SQL from a UTF-8 file; use `-` for stdin.
    #[arg(
        long,
        value_name = "FILE",
        required_unless_present = "sql",
        conflicts_with = "sql"
    )]
    pub sql_file: Option<PathBuf>,
}

impl QuerySource {
    fn resolve(self, input: &str) -> Result<Self> {
        let path = Path::new(input);
        match self {
            Self::Auto if is_lance_object_store_uri(input) => Ok(Self::Lance),
            Self::Auto if path.join("CURRENT").is_file() => Ok(Self::Lance),
            Self::Auto if path.exists() => Ok(Self::Atif),
            Self::Auto => bail!("query input does not exist: {input}"),
            explicit => Ok(explicit),
        }
    }
}

fn is_lance_object_store_uri(input: &str) -> bool {
    let Some((scheme, _)) = input.split_once("://") else {
        return false;
    };
    matches!(
        scheme.to_ascii_lowercase().as_str(),
        "s3" | "az" | "gs" | "memory" | "shared-memory" | "file"
    )
}

/// Execute one read-only SQL statement and write JSONL rows to stdout.
pub async fn run_query(args: QueryArgs) -> Result<()> {
    let sql = read_sql(&args)?;
    let external_tables = parse_external_tables(&args.tables)?;
    let engine = match args.source.resolve(&args.input)? {
        QuerySource::Lance => ChronicleQueryEngine::open_lance_uri(&args.input)
            .await
            .with_context(|| format!("open Lance store {}", args.input))?,
        QuerySource::Atif => ChronicleQueryEngine::open_atif(Path::new(&args.input))
            .with_context(|| format!("open ATIF input {}", args.input))?,
        QuerySource::Auto => unreachable!("auto source is resolved above"),
    };
    for table in &external_tables {
        engine.register_external_table(table).await?;
    }
    let output = engine.query_jsonl(&sql).await?;
    match io::stdout().lock().write_all(output.as_bytes()) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        Err(error) => Err(error).context("write query JSONL to stdout"),
    }
}

fn parse_external_tables(values: &[String]) -> Result<Vec<ExternalTableSpec>> {
    let mut names = HashSet::with_capacity(values.len());
    values
        .iter()
        .map(|value| {
            let (name, source) = value.split_once('=').ok_or_else(|| {
                anyhow::anyhow!("invalid --table {value:?}; expected NAME=FORMAT:PATH")
            })?;
            let (format, path) = source.split_once(':').ok_or_else(|| {
                anyhow::anyhow!("invalid --table {value:?}; expected NAME=FORMAT:PATH")
            })?;
            let name = name.trim();
            let path = path.trim();
            if name.is_empty() || path.is_empty() {
                bail!("invalid --table {value:?}; NAME and PATH must not be empty");
            }
            let format = match format.trim().to_ascii_lowercase().as_str() {
                "csv" => ExternalTableFormat::Csv,
                "json" => ExternalTableFormat::Json,
                "jsonl" | "ndjson" => ExternalTableFormat::JsonLines,
                other => bail!(
                    "unsupported --table format {other:?}; expected csv, json, jsonl, or ndjson"
                ),
            };
            if !names.insert(name.to_string()) {
                bail!("duplicate --table name {name:?}");
            }
            Ok(ExternalTableSpec::new(name, format, path))
        })
        .collect()
}

fn read_sql(args: &QueryArgs) -> Result<String> {
    match (&args.sql, &args.sql_file) {
        (Some(sql), None) => Ok(sql.clone()),
        (None, Some(path)) if path == Path::new("-") => {
            let mut sql = String::new();
            io::stdin()
                .read_to_string(&mut sql)
                .context("read SQL from stdin")?;
            Ok(sql)
        }
        (None, Some(path)) => {
            fs::read_to_string(path).with_context(|| format!("read SQL file {}", path.display()))
        }
        _ => bail!("provide exactly one of --sql or --sql-file"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_detects_lance_root_and_atif_file() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let lance = temp.path().join("lance");
        fs::create_dir(&lance)?;
        fs::write(lance.join("CURRENT"), "gen-test\n")?;
        let atif = temp.path().join("input.jsonl");
        fs::write(&atif, "{}\n")?;

        assert!(matches!(
            QuerySource::Auto.resolve(lance.to_str().unwrap())?,
            QuerySource::Lance
        ));
        assert!(matches!(
            QuerySource::Auto.resolve(atif.to_str().unwrap())?,
            QuerySource::Atif
        ));
        for uri in [
            "s3://trajectory-bucket/storylines",
            "S3://trajectory-bucket/storylines",
            "az://container/storylines",
            "gs://bucket/storylines",
            "memory://storylines",
            "shared-memory://process/storylines",
            "file:///tmp/storylines",
        ] {
            assert!(
                matches!(QuerySource::Auto.resolve(uri)?, QuerySource::Lance),
                "expected Lance auto-detection for {uri}"
            );
        }
        Ok(())
    }

    #[test]
    fn auto_rejects_unknown_uri_and_missing_local_input() {
        for input in [
            "https://example.com/storylines",
            "/definitely/missing/input",
        ] {
            let error = QuerySource::Auto.resolve(input).unwrap_err();
            assert!(error.to_string().contains("does not exist"), "{error:#}");
        }
        assert!(matches!(
            QuerySource::Lance
                .resolve("https://example.com/explicit")
                .unwrap(),
            QuerySource::Lance
        ));
    }

    #[test]
    fn reads_inline_and_file_sql() -> Result<()> {
        let inline = QueryArgs {
            input: "ignored".into(),
            source: QuerySource::Auto,
            tables: Vec::new(),
            sql: Some("SELECT 1".into()),
            sql_file: None,
        };
        assert_eq!(read_sql(&inline)?, "SELECT 1");

        let temp = tempfile::NamedTempFile::new()?;
        fs::write(temp.path(), "SELECT * FROM steps")?;
        let file = QueryArgs {
            input: "ignored".into(),
            source: QuerySource::Auto,
            tables: Vec::new(),
            sql: None,
            sql_file: Some(temp.path().to_owned()),
        };
        assert_eq!(read_sql(&file)?, "SELECT * FROM steps");
        Ok(())
    }

    #[test]
    fn parses_repeatable_external_tables_and_preserves_uri_colons() -> Result<()> {
        let tables = parse_external_tables(&[
            "labels=csv:/tmp/labels.csv".into(),
            "metadata=json:s3://bucket/metadata.json".into(),
            "events=ndjson:/tmp/events.ndjson".into(),
        ])?;

        assert_eq!(
            tables,
            vec![
                ExternalTableSpec::new("labels", ExternalTableFormat::Csv, "/tmp/labels.csv"),
                ExternalTableSpec::new(
                    "metadata",
                    ExternalTableFormat::Json,
                    "s3://bucket/metadata.json"
                ),
                ExternalTableSpec::new(
                    "events",
                    ExternalTableFormat::JsonLines,
                    "/tmp/events.ndjson"
                ),
            ]
        );
        Ok(())
    }

    #[test]
    fn rejects_invalid_external_table_specs() {
        for value in [
            "missing-format",
            "=csv:/tmp/labels.csv",
            "labels=csv:",
            "labels=parquet:/tmp/labels.parquet",
            "labels=csv:/tmp/a.csv,labels=csv:/tmp/b.csv",
        ] {
            let values = if value.contains(',') {
                value.split(',').map(str::to_owned).collect::<Vec<_>>()
            } else {
                vec![value.to_owned()]
            };
            assert!(parse_external_tables(&values).is_err(), "accepted {value}");
        }
    }

    #[test]
    fn query_help_documents_repeatable_external_tables() {
        let mut command = QueryArgs::augment_args(clap::Command::new("query"));
        let help = command.render_long_help().to_string();
        assert!(help.contains("--table <NAME=FORMAT:PATH>"), "{help}");
        assert!(help.contains("NAME=jsonl:PATH"), "{help}");
    }
}
