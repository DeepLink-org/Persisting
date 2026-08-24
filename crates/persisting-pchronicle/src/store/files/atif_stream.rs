use super::projected_steps::{
    canonical_json_text, emit_projected_step_batch, projected_timing_from_metrics, ProjectedStepRow,
};
use super::*;
use crate::formats::common::json_stream::{visit_json_stream, BoundedCountingReader};
use serde::Deserialize;
use std::io::{self, BufRead};

#[derive(Clone, Copy)]
struct ProjectedAtifScanFlags {
    timestamp: bool,
    model_name: bool,
    reasoning_effort_json: bool,
    message_json: bool,
    reasoning_content: bool,
    kind_fields: bool,
    had_observation: bool,
    metrics: bool,
    extra_json: bool,
    llm_call_count: bool,
    is_copied_context: bool,
}

impl ProjectedAtifScanFlags {
    fn new(scan: &FileScanSpec) -> Self {
        Self {
            timestamp: scan.wants("timestamp"),
            model_name: scan.wants("model_name"),
            reasoning_effort_json: scan.wants("reasoning_effort_json"),
            message_json: scan.wants("message_json"),
            reasoning_content: scan.wants("reasoning_content"),
            kind_fields: scan.wants("kind") || scan.wants("effective_kind"),
            had_observation: scan.wants("had_observation"),
            metrics: scan.wants("metrics_json")
                || scan.wants("latency_ms")
                || scan.wants("ttft_ms"),
            extra_json: scan.wants("extra_json"),
            llm_call_count: scan.wants("llm_call_count"),
            is_copied_context: scan.wants("is_copied_context"),
        }
    }

    fn source_only_steps(self) -> bool {
        !self.timestamp
            && !self.model_name
            && !self.reasoning_effort_json
            && !self.message_json
            && !self.reasoning_content
            && !self.kind_fields
            && !self.had_observation
            && !self.metrics
            && !self.extra_json
            && !self.llm_call_count
            && !self.is_copied_context
    }
}

fn raw_json_value_present(raw: &str) -> bool {
    !matches!(raw.trim(), "" | "null" | "[]" | "{}")
}

#[derive(Debug, serde::Deserialize)]
struct SourceOnlyAtifStep {
    step_id: i64,
    source: String,
}

#[derive(Debug, serde::Deserialize)]
struct SourceOnlyAtifTrajectory {
    schema_version: String,
    session_id: Option<String>,
    trajectory_id: Option<String>,
    agent: ProjectedAtifAgent,
    steps: Vec<SourceOnlyAtifStep>,
    #[serde(default)]
    subagent_trajectories: Option<Vec<SourceOnlyAtifTrajectory>>,
}

impl From<SourceOnlyAtifStep> for ProjectedAtifStep {
    fn from(step: SourceOnlyAtifStep) -> Self {
        Self {
            step_id: step.step_id,
            source: step.source,
            timestamp: None,
            model_name: None,
            reasoning_effort_json: None,
            message_json: None,
            reasoning_content: None,
            tool_calls_nonempty: false,
            observation_present: false,
            metrics_json: None,
            extra_json: None,
            llm_call_count: None,
            is_copied_context: None,
        }
    }
}

impl From<SourceOnlyAtifTrajectory> for ProjectedAtifTrajectory {
    fn from(trajectory: SourceOnlyAtifTrajectory) -> Self {
        Self {
            schema_version: trajectory.schema_version,
            session_id: trajectory.session_id,
            trajectory_id: trajectory.trajectory_id,
            agent: trajectory.agent,
            steps: trajectory.steps.into_iter().map(Into::into).collect(),
            subagent_trajectories: trajectory
                .subagent_trajectories
                .unwrap_or_default()
                .into_iter()
                .map(Into::into)
                .collect(),
            skipped_steps: 0,
        }
    }
}

#[derive(Debug, serde::Deserialize)]
struct ProjectedAtifAgent {
    name: String,
    version: String,
}

#[derive(Debug)]
struct ProjectedAtifTrajectory {
    schema_version: String,
    session_id: Option<String>,
    trajectory_id: Option<String>,
    agent: ProjectedAtifAgent,
    steps: Vec<ProjectedAtifStep>,
    subagent_trajectories: Vec<ProjectedAtifTrajectory>,
    skipped_steps: usize,
}

impl ProjectedAtifTrajectory {
    fn effective_session_id<'a>(
        &'a self,
        inherited_session_id: Option<&'a str>,
    ) -> Result<&'a str> {
        self.session_id
            .as_deref()
            .filter(|value| !value.is_empty())
            .or(inherited_session_id)
            .or_else(|| {
                self.trajectory_id
                    .as_deref()
                    .filter(|value| !value.is_empty())
            })
            .context("ATIF trajectory requires session_id or trajectory_id")
    }

    fn step_count(&self) -> usize {
        self.steps.len() + self.skipped_steps
    }
}

#[derive(Debug)]
struct ProjectedAtifStep {
    step_id: i64,
    timestamp: Option<String>,
    source: String,
    model_name: Option<String>,
    reasoning_effort_json: Option<Box<serde_json::value::RawValue>>,
    message_json: Option<Box<serde_json::value::RawValue>>,
    reasoning_content: Option<String>,
    tool_calls_nonempty: bool,
    observation_present: bool,
    metrics_json: Option<Box<serde_json::value::RawValue>>,
    extra_json: Option<Box<serde_json::value::RawValue>>,
    llm_call_count: Option<i64>,
    is_copied_context: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Deserialize)]
#[serde(field_identifier, rename_all = "snake_case")]
enum ProjectedAtifTrajectoryField {
    SchemaVersion,
    SessionId,
    TrajectoryId,
    Agent,
    Steps,
    SubagentTrajectories,
    #[serde(other)]
    Other,
}

impl ProjectedAtifTrajectoryField {
    fn name(self) -> &'static str {
        match self {
            Self::SchemaVersion => "schema_version",
            Self::SessionId => "session_id",
            Self::TrajectoryId => "trajectory_id",
            Self::Agent => "agent",
            Self::Steps => "steps",
            Self::SubagentTrajectories => "subagent_trajectories",
            Self::Other => "<unknown>",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Deserialize)]
#[serde(field_identifier, rename_all = "snake_case")]
enum ProjectedAtifStepField {
    StepId,
    Timestamp,
    Source,
    ModelName,
    ReasoningEffort,
    Message,
    ReasoningContent,
    ToolCalls,
    Observation,
    Metrics,
    Extra,
    LlmCallCount,
    IsCopiedContext,
    #[serde(other)]
    Other,
}

struct ProjectedAtifTrajectorySeed<'a> {
    scan: &'a FileScanSpec,
}

impl<'de> DeserializeSeed<'de> for ProjectedAtifTrajectorySeed<'_> {
    type Value = ProjectedAtifTrajectory;

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        if ProjectedAtifScanFlags::new(self.scan).source_only_steps()
            && self.scan.step_filters.is_empty()
        {
            SourceOnlyAtifTrajectory::deserialize(deserializer).map(Into::into)
        } else {
            deserializer.deserialize_map(ProjectedAtifTrajectoryVisitor { scan: self.scan })
        }
    }
}

struct ProjectedAtifTrajectoryVisitor<'a> {
    scan: &'a FileScanSpec,
}

impl<'de> Visitor<'de> for ProjectedAtifTrajectoryVisitor<'_> {
    type Value = ProjectedAtifTrajectory;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an ATIF trajectory object")
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut seen = HashSet::new();
        let mut schema_version = None;
        let mut session_id = None;
        let mut trajectory_id = None;
        let mut agent = None;
        let mut steps = None;
        let mut subagent_trajectories = Vec::new();
        let mut skipped_steps = 0;

        while let Some(field) = map.next_key::<ProjectedAtifTrajectoryField>()? {
            if field != ProjectedAtifTrajectoryField::Other && !seen.insert(field) {
                return Err(de::Error::duplicate_field(field.name()));
            }
            match field {
                ProjectedAtifTrajectoryField::SchemaVersion => {
                    schema_version = Some(map.next_value::<String>()?);
                }
                ProjectedAtifTrajectoryField::SessionId => {
                    session_id = map.next_value::<Option<String>>()?;
                }
                ProjectedAtifTrajectoryField::TrajectoryId => {
                    trajectory_id = map.next_value::<Option<String>>()?;
                }
                ProjectedAtifTrajectoryField::Agent => {
                    agent = Some(map.next_value::<ProjectedAtifAgent>()?);
                }
                ProjectedAtifTrajectoryField::Steps => {
                    let flags = ProjectedAtifScanFlags::new(self.scan);
                    let known_session = session_id.as_deref().filter(|value| !value.is_empty());
                    if known_session.is_some_and(|value| !self.scan.matches_document(value)) {
                        skipped_steps = map.next_value_seed(CountSequenceSeed)?;
                        steps = Some(Vec::new());
                    } else {
                        steps = Some(map.next_value_seed(ProjectedAtifStepsSeed { flags })?);
                    }
                }
                ProjectedAtifTrajectoryField::SubagentTrajectories => {
                    subagent_trajectories =
                        map.next_value_seed(OptionalProjectedAtifTrajectoriesSeed {
                            scan: self.scan,
                        })?;
                }
                ProjectedAtifTrajectoryField::Other => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }

        Ok(ProjectedAtifTrajectory {
            schema_version: schema_version
                .ok_or_else(|| de::Error::missing_field("schema_version"))?,
            session_id,
            trajectory_id,
            agent: agent.ok_or_else(|| de::Error::missing_field("agent"))?,
            steps: steps.ok_or_else(|| de::Error::missing_field("steps"))?,
            subagent_trajectories,
            skipped_steps,
        })
    }
}

struct OptionalProjectedAtifTrajectoriesSeed<'a> {
    scan: &'a FileScanSpec,
}

impl<'de> DeserializeSeed<'de> for OptionalProjectedAtifTrajectoriesSeed<'_> {
    type Value = Vec<ProjectedAtifTrajectory>;

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer
            .deserialize_option(OptionalProjectedAtifTrajectoriesVisitor { scan: self.scan })
    }
}

struct OptionalProjectedAtifTrajectoriesVisitor<'a> {
    scan: &'a FileScanSpec,
}

impl<'de> Visitor<'de> for OptionalProjectedAtifTrajectoriesVisitor<'_> {
    type Value = Vec<ProjectedAtifTrajectory>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("null or an array of embedded ATIF trajectories")
    }

    fn visit_none<E>(self) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(Vec::new())
    }

    fn visit_unit<E>(self) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(Vec::new())
    }

    fn visit_some<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_seq(ProjectedAtifTrajectoriesVisitor { scan: self.scan })
    }
}

struct ProjectedAtifTrajectoriesVisitor<'a> {
    scan: &'a FileScanSpec,
}

impl<'de> Visitor<'de> for ProjectedAtifTrajectoriesVisitor<'_> {
    type Value = Vec<ProjectedAtifTrajectory>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an array of embedded ATIF trajectories")
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut trajectories = Vec::with_capacity(sequence.size_hint().unwrap_or_default());
        while let Some(trajectory) =
            sequence.next_element_seed(ProjectedAtifTrajectorySeed { scan: self.scan })?
        {
            trajectories.push(trajectory);
        }
        Ok(trajectories)
    }
}

struct ProjectedAtifStepsSeed {
    flags: ProjectedAtifScanFlags,
}

impl<'de> DeserializeSeed<'de> for ProjectedAtifStepsSeed {
    type Value = Vec<ProjectedAtifStep>;

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_seq(ProjectedAtifStepsVisitor { flags: self.flags })
    }
}

struct ProjectedAtifStepsVisitor {
    flags: ProjectedAtifScanFlags,
}

impl<'de> Visitor<'de> for ProjectedAtifStepsVisitor {
    type Value = Vec<ProjectedAtifStep>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an ATIF steps array")
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut steps = Vec::with_capacity(sequence.size_hint().unwrap_or_default().min(8192));
        while let Some(step) =
            sequence.next_element_seed(ProjectedAtifStepSeed { flags: self.flags })?
        {
            steps.push(step);
        }
        Ok(steps)
    }
}

struct ProjectedAtifStepSeed {
    flags: ProjectedAtifScanFlags,
}

impl<'de> DeserializeSeed<'de> for ProjectedAtifStepSeed {
    type Value = ProjectedAtifStep;

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_map(ProjectedAtifStepVisitor { flags: self.flags })
    }
}

struct ProjectedAtifStepVisitor {
    flags: ProjectedAtifScanFlags,
}

impl<'de> Visitor<'de> for ProjectedAtifStepVisitor {
    type Value = ProjectedAtifStep;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an ATIF step object")
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut step_id = None;
        let mut timestamp = None;
        let mut source = None;
        let mut model_name = None;
        let mut reasoning_effort_json = None;
        let mut message_json = None;
        let mut message_seen = false;
        let mut reasoning_content = None;
        let mut tool_calls_nonempty = false;
        let mut observation_present = false;
        let mut metrics_json = None;
        let mut extra_json = None;
        let mut llm_call_count = None;
        let mut is_copied_context = None;

        while let Some(key) = map.next_key::<ProjectedAtifStepField>()? {
            match key {
                ProjectedAtifStepField::StepId => step_id = Some(map.next_value::<i64>()?),
                ProjectedAtifStepField::Timestamp => {
                    if self.flags.timestamp {
                        timestamp = map.next_value::<Option<String>>()?;
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                ProjectedAtifStepField::Source => source = Some(map.next_value::<String>()?),
                ProjectedAtifStepField::ModelName => {
                    if self.flags.model_name {
                        model_name = map.next_value::<Option<String>>()?;
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                ProjectedAtifStepField::ReasoningEffort => {
                    if self.flags.reasoning_effort_json {
                        reasoning_effort_json =
                            map.next_value::<Option<Box<serde_json::value::RawValue>>>()?;
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                ProjectedAtifStepField::Message => {
                    message_seen = true;
                    if self.flags.message_json {
                        message_json = Some(map.next_value::<Box<serde_json::value::RawValue>>()?);
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                ProjectedAtifStepField::ReasoningContent => {
                    if self.flags.reasoning_content {
                        reasoning_content = map.next_value::<Option<String>>()?;
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                ProjectedAtifStepField::ToolCalls => {
                    if self.flags.kind_fields {
                        let raw = map.next_value::<Option<Box<serde_json::value::RawValue>>>()?;
                        tool_calls_nonempty =
                            raw.is_some_and(|value| raw_json_value_present(value.get()));
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                ProjectedAtifStepField::Observation => {
                    if self.flags.had_observation {
                        observation_present = map.next_value::<Option<IgnoredAny>>()?.is_some();
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                ProjectedAtifStepField::Metrics => {
                    if self.flags.metrics {
                        metrics_json =
                            map.next_value::<Option<Box<serde_json::value::RawValue>>>()?;
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                ProjectedAtifStepField::Extra => {
                    if self.flags.extra_json {
                        extra_json =
                            map.next_value::<Option<Box<serde_json::value::RawValue>>>()?;
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                ProjectedAtifStepField::LlmCallCount => {
                    if self.flags.llm_call_count {
                        llm_call_count = map.next_value::<Option<i64>>()?;
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                ProjectedAtifStepField::IsCopiedContext => {
                    if self.flags.is_copied_context {
                        is_copied_context = map.next_value::<Option<bool>>()?;
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                ProjectedAtifStepField::Other => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        if !message_seen {
            return Err(de::Error::missing_field("message"));
        }
        Ok(ProjectedAtifStep {
            step_id: step_id.ok_or_else(|| de::Error::missing_field("step_id"))?,
            timestamp,
            source: source.ok_or_else(|| de::Error::missing_field("source"))?,
            model_name,
            reasoning_effort_json,
            message_json,
            reasoning_content,
            tool_calls_nonempty,
            observation_present,
            metrics_json,
            extra_json,
            llm_call_count,
            is_copied_context,
        })
    }
}

struct CountSequenceSeed;

impl<'de> DeserializeSeed<'de> for CountSequenceSeed {
    type Value = usize;

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_seq(CountSequenceVisitor)
    }
}

struct CountSequenceVisitor;

impl<'de> Visitor<'de> for CountSequenceVisitor {
    type Value = usize;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON array")
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut count = 0;
        while sequence
            .next_element::<Box<serde_json::value::RawValue>>()?
            .is_some()
        {
            count += 1;
        }
        Ok(count)
    }
}

const PROJECTED_QUERY_CANCELLED: &str = "pChronicle projected query receiver closed";

struct ProjectedAtifStream<'a> {
    file: &'a Arc<FileState>,
    runtime: &'a Arc<FileTrajectoryRuntime>,
    schema: &'a SchemaRef,
    batch_size: usize,
    scan: &'a FileScanSpec,
    tx: &'a Sender<datafusion::common::Result<RecordBatch>>,
    pending: Vec<ProjectedStepRow>,
    document_ids: HashSet<String>,
    cancelled: bool,
}

impl<'a> ProjectedAtifStream<'a> {
    fn new(
        file: &'a Arc<FileState>,
        runtime: &'a Arc<FileTrajectoryRuntime>,
        schema: &'a SchemaRef,
        batch_size: usize,
        scan: &'a FileScanSpec,
        tx: &'a Sender<datafusion::common::Result<RecordBatch>>,
    ) -> Self {
        Self {
            file,
            runtime,
            schema,
            batch_size,
            scan,
            tx,
            pending: Vec::with_capacity(batch_size),
            document_ids: HashSet::new(),
            cancelled: false,
        }
    }

    fn consume(&mut self, trajectory: ProjectedAtifTrajectory) -> Result<()> {
        self.runtime
            .metrics
            .inner
            .streamed_records
            .fetch_add(1, Ordering::Relaxed);
        if !project_atif_trajectory(
            trajectory,
            self.file,
            self.runtime,
            self.schema,
            self.batch_size,
            self.scan,
            self.tx,
            &mut self.pending,
            &mut self.document_ids,
            None,
            false,
        )? {
            self.cancelled = true;
            anyhow::bail!(PROJECTED_QUERY_CANCELLED);
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<()> {
        if !self.cancelled {
            let _ = emit_projected_step_batch(
                &mut self.pending,
                self.file,
                self.runtime,
                self.schema,
                self.tx,
            )?;
        }
        Ok(())
    }
}

fn consume_projected_atif_reader<R: BufRead>(
    reader: &mut R,
    scan: &FileScanSpec,
    stream: &mut ProjectedAtifStream<'_>,
) -> Result<()> {
    let mut deserializer = serde_json::Deserializer::from_reader(reader);
    let trajectory = ProjectedAtifTrajectorySeed { scan }
        .deserialize(&mut deserializer)
        .map_err(anyhow::Error::from)?;
    deserializer.end().map_err(anyhow::Error::from)?;
    stream.consume(trajectory)
}

fn deserialize_projected_atif_from_slice(
    record: &[u8],
    scan: &FileScanSpec,
) -> Result<ProjectedAtifTrajectory> {
    let mut deserializer = serde_json::Deserializer::from_slice(record);
    let trajectory = ProjectedAtifTrajectorySeed { scan }
        .deserialize(&mut deserializer)
        .map_err(anyhow::Error::from)?;
    deserializer.end().map_err(anyhow::Error::from)?;
    Ok(trajectory)
}

pub(super) fn stream_projected_atif_steps(
    file: &Arc<FileState>,
    runtime: &Arc<FileTrajectoryRuntime>,
    schema: &SchemaRef,
    batch_size: usize,
    scan: &FileScanSpec,
    tx: &Sender<datafusion::common::Result<RecordBatch>>,
) -> Result<()> {
    let _permit = runtime.limiter.acquire()?;
    file.file.validate_unchanged()?;
    anyhow::ensure!(
        file.file.size_bytes() <= runtime.options.max_file_bytes,
        "ATIF input {} is {} bytes, exceeding max_file_bytes {}",
        file.file.path().display(),
        file.file.size_bytes(),
        runtime.options.max_file_bytes
    );
    let input = File::open(file.file.path())
        .with_context(|| format!("open ATIF input {}", file.file.path().display()))?;
    let mut reader = BufReader::with_capacity(
        64 * 1024,
        BoundedCountingReader::new(input, runtime.options.max_file_bytes),
    );
    runtime
        .metrics
        .inner
        .streaming_buffer_peak_bytes
        .fetch_max(reader.capacity() as u64, Ordering::Relaxed);
    let mut stream = ProjectedAtifStream::new(file, runtime, schema, batch_size, scan, tx);

    let reader_capacity = reader.capacity() as u64;
    let max_record_bytes = runtime.options.max_record_bytes;
    let result = visit_json_stream(
        file.file.path(),
        &mut reader,
        max_record_bytes,
        &mut stream,
        // A single object spans the whole file and is already bounded by
        // `max_file_bytes`, so deserialize it without an intermediate copy.
        |reader, stream| {
            consume_projected_atif_reader(reader, scan, stream)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
        },
        |record, location, stream| {
            runtime.metrics.inner.streaming_buffer_peak_bytes.fetch_max(
                reader_capacity.saturating_add(record.len() as u64),
                Ordering::Relaxed,
            );
            let trajectory = deserialize_projected_atif_from_slice(record, scan)
                .with_context(|| {
                    format!(
                        "parse projected ATIF {location} in {}",
                        file.file.path().display()
                    )
                })
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            stream
                .consume(trajectory)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            Ok(())
        },
    )
    .map(|_| ())
    .map_err(anyhow::Error::from)
    .or_else(|error| {
        if stream.cancelled {
            Ok(())
        } else if error.to_string().contains("JSON array contains no objects") {
            Err(anyhow::anyhow!("ATIF input contains no trajectories"))
        } else {
            Err(error)
        }
    });
    if let Err(error) = result {
        if !stream.cancelled {
            return Err(error).with_context(|| {
                format!("parse projected ATIF input {}", file.file.path().display())
            });
        }
    }
    stream.finish()?;
    let bytes_read = reader.get_ref().bytes_read();
    runtime
        .metrics
        .inner
        .source_bytes_read
        .fetch_add(bytes_read, Ordering::Relaxed);
    runtime
        .metrics
        .inner
        .files_parsed
        .fetch_add(1, Ordering::Relaxed);
    runtime
        .metrics
        .inner
        .projected_files
        .fetch_add(1, Ordering::Relaxed);
    if !stream.cancelled {
        file.file.validate_unchanged()?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn project_atif_trajectory(
    trajectory: ProjectedAtifTrajectory,
    file: &Arc<FileState>,
    runtime: &Arc<FileTrajectoryRuntime>,
    schema: &SchemaRef,
    batch_size: usize,
    scan: &FileScanSpec,
    tx: &Sender<datafusion::common::Result<RecordBatch>>,
    pending: &mut Vec<ProjectedStepRow>,
    document_ids: &mut HashSet<String>,
    inherited_session_id: Option<&str>,
    embedded: bool,
) -> Result<bool> {
    let session_id = trajectory
        .effective_session_id(inherited_session_id)?
        .to_string();
    let document_id = trajectory
        .trajectory_id
        .as_deref()
        .unwrap_or(&session_id)
        .to_string();
    anyhow::ensure!(
        !embedded
            || trajectory
                .trajectory_id
                .as_deref()
                .is_some_and(|id| !id.is_empty()),
        "embedded ATIF trajectory requires trajectory_id"
    );
    anyhow::ensure!(
        !trajectory.agent.name.is_empty(),
        "ATIF agent.name is required"
    );
    anyhow::ensure!(
        !trajectory.agent.version.is_empty(),
        "ATIF agent.version is required"
    );
    let _ = &trajectory.schema_version;
    anyhow::ensure!(
        document_ids.insert(document_id.clone()),
        "duplicate ATIF document_id '{}' in {}",
        document_id,
        file.file.path().display()
    );
    runtime
        .metrics
        .inner
        .documents_scanned
        .fetch_add(1, Ordering::Relaxed);
    runtime
        .metrics
        .inner
        .rows_scanned
        .fetch_add(trajectory.step_count() as u64, Ordering::Relaxed);
    if !scan.matches_document(&session_id) {
        runtime
            .metrics
            .inner
            .documents_pruned
            .fetch_add(1, Ordering::Relaxed);
        runtime
            .metrics
            .inner
            .rows_pruned
            .fetch_add(trajectory.step_count() as u64, Ordering::Relaxed);
    } else {
        let mut rows = Vec::with_capacity(trajectory.steps.len());
        let mut step_ids = HashSet::with_capacity(trajectory.steps.len());
        for step in trajectory.steps {
            anyhow::ensure!(step.step_id >= 1, "ATIF step_id must start from 1");
            anyhow::ensure!(
                step_ids.insert(step.step_id),
                "duplicate ATIF step_id {} in document {}",
                step.step_id,
                document_id
            );
            if !scan.matches_step(step.step_id, &step.source) {
                runtime
                    .metrics
                    .inner
                    .rows_pruned
                    .fetch_add(1, Ordering::Relaxed);
                continue;
            }
            rows.push(project_atif_step(&document_id, &session_id, step, scan));
        }
        rows.sort_by_key(|row| row.step_id);
        runtime
            .metrics
            .inner
            .rows_emitted
            .fetch_add(rows.len() as u64, Ordering::Relaxed);
        for row in rows {
            pending.push(row);
            if pending.len() == batch_size
                && !emit_projected_step_batch(pending, file, runtime, schema, tx)?
            {
                return Ok(false);
            }
        }
    }

    for child in trajectory.subagent_trajectories {
        if !project_atif_trajectory(
            child,
            file,
            runtime,
            schema,
            batch_size,
            scan,
            tx,
            pending,
            document_ids,
            Some(&session_id),
            true,
        )? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn project_atif_step(
    document_id: &str,
    session_id: &str,
    step: ProjectedAtifStep,
    scan: &FileScanSpec,
) -> ProjectedStepRow {
    let wants_kind = scan.wants("kind");
    let wants_effective_kind = scan.wants("effective_kind");
    let effective_kind = match step.source.as_str() {
        "user" => "dialogue",
        "system" => "internal",
        "agent" if step.tool_calls_nonempty => "autonomous",
        _ => "dialogue",
    };
    let kind = if matches!(
        (step.source.as_str(), effective_kind),
        ("user", "dialogue") | ("system", "internal") | ("agent", "dialogue")
    ) {
        None
    } else {
        Some(effective_kind.to_string())
    };

    let (latency_ms, ttft_ms) =
        projected_timing_from_metrics(step.metrics_json.as_ref().map(|value| value.get()));
    ProjectedStepRow {
        document_id: if scan.wants("document_id") {
            document_id.to_string()
        } else {
            String::new()
        },
        run_id: None,
        session_id: if scan.wants("session_id") {
            session_id.to_string()
        } else {
            String::new()
        },
        step_id: step.step_id,
        kind: wants_kind.then_some(kind).flatten(),
        effective_kind: if wants_effective_kind {
            effective_kind.to_string()
        } else {
            String::new()
        },
        timestamp: scan.wants("timestamp").then_some(step.timestamp).flatten(),
        source: if scan.wants("source") {
            step.source
        } else {
            String::new()
        },
        message_json: if scan.wants("message_json") {
            step.message_json
                .as_deref()
                .map(canonical_json_text)
                .unwrap_or_else(|| "null".to_string())
        } else {
            "null".to_string()
        },
        reasoning_content: scan
            .wants("reasoning_content")
            .then_some(step.reasoning_content)
            .flatten(),
        reasoning_effort_json: scan
            .wants("reasoning_effort_json")
            .then_some(
                step.reasoning_effort_json
                    .as_deref()
                    .map(canonical_json_text),
            )
            .flatten(),
        metrics_json: scan
            .wants("metrics_json")
            .then_some(step.metrics_json.as_deref().map(canonical_json_text))
            .flatten(),
        model_name: scan
            .wants("model_name")
            .then_some(step.model_name)
            .flatten(),
        llm_call_count: scan
            .wants("llm_call_count")
            .then_some(step.llm_call_count)
            .flatten(),
        is_copied_context: scan
            .wants("is_copied_context")
            .then_some(step.is_copied_context)
            .flatten(),
        latency_ms: scan.wants("latency_ms").then_some(latency_ms).flatten(),
        ttft_ms: scan.wants("ttft_ms").then_some(ttft_ms).flatten(),
        had_observation: scan.wants("had_observation") && step.observation_present,
        extra_json: scan
            .wants("extra_json")
            .then_some(step.extra_json.as_deref().map(canonical_json_text))
            .flatten(),
    }
}
