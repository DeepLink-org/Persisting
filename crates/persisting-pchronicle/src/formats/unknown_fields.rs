use crate::format::DocumentFormat;
use crate::formats::StorylineDocument;
use crate::{InputIssue, InputResult, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{btree_map::Entry, BTreeMap, BTreeSet};

/// Default unknown-field count limit. `usize::MAX` means unbounded.
pub const DEFAULT_MAX_UNKNOWN_FIELDS: usize = usize::MAX;
/// Default unknown-field byte limit. `usize::MAX` means unbounded.
pub const DEFAULT_MAX_UNKNOWN_BYTES: usize = usize::MAX;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnknownFieldLimits {
    /// Maximum logical unknown fields, or `usize::MAX` for no field-count limit.
    pub max_fields: usize,
    /// Maximum logical JSON bytes, or `usize::MAX` for no byte limit.
    pub max_bytes: usize,
}

impl Default for UnknownFieldLimits {
    fn default() -> Self {
        Self {
            max_fields: DEFAULT_MAX_UNKNOWN_FIELDS,
            max_bytes: DEFAULT_MAX_UNKNOWN_BYTES,
        }
    }
}

impl UnknownFieldLimits {
    pub fn validate(self) -> InputResult<()> {
        if self.max_fields == 0 {
            return Err(InputIssue::invalid("unknown field limit must be positive"));
        }
        if self.max_bytes == 0 {
            return Err(InputIssue::invalid("unknown byte limit must be positive"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SourceUnknownFields {
    pub source_document_id: String,
    pub fields: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StorylineUnknownFields {
    pub sources: BTreeMap<String, SourceUnknownFields>,
}

pub type UnknownFieldCounts = BTreeMap<String, u64>;
pub type UnknownKeyCounts = BTreeMap<String, UnknownFieldCounts>;

/// Aggregates normalized unknown-field key counts for one import command.
///
/// Deduplication is by `(source format, normalized key)`. Directory importers
/// reuse one aggregator across every nested Source in the command.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct UnknownFieldImportWarnings {
    counts: UnknownKeyCounts,
}

impl UnknownFieldImportWarnings {
    /// Observe all Storylines decoded from one physical input Source.
    ///
    /// Converters may attach a document-level unknown pointer to multiple
    /// Storylines. Within this call, identical physical pointers are counted
    /// once by `(source format, source document id, exact pointer)`. Callers
    /// invoke this method separately for each input Source so identical files
    /// still contribute independently to command-wide occurrence totals.
    pub fn observe_storylines<'a>(
        &mut self,
        storylines: impl IntoIterator<Item = &'a StorylineDocument>,
    ) -> InputResult<()> {
        let mut seen = BTreeSet::new();
        for story in storylines {
            for (source, source_fields) in &story.unknown_fields.sources {
                for pointer in source_fields.fields.keys() {
                    let occurrence = (
                        source.clone(),
                        source_fields.source_document_id.clone(),
                        pointer.clone(),
                    );
                    if !seen.insert(occurrence) {
                        continue;
                    }
                    let normalized_pointer = normalize_unknown_pointer(source, pointer)?;
                    let total = self
                        .counts
                        .entry(source.clone())
                        .or_default()
                        .entry(normalized_pointer)
                        .or_default();
                    *total = total.saturating_add(1);
                }
            }
        }
        Ok(())
    }

    pub fn observe(&mut self, counts: &UnknownKeyCounts) {
        for (source, keys) in counts {
            let source_counts = self.counts.entry(source.clone()).or_default();
            for (key, occurrences) in keys {
                let total = source_counts.entry(key.clone()).or_default();
                *total = total.saturating_add(*occurrences);
            }
        }
    }

    pub fn warning_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        for (source, keys) in &self.counts {
            for (key, occurrences) in keys {
                if *occurrences == 0 {
                    continue;
                }
                let source = escape_warning_atom(source);
                let key = escape_warning_atom(key);
                lines.push(format!(
                    "warning: unknown field source={source} key={key} occurrences={occurrences}"
                ));
            }
        }
        lines
    }
}

fn escape_warning_atom(value: &str) -> String {
    let Ok(encoded) = serde_json::to_string(value) else {
        return value.escape_debug().to_string();
    };
    encoded
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .unwrap_or(&encoded)
        .to_owned()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CarrierBinding {
    pub story_index: usize,
    pub pointer: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StorylineEnvelopeWire {
    unknown_fields: UnknownFieldsEnvelopeWire,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct UnknownFieldsEnvelopeWire {
    version: u32,
    by_trajectory: BTreeMap<String, UnknownFieldsCarrierWire>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct UnknownFieldsCarrierWire {
    sources: BTreeMap<String, SourceUnknownFieldsWire>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceUnknownFieldsWire {
    source_document_id: String,
    fields: BTreeMap<String, Value>,
}

pub(crate) fn take_unknown_fields_envelope(
    document: &mut Value,
) -> InputResult<BTreeMap<String, StorylineUnknownFields>> {
    let Some(raw_envelope) = document
        .as_object()
        .and_then(|object| object.get("_storyline"))
        .cloned()
    else {
        return Ok(BTreeMap::new());
    };

    let envelope: StorylineEnvelopeWire = serde_json::from_value(raw_envelope)
        .map_err(|error| InputIssue::invalid(format!("invalid _storyline envelope: {error}")))?;
    if envelope.unknown_fields.version != 1 {
        return Err(InputIssue::invalid(format!(
            "unsupported _storyline unknown_fields version {}; expected 1",
            envelope.unknown_fields.version
        )));
    }

    let mut carried = BTreeMap::new();
    for (carrier, fields) in envelope.unknown_fields.by_trajectory {
        validate_json_pointer(&carrier)?;
        let fields = StorylineUnknownFields {
            sources: fields
                .sources
                .into_iter()
                .map(|(source, fields)| {
                    (
                        source,
                        SourceUnknownFields {
                            source_document_id: fields.source_document_id,
                            fields: fields.fields,
                        },
                    )
                })
                .collect(),
        };
        compute_unknown_key_counts(&fields)?;
        carried.insert(carrier, fields);
    }

    let document = document
        .as_object_mut()
        .ok_or_else(|| InputIssue::invalid("_storyline envelope requires an object document"))?;
    document.remove("_storyline");
    Ok(carried)
}

pub(crate) fn attach_carried_unknown_fields(
    target_format: DocumentFormat,
    envelope: BTreeMap<String, StorylineUnknownFields>,
    carriers: &[CarrierBinding],
    stories: &mut [StorylineDocument],
    limits: UnknownFieldLimits,
) -> InputResult<()> {
    limits.validate()?;
    let by_pointer = validate_carrier_bindings(carriers, stories.len())?;

    let mut merged = stories
        .iter()
        .map(|story| story.unknown_fields.clone())
        .collect::<Vec<_>>();
    for (pointer, carried) in envelope {
        validate_json_pointer(&pointer)?;
        let target_source = target_format.as_str();
        if carried.sources.contains_key(target_source) {
            return Err(InputIssue::invalid(format!(
                "_storyline envelope must not carry target source '{target_source}'"
            )));
        }
        let story_index = by_pointer.get(&pointer).ok_or_else(|| {
            InputIssue::invalid(format!(
                "_storyline envelope carrier '{pointer}' is not bound to a trajectory"
            ))
        })?;
        merge_unknown_fields(&mut merged[*story_index], carried)?;
    }

    let counts = merged
        .iter()
        .map(|fields| validate_unknown_fields(fields, limits))
        .collect::<InputResult<Vec<_>>>()?;
    for ((story, fields), counts) in stories.iter_mut().zip(merged).zip(counts) {
        story.unknown_fields = fields;
        story.unknown_key_counts = counts;
    }
    Ok(())
}

pub(crate) fn write_foreign_unknown_fields_envelope(
    target_format: DocumentFormat,
    document: &mut Value,
    stories: &[StorylineDocument],
    carriers: &[CarrierBinding],
) -> Result<()> {
    if document
        .as_object()
        .is_some_and(|object| object.contains_key("_storyline"))
    {
        anyhow::bail!("target document already contains reserved key '_storyline'");
    }

    let by_pointer = validate_carrier_bindings(carriers, stories.len())?;
    for pointer in by_pointer.keys() {
        if document.pointer(pointer).is_none() {
            anyhow::bail!("carrier JSON Pointer '{pointer}' does not exist in target document");
        }
    }

    let bound_story_indexes = carriers
        .iter()
        .map(|carrier| carrier.story_index)
        .collect::<BTreeSet<_>>();
    let target_source = target_format.as_str();
    for (story_index, story) in stories.iter().enumerate() {
        let has_foreign_fields = story
            .unknown_fields
            .sources
            .iter()
            .any(|(source, fields)| source != target_source && !fields.fields.is_empty());
        if has_foreign_fields && !bound_story_indexes.contains(&story_index) {
            anyhow::bail!(
                "Storyline at index {story_index} has foreign unknown fields but no carrier binding"
            );
        }
    }

    let mut by_trajectory = BTreeMap::new();
    for carrier in carriers {
        let story = &stories[carrier.story_index];
        let sources = story
            .unknown_fields
            .sources
            .iter()
            .filter(|(source, fields)| {
                source.as_str() != target_source && !fields.fields.is_empty()
            })
            .map(|(source, fields)| {
                for pointer in fields.fields.keys() {
                    validate_json_pointer(pointer)?;
                }
                Ok((
                    source.clone(),
                    SourceUnknownFieldsWire {
                        source_document_id: fields.source_document_id.clone(),
                        fields: fields.fields.clone(),
                    },
                ))
            })
            .collect::<InputResult<BTreeMap<_, _>>>()?;
        if !sources.is_empty() {
            by_trajectory.insert(
                carrier.pointer.clone(),
                UnknownFieldsCarrierWire { sources },
            );
        }
    }

    if by_trajectory.is_empty() {
        return Ok(());
    }
    let object = document
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("foreign unknown fields require an object document root"))?;
    let envelope = StorylineEnvelopeWire {
        unknown_fields: UnknownFieldsEnvelopeWire {
            version: 1,
            by_trajectory,
        },
    };
    object.insert("_storyline".into(), serde_json::to_value(envelope)?);
    Ok(())
}

fn validate_carrier_bindings(
    carriers: &[CarrierBinding],
    story_count: usize,
) -> InputResult<BTreeMap<String, usize>> {
    let mut by_pointer = BTreeMap::new();
    for carrier in carriers {
        validate_json_pointer(&carrier.pointer)?;
        if carrier.story_index >= story_count {
            return Err(InputIssue::invalid(format!(
                "carrier '{}' references missing Storyline index {}",
                carrier.pointer, carrier.story_index
            )));
        }
        if by_pointer
            .insert(carrier.pointer.clone(), carrier.story_index)
            .is_some()
        {
            return Err(InputIssue::invalid(format!(
                "duplicate carrier binding '{}'",
                carrier.pointer
            )));
        }
    }
    Ok(by_pointer)
}

fn merge_unknown_fields(
    target: &mut StorylineUnknownFields,
    incoming: StorylineUnknownFields,
) -> InputResult<()> {
    for (source, incoming_source) in incoming.sources {
        match target.sources.entry(source.clone()) {
            Entry::Vacant(entry) => {
                entry.insert(incoming_source);
            }
            Entry::Occupied(mut entry) => {
                let existing = entry.get_mut();
                if existing.source_document_id != incoming_source.source_document_id {
                    return Err(InputIssue::invalid(format!(
                        "unknown fields source '{source}' cannot change source_document_id"
                    )));
                }
                for (pointer, value) in incoming_source.fields {
                    match existing.fields.entry(pointer.clone()) {
                        Entry::Vacant(entry) => {
                            entry.insert(value);
                        }
                        Entry::Occupied(entry) if entry.get() == &value => {}
                        Entry::Occupied(_) => {
                            return Err(InputIssue::invalid(format!(
                                "unknown fields source '{source}' has conflicting values at '{pointer}'"
                            )))
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

impl StorylineUnknownFields {
    pub fn is_empty(&self) -> bool {
        self.sources.values().all(|source| source.fields.is_empty())
    }

    pub fn insert(
        &mut self,
        source: impl Into<String>,
        source_document_id: impl Into<String>,
        pointer: impl Into<String>,
        value: Value,
    ) -> InputResult<()> {
        let source = source.into();
        let source_document_id = source_document_id.into();
        let pointer = pointer.into();
        validate_json_pointer(&pointer)?;

        match self.sources.get_mut(&source) {
            Some(existing) => {
                if existing.source_document_id != source_document_id {
                    return Err(InputIssue::invalid(format!(
                        "unknown fields source '{source}' cannot change source_document_id"
                    )));
                }
                existing.fields.insert(pointer, value);
            }
            None => {
                self.sources.insert(
                    source,
                    SourceUnknownFields {
                        source_document_id,
                        fields: BTreeMap::from([(pointer, value)]),
                    },
                );
            }
        }
        Ok(())
    }

    pub fn validate_with<F>(
        &self,
        limits: UnknownFieldLimits,
        normalize: F,
    ) -> InputResult<UnknownKeyCounts>
    where
        F: FnMut(&str, &str) -> InputResult<String>,
    {
        validate_unknown_fields_with(self, limits, normalize)
    }
}

pub fn validate_json_pointer(pointer: &str) -> InputResult<()> {
    decode_json_pointer(pointer).map(|_| ())
}

pub fn compute_unknown_key_counts(
    fields: &StorylineUnknownFields,
) -> InputResult<UnknownKeyCounts> {
    compute_unknown_key_counts_with(fields, normalize_unknown_pointer)
}

pub fn validate_unknown_fields(
    fields: &StorylineUnknownFields,
    limits: UnknownFieldLimits,
) -> InputResult<UnknownKeyCounts> {
    validate_unknown_fields_with(fields, limits, normalize_unknown_pointer)
}

pub fn validate_unknown_fields_with<F>(
    fields: &StorylineUnknownFields,
    limits: UnknownFieldLimits,
    normalize: F,
) -> InputResult<UnknownKeyCounts>
where
    F: FnMut(&str, &str) -> InputResult<String>,
{
    limits.validate()?;

    let (field_count, byte_count) = logical_size(fields)?;
    if limits.max_fields != usize::MAX && field_count > limits.max_fields {
        return Err(InputIssue::invalid(format!(
            "unknown field count {field_count} exceeds configured limit {}",
            limits.max_fields
        )));
    }
    if limits.max_bytes != usize::MAX && byte_count > limits.max_bytes {
        return Err(InputIssue::invalid(format!(
            "unknown field byte size {byte_count} exceeds configured limit {}",
            limits.max_bytes
        )));
    }

    compute_unknown_key_counts_with(fields, normalize)
}

fn logical_size(fields: &StorylineUnknownFields) -> InputResult<(usize, usize)> {
    let mut field_count = 0usize;
    let mut byte_count = 0usize;
    for source in fields.sources.values() {
        byte_count = checked_size_add(byte_count, source.source_document_id.len())?;
        for (pointer, value) in &source.fields {
            field_count = field_count
                .checked_add(1)
                .ok_or_else(|| InputIssue::invalid("unknown field count overflow"))?;
            byte_count = checked_size_add(byte_count, pointer.len())?;
            byte_count = checked_size_add(
                byte_count,
                serde_json::to_vec(value)
                    .map_err(|error| InputIssue::invalid(error.to_string()))?
                    .len(),
            )?;
        }
    }
    Ok((field_count, byte_count))
}

fn checked_size_add(total: usize, additional: usize) -> InputResult<usize> {
    total
        .checked_add(additional)
        .ok_or_else(|| InputIssue::invalid("unknown field byte count overflow"))
}

fn compute_unknown_key_counts_with<F>(
    fields: &StorylineUnknownFields,
    mut normalize: F,
) -> InputResult<UnknownKeyCounts>
where
    F: FnMut(&str, &str) -> InputResult<String>,
{
    let mut counts = UnknownKeyCounts::new();
    for (source, source_fields) in &fields.sources {
        for pointer in source_fields.fields.keys() {
            validate_json_pointer(pointer)?;
            let normalized_pointer = normalize(source, pointer)?;
            let source_counts = counts.entry(source.clone()).or_default();
            let count = source_counts.entry(normalized_pointer).or_default();
            *count = count.saturating_add(1);
        }
    }
    Ok(counts)
}

fn normalize_unknown_pointer(source: &str, pointer: &str) -> InputResult<String> {
    match source {
        "atif" => normalize_atif_pointer(source, pointer),
        "actf" => normalize_actf_pointer(source, pointer),
        "openai-msg" => normalize_openai_pointer(source, pointer),
        "agenticmd" => normalize_agenticmd_unknown_pointer(source, pointer),
        "codex" | "claude-code" => normalize_jsonl_event_pointer(pointer),
        _ => {
            validate_json_pointer(pointer)?;
            Ok(pointer.to_owned())
        }
    }
}

pub(crate) fn normalize_atif_pointer(source: &str, pointer: &str) -> InputResult<String> {
    let mut tokens = decode_json_pointer(pointer)?;
    if source != "atif" {
        return Ok(encode_json_pointer(&tokens));
    }

    fn normalize_trajectory(tokens: &mut [String], start: usize) {
        if tokens.get(start).map(String::as_str) == Some("steps") {
            if tokens
                .get(start + 1)
                .is_some_and(|token| token.parse::<usize>().is_ok())
            {
                tokens[start + 1] = "*".into();
                if tokens.get(start + 2).map(String::as_str) == Some("tool_calls")
                    && tokens
                        .get(start + 3)
                        .is_some_and(|token| token.parse::<usize>().is_ok())
                {
                    tokens[start + 3] = "*".into();
                }
            }
        } else if tokens.get(start).map(String::as_str) == Some("subagent_trajectories")
            && tokens
                .get(start + 1)
                .is_some_and(|token| token.parse::<usize>().is_ok())
        {
            tokens[start + 1] = "*".into();
            normalize_trajectory(tokens, start + 2);
        }
    }

    normalize_trajectory(&mut tokens, 0);
    Ok(encode_json_pointer(&tokens))
}

pub(crate) fn normalize_actf_pointer(source: &str, pointer: &str) -> InputResult<String> {
    let mut tokens = decode_json_pointer(pointer)?;
    if source == "actf"
        && tokens.first().map(String::as_str) == Some("attempts")
        && tokens.get(2).map(String::as_str) == Some("trajectory")
        && tokens.get(3).map(String::as_str) == Some("steps")
        && tokens
            .get(4)
            .is_some_and(|token| token.parse::<usize>().is_ok())
    {
        tokens[4] = "*".into();
        if matches!(
            tokens.get(5).map(String::as_str),
            Some("tools" | "observation")
        ) && tokens
            .get(6)
            .is_some_and(|token| token.parse::<usize>().is_ok())
        {
            tokens[6] = "*".into();
        } else if tokens.get(5).map(String::as_str) == Some("assistant_content")
            && tokens.get(6).map(String::as_str) == Some("tool_calls")
            && tokens
                .get(7)
                .is_some_and(|token| token.parse::<usize>().is_ok())
        {
            tokens[7] = "*".into();
        }
    }
    Ok(encode_json_pointer(&tokens))
}

pub(crate) fn normalize_openai_pointer(source: &str, pointer: &str) -> InputResult<String> {
    let mut tokens = decode_json_pointer(pointer)?;
    if source == "openai-msg"
        && tokens.first().map(String::as_str) == Some("session_steps")
        && tokens
            .get(1)
            .is_some_and(|token| token.parse::<usize>().is_ok())
    {
        tokens[1] = "*".into();
        if tokens.get(2).map(String::as_str) == Some("messages")
            && tokens
                .get(3)
                .is_some_and(|token| token.parse::<usize>().is_ok())
        {
            tokens[3] = "*".into();
            if tokens.get(4).map(String::as_str) == Some("tool_calls")
                && tokens
                    .get(5)
                    .is_some_and(|token| token.parse::<usize>().is_ok())
            {
                tokens[5] = "*".into();
            }
        } else if tokens.get(2).map(String::as_str) == Some("response")
            && tokens.get(3).map(String::as_str) == Some("tool_calls")
            && tokens
                .get(4)
                .is_some_and(|token| token.parse::<usize>().is_ok())
        {
            tokens[4] = "*".into();
        }
    }
    Ok(encode_json_pointer(&tokens))
}

/// Collapse JSONL event-row indexes used by Codex and Claude Code adapters.
/// Stored pointers stay exact (`/events/12`); warning and count keys use `*`.
fn normalize_jsonl_event_pointer(pointer: &str) -> InputResult<String> {
    let mut tokens = decode_json_pointer(pointer)?;
    if tokens.first().map(String::as_str) == Some("events")
        && tokens.get(1).is_some_and(|token| is_array_index(token))
    {
        tokens[1] = "*".into();
        if tokens.get(2).map(String::as_str) == Some("content")
            && tokens.get(3).is_some_and(|token| is_array_index(token))
        {
            tokens[3] = "*".into();
        }
    }
    Ok(encode_json_pointer(&tokens))
}

/// Normalize native AgenticMD block positions while retaining all other
/// pointer tokens literally. `blocks` is the only array in the logical
/// unknown-fields document; numeric object keys in frontmatter and header values
/// must remain distinguishable.
pub(crate) fn normalize_agenticmd_unknown_pointer(
    source: &str,
    pointer: &str,
) -> InputResult<String> {
    let mut tokens = decode_json_pointer(pointer)?;
    if source == "agenticmd"
        && tokens.first().map(String::as_str) == Some("blocks")
        && tokens.get(1).is_some_and(|token| is_array_index(token))
    {
        tokens[1] = "*".into();
    }
    Ok(encode_json_pointer(&tokens))
}

fn encode_json_pointer(tokens: &[String]) -> String {
    tokens.iter().fold(String::new(), |mut pointer, token| {
        pointer.push('/');
        pointer.push_str(&token.replace('~', "~0").replace('/', "~1"));
        pointer
    })
}

pub(crate) fn pointer_join(parent: &str, token: &str) -> String {
    format!("{parent}/{}", token.replace('~', "~0").replace('/', "~1"))
}

pub(crate) fn insert_unknown_map(
    story: &mut StorylineDocument,
    source_format: &str,
    source_document_id: &str,
    parent: &str,
    fields: &Map<String, Value>,
) -> InputResult<()> {
    for (key, value) in fields {
        story.unknown_fields.insert(
            source_format,
            source_document_id,
            pointer_join(parent, key),
            value.clone(),
        )?;
    }
    Ok(())
}

fn is_array_index(token: &str) -> bool {
    !token.is_empty()
        && !(token.len() > 1 && token.starts_with('0'))
        && token.parse::<usize>().is_ok()
}

pub(crate) fn decode_json_pointer(pointer: &str) -> InputResult<Vec<String>> {
    if pointer.is_empty() {
        return Ok(Vec::new());
    }
    let Some(pointer) = pointer.strip_prefix('/') else {
        return Err(InputIssue::invalid(
            "JSON Pointer must be empty or start with '/'",
        ));
    };
    pointer.split('/').map(decode_pointer_token).collect()
}

fn decode_pointer_token(token: &str) -> InputResult<String> {
    let mut decoded = String::with_capacity(token.len());
    let mut chars = token.chars();
    while let Some(character) = chars.next() {
        if character != '~' {
            decoded.push(character);
            continue;
        }
        match chars.next() {
            Some('0') => decoded.push('~'),
            Some('1') => decoded.push('/'),
            _ => {
                return Err(InputIssue::invalid(
                    "JSON Pointer contains an invalid '~' escape",
                ))
            }
        }
    }
    Ok(decoded)
}

pub(crate) enum PointerWrite {
    InsertOnly,
    ReplaceSourceOwned,
}

pub(crate) fn restore_json_pointer(
    target: &mut Value,
    pointer: &str,
    value: Value,
    write: PointerWrite,
) -> Result<()> {
    let tokens = decode_json_pointer(pointer)?;
    if tokens.is_empty() {
        return match write {
            PointerWrite::InsertOnly => anyhow::bail!(
                "cannot insert unknown field at existing root JSON Pointer '{pointer}'"
            ),
            PointerWrite::ReplaceSourceOwned => {
                *target = value;
                Ok(())
            }
        };
    }

    let Some((last, parents)) = tokens.split_last() else {
        anyhow::bail!("JSON Pointer unexpectedly decoded to an empty token sequence");
    };
    let mut parent = target;
    for token in parents {
        parent = match parent {
            Value::Object(object) => object.get_mut(token).ok_or_else(|| {
                anyhow::anyhow!("JSON Pointer '{pointer}' has a missing object parent")
            })?,
            Value::Array(array) => {
                let index = array_index(token, pointer)?;
                array.get_mut(index).ok_or_else(|| {
                    anyhow::anyhow!("JSON Pointer '{pointer}' references a missing array slot")
                })?
            }
            _ => anyhow::bail!("JSON Pointer '{pointer}' has a non-container parent"),
        };
    }

    match parent {
        Value::Object(object) => match write {
            PointerWrite::InsertOnly => {
                if object.contains_key(last) {
                    anyhow::bail!("JSON Pointer '{pointer}' collides with a canonical value");
                }
                object.insert(last.clone(), value);
            }
            PointerWrite::ReplaceSourceOwned => {
                object.insert(last.clone(), value);
            }
        },
        Value::Array(array) => {
            let index = array_index(last, pointer)?;
            let slot = array.get_mut(index).ok_or_else(|| {
                anyhow::anyhow!("JSON Pointer '{pointer}' references a missing array slot")
            })?;
            match write {
                PointerWrite::InsertOnly => {
                    anyhow::bail!("JSON Pointer '{pointer}' collides with an existing array value")
                }
                PointerWrite::ReplaceSourceOwned => *slot = value,
            }
        }
        _ => anyhow::bail!("JSON Pointer '{pointer}' has a non-container parent"),
    }
    Ok(())
}

fn array_index(token: &str, pointer: &str) -> Result<usize> {
    if token.is_empty() || (token.len() > 1 && token.starts_with('0')) {
        anyhow::bail!("JSON Pointer '{pointer}' has an invalid array index '{token}'");
    }
    token.parse::<usize>().map_err(|_| {
        anyhow::anyhow!("JSON Pointer '{pointer}' has an invalid array index '{token}'")
    })
}

pub(crate) fn canonical_source_document_id(value: &Value) -> Result<String> {
    let mut value = value.clone();
    if let Value::Object(object) = &mut value {
        object.remove("_storyline");
    }
    let canonical = canonicalize_json_value(value);
    Ok(blake3::hash(&serde_json::to_vec(&canonical)?)
        .to_hex()
        .to_string())
}

fn canonicalize_json_value(value: Value) -> Value {
    match value {
        Value::Array(values) => {
            Value::Array(values.into_iter().map(canonicalize_json_value).collect())
        }
        Value::Object(object) => {
            let sorted = object
                .into_iter()
                .map(|(key, value)| (key, canonicalize_json_value(value)))
                .collect::<BTreeMap<_, _>>();
            Value::Object(sorted.into_iter().collect())
        }
        value => value,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::StorylineDocument;
    use serde_json::json;

    #[test]
    fn pointer_join_appends_rfc6901_escaped_tokens() {
        assert_eq!(pointer_join("", "steps"), "/steps");
        assert_eq!(pointer_join("/steps", "0"), "/steps/0");
        assert_eq!(pointer_join("/x", "a/b"), "/x/a~1b");
        assert_eq!(pointer_join("/x", "tilde~"), "/x/tilde~0");
        assert_eq!(pointer_join("/attempts", "1"), "/attempts/1");
    }

    #[test]
    fn insert_unknown_map_copies_fields_under_source_and_parent_pointer() {
        let mut story = StorylineDocument::new("s", "a");
        let fields = serde_json::Map::from_iter([
            ("vendor".into(), json!({"trace": 7})),
            ("a/b".into(), json!(null)),
        ]);
        insert_unknown_map(&mut story, "atif", "doc-1", "/steps/0", &fields).unwrap();
        let stored = &story.unknown_fields.sources["atif"];
        assert_eq!(stored.source_document_id, "doc-1");
        assert_eq!(stored.fields["/steps/0/vendor"], json!({"trace": 7}));
        assert_eq!(stored.fields["/steps/0/a~1b"], json!(null));
    }

    #[test]
    fn import_warnings_dedupe_by_source_and_normalized_key_across_observations() {
        let mut warnings = UnknownFieldImportWarnings::default();
        warnings.observe(&BTreeMap::from([(
            "actf".into(),
            BTreeMap::from([("/attempts/1/trajectory/steps/*/user_content".into(), 2_u64)]),
        )]));
        warnings.observe(&BTreeMap::from([(
            "actf".into(),
            BTreeMap::from([
                (
                    "/attempts/1/trajectory/steps/*/user_content".into(),
                    2_831_u64 - 2,
                ),
                ("/attempts/1/trajectory/steps/*/system_prompt".into(), 3_u64),
            ]),
        )]));
        warnings.observe(&BTreeMap::from([(
            "atif".into(),
            BTreeMap::from([("/steps/*/vendor".into(), 1_u64)]),
        )]));

        assert_eq!(
            warnings.warning_lines(),
            vec![
                "warning: unknown field source=actf key=/attempts/1/trajectory/steps/*/system_prompt occurrences=3"
                    .to_owned(),
                "warning: unknown field source=actf key=/attempts/1/trajectory/steps/*/user_content occurrences=2831"
                    .to_owned(),
                "warning: unknown field source=atif key=/steps/*/vendor occurrences=1".to_owned(),
            ]
        );
    }

    #[test]
    fn import_warnings_count_shared_source_pointer_once_per_input_source() {
        let mut first = StorylineDocument::new("first", "agent");
        first
            .unknown_fields
            .insert("actf", "shared-document", "/vendor_root", json!(true))
            .unwrap();
        first.refresh_unknown_key_counts().unwrap();
        let mut second = StorylineDocument::new("second", "agent");
        second
            .unknown_fields
            .insert("actf", "shared-document", "/vendor_root", json!(true))
            .unwrap();
        second.refresh_unknown_key_counts().unwrap();

        let mut warnings = UnknownFieldImportWarnings::default();
        warnings.observe_storylines([&first, &second]).unwrap();
        assert_eq!(
            warnings.warning_lines(),
            ["warning: unknown field source=actf key=/vendor_root occurrences=1"]
        );

        // A second call represents a distinct input Source, even if its source
        // document identifier and contents happen to be identical.
        warnings.observe_storylines([&first, &second]).unwrap();
        assert_eq!(
            warnings.warning_lines(),
            ["warning: unknown field source=actf key=/vendor_root occurrences=2"]
        );
    }

    #[test]
    fn import_warnings_collapse_jsonl_event_indexes() {
        let mut story = StorylineDocument::new("sess", "codex");
        story
            .unknown_fields
            .insert(
                "codex",
                "rollout.jsonl",
                "/events/999",
                json!({"type": "world_state"}),
            )
            .unwrap();
        story
            .unknown_fields
            .insert(
                "codex",
                "rollout.jsonl",
                "/events/9989",
                json!({"type": "world_state"}),
            )
            .unwrap();
        story
            .unknown_fields
            .insert(
                "codex",
                "rollout.jsonl",
                "/events/2/content/1",
                json!({"type": "input_image"}),
            )
            .unwrap();
        story.refresh_unknown_key_counts().unwrap();

        let mut warnings = UnknownFieldImportWarnings::default();
        warnings.observe_storylines([&story]).unwrap();
        assert_eq!(
            warnings.warning_lines(),
            [
                "warning: unknown field source=codex key=/events/* occurrences=2",
                "warning: unknown field source=codex key=/events/*/content/* occurrences=1",
            ]
        );
        assert_eq!(story.unknown_key_counts["codex"]["/events/*"], 2);
        assert_eq!(story.unknown_key_counts["codex"]["/events/*/content/*"], 1);
        assert!(story.unknown_fields.sources["codex"]
            .fields
            .contains_key("/events/9989"));
    }

    #[test]
    fn import_warning_atoms_are_json_escaped_onto_one_physical_line() {
        let mut warnings = UnknownFieldImportWarnings::default();
        warnings.observe(&BTreeMap::from([
            (
                "actf\nsource".into(),
                BTreeMap::from([("/line\n\u{1b}[31m".into(), 1_u64)]),
            ),
            ("atif".into(), BTreeMap::from([("".into(), 1_u64)])),
        ]));

        assert_eq!(
            warnings.warning_lines(),
            vec![
                "warning: unknown field source=actf\\nsource key=/line\\n\\u001b[31m occurrences=1"
                    .to_owned(),
                "warning: unknown field source=atif key= occurrences=1".to_owned(),
            ]
        );
        assert!(warnings
            .warning_lines()
            .iter()
            .all(|line| !line.contains(['\n', '\r', '\u{1b}'])));
    }

    #[test]
    fn empty_source_does_not_create_a_key_count_entry() {
        let fields = StorylineUnknownFields {
            sources: BTreeMap::from([(
                "openai-msg".into(),
                SourceUnknownFields {
                    source_document_id: "source.json".into(),
                    fields: BTreeMap::new(),
                },
            )]),
        };

        assert!(compute_unknown_key_counts(&fields).unwrap().is_empty());
    }

    #[test]
    fn envelope_distributes_foreign_sources_by_carrier() {
        let mut raw = json!({
            "attempts": {"1": {}},
            "_storyline": {"unknown_fields": {"version": 1, "by_trajectory": {
                "/attempts/1": {"sources": {
                    "atif": {"source_document_id": "a", "fields": {"/vendor": 7}}
                }}
            }}}
        });
        let envelope = take_unknown_fields_envelope(&mut raw).unwrap();
        assert!(raw.get("_storyline").is_none());
        let mut stories = vec![StorylineDocument::new("s", "a")];
        attach_carried_unknown_fields(
            DocumentFormat::Actf,
            envelope,
            &[CarrierBinding {
                story_index: 0,
                pointer: "/attempts/1".into(),
            }],
            &mut stories,
            UnknownFieldLimits::default(),
        )
        .unwrap();
        assert_eq!(
            stories[0].unknown_fields.sources["atif"].fields["/vendor"],
            7
        );
    }

    #[test]
    fn envelope_rejects_reserved_shape_version_and_extra_keys() {
        let invalid = [
            json!({"_storyline": null}),
            json!({"_storyline": {"unknown_fields": {"by_trajectory": {}}}}),
            json!({"_storyline": {"unknown_fields": {"version": 2, "by_trajectory": {}}}}),
            json!({"_storyline": {"unknown_fields": {
                "version": 1, "by_trajectory": {}, "extra": true
            }}}),
            json!({"_storyline": {
                "unknown_fields": {"version": 1, "by_trajectory": {}},
                "extra": true
            }}),
        ];
        for mut raw in invalid {
            assert!(
                take_unknown_fields_envelope(&mut raw).is_err(),
                "accepted {raw}"
            );
            assert!(raw.get("_storyline").is_some());
        }
    }

    #[test]
    fn envelope_rejects_bad_duplicate_and_unbound_carriers() {
        let mut bad_pointer = json!({"_storyline": {"unknown_fields": {
            "version": 1,
            "by_trajectory": {"bad": {"sources": {}}}
        }}});
        assert!(take_unknown_fields_envelope(&mut bad_pointer).is_err());

        let stories = &mut [StorylineDocument::new("s", "a")];
        assert!(attach_carried_unknown_fields(
            DocumentFormat::OpenaiMsg,
            BTreeMap::new(),
            &[
                CarrierBinding {
                    story_index: 0,
                    pointer: "/same".into()
                },
                CarrierBinding {
                    story_index: 0,
                    pointer: "/same".into()
                },
            ],
            stories,
            UnknownFieldLimits::default(),
        )
        .is_err());

        let unbound = BTreeMap::from([("/missing".into(), StorylineUnknownFields::default())]);
        assert!(attach_carried_unknown_fields(
            DocumentFormat::OpenaiMsg,
            unbound,
            &[CarrierBinding {
                story_index: 0,
                pointer: "/bound".into()
            }],
            stories,
            UnknownFieldLimits::default(),
        )
        .is_err());
    }

    #[test]
    fn envelope_attachment_rejects_source_id_changes_and_total_limit_splitting() {
        let mut story = StorylineDocument::new("s", "a");
        story
            .unknown_fields
            .insert("atif", "first", "/owned", json!(1))
            .unwrap();
        story.refresh_unknown_key_counts().unwrap();

        let changed_id = BTreeMap::from([(
            "".into(),
            StorylineUnknownFields {
                sources: BTreeMap::from([(
                    "atif".into(),
                    SourceUnknownFields {
                        source_document_id: "second".into(),
                        fields: BTreeMap::from([("/foreign".into(), json!(2))]),
                    },
                )]),
            },
        )]);
        assert!(attach_carried_unknown_fields(
            DocumentFormat::OpenaiMsg,
            changed_id,
            &[CarrierBinding {
                story_index: 0,
                pointer: "".into()
            }],
            std::slice::from_mut(&mut story),
            UnknownFieldLimits::default(),
        )
        .is_err());
        assert_eq!(
            story.unknown_fields.sources["atif"].source_document_id,
            "first"
        );

        let carried = BTreeMap::from([(
            "".into(),
            StorylineUnknownFields {
                sources: BTreeMap::from([(
                    "actf".into(),
                    SourceUnknownFields {
                        source_document_id: "doc".into(),
                        fields: BTreeMap::from([("/foreign".into(), json!(2))]),
                    },
                )]),
            },
        )]);
        assert!(attach_carried_unknown_fields(
            DocumentFormat::OpenaiMsg,
            carried,
            &[CarrierBinding {
                story_index: 0,
                pointer: "".into()
            }],
            std::slice::from_mut(&mut story),
            UnknownFieldLimits {
                max_fields: 1,
                max_bytes: 1024
            },
        )
        .is_err());
        assert!(!story.unknown_fields.sources.contains_key("actf"));
    }

    #[test]
    fn envelope_writer_excludes_target_source_and_rejects_reserved_collision() {
        let mut story = StorylineDocument::new("s", "a");
        story
            .unknown_fields
            .insert("actf", "actf-doc", "/owned", json!(1))
            .unwrap();
        story
            .unknown_fields
            .insert("atif", "atif-doc", "/vendor", json!(7))
            .unwrap();
        story.refresh_unknown_key_counts().unwrap();
        let carriers = [CarrierBinding {
            story_index: 0,
            pointer: "/attempts/1".into(),
        }];
        let mut target = json!({"attempts": {"1": {}}});
        write_foreign_unknown_fields_envelope(
            DocumentFormat::Actf,
            &mut target,
            &[story.clone()],
            &carriers,
        )
        .unwrap();
        assert_eq!(target["_storyline"]["unknown_fields"]["version"], 1);
        let sources =
            &target["_storyline"]["unknown_fields"]["by_trajectory"]["/attempts/1"]["sources"];
        assert!(sources.get("actf").is_none());
        assert_eq!(sources["atif"]["fields"]["/vendor"], 7);

        let mut collision = json!({"attempts": {"1": {}}, "_storyline": {}});
        assert!(write_foreign_unknown_fields_envelope(
            DocumentFormat::Actf,
            &mut collision,
            &[story],
            &carriers,
        )
        .is_err());
    }

    // This catches a missing validation/normalization pass that would otherwise
    // admit malformed pointers or return unnormalized key counts.
    fn normalize_test_pointer(source: &str, pointer: &str) -> InputResult<String> {
        assert_eq!(source, "atif");
        Ok(pointer.replacen("/steps/0/", "/steps/*/", 1))
    }

    #[test]
    fn unknown_fields_validate_pointer_counts_and_limits() {
        let mut fields = StorylineUnknownFields::default();
        fields
            .insert(
                "atif",
                "source-1",
                "/steps/0/vendor~1field",
                json!({"kept": true}),
            )
            .unwrap();
        let counts = fields
            .validate_with(UnknownFieldLimits::default(), normalize_test_pointer)
            .unwrap();
        assert_eq!(counts["atif"]["/steps/*/vendor~1field"], 1);

        let too_many = UnknownFieldLimits {
            max_fields: 0,
            max_bytes: 1_048_576,
        };
        assert!(fields
            .validate_with(too_many, normalize_test_pointer)
            .is_err());
        assert!(validate_json_pointer("/bad~2escape").is_err());
    }

    #[test]
    fn default_unknown_field_count_and_bytes_are_unbounded() {
        let mut fields = StorylineUnknownFields::default();
        for index in 0..(4_096 + 1) {
            fields
                .insert("atif", "s", format!("/{index}"), json!(null))
                .unwrap();
        }
        fields
            .insert(
                "actf",
                "large.actf.json",
                "/attempts/1/extra",
                json!("x".repeat(1024 * 1024 + 1)),
            )
            .unwrap();

        validate_unknown_fields(&fields, UnknownFieldLimits::default()).unwrap();
        assert!(UnknownFieldLimits {
            max_fields: usize::MAX,
            max_bytes: usize::MAX,
        }
        .validate()
        .is_ok());
    }

    #[test]
    fn configured_unknown_field_limits_accept_exact_entry_and_byte_boundaries() {
        let field_limits = UnknownFieldLimits {
            max_fields: 4_096,
            max_bytes: usize::MAX,
        };
        let mut fields = StorylineUnknownFields::default();
        for index in 0..field_limits.max_fields {
            fields
                .insert("atif", "s", format!("/{index}"), json!(null))
                .unwrap();
        }
        assert!(validate_unknown_fields(&fields, field_limits).is_ok());
        fields
            .insert("atif", "s", "/too-many", json!(null))
            .unwrap();
        assert!(validate_unknown_fields(&fields, field_limits).is_err());

        let byte_limits = UnknownFieldLimits {
            max_fields: 4_096,
            max_bytes: 64,
        };
        let exact_string_len = byte_limits.max_bytes - 4;
        let exact = json!("x".repeat(exact_string_len));
        let mut bytes_at_limit = StorylineUnknownFields::default();
        bytes_at_limit.insert("atif", "s", "/", exact).unwrap();
        assert!(validate_unknown_fields(&bytes_at_limit, byte_limits).is_ok());

        let over_limit = json!("x".repeat(exact_string_len + 1));
        let mut bytes_over_limit = StorylineUnknownFields::default();
        bytes_over_limit
            .insert("atif", "s", "/", over_limit)
            .unwrap();
        assert!(validate_unknown_fields(&bytes_over_limit, byte_limits).is_err());
    }

    #[test]
    fn insert_rejects_source_document_id_changes() {
        let mut fields = StorylineUnknownFields::default();
        fields.insert("atif", "first", "/one", json!(1)).unwrap();
        assert!(fields.insert("atif", "second", "/two", json!(2)).is_err());
    }

    #[test]
    fn validate_json_pointer_accepts_only_strict_rfc_6901_escapes() {
        for pointer in ["", "/", "/a~0b/~1c", "/0"] {
            assert!(
                validate_json_pointer(pointer).is_ok(),
                "rejected {pointer:?}"
            );
        }
        for pointer in ["not-a-pointer", "/bad~", "/bad~2", "/bad~~"] {
            assert!(
                validate_json_pointer(pointer).is_err(),
                "accepted {pointer:?}"
            );
        }
    }

    #[test]
    fn validation_rejects_malformed_deserialized_pointers_before_normalization() {
        let fields = StorylineUnknownFields {
            sources: BTreeMap::from([(
                "atif".into(),
                SourceUnknownFields {
                    source_document_id: "source".into(),
                    fields: BTreeMap::from([("/bad~2escape".into(), json!(true))]),
                },
            )]),
        };

        assert!(fields
            .validate_with(UnknownFieldLimits::default(), |_, pointer| Ok(
                pointer.into()
            ))
            .is_err());
    }

    #[test]
    fn restore_pointer_rejects_canonical_collision_and_missing_array_slot() {
        let mut target = json!({"steps": [{"message": "canonical"}]});
        let error = restore_json_pointer(
            &mut target,
            "/steps/0/message",
            json!("unknown"),
            PointerWrite::InsertOnly,
        )
        .unwrap_err();
        assert!(error.to_string().contains("/steps/0/message"));

        let error = restore_json_pointer(
            &mut target,
            "/steps/1/vendor",
            json!(true),
            PointerWrite::InsertOnly,
        )
        .unwrap_err();
        assert!(error.to_string().contains("/steps/1/vendor"));
        assert_eq!(target, json!({"steps": [{"message": "canonical"}]}));
    }

    #[test]
    fn canonical_source_document_id_ignores_envelope_and_object_key_order() {
        let left = json!({"b": [ {"z": 1, "a": 2} ], "a": true, "_storyline": {"ignored": true}});
        let right = json!({"a": true, "b": [ {"a": 2, "z": 1} ]});
        assert_eq!(
            canonical_source_document_id(&left).unwrap(),
            canonical_source_document_id(&right).unwrap(),
        );
    }
}
