//! Public pPilot SQL query command backed by pChronicle.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::{Args, ValueEnum};
use persisting_pchronicle::ChronicleQueryEngine;

#[derive(Debug, Clone, Copy, Default, ValueEnum)]
pub enum QuerySource {
    #[default]
    Auto,
    Lance,
    Atif,
}

#[derive(Debug, Args)]
pub struct QueryArgs {
    /// Three-table Lance store root, or an ATIF JSON/JSONL file or directory.
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Input representation. `auto` treats a directory containing CURRENT as Lance.
    #[arg(long, value_enum, default_value_t = QuerySource::Auto)]
    pub source: QuerySource,

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
    fn resolve(self, input: &Path) -> Result<Self> {
        match self {
            Self::Auto if input.join("CURRENT").is_file() => Ok(Self::Lance),
            Self::Auto if input.exists() => Ok(Self::Atif),
            Self::Auto => bail!("query input does not exist: {}", input.display()),
            explicit => Ok(explicit),
        }
    }
}

/// Execute one read-only SQL statement and write JSONL rows to stdout.
pub async fn run_query(args: QueryArgs) -> Result<()> {
    let sql = read_sql(&args)?;
    let engine = match args.source.resolve(&args.input)? {
        QuerySource::Lance => ChronicleQueryEngine::open_lance(&args.input)
            .await
            .with_context(|| format!("open Lance store {}", args.input.display()))?,
        QuerySource::Atif => ChronicleQueryEngine::open_atif(&args.input)
            .with_context(|| format!("open ATIF input {}", args.input.display()))?,
        QuerySource::Auto => unreachable!("auto source is resolved above"),
    };
    let output = engine.query_jsonl(&sql).await?;
    match io::stdout().lock().write_all(output.as_bytes()) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        Err(error) => Err(error).context("write query JSONL to stdout"),
    }
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
            QuerySource::Auto.resolve(&lance)?,
            QuerySource::Lance
        ));
        assert!(matches!(
            QuerySource::Auto.resolve(&atif)?,
            QuerySource::Atif
        ));
        Ok(())
    }

    #[test]
    fn reads_inline_and_file_sql() -> Result<()> {
        let inline = QueryArgs {
            input: PathBuf::from("ignored"),
            source: QuerySource::Auto,
            sql: Some("SELECT 1".into()),
            sql_file: None,
        };
        assert_eq!(read_sql(&inline)?, "SELECT 1");

        let temp = tempfile::NamedTempFile::new()?;
        fs::write(temp.path(), "SELECT * FROM steps")?;
        let file = QueryArgs {
            input: PathBuf::from("ignored"),
            source: QuerySource::Auto,
            sql: None,
            sql_file: Some(temp.path().to_owned()),
        };
        assert_eq!(read_sql(&file)?, "SELECT * FROM steps");
        Ok(())
    }
}
