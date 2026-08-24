//! Bounded-memory ACTF document reader for local query paths.

use std::io::{self, BufRead, Read};
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::convert::actf_to_storylines;
use crate::formats::actf::ActfDocument;
use crate::formats::storyline::StorylineDocument;
use crate::InputIssue;

use crate::formats::common::json_stream::{visit_json_stream, ScopedJsonObjectReader};

#[cfg(test)]
pub(crate) fn parse_actf_storylines_from_reader<R: BufRead>(
    path: &Path,
    reader: &mut R,
    max_record_bytes: usize,
) -> Result<Vec<StorylineDocument>> {
    parse_actf_storylines_from_reader_with_stats(path, reader, max_record_bytes)
        .map(|(stories, _)| stories)
}

pub(super) fn parse_actf_storylines_from_reader_with_stats<R: BufRead>(
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
            push_actf_stories_from_scoped(
                &mut ScopedJsonObjectReader::new(reader, max_record_bytes),
                stories,
            )
        },
        |record, location, stories| {
            push_actf_stories_from_slice(record, stories).map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!("parse ACTF {location} in {}: {error}", path.display()),
                )
            })
        },
    )
    .map_err(|error| InputIssue::invalid(error.to_string()).at(path.display().to_string()))
    .with_context(|| format!("read ACTF input {}", path.display()))?;
    anyhow::ensure!(
        visit.record_count > 0 && !stories.is_empty(),
        "ACTF input contains no trajectories: {}",
        path.display()
    );
    Ok((stories, visit.peak_record_bytes))
}

fn push_actf_stories_from_scoped<R: BufRead>(
    scoped: &mut ScopedJsonObjectReader<'_, R>,
    stories: &mut Vec<StorylineDocument>,
) -> io::Result<()> {
    let document = deserialize_actf_document(&mut *scoped)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if !scoped.is_finished() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "ACTF JSON object was not fully consumed",
        ));
    }
    extend_actf_stories(document, stories)
}

fn push_actf_stories_from_slice(
    record: &[u8],
    stories: &mut Vec<StorylineDocument>,
) -> io::Result<()> {
    let mut deserializer = serde_json::Deserializer::from_slice(record);
    let document = ActfDocument::deserialize(&mut deserializer)
        .and_then(|document| {
            deserializer.end()?;
            Ok(document)
        })
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    document
        .validate()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    extend_actf_stories(document, stories)
}

fn extend_actf_stories(
    document: ActfDocument,
    stories: &mut Vec<StorylineDocument>,
) -> io::Result<()> {
    stories.extend(
        actf_to_storylines(&document)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
    );
    Ok(())
}

fn deserialize_actf_document<R: Read>(reader: R) -> Result<ActfDocument> {
    let mut deserializer = serde_json::Deserializer::from_reader(reader);
    let document =
        ActfDocument::deserialize(&mut deserializer).context("deserialize ACTF document")?;
    deserializer
        .end()
        .context("finish ACTF document deserialization")?;
    document
        .validate()
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    Ok(document)
}

#[cfg(test)]
mod tests {
    use super::super::test_support::fixture_path;
    use super::*;
    use std::io::Cursor;

    #[test]
    fn streams_single_actf_object_without_whole_file_buffer() {
        let path = fixture_path("import_roundtrip/protein-assembly_trimmed.actf.json");
        let raw = std::fs::read(&path).unwrap();
        let mut reader = Cursor::new(raw);
        let stories =
            parse_actf_storylines_from_reader(&path, &mut reader, 64 * 1024 * 1024).unwrap();
        assert!(!stories.is_empty());
    }

    #[test]
    fn streams_actf_array_without_record_vec() {
        let path = fixture_path("import_roundtrip/protein-assembly_trimmed.actf.json");
        let object = std::fs::read_to_string(&path).unwrap();
        let corpus = format!("[{object},{object}]");
        let mut reader = Cursor::new(corpus.into_bytes());
        let stories =
            parse_actf_storylines_from_reader(&path, &mut reader, 64 * 1024 * 1024).unwrap();
        assert!(stories.len() >= 2);
    }

    #[test]
    fn enforces_max_record_bytes_on_actf_array_elements() {
        let path = fixture_path("import_roundtrip/protein-assembly_trimmed.actf.json");
        let object = std::fs::read_to_string(&path).unwrap();
        let corpus = format!("[{object}]");
        let mut reader = Cursor::new(corpus.into_bytes());
        let error = parse_actf_storylines_from_reader(&path, &mut reader, 512).unwrap_err();
        assert!(
            format!("{error:#}").contains("max_record_bytes 512"),
            "{error:#}"
        );
    }

    #[test]
    fn invalid_actf_stream_preserves_the_input_issue_boundary() {
        let path = Path::new("invalid.actf.json");
        let mut reader = Cursor::new(b"not-json");
        let error = parse_actf_storylines_from_reader(path, &mut reader, 1024).unwrap_err();

        assert!(
            error
                .chain()
                .any(|source| source.downcast_ref::<crate::InputIssue>().is_some()),
            "missing InputIssue source: {error:#}"
        );
    }
}
