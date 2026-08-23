//! Bounded-memory ATIF document reader shared by conversion and query paths.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Read};
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::convert::atif_collection_to_storylines;
use crate::format::DocumentFormat;
use crate::formats::storyline::StorylineDocument;
use crate::InputIssue;

use super::json_stream::{
    read_bounded_line, trim_ascii_whitespace, visit_json_stream, BoundedCountingReader,
    ScopedJsonObjectReader,
};
use super::{
    LocalQueryInputFile, LocalQueryManifest, DEFAULT_LOCAL_QUERY_MAX_FILE_BYTES,
    DEFAULT_LOCAL_QUERY_MAX_RECORD_BYTES,
};

/// Bounded-memory ATIF reader.
///
/// JSONL/NDJSON inputs are decoded one non-empty line at a time. Object and
/// array `.json` files are streamed through the shared JSON document reader
/// without loading the whole file into memory first.
pub(crate) struct AtifReader {
    files: std::vec::IntoIter<LocalQueryInputFile>,
    current: Option<AtifFileReader>,
    pending: std::vec::IntoIter<StorylineDocument>,
    max_file_bytes: u64,
    max_record_bytes: usize,
}

enum AtifFileReader {
    Ndjson {
        file: LocalQueryInputFile,
        reader: BufReader<BoundedCountingReader<File>>,
        record: Vec<u8>,
        line_number: usize,
    },
    Documents(std::vec::IntoIter<StorylineDocument>),
}

impl AtifReader {
    pub(crate) fn open(path: impl AsRef<Path>) -> Result<Self> {
        let manifest = LocalQueryManifest::for_format(path, DocumentFormat::Atif)?;
        Ok(Self::from_manifest(
            &manifest,
            DEFAULT_LOCAL_QUERY_MAX_FILE_BYTES,
            DEFAULT_LOCAL_QUERY_MAX_RECORD_BYTES,
        ))
    }

    pub(crate) fn from_manifest(
        manifest: &LocalQueryManifest,
        max_file_bytes: u64,
        max_record_bytes: usize,
    ) -> Self {
        Self {
            files: manifest.files().to_vec().into_iter(),
            current: None,
            pending: Vec::new().into_iter(),
            max_file_bytes,
            max_record_bytes,
        }
    }

    fn open_file(&self, file: LocalQueryInputFile) -> Result<AtifFileReader> {
        file.validate_unchanged()?;
        anyhow::ensure!(
            file.size_bytes() <= self.max_file_bytes,
            "ATIF input {} is {} bytes, exceeding max_file_bytes {}",
            file.path().display(),
            file.size_bytes(),
            self.max_file_bytes
        );
        let input = File::open(file.path())
            .with_context(|| format!("open ATIF datasource {}", file.path().display()))?;
        let mut reader = BufReader::new(BoundedCountingReader::new(input, self.max_file_bytes));
        match file.path().extension().and_then(|value| value.to_str()) {
            Some("jsonl" | "ndjson") => Ok(AtifFileReader::Ndjson {
                file,
                reader,
                record: Vec::new(),
                line_number: 0,
            }),
            _ => {
                let documents = parse_atif_storylines_from_reader(
                    file.path(),
                    &mut reader,
                    self.max_record_bytes,
                )
                .with_context(|| format!("parse ATIF datasource {}", file.path().display()))?;
                file.validate_unchanged()?;
                Ok(AtifFileReader::Documents(documents.into_iter()))
            }
        }
    }
}

impl Iterator for AtifReader {
    type Item = Result<StorylineDocument>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(story) = self.pending.next() {
                return Some(Ok(story));
            }
            if let Some(current) = &mut self.current {
                match current {
                    AtifFileReader::Documents(documents) => {
                        if let Some(document) = documents.next() {
                            return Some(Ok(document));
                        }
                    }
                    AtifFileReader::Ndjson {
                        file,
                        reader,
                        record,
                        line_number,
                    } => loop {
                        *line_number += 1;
                        let length = match read_bounded_line(reader, record, self.max_record_bytes)
                        {
                            Ok(length) => length,
                            Err(error) => {
                                return Some(
                                    Err(anyhow::Error::new(
                                        InputIssue::invalid(error.to_string()).at(format!(
                                            "{} line {}",
                                            file.path().display(),
                                            line_number
                                        )),
                                    ))
                                    .with_context(|| {
                                        format!(
                                            "read ATIF datasource {} line {}",
                                            file.path().display(),
                                            line_number
                                        )
                                    }),
                                );
                            }
                        };
                        if length == 0 {
                            if let Err(error) = file.validate_unchanged() {
                                return Some(Err(error));
                            }
                            break;
                        }
                        let line = trim_ascii_whitespace(record);
                        if line.is_empty() {
                            continue;
                        }
                        let value =
                            match serde_json::from_slice(line)
                                .map_err(|error| {
                                    anyhow::Error::new(InputIssue::invalid(error.to_string()).at(
                                        format!("{} line {}", file.path().display(), line_number),
                                    ))
                                })
                                .with_context(|| {
                                    format!(
                                        "parse ATIF datasource {} line {}",
                                        file.path().display(),
                                        line_number
                                    )
                                }) {
                                Ok(value) => value,
                                Err(error) => return Some(Err(error)),
                            };
                        let stories = match atif_collection_to_storylines(value) {
                            Ok(stories) => stories,
                            Err(error) => return Some(Err(error)),
                        };
                        self.pending = stories.into_iter();
                        return self.pending.next().map(Ok);
                    },
                }
                self.current = None;
            }

            let file = self.files.next()?;
            match self.open_file(file) {
                Ok(reader) => self.current = Some(reader),
                Err(error) => return Some(Err(error)),
            }
        }
    }
}

fn parse_atif_storylines_from_reader<R: BufRead>(
    path: &Path,
    reader: &mut R,
    max_record_bytes: usize,
) -> Result<Vec<StorylineDocument>> {
    parse_atif_storylines_from_reader_with_stats(path, reader, max_record_bytes)
        .map(|(stories, _)| stories)
}

pub(super) fn parse_atif_storylines_from_reader_with_stats<R: BufRead>(
    path: &Path,
    reader: &mut R,
    max_record_bytes: usize,
) -> Result<(Vec<StorylineDocument>, usize)> {
    let mut stories = Vec::new();
    let visit = visit_json_stream(
        path,
        reader,
        max_record_bytes,
        &mut stories,
        |reader, stories| {
            push_atif_stories_from_scoped(
                &mut ScopedJsonObjectReader::new(reader, max_record_bytes),
                stories,
            )
        },
        |record, location, stories| {
            push_atif_stories_from_slice(record, stories).map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!("parse ATIF {location} in {}: {error}", path.display()),
                )
            })
        },
    )
    .map_err(|error| InputIssue::invalid(error.to_string()).at(path.display().to_string()))
    .with_context(|| format!("read ATIF input {}", path.display()))?;
    anyhow::ensure!(
        visit.record_count > 0 && !stories.is_empty(),
        "ATIF input contains no trajectories: {}",
        path.display()
    );
    Ok((stories, visit.peak_record_bytes))
}

fn push_atif_stories_from_slice(
    record: &[u8],
    stories: &mut Vec<StorylineDocument>,
) -> io::Result<()> {
    let mut deserializer = serde_json::Deserializer::from_slice(record);
    let value = serde_json::Value::deserialize(&mut deserializer)
        .and_then(|value| {
            deserializer.end()?;
            Ok(value)
        })
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    extend_atif_stories(value, stories)
}

fn push_atif_stories_from_scoped<R: BufRead>(
    scoped: &mut ScopedJsonObjectReader<'_, R>,
    stories: &mut Vec<StorylineDocument>,
) -> io::Result<()> {
    let value = deserialize_atif_value(&mut *scoped)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if !scoped.is_finished() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "ATIF JSON object was not fully consumed",
        ));
    }
    extend_atif_stories(value, stories)
}

fn extend_atif_stories(
    value: serde_json::Value,
    stories: &mut Vec<StorylineDocument>,
) -> io::Result<()> {
    stories.extend(
        atif_collection_to_storylines(value)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
    );
    Ok(())
}

fn deserialize_atif_value<R: Read>(reader: R) -> Result<serde_json::Value> {
    let mut deserializer = serde_json::Deserializer::from_reader(reader);
    let value =
        serde_json::Value::deserialize(&mut deserializer).context("deserialize ATIF trajectory")?;
    deserializer
        .end()
        .context("finish ATIF trajectory deserialization")?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::super::test_support::fixture_path;
    use super::*;
    use std::io::Cursor;

    #[test]
    fn streams_atif_fixture_without_whole_file_buffer() {
        let path = fixture_path("atif/dialogue_10.json");
        let raw = std::fs::read(&path).unwrap();
        let mut reader = Cursor::new(raw);
        let stories = parse_atif_storylines_from_reader(
            &path,
            &mut reader,
            DEFAULT_LOCAL_QUERY_MAX_RECORD_BYTES,
        )
        .unwrap();
        assert!(!stories.is_empty());
    }

    #[test]
    fn ndjson_reader_enforces_the_configured_record_limit() {
        let input = tempfile::NamedTempFile::with_suffix(".ndjson").unwrap();
        let trajectory: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(fixture_path("atif/dialogue_10.json")).unwrap(),
        )
        .unwrap();
        std::fs::write(
            input.path(),
            format!("{}\n", serde_json::to_string(&trajectory).unwrap()),
        )
        .unwrap();
        let manifest = LocalQueryManifest::for_format(input.path(), DocumentFormat::Atif).unwrap();
        let mut reader =
            AtifReader::from_manifest(&manifest, DEFAULT_LOCAL_QUERY_MAX_FILE_BYTES, 512);

        let error = reader.next().unwrap().unwrap_err();
        assert!(
            format!("{error:#}").contains("max_record_bytes 512"),
            "{error:#}"
        );
    }
}
