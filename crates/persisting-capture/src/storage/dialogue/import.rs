use anyhow::{Context, Result};

use super::block::block_to_capture_record;
use crate::record::record_to_engine_line;
use crate::storage::markdown;

pub fn import_markdown_to_engine_lines(doc: &str) -> Result<String> {
    markdown::parse_document(doc)?
        .iter()
        .enumerate()
        .map(|(i, b)| {
            record_to_engine_line(&block_to_capture_record(b)?)
                .with_context(|| format!("block[{i}]"))
        })
        .collect::<Result<Vec<_>>>()
        .map(|v| v.join("\n"))
}
