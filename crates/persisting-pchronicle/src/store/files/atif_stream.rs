use super::*;

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
    reasoning_effort: Option<serde_json::Value>,
    message: serde_json::Value,
    reasoning_content: Option<String>,
    tool_calls_nonempty: bool,
    observation_present: bool,
    metrics: Option<serde_json::Value>,
    extra: Option<serde_json::Value>,
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

impl ProjectedAtifStepField {
    fn name(self) -> &'static str {
        match self {
            Self::StepId => "step_id",
            Self::Timestamp => "timestamp",
            Self::Source => "source",
            Self::ModelName => "model_name",
            Self::ReasoningEffort => "reasoning_effort",
            Self::Message => "message",
            Self::ReasoningContent => "reasoning_content",
            Self::ToolCalls => "tool_calls",
            Self::Observation => "observation",
            Self::Metrics => "metrics",
            Self::Extra => "extra",
            Self::LlmCallCount => "llm_call_count",
            Self::IsCopiedContext => "is_copied_context",
            Self::Other => "<unknown>",
        }
    }
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
        deserializer.deserialize_map(ProjectedAtifTrajectoryVisitor { scan: self.scan })
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
                    let known_session = session_id.as_deref().filter(|value| !value.is_empty());
                    if known_session.is_some_and(|value| !self.scan.matches_document(value)) {
                        skipped_steps = map.next_value_seed(CountSequenceSeed)?;
                        steps = Some(Vec::new());
                    } else {
                        steps =
                            Some(map.next_value_seed(ProjectedAtifStepsSeed { scan: self.scan })?);
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

struct ProjectedAtifStepsSeed<'a> {
    scan: &'a FileScanSpec,
}

impl<'de> DeserializeSeed<'de> for ProjectedAtifStepsSeed<'_> {
    type Value = Vec<ProjectedAtifStep>;

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_seq(ProjectedAtifStepsVisitor { scan: self.scan })
    }
}

struct ProjectedAtifStepsVisitor<'a> {
    scan: &'a FileScanSpec,
}

impl<'de> Visitor<'de> for ProjectedAtifStepsVisitor<'_> {
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
            sequence.next_element_seed(ProjectedAtifStepSeed { scan: self.scan })?
        {
            steps.push(step);
        }
        Ok(steps)
    }
}

struct ProjectedAtifStepSeed<'a> {
    scan: &'a FileScanSpec,
}

impl<'de> DeserializeSeed<'de> for ProjectedAtifStepSeed<'_> {
    type Value = ProjectedAtifStep;

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_map(ProjectedAtifStepVisitor { scan: self.scan })
    }
}

struct ProjectedAtifStepVisitor<'a> {
    scan: &'a FileScanSpec,
}

impl<'de> Visitor<'de> for ProjectedAtifStepVisitor<'_> {
    type Value = ProjectedAtifStep;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an ATIF step object")
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut seen = HashSet::new();
        let mut step_id = None;
        let mut timestamp = None;
        let mut source = None;
        let mut model_name = None;
        let mut reasoning_effort = None;
        let mut message = serde_json::Value::Null;
        let mut message_seen = false;
        let mut reasoning_content = None;
        let mut tool_calls_nonempty = false;
        let mut observation_present = false;
        let mut metrics = None;
        let mut extra = None;
        let mut llm_call_count = None;
        let mut is_copied_context = None;

        while let Some(field) = map.next_key::<ProjectedAtifStepField>()? {
            if field != ProjectedAtifStepField::Other && !seen.insert(field) {
                return Err(de::Error::duplicate_field(field.name()));
            }
            match field {
                ProjectedAtifStepField::StepId => step_id = Some(map.next_value::<i64>()?),
                ProjectedAtifStepField::Timestamp => {
                    if self.scan.wants("timestamp") {
                        timestamp = map.next_value::<Option<String>>()?;
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                ProjectedAtifStepField::Source => source = Some(map.next_value::<String>()?),
                ProjectedAtifStepField::ModelName => {
                    if self.scan.wants("model_name") {
                        model_name = map.next_value::<Option<String>>()?;
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                ProjectedAtifStepField::ReasoningEffort => {
                    if self.scan.wants("reasoning_effort_json") {
                        reasoning_effort = map.next_value::<Option<serde_json::Value>>()?;
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                ProjectedAtifStepField::Message => {
                    message_seen = true;
                    if self.scan.wants("message_json") {
                        message = map.next_value::<serde_json::Value>()?;
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                ProjectedAtifStepField::ReasoningContent => {
                    if self.scan.wants("reasoning_content") {
                        reasoning_content = map.next_value::<Option<String>>()?;
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                ProjectedAtifStepField::ToolCalls => {
                    if self.scan.wants("kind") || self.scan.wants("effective_kind") {
                        tool_calls_nonempty = map
                            .next_value::<Option<Vec<IgnoredAny>>>()?
                            .is_some_and(|calls| !calls.is_empty());
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                ProjectedAtifStepField::Observation => {
                    if self.scan.wants("had_observation") {
                        observation_present = map.next_value::<Option<IgnoredAny>>()?.is_some();
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                ProjectedAtifStepField::Metrics => {
                    if self.scan.wants("metrics_json")
                        || self.scan.wants("latency_ms")
                        || self.scan.wants("ttft_ms")
                    {
                        metrics = map.next_value::<Option<serde_json::Value>>()?;
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                ProjectedAtifStepField::Extra => {
                    if self.scan.wants("extra_json") {
                        extra = map.next_value::<Option<serde_json::Value>>()?;
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                ProjectedAtifStepField::LlmCallCount => {
                    if self.scan.wants("llm_call_count") {
                        llm_call_count = map.next_value::<Option<i64>>()?;
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                ProjectedAtifStepField::IsCopiedContext => {
                    if self.scan.wants("is_copied_context") {
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
            reasoning_effort,
            message,
            reasoning_content,
            tool_calls_nonempty,
            observation_present,
            metrics,
            extra,
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
        while sequence.next_element::<IgnoredAny>()?.is_some() {
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
    pending: Vec<StoryStepRow>,
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

struct BoundedCountingReader<R> {
    inner: R,
    bytes_read: u64,
    maximum: u64,
}

impl<R> BoundedCountingReader<R> {
    fn new(inner: R, maximum: u64) -> Self {
        Self {
            inner,
            bytes_read: 0,
            maximum,
        }
    }

    fn bytes_read(&self) -> u64 {
        self.bytes_read
    }
}

impl<R: Read> Read for BoundedCountingReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let remaining = self.maximum.saturating_sub(self.bytes_read);
        if remaining == 0 {
            let mut probe = [0_u8; 1];
            if self.inner.read(&mut probe)? == 0 {
                return Ok(0);
            }
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("trajectory input exceeded {} bytes", self.maximum),
            ));
        }
        let maximum = usize::try_from(remaining.min(buffer.len() as u64)).unwrap_or(buffer.len());
        let read = self.inner.read(&mut buffer[..maximum])?;
        self.bytes_read += read as u64;
        Ok(read)
    }
}

fn read_bounded_line<R: BufRead>(
    reader: &mut R,
    buffer: &mut Vec<u8>,
    maximum: usize,
) -> io::Result<usize> {
    buffer.clear();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(buffer.len());
        }
        let end = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index + 1);
        if buffer.len().saturating_add(end) > maximum {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("JSONL record exceeded max_record_bytes {maximum}"),
            ));
        }
        buffer.extend_from_slice(&available[..end]);
        let ended = available[end - 1] == b'\n';
        reader.consume(end);
        if ended {
            return Ok(buffer.len());
        }
    }
}

/// Copy one complete top-level JSON object out of a buffered stream.
///
/// This scanner only discovers the record boundary; serde remains the source
/// of truth for JSON syntax and ATIF validation. Strings and escapes are
/// tracked so braces inside message text do not terminate the record.
fn read_bounded_json_object<R: BufRead>(
    reader: &mut R,
    buffer: &mut Vec<u8>,
    maximum: usize,
) -> io::Result<usize> {
    buffer.clear();
    let mut depth = 0_usize;
    let mut in_string = false;
    let mut escaped = false;

    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "unterminated JSON object in array",
            ));
        }
        let mut end = available.len();
        let mut finished = false;
        for (index, byte) in available.iter().copied().enumerate() {
            if in_string {
                if escaped {
                    escaped = false;
                } else if byte == b'\\' {
                    escaped = true;
                } else if byte == b'"' {
                    in_string = false;
                }
                continue;
            }
            match byte {
                b'"' => in_string = true,
                b'{' | b'[' => {
                    depth = depth.checked_add(1).ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidData, "JSON nesting depth overflow")
                    })?;
                }
                b'}' | b']' => {
                    if depth == 0 {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "unexpected JSON closing delimiter",
                        ));
                    }
                    depth -= 1;
                    if depth == 0 {
                        if byte != b'}' {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                "ATIF array element must be a JSON object",
                            ));
                        }
                        end = index + 1;
                        finished = true;
                        break;
                    }
                }
                _ => {}
            }
        }
        if buffer.len().saturating_add(end) > maximum {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("JSON array record exceeded max_record_bytes {maximum}"),
            ));
        }
        buffer.extend_from_slice(&available[..end]);
        reader.consume(end);
        if finished {
            return Ok(buffer.len());
        }
    }
}

fn trim_ascii_whitespace(mut input: &[u8]) -> &[u8] {
    while input.first().is_some_and(u8::is_ascii_whitespace) {
        input = &input[1..];
    }
    while input.last().is_some_and(u8::is_ascii_whitespace) {
        input = &input[..input.len() - 1];
    }
    input
}

fn first_non_whitespace<R: BufRead>(reader: &mut R) -> io::Result<Option<u8>> {
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(None);
        }
        if let Some(index) = available
            .iter()
            .position(|byte| !byte.is_ascii_whitespace())
        {
            let first = available[index];
            reader.consume(index);
            return Ok(Some(first));
        }
        let length = available.len();
        reader.consume(length);
    }
}

fn is_ndjson(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| matches!(value.to_ascii_lowercase().as_str(), "jsonl" | "ndjson"))
}

fn stream_projected_atif_array<R: BufRead>(
    reader: &mut R,
    reader_capacity: usize,
    stream: &mut ProjectedAtifStream<'_>,
    maximum_record_bytes: usize,
) -> Result<()> {
    anyhow::ensure!(
        first_non_whitespace(reader)? == Some(b'['),
        "projected ATIF array must start with '['"
    );
    reader.consume(1);

    let mut first = true;
    let mut ordinal = 0_usize;
    let mut record = Vec::new();
    loop {
        if !first {
            match first_non_whitespace(reader)? {
                Some(b']') => {
                    reader.consume(1);
                    anyhow::ensure!(
                        first_non_whitespace(reader)?.is_none(),
                        "trailing content after ATIF JSON array"
                    );
                    return Ok(());
                }
                Some(b',') => reader.consume(1),
                Some(other) => {
                    anyhow::bail!("ATIF JSON array expected ',' or ']', found byte 0x{other:02x}")
                }
                None => anyhow::bail!("unterminated ATIF JSON array"),
            }
        }

        match first_non_whitespace(reader)? {
            Some(b']') if first => anyhow::bail!("ATIF input contains no trajectories"),
            Some(b'{') => {}
            Some(other) => {
                anyhow::bail!("ATIF JSON array element must be an object, found byte 0x{other:02x}")
            }
            None => anyhow::bail!("unterminated ATIF JSON array"),
        }

        ordinal += 1;
        read_bounded_json_object(reader, &mut record, maximum_record_bytes)
            .with_context(|| format!("read projected ATIF array element {ordinal}"))?;
        stream
            .runtime
            .metrics
            .inner
            .streaming_buffer_peak_bytes
            .fetch_max(
                reader_capacity.saturating_add(record.capacity()) as u64,
                Ordering::Relaxed,
            );
        let mut deserializer = serde_json::Deserializer::from_slice(&record);
        let trajectory = ProjectedAtifTrajectorySeed { scan: stream.scan }
            .deserialize(&mut deserializer)
            .with_context(|| format!("parse projected ATIF array element {ordinal}"))?;
        deserializer
            .end()
            .with_context(|| format!("finish projected ATIF array element {ordinal}"))?;
        stream.consume(trajectory)?;
        first = false;
    }
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

    if is_ndjson(file.file.path()) {
        let mut record = Vec::new();
        let mut line_number = 0_usize;
        let mut parsed_records = 0_usize;
        loop {
            let read =
                read_bounded_line(&mut reader, &mut record, runtime.options.max_record_bytes)
                    .with_context(|| {
                        format!("read projected ATIF JSONL {}", file.file.path().display())
                    })?;
            if read == 0 {
                break;
            }
            line_number += 1;
            runtime.metrics.inner.streaming_buffer_peak_bytes.fetch_max(
                reader.capacity().saturating_add(record.capacity()) as u64,
                Ordering::Relaxed,
            );
            let record = trim_ascii_whitespace(&record);
            if record.is_empty() {
                continue;
            }
            let mut deserializer = serde_json::Deserializer::from_slice(record);
            let trajectory = ProjectedAtifTrajectorySeed { scan }
                .deserialize(&mut deserializer)
                .with_context(|| {
                    format!(
                        "parse projected ATIF JSONL {} line {line_number}",
                        file.file.path().display()
                    )
                })?;
            deserializer.end().with_context(|| {
                format!(
                    "finish projected ATIF JSONL {} line {line_number}",
                    file.file.path().display()
                )
            })?;
            if let Err(error) = stream.consume(trajectory) {
                if stream.cancelled {
                    break;
                }
                return Err(error);
            }
            parsed_records += 1;
        }
        anyhow::ensure!(
            parsed_records > 0 || stream.cancelled,
            "ATIF input contains no trajectories: {}",
            file.file.path().display()
        );
    } else {
        let shape = first_non_whitespace(&mut reader)
            .with_context(|| format!("inspect ATIF input {}", file.file.path().display()))?
            .with_context(|| format!("ATIF input is empty: {}", file.file.path().display()))?;
        let result = match shape {
            b'{' => {
                let mut deserializer = serde_json::Deserializer::from_reader(&mut reader);
                let result = ProjectedAtifTrajectorySeed { scan }
                    .deserialize(&mut deserializer)
                    .map_err(anyhow::Error::from)
                    .and_then(|trajectory| stream.consume(trajectory));
                match result {
                    Ok(()) => deserializer.end().map_err(anyhow::Error::from),
                    Err(error) => Err(error),
                }
            }
            b'[' => {
                let reader_capacity = reader.capacity();
                stream_projected_atif_array(
                    &mut reader,
                    reader_capacity,
                    &mut stream,
                    runtime.options.max_record_bytes,
                )
            }
            _ => anyhow::bail!(
                "ATIF input {} must contain an object, array, JSONL, or NDJSON",
                file.file.path().display()
            ),
        };
        if let Err(error) = result {
            if !stream.cancelled {
                return Err(error).with_context(|| {
                    format!("parse projected ATIF input {}", file.file.path().display())
                });
            }
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
    pending: &mut Vec<StoryStepRow>,
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
) -> StoryStepRow {
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

    let (latency_ms, ttft_ms) = projected_timing_from_metrics(step.metrics.as_ref());
    StoryStepRow {
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
        message: if scan.wants("message_json") {
            step.message
        } else {
            serde_json::Value::Null
        },
        reasoning_content: scan
            .wants("reasoning_content")
            .then_some(step.reasoning_content)
            .flatten(),
        reasoning_effort: scan
            .wants("reasoning_effort_json")
            .then_some(step.reasoning_effort)
            .flatten(),
        metrics: scan.wants("metrics_json").then_some(step.metrics).flatten(),
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
        extra: scan.wants("extra_json").then_some(step.extra).flatten(),
    }
}

fn projected_timing_from_metrics(
    metrics: Option<&serde_json::Value>,
) -> (Option<i64>, Option<i64>) {
    let Some(metrics) = metrics else {
        return (None, None);
    };
    let latency_ms = metrics
        .get("latency_ms")
        .or_else(|| metrics.get("elapsed_ms"))
        .or_else(|| metrics.get("duration_ms"))
        .and_then(|value| {
            value
                .as_i64()
                .or_else(|| value.as_f64().map(|value| value as i64))
        });
    let ttft_ms = metrics.get("ttft_ms").and_then(|value| {
        value
            .as_i64()
            .or_else(|| value.as_f64().map(|value| value as i64))
    });
    (latency_ms, ttft_ms)
}

fn emit_projected_step_batch(
    rows: &mut Vec<StoryStepRow>,
    file: &Arc<FileState>,
    runtime: &Arc<FileTrajectoryRuntime>,
    schema: &SchemaRef,
    tx: &Sender<datafusion::common::Result<RecordBatch>>,
) -> Result<bool> {
    if rows.is_empty() {
        return Ok(true);
    }
    let batch = projected_step_rows_to_batch(rows, file.file.relative_path(), schema.clone())?;
    rows.clear();
    runtime
        .metrics
        .inner
        .projected_arrow_bytes
        .fetch_add(batch.get_array_memory_size() as u64, Ordering::Relaxed);
    Ok(tx.blocking_send(Ok(batch)).is_ok())
}

fn projected_step_rows_to_batch(
    rows: &[StoryStepRow],
    relative_path: &str,
    schema: SchemaRef,
) -> Result<RecordBatch> {
    let mut columns = Vec::<ArrayRef>::with_capacity(schema.fields().len());
    for field in schema.fields() {
        let column: ArrayRef = match field.name().as_str() {
            "document_id" => Arc::new(StringArray::from_iter_values(
                rows.iter().map(|row| row.document_id.as_str()),
            )),
            "run_id" => Arc::new(StringArray::from_iter(
                rows.iter().map(|row| row.run_id.as_deref()),
            )),
            "session_id" => Arc::new(StringArray::from_iter_values(
                rows.iter().map(|row| row.session_id.as_str()),
            )),
            "step_id" => Arc::new(Int64Array::from(
                rows.iter().map(|row| row.step_id).collect::<Vec<_>>(),
            )),
            "kind" => Arc::new(StringArray::from_iter(
                rows.iter().map(|row| row.kind.as_deref()),
            )),
            "effective_kind" => Arc::new(StringArray::from_iter_values(
                rows.iter().map(|row| row.effective_kind.as_str()),
            )),
            "timestamp" => Arc::new(timestamp_array(
                rows.iter().map(|row| row.timestamp.as_deref()),
            )?),
            "timestamp_rfc3339" => Arc::new(StringArray::from_iter(
                rows.iter().map(|row| row.timestamp.as_deref()),
            )),
            "source" => Arc::new(StringArray::from_iter_values(
                rows.iter().map(|row| row.source.as_str()),
            )),
            "message_json" => Arc::new(StringArray::from_iter_values(
                rows.iter()
                    .map(|row| serde_json::to_string(&row.message))
                    .collect::<serde_json::Result<Vec<_>>>()?
                    .iter()
                    .map(String::as_str),
            )),
            "reasoning_content" => Arc::new(StringArray::from_iter(
                rows.iter().map(|row| row.reasoning_content.as_deref()),
            )),
            "reasoning_effort_json" => Arc::new(optional_json_array(
                rows.iter().map(|row| row.reasoning_effort.as_ref()),
            )?),
            "metrics_json" => Arc::new(optional_json_array(
                rows.iter().map(|row| row.metrics.as_ref()),
            )?),
            "model_name" => Arc::new(StringArray::from_iter(
                rows.iter().map(|row| row.model_name.as_deref()),
            )),
            "llm_call_count" => Arc::new(Int64Array::from(
                rows.iter()
                    .map(|row| row.llm_call_count)
                    .collect::<Vec<_>>(),
            )),
            "is_copied_context" => Arc::new(BooleanArray::from(
                rows.iter()
                    .map(|row| row.is_copied_context)
                    .collect::<Vec<_>>(),
            )),
            "latency_ms" => Arc::new(Int64Array::from(
                rows.iter().map(|row| row.latency_ms).collect::<Vec<_>>(),
            )),
            "ttft_ms" => Arc::new(Int64Array::from(
                rows.iter().map(|row| row.ttft_ms).collect::<Vec<_>>(),
            )),
            "had_observation" => Arc::new(BooleanArray::from(
                rows.iter()
                    .map(|row| row.had_observation)
                    .collect::<Vec<_>>(),
            )),
            "extra_json" => Arc::new(optional_json_array(
                rows.iter().map(|row| row.extra.as_ref()),
            )?),
            SOURCE_FILE_COLUMN => Arc::new(StringArray::from_iter_values(std::iter::repeat_n(
                relative_path,
                rows.len(),
            ))),
            name => anyhow::bail!("unsupported projected ATIF steps column '{name}'"),
        };
        columns.push(column);
    }
    let options = RecordBatchOptions::new().with_row_count(Some(rows.len()));
    RecordBatch::try_new_with_options(schema, columns, &options)
        .context("build projected ATIF steps batch")
}

fn optional_json_array<'a>(
    values: impl IntoIterator<Item = Option<&'a serde_json::Value>>,
) -> Result<StringArray> {
    Ok(StringArray::from(
        values
            .into_iter()
            .map(|value| value.map(serde_json::to_string).transpose())
            .collect::<serde_json::Result<Vec<_>>>()?,
    ))
}
