use super::super::*;
use anyhow::{Context, Result};
use serde::Serialize;
use std::io::{Read, Write};
use std::path::Path;

#[derive(Serialize)]
struct DropResponse {
    dataset_uri: String,
    dropped: bool,
}

pub(crate) async fn run_drop(
    args: DropArgs,
    settings_override: Option<&Path>,
    stdin_is_terminal: bool,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Result<()> {
    let dataset_uri = expand_dataset_reference(&args.dataset_uri, settings_override, false)?;
    let mut location = DatasetLocation::parse(&dataset_uri)?;
    if !location.exists().await? {
        return Err(cli_boundary_error(
            BoundaryCode::NotFound,
            format!("Dataset does not exist: {}", location.as_str()),
        ));
    }
    if location.local_path().is_some() {
        location = location.into_existing()?;
    }
    confirm_destructive_dataset(
        "drop",
        location.as_str(),
        args.yes,
        stdin_is_terminal,
        stdin,
        stderr,
    )?;
    location.remove_all().await?;
    let response = DropResponse {
        dataset_uri: location.as_str().to_string(),
        dropped: true,
    };
    serde_json::to_writer_pretty(&mut *stdout, &response).context("encode pChronicle drop JSON")?;
    writeln!(stdout).context("write pChronicle drop JSON")?;
    writeln!(
        stderr,
        "dataset_uri={} status=dropped",
        response.dataset_uri
    )
    .context("write pChronicle drop metadata")?;
    Ok(())
}

pub(crate) fn confirm_destructive_dataset(
    action: &str,
    dataset_uri: &str,
    yes: bool,
    stdin_is_terminal: bool,
    stdin: &mut dyn Read,
    stderr: &mut dyn Write,
) -> Result<()> {
    if yes {
        return Ok(());
    }
    if !stdin_is_terminal {
        return Err(cli_boundary_error(
            BoundaryCode::InvalidRequest,
            format!("{action} requires confirmation; rerun with --yes"),
        ));
    }
    write!(
        stderr,
        "Permanently {action} Dataset '{dataset_uri}'? [y/N] "
    )
    .context("write Dataset confirmation prompt")?;
    stderr
        .flush()
        .context("flush Dataset confirmation prompt")?;
    let mut answer = Vec::new();
    let mut byte = [0u8; 1];
    while answer.len() <= 16 && stdin.read(&mut byte).context("read Dataset confirmation")? == 1 {
        if byte[0] == b'\n' {
            break;
        }
        answer.push(byte[0]);
    }
    let answer = std::str::from_utf8(&answer)
        .context("Dataset confirmation is not UTF-8")?
        .trim();
    if matches!(answer.to_ascii_lowercase().as_str(), "y" | "yes") {
        return Ok(());
    }
    Err(cli_boundary_error(
        BoundaryCode::InvalidRequest,
        format!("{action} cancelled"),
    ))
}
