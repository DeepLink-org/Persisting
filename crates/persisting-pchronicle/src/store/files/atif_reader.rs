//! Bounded-memory ATIF document reader shared by conversion and query paths.

use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use anyhow::{Context, Result};

use crate::format::DocumentFormat;
use crate::formats::atif::AtifFormat;
use crate::formats::codec::{DecodeContext, DocumentSource, TrajectoryFormat};
use crate::formats::common::json_stream::BoundedCountingReader;
use crate::formats::storyline::StorylineDocument;

use super::{
    DEFAULT_LOCAL_QUERY_MAX_FILE_BYTES, DEFAULT_LOCAL_QUERY_MAX_RECORD_BYTES, LocalQueryInputFile,
    LocalQueryManifest,
};

/// Bounded-memory ATIF reader.
///
/// Each file is decoded through [`AtifFormat`] so JSON objects, arrays, and
/// JSONL share the same `DecodeContext` limits as the generic codec trait.
pub(crate) struct AtifReader {
    files: std::vec::IntoIter<LocalQueryInputFile>,
    pending: std::vec::IntoIter<StorylineDocument>,
    max_file_bytes: u64,
    max_record_bytes: usize,
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
            pending: Vec::new().into_iter(),
            max_file_bytes,
            max_record_bytes,
        }
    }

    fn decode_file(&self, file: LocalQueryInputFile) -> Result<Vec<StorylineDocument>> {
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
        let source = DocumentSource::new(file.relative_path());
        let ctx =
            DecodeContext::new(&source).with_limits(self.max_file_bytes, self.max_record_bytes);
        let mut stories = Vec::new();
        AtifFormat
            .decode(&mut reader, &ctx, &mut |story| {
                stories.push(story);
                Ok(())
            })
            .map_err(anyhow::Error::from)
            .with_context(|| format!("parse ATIF datasource {}", file.path().display()))?;
        file.validate_unchanged()?;
        Ok(stories)
    }
}

impl Iterator for AtifReader {
    type Item = Result<StorylineDocument>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(story) = self.pending.next() {
                return Some(Ok(story));
            }
            let file = self.files.next()?;
            match self.decode_file(file) {
                Ok(stories) => self.pending = stories.into_iter(),
                Err(error) => return Some(Err(error)),
            }
        }
    }
}

#[cfg(test)]
fn parse_atif_storylines_from_reader<R: std::io::BufRead>(
    path: &Path,
    reader: &mut R,
    max_record_bytes: usize,
) -> Result<Vec<StorylineDocument>> {
    let source = DocumentSource::new(path.to_string_lossy().replace('\\', "/"));
    let ctx = DecodeContext::new(&source).with_limits(u64::MAX, max_record_bytes);
    let mut stories = Vec::new();
    AtifFormat
        .decode(reader, &ctx, &mut |story| {
            stories.push(story);
            Ok(())
        })
        .map_err(anyhow::Error::from)?;
    Ok(stories)
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

    #[cfg(feature = "proptest")]
    mod proptests {
        use proptest::prelude::*;
        use serde_json::json;

        use super::*;

        proptest! {
            #[test]
            fn reader_roundtrips_valid_single_step_documents(
                message in proptest::string::string_regex("[A-Za-z0-9 .,!?_-]{0,64}").unwrap(),
            ) {
                let document = json!({
                    "schema_version": "ATIF-v1.7",
                    "session_id": "session",
                    "trajectory_id": "trajectory",
                    "agent": {"name": "agent", "version": "1"},
                    "steps": [{
                        "step_id": 1,
                        "timestamp": "2026-01-01T00:00:00Z",
                        "source": "user",
                        "message": message,
                    }]
                });
                let raw = serde_json::to_vec(&document).unwrap();
                let path = Path::new("generated.atif.json");
                let mut reader = Cursor::new(raw);
                let stories = parse_atif_storylines_from_reader(
                    path,
                    &mut reader,
                    DEFAULT_LOCAL_QUERY_MAX_RECORD_BYTES,
                ).unwrap();
                prop_assert_eq!(stories.len(), 1);
                prop_assert_eq!(stories[0].turns.len(), 1);
                prop_assert_eq!(&stories[0].turns[0].message, &json!(message));
            }
        }
    }
}
