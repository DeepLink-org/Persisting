use super::json_stream::{visit_json_stream, BoundedCountingReader};
use super::projected_steps::{
    canonical_json_text, emit_projected_step_batch, projected_timing_from_actf_metrics,
    ProjectedStepRow,
};
use super::*;
use std::fmt;
use std::io::{self, BufRead};

#[derive(Debug)]
struct ProjectedActfDocument {
    task_id: String,
    attempts: Vec<(String, ProjectedActfAttempt)>,
}

impl ProjectedActfDocument {
    fn attempt_count(&self) -> usize {
        self.attempts.len()
    }
}

#[derive(Debug)]
struct ProjectedActfAttempt {
    steps: Vec<ProjectedActfStep>,
}

impl ProjectedActfAttempt {
    fn step_count(&self) -> usize {
        self.steps.len()
    }
}

#[derive(Debug)]
struct ProjectedActfStep {
    step_id: i64,
    started_at: String,
    content: String,
    reasoning_content: Option<String>,
    tools_nonempty: bool,
    observation_present: bool,
    metrics_json: Option<Box<serde_json::value::RawValue>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Deserialize)]
#[serde(field_identifier, rename_all = "snake_case")]
enum ProjectedActfDocumentField {
    TaskId,
    Attempts,
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Deserialize)]
#[serde(field_identifier, rename_all = "snake_case")]
enum ProjectedActfAttemptField {
    Trajectory,
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Deserialize)]
#[serde(field_identifier, rename_all = "snake_case")]
enum ProjectedActfTrajectoryField {
    Steps,
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Deserialize)]
#[serde(field_identifier, rename_all = "snake_case")]
enum ProjectedActfStepField {
    StepId,
    AssistantContent,
    Metric,
    Tools,
    Observation,
    StartedAt,
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Deserialize)]
#[serde(field_identifier, rename_all = "snake_case")]
enum ProjectedActfAssistantContentField {
    Content,
    ReasoningContent,
    #[serde(other)]
    Other,
}

struct ProjectedActfDocumentSeed<'a> {
    scan: &'a FileScanSpec,
}

impl<'de> DeserializeSeed<'de> for ProjectedActfDocumentSeed<'_> {
    type Value = ProjectedActfDocument;

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_map(ProjectedActfDocumentVisitor { scan: self.scan })
    }
}

struct ProjectedActfDocumentVisitor<'a> {
    scan: &'a FileScanSpec,
}

impl<'de> Visitor<'de> for ProjectedActfDocumentVisitor<'_> {
    type Value = ProjectedActfDocument;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an ACTF document object")
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut task_id = None;
        let mut attempts = None;
        while let Some(key) = map.next_key::<ProjectedActfDocumentField>()? {
            match key {
                ProjectedActfDocumentField::TaskId => {
                    task_id = Some(map.next_value::<String>()?);
                }
                ProjectedActfDocumentField::Attempts => {
                    attempts =
                        Some(map.next_value_seed(ProjectedActfAttemptsSeed { scan: self.scan })?);
                }
                ProjectedActfDocumentField::Other => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(ProjectedActfDocument {
            task_id: task_id.ok_or_else(|| de::Error::missing_field("task_id"))?,
            attempts: attempts.ok_or_else(|| de::Error::missing_field("attempts"))?,
        })
    }
}

struct ProjectedActfAttemptsSeed<'a> {
    scan: &'a FileScanSpec,
}

impl<'de> DeserializeSeed<'de> for ProjectedActfAttemptsSeed<'_> {
    type Value = Vec<(String, ProjectedActfAttempt)>;

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_map(ProjectedActfAttemptsVisitor { scan: self.scan })
    }
}

struct ProjectedActfAttemptsVisitor<'a> {
    scan: &'a FileScanSpec,
}

impl<'de> Visitor<'de> for ProjectedActfAttemptsVisitor<'_> {
    type Value = Vec<(String, ProjectedActfAttempt)>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an ACTF attempts map")
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut attempts = Vec::new();
        while let Some(attempt_id) = map.next_key::<String>()? {
            let attempt = map.next_value_seed(ProjectedActfAttemptSeed { scan: self.scan })?;
            attempts.push((attempt_id, attempt));
        }
        if attempts.is_empty() {
            return Err(de::Error::custom("ACTF attempts must not be empty"));
        }
        Ok(attempts)
    }
}

struct ProjectedActfAttemptSeed<'a> {
    scan: &'a FileScanSpec,
}

impl<'de> DeserializeSeed<'de> for ProjectedActfAttemptSeed<'_> {
    type Value = ProjectedActfAttempt;

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_map(ProjectedActfAttemptVisitor { scan: self.scan })
    }
}

struct ProjectedActfAttemptVisitor<'a> {
    scan: &'a FileScanSpec,
}

impl<'de> Visitor<'de> for ProjectedActfAttemptVisitor<'_> {
    type Value = ProjectedActfAttempt;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an ACTF attempt object")
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut trajectory = None;
        while let Some(key) = map.next_key::<ProjectedActfAttemptField>()? {
            match key {
                ProjectedActfAttemptField::Trajectory => {
                    trajectory =
                        Some(map.next_value_seed(ProjectedActfTrajectorySeed { scan: self.scan })?);
                }
                ProjectedActfAttemptField::Other => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        trajectory.ok_or_else(|| de::Error::missing_field("trajectory"))
    }
}

struct ProjectedActfTrajectorySeed<'a> {
    scan: &'a FileScanSpec,
}

pub(super) const ACTF_TRAJECTORY_NOT_PROJECTABLE: &str =
    "ACTF trajectory is an event log; use full decode";

impl<'de> DeserializeSeed<'de> for ProjectedActfTrajectorySeed<'_> {
    type Value = ProjectedActfAttempt;

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(ProjectedActfTrajectoryVisitor { scan: self.scan })
    }
}

struct ProjectedActfTrajectoryVisitor<'a> {
    scan: &'a FileScanSpec,
}

impl<'de> Visitor<'de> for ProjectedActfTrajectoryVisitor<'_> {
    type Value = ProjectedActfAttempt;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an ACTF trajectory object or event-log array")
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence.next_element::<IgnoredAny>()?.is_some() {}
        Err(de::Error::custom(ACTF_TRAJECTORY_NOT_PROJECTABLE))
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut steps = None;
        while let Some(key) = map.next_key::<ProjectedActfTrajectoryField>()? {
            match key {
                ProjectedActfTrajectoryField::Steps => {
                    steps = Some(map.next_value_seed(ProjectedActfStepsSeed { scan: self.scan })?);
                }
                ProjectedActfTrajectoryField::Other => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        steps.ok_or_else(|| de::Error::missing_field("steps"))
    }
}

struct ProjectedActfStepsSeed<'a> {
    scan: &'a FileScanSpec,
}

impl<'de> DeserializeSeed<'de> for ProjectedActfStepsSeed<'_> {
    type Value = ProjectedActfAttempt;

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_seq(ProjectedActfStepsVisitor { scan: self.scan })
    }
}

struct ProjectedActfStepsVisitor<'a> {
    scan: &'a FileScanSpec,
}

impl<'de> Visitor<'de> for ProjectedActfStepsVisitor<'_> {
    type Value = ProjectedActfAttempt;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an ACTF steps array")
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut steps = Vec::new();
        while let Some(step) =
            sequence.next_element_seed(ProjectedActfStepSeed { scan: self.scan })?
        {
            steps.push(step);
        }
        if steps.is_empty() {
            return Err(de::Error::custom("ACTF trajectory steps must not be empty"));
        }
        Ok(ProjectedActfAttempt { steps })
    }
}

struct ProjectedActfStepSeed<'a> {
    scan: &'a FileScanSpec,
}

impl<'de> DeserializeSeed<'de> for ProjectedActfStepSeed<'_> {
    type Value = ProjectedActfStep;

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_map(ProjectedActfStepVisitor { scan: self.scan })
    }
}

struct ProjectedActfStepVisitor<'a> {
    scan: &'a FileScanSpec,
}

impl<'de> Visitor<'de> for ProjectedActfStepVisitor<'_> {
    type Value = ProjectedActfStep;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an ACTF step object")
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut step_id = None;
        let mut started_at = None;
        let mut content = String::new();
        let mut reasoning_content = None;
        let mut tools_nonempty = false;
        let mut observation_present = false;
        let mut metrics_json = None;

        while let Some(key) = map.next_key::<ProjectedActfStepField>()? {
            match key {
                ProjectedActfStepField::StepId => {
                    step_id = Some(map.next_value::<i64>()?);
                }
                ProjectedActfStepField::StartedAt => {
                    started_at = Some(map.next_value::<String>()?);
                }
                ProjectedActfStepField::AssistantContent => {
                    let assistant =
                        map.next_value_seed(ProjectedActfAssistantContentSeed { scan: self.scan })?;
                    content = assistant.content;
                    reasoning_content = assistant.reasoning_content;
                }
                ProjectedActfStepField::Metric => {
                    if self.scan.wants("metrics_json") || self.scan.wants("latency_ms") {
                        metrics_json =
                            map.next_value::<Option<Box<serde_json::value::RawValue>>>()?;
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                ProjectedActfStepField::Tools => {
                    if self.scan.wants("kind") || self.scan.wants("effective_kind") {
                        tools_nonempty = map
                            .next_value::<Option<Vec<IgnoredAny>>>()?
                            .is_some_and(|calls| !calls.is_empty());
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                ProjectedActfStepField::Observation => {
                    if self.scan.wants("had_observation") {
                        observation_present = map
                            .next_value::<Option<Vec<IgnoredAny>>>()?
                            .is_some_and(|observations| !observations.is_empty());
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                ProjectedActfStepField::Other => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }

        Ok(ProjectedActfStep {
            step_id: step_id.ok_or_else(|| de::Error::missing_field("step_id"))?,
            started_at: started_at.ok_or_else(|| de::Error::missing_field("started_at"))?,
            content,
            reasoning_content,
            tools_nonempty,
            observation_present,
            metrics_json,
        })
    }
}

struct ProjectedActfAssistantContent {
    content: String,
    reasoning_content: Option<String>,
}

struct ProjectedActfAssistantContentSeed<'a> {
    scan: &'a FileScanSpec,
}

impl<'de> DeserializeSeed<'de> for ProjectedActfAssistantContentSeed<'_> {
    type Value = ProjectedActfAssistantContent;

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_map(ProjectedActfAssistantContentVisitor { scan: self.scan })
    }
}

struct ProjectedActfAssistantContentVisitor<'a> {
    scan: &'a FileScanSpec,
}

impl<'de> Visitor<'de> for ProjectedActfAssistantContentVisitor<'_> {
    type Value = ProjectedActfAssistantContent;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ACTF assistant_content object")
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut content = String::new();
        let mut reasoning_content = None;
        while let Some(key) = map.next_key::<ProjectedActfAssistantContentField>()? {
            match key {
                ProjectedActfAssistantContentField::Content => {
                    if self.scan.wants("message_json") {
                        content = map.next_value::<Option<String>>()?.unwrap_or_default();
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                ProjectedActfAssistantContentField::ReasoningContent => {
                    if self.scan.wants("reasoning_content") {
                        if let Some(value) = map
                            .next_value::<Option<String>>()?
                            .filter(|value| !value.is_empty())
                        {
                            reasoning_content = Some(value);
                        }
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                ProjectedActfAssistantContentField::Other => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(ProjectedActfAssistantContent {
            content,
            reasoning_content,
        })
    }
}

const PROJECTED_QUERY_CANCELLED: &str = "pChronicle projected query receiver closed";

struct ProjectedActfStream<'a> {
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

impl<'a> ProjectedActfStream<'a> {
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

    fn consume(&mut self, document: ProjectedActfDocument) -> Result<()> {
        self.runtime
            .metrics
            .inner
            .streamed_records
            .fetch_add(1, Ordering::Relaxed);
        if !project_actf_document(
            document,
            self.file,
            self.runtime,
            self.schema,
            self.batch_size,
            self.scan,
            self.tx,
            &mut self.pending,
            &mut self.document_ids,
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

fn deserialize_projected_actf_from_slice(
    record: &[u8],
    scan: &FileScanSpec,
) -> Result<ProjectedActfDocument> {
    let mut deserializer = serde_json::Deserializer::from_slice(record);
    let document = ProjectedActfDocumentSeed { scan }
        .deserialize(&mut deserializer)
        .map_err(anyhow::Error::from)?;
    deserializer.end().map_err(anyhow::Error::from)?;
    Ok(document)
}

fn consume_projected_actf_reader<R: BufRead>(
    reader: &mut R,
    scan: &FileScanSpec,
    stream: &mut ProjectedActfStream<'_>,
) -> Result<()> {
    let mut deserializer = serde_json::Deserializer::from_reader(reader);
    let document = ProjectedActfDocumentSeed { scan }
        .deserialize(&mut deserializer)
        .map_err(anyhow::Error::from)?;
    deserializer.end().map_err(anyhow::Error::from)?;
    stream.consume(document)
}

pub(super) fn stream_projected_actf_steps(
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
        "ACTF input {} is {} bytes, exceeding max_file_bytes {}",
        file.file.path().display(),
        file.file.size_bytes(),
        runtime.options.max_file_bytes
    );
    let input = File::open(file.file.path())
        .with_context(|| format!("open ACTF input {}", file.file.path().display()))?;
    let mut reader = BufReader::with_capacity(
        64 * 1024,
        BoundedCountingReader::new(input, runtime.options.max_file_bytes),
    );
    runtime
        .metrics
        .inner
        .streaming_buffer_peak_bytes
        .fetch_max(reader.capacity() as u64, Ordering::Relaxed);
    let mut stream = ProjectedActfStream::new(file, runtime, schema, batch_size, scan, tx);

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
            consume_projected_actf_reader(reader, scan, stream)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
        },
        |record, location, stream| {
            runtime.metrics.inner.streaming_buffer_peak_bytes.fetch_max(
                reader_capacity.saturating_add(record.len() as u64),
                Ordering::Relaxed,
            );
            let document = deserialize_projected_actf_from_slice(record, scan)
                .with_context(|| {
                    format!(
                        "parse projected ACTF {location} in {}",
                        file.file.path().display()
                    )
                })
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            stream
                .consume(document)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
        },
    )
    .map(|_| ())
    .map_err(anyhow::Error::from)
    .or_else(|error| {
        if stream.cancelled {
            Ok(())
        } else if error.to_string().contains("JSON array contains no objects") {
            Err(anyhow::anyhow!("ACTF input contains no trajectories"))
        } else {
            Err(error)
        }
    });
    if let Err(error) = result {
        if !stream.cancelled {
            return Err(error).with_context(|| {
                format!("parse projected ACTF input {}", file.file.path().display())
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
fn project_actf_document(
    document: ProjectedActfDocument,
    file: &Arc<FileState>,
    runtime: &Arc<FileTrajectoryRuntime>,
    schema: &SchemaRef,
    batch_size: usize,
    scan: &FileScanSpec,
    tx: &Sender<datafusion::common::Result<RecordBatch>>,
    pending: &mut Vec<ProjectedStepRow>,
    document_ids: &mut HashSet<String>,
) -> Result<bool> {
    anyhow::ensure!(
        !document.task_id.trim().is_empty(),
        "ACTF task_id is required"
    );
    let multiple_attempts = document.attempt_count() > 1;
    for (attempt_id, attempt) in document.attempts {
        let session_id = if multiple_attempts {
            format!("{}#attempt-{attempt_id}", document.task_id)
        } else {
            document.task_id.clone()
        };
        let document_id = session_id.clone();
        anyhow::ensure!(
            document_ids.insert(document_id.clone()),
            "duplicate ACTF document_id '{}' in {}",
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
            .fetch_add(attempt.step_count() as u64, Ordering::Relaxed);
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
                .fetch_add(attempt.step_count() as u64, Ordering::Relaxed);
            continue;
        }

        let mut rows = Vec::with_capacity(attempt.steps.len());
        let mut step_ids = HashSet::with_capacity(attempt.steps.len());
        for step in attempt.steps {
            anyhow::ensure!(step.step_id >= 1, "ACTF step_id must start from 1");
            anyhow::ensure!(
                step_ids.insert(step.step_id),
                "duplicate ACTF step_id {} in document {}",
                step.step_id,
                document_id
            );
            if !scan.matches_step(step.step_id, "agent") {
                runtime
                    .metrics
                    .inner
                    .rows_pruned
                    .fetch_add(1, Ordering::Relaxed);
                continue;
            }
            rows.push(project_actf_step(
                &document_id,
                &session_id,
                &document.task_id,
                step,
                scan,
            ));
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
    Ok(true)
}

fn project_actf_step(
    document_id: &str,
    session_id: &str,
    task_id: &str,
    step: ProjectedActfStep,
    scan: &FileScanSpec,
) -> ProjectedStepRow {
    let effective_kind = if step.tools_nonempty {
        "autonomous"
    } else {
        "dialogue"
    };
    let kind = step.tools_nonempty.then(|| "autonomous".to_string());
    let latency_ms =
        projected_timing_from_actf_metrics(step.metrics_json.as_ref().map(|value| value.get()));
    let message_json = if scan.wants("message_json") {
        serde_json::to_string(&step.content).unwrap_or_else(|_| "\"\"".to_string())
    } else {
        "null".to_string()
    };

    ProjectedStepRow {
        document_id: if scan.wants("document_id") {
            document_id.to_string()
        } else {
            String::new()
        },
        run_id: scan.wants("run_id").then(|| task_id.to_string()),
        session_id: if scan.wants("session_id") {
            session_id.to_string()
        } else {
            String::new()
        },
        step_id: step.step_id,
        kind: scan.wants("kind").then_some(kind).flatten(),
        effective_kind: if scan.wants("effective_kind") {
            effective_kind.to_string()
        } else {
            String::new()
        },
        timestamp: scan.wants("timestamp").then_some(step.started_at),
        source: if scan.wants("source") {
            "agent".to_string()
        } else {
            String::new()
        },
        message_json,
        reasoning_content: scan
            .wants("reasoning_content")
            .then_some(step.reasoning_content)
            .flatten(),
        reasoning_effort_json: None,
        metrics_json: scan
            .wants("metrics_json")
            .then_some(step.metrics_json.as_deref().map(canonical_json_text))
            .flatten(),
        model_name: None,
        llm_call_count: scan.wants("llm_call_count").then_some(1),
        is_copied_context: None,
        latency_ms: scan.wants("latency_ms").then_some(latency_ms).flatten(),
        ttft_ms: None,
        had_observation: scan.wants("had_observation") && step.observation_present,
        extra_json: None,
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::fixture_path;
    use super::*;

    #[test]
    fn projected_actf_document_parses_trimmed_fixture() {
        let path = fixture_path("import_roundtrip/protein-assembly_trimmed.actf.json");
        let raw = std::fs::read(&path).unwrap();
        let scan = FileScanSpec::new(Some(&vec![0, 1, 2, 3]), &[], &story_steps_arrow_schema());
        let document = deserialize_projected_actf_from_slice(&raw, &scan).unwrap();
        assert_eq!(document.task_id, "protein-assembly-trimmed");
        assert_eq!(document.attempts.len(), 1);
        assert_eq!(document.attempts[0].1.steps.len(), 2);
    }
}
