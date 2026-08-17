//! Bounded-memory ATIF document reader shared by conversion and query paths.

use std::fs::File;
use std::io::{BufRead, BufReader, Lines};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::atif::AtifTrajectory;
use crate::format::DocumentFormat;

use super::{LocalQueryInputFile, LocalQueryManifest};

/// Bounded-memory ATIF reader.
///
/// JSONL/NDJSON inputs are decoded one non-empty line at a time. Directories
/// are traversed in stable path order and only the current file is open. A
/// regular `.json` file may contain one object or an array and is buffered per
/// file for compatibility; large corpora should use NDJSON.
pub struct AtifReader {
    files: std::vec::IntoIter<PathBuf>,
    current: Option<AtifFileReader>,
}

enum AtifFileReader {
    Lines {
        path: PathBuf,
        lines: Lines<BufReader<File>>,
        line_number: usize,
    },
    Documents(std::vec::IntoIter<AtifTrajectory>),
}

impl AtifReader {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let manifest = LocalQueryManifest::for_format(path, DocumentFormat::Atif)?;
        Ok(Self::from_files(manifest.files()))
    }

    pub(crate) fn from_manifest(manifest: &LocalQueryManifest) -> Self {
        Self::from_files(manifest.files())
    }

    fn from_files(files: &[LocalQueryInputFile]) -> Self {
        Self {
            files: files
                .iter()
                .map(|file| file.path().to_path_buf())
                .collect::<Vec<_>>()
                .into_iter(),
            current: None,
        }
    }

    fn open_file(path: PathBuf) -> Result<AtifFileReader> {
        match path.extension().and_then(|value| value.to_str()) {
            Some("jsonl" | "ndjson") => {
                let file = File::open(&path)
                    .with_context(|| format!("open ATIF datasource {}", path.display()))?;
                Ok(AtifFileReader::Lines {
                    path,
                    lines: BufReader::new(file).lines(),
                    line_number: 0,
                })
            }
            _ => {
                let input = std::fs::read_to_string(&path)
                    .with_context(|| format!("read ATIF datasource {}", path.display()))?;
                let documents = parse_documents(&input)
                    .with_context(|| format!("parse ATIF datasource {}", path.display()))?;
                Ok(AtifFileReader::Documents(documents.into_iter()))
            }
        }
    }
}

impl Iterator for AtifReader {
    type Item = Result<AtifTrajectory>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(current) = &mut self.current {
                match current {
                    AtifFileReader::Documents(documents) => {
                        if let Some(document) = documents.next() {
                            return Some(Ok(document));
                        }
                    }
                    AtifFileReader::Lines {
                        path,
                        lines,
                        line_number,
                    } => {
                        for line in lines.by_ref() {
                            *line_number += 1;
                            let line = match line {
                                Ok(line) => line,
                                Err(error) => {
                                    return Some(Err(error).with_context(|| {
                                        format!(
                                            "read ATIF datasource {} line {}",
                                            path.display(),
                                            line_number
                                        )
                                    }));
                                }
                            };
                            if line.trim().is_empty() {
                                continue;
                            }
                            return Some(
                                AtifTrajectory::from_json_str(line.trim())
                                    .map_err(anyhow::Error::from)
                                    .with_context(|| {
                                        format!(
                                            "parse ATIF datasource {} line {}",
                                            path.display(),
                                            line_number
                                        )
                                    }),
                            );
                        }
                    }
                }
                self.current = None;
            }

            let path = self.files.next()?;
            match Self::open_file(path) {
                Ok(reader) => self.current = Some(reader),
                Err(error) => return Some(Err(error)),
            }
        }
    }
}

/// Load and validate ATIF documents for callers that partition work before
/// building a query engine.
pub fn load_atif_trajectories(path: impl AsRef<Path>) -> Result<Vec<AtifTrajectory>> {
    AtifReader::open(path)?.collect()
}

pub(crate) fn parse_documents(input: &str) -> Result<Vec<AtifTrajectory>> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        anyhow::bail!("ATIF input is empty");
    }
    if let Ok(trajectory) = serde_json::from_str::<AtifTrajectory>(trimmed) {
        trajectory.validate().map_err(anyhow::Error::from)?;
        return Ok(vec![trajectory]);
    }
    if let Ok(trajectories) = serde_json::from_str::<Vec<AtifTrajectory>>(trimmed) {
        for trajectory in &trajectories {
            trajectory.validate().map_err(anyhow::Error::from)?;
        }
        return Ok(trajectories);
    }
    trimmed
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, line)| {
            AtifTrajectory::from_json_str(line)
                .map_err(anyhow::Error::from)
                .with_context(|| format!("parse ATIF JSONL line {}", index + 1))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_object_array_and_jsonl() {
        let object = r#"{"session_id":"s","agent":{"name":"a"},"steps":[]}"#;
        assert_eq!(parse_documents(object).unwrap().len(), 1);
        assert_eq!(parse_documents(&format!("[{object}]")).unwrap().len(), 1);
        assert_eq!(
            parse_documents(&format!("{object}\n{object}\n"))
                .unwrap()
                .len(),
            2
        );
        assert!(parse_documents("").is_err());
    }
}
