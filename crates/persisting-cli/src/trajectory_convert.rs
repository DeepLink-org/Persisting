//! `traj convert`: chronicle format interchange via the pChronicle storyline hub.

use std::fs;
use std::io::{self, Read, Write};
use std::path::Path;

use anyhow::{bail, Context, Result};
use clap::{Args, ValueEnum};
use persisting_capture::record::{record_to_engine_line, CaptureRecord};
use persisting_engine::trajectory::{
    resolve_traj_read_location, truncate_async, LanceTrajectoryStore, TrajectorySession,
    TrajectoryStore,
};
use persisting_pchronicle::convert::{
    events_to_storyline, from_storyline, into_storyline, storyline_to_events,
};
use persisting_pchronicle::{
    detect_format, ChronicleFormat, EventRecord, EventsDocument, StorylineDocument,
};
use persisting_proto::TrajectoryTruncateRequest;

/// CLI mirror of [`ChronicleFormat`] with clap aliases.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum ChronicleFormatCli {
    Storyline,
    #[value(alias = "lance", alias = "bin", alias = "event")]
    Events,
    #[value(alias = "md", alias = "markdown", alias = "tlv")]
    Agenticmd,
    #[value(
        name = "openai_msg",
        alias = "openai",
        alias = "openai-msg",
        alias = "dlcapt"
    )]
    OpenaiMsg,
    #[value(alias = "harbor")]
    Atif,
}

impl From<ChronicleFormatCli> for ChronicleFormat {
    fn from(v: ChronicleFormatCli) -> Self {
        match v {
            ChronicleFormatCli::Storyline => ChronicleFormat::Storyline,
            ChronicleFormatCli::Events => ChronicleFormat::Events,
            ChronicleFormatCli::Agenticmd => ChronicleFormat::Agenticmd,
            ChronicleFormatCli::OpenaiMsg => ChronicleFormat::OpenaiMsg,
            ChronicleFormatCli::Atif => ChronicleFormat::Atif,
        }
    }
}

impl ChronicleFormatCli {
    fn as_chronicle(self) -> ChronicleFormat {
        self.into()
    }
}

#[derive(Debug, Args)]
pub struct TrajectoryConvertArgs {
    /// Input file (`-` = stdin), session/run directory, or `events.lance` (for `--from events`).
    #[arg(value_name = "INPUT")]
    pub input: String,
    /// Destination file (`-` = stdout), or storage/session directory (for `--fmt events`).
    #[arg(short = 'o', long, value_name = "DEST")]
    pub output: String,
    /// Output chronicle format.
    #[arg(short = 'f', long = "fmt", value_enum, visible_alias = "to")]
    pub fmt: ChronicleFormatCli,
    /// Input format (default: auto-detect from path / content).
    #[arg(long, value_enum)]
    pub from: Option<ChronicleFormatCli>,
    #[arg(long, value_name = "SEG")]
    pub agent_id: Option<String>,
    #[arg(long, value_name = "SEG")]
    pub session_id: Option<String>,
    #[arg(long, value_name = "SEG")]
    pub root_session_id: Option<String>,
    /// Overwrite an existing Lance event log when `--fmt events`.
    #[arg(long)]
    pub force: bool,
}

pub fn run_traj_convert(args: &TrajectoryConvertArgs) -> Result<()> {
    let to = args.fmt.as_chronicle();
    let from = resolve_from_format(args)?;

    if from == to && !from.is_lance_only() {
        let text = read_string_input(&args.input)?;
        write_string_output(&args.output, &text)?;
        eprintln!(
            "[persisting-cli] traj convert: {from} → {to} (identity) input={} output={}",
            display_io(&args.input),
            display_io(&args.output)
        );
        return Ok(());
    }

    let story = load_storyline(args, from)?;
    write_converted(args, from, to, &story)?;
    Ok(())
}

fn resolve_from_format(args: &TrajectoryConvertArgs) -> Result<ChronicleFormat> {
    if let Some(from) = args.from {
        return Ok(from.as_chronicle());
    }

    if args.input == "-" {
        bail!(
            "when INPUT is '-' (stdin), set --from explicitly \
             (storyline|atif|openai_msg|agenticmd)"
        );
    }

    let input_path = Path::new(&args.input);
    if let Some(fmt) = detect_format(Some(input_path), None)? {
        return Ok(fmt);
    }
    // Session / run directory → events when Lance dataset is present.
    if input_path.is_dir()
        || input_path
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|n| n.eq_ignore_ascii_case("events.lance"))
    {
        return Ok(ChronicleFormat::Events);
    }

    let text = read_string_input(&args.input)?;
    if let Some(fmt) = detect_format(Some(input_path), Some(&text))? {
        return Ok(fmt);
    }

    bail!(
        "cannot auto-detect input format for {}; set --from explicitly \
         (storyline|atif|openai_msg|agenticmd|events)",
        display_io(&args.input)
    );
}

fn load_storyline(
    args: &TrajectoryConvertArgs,
    from: ChronicleFormat,
) -> Result<StorylineDocument> {
    match from {
        ChronicleFormat::Events => {
            reject_events_jsonl_input(&args.input)?;
            let session = resolve_events_session(
                "convert --from events",
                &args.input,
                args.agent_id.clone(),
                args.session_id.clone(),
                args.root_session_id.clone(),
            )?;
            let doc = load_events_document(&session)?;
            events_to_storyline(&doc).map_err(|e| anyhow::anyhow!("{e}"))
        }
        other => {
            let text = read_string_input(&args.input)?;
            into_storyline(other, &text).map_err(|e| anyhow::anyhow!("{e}"))
        }
    }
}

fn write_converted(
    args: &TrajectoryConvertArgs,
    from: ChronicleFormat,
    to: ChronicleFormat,
    story: &StorylineDocument,
) -> Result<()> {
    match to {
        ChronicleFormat::Events => {
            let session = resolve_events_dest_session(args, story)?;
            write_events_lance(&session, story, args.force)?;
            eprintln!(
                "[persisting-cli] traj convert: {from} → events input={} output={}",
                display_io(&args.input),
                session.lance_event_path()?.display()
            );
        }
        other => {
            let text = from_storyline(other, story).map_err(|e| anyhow::anyhow!("{e}"))?;
            write_string_output(&args.output, &text)?;
            eprintln!(
                "[persisting-cli] traj convert: {from} → {other} input={} output={}",
                display_io(&args.input),
                display_io(&args.output)
            );
        }
    }
    Ok(())
}

fn reject_events_jsonl_input(input: &str) -> Result<()> {
    if input == "-" {
        bail!(
            "events is Lance-only; cannot read JSON/JSONL from stdin. \
             Pass a session directory or events.lance path with --from events."
        );
    }
    let lower = input.to_ascii_lowercase();
    if lower.ends_with(".jsonl") || lower.ends_with(".json") {
        bail!(
            "events is Lance-only (events.lance); refusing JSON/JSONL input {input:?}. \
             Pass a capture session/run directory or events.lance path."
        );
    }
    Ok(())
}

fn resolve_events_session(
    op: &str,
    input: &str,
    agent_id: Option<String>,
    session_id: Option<String>,
    root_session_id: Option<String>,
) -> Result<TrajectorySession> {
    let path = Path::new(input);
    let path_arg = if path
        .file_name()
        .and_then(|s| s.to_str())
        .is_some_and(|n| n.eq_ignore_ascii_case("events.lance"))
    {
        path.parent()
            .map(|p| p.to_string_lossy().into_owned())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| ".".into())
    } else {
        input.to_string()
    };
    resolve_traj_read_location(op, path_arg, agent_id, session_id, root_session_id)
}

fn resolve_events_dest_session(
    args: &TrajectoryConvertArgs,
    story: &StorylineDocument,
) -> Result<TrajectorySession> {
    let out = Path::new(&args.output);
    if args.output == "-" {
        bail!(
            "--fmt events writes Lance (events.lance); cannot use stdout. \
             Pass -o <storage-or-session-dir> [--agent-id …] [--session-id …]."
        );
    }
    if out.is_file() {
        bail!(
            "--fmt events expects a storage or session directory, not a file: {}",
            args.output
        );
    }

    // Prefer explicit CLI coords; else infer from -o path; else storyline ids.
    let agent = args.agent_id.clone().or_else(|| {
        if out.exists() {
            None
        } else {
            Some(story.agent.id.clone())
        }
    });
    let session = args.session_id.clone().or_else(|| {
        if out.exists() {
            None
        } else {
            Some(story.session_id.clone())
        }
    });

    match resolve_traj_read_location(
        "convert --fmt events",
        args.output.clone(),
        agent.clone(),
        session.clone(),
        args.root_session_id.clone(),
    ) {
        Ok(loc) => Ok(loc),
        Err(_) if agent.is_some() && session.is_some() => {
            // Brand-new storage root: construct coords directly.
            Ok(TrajectorySession::new(
                args.output.clone(),
                agent.unwrap(),
                session.unwrap(),
                args.root_session_id.clone(),
            ))
        }
        Err(e) => Err(e).context(
            "resolve destination session for --fmt events \
             (pass -o <storage> --agent-id <id> --session-id <id>, or an existing session dir)",
        ),
    }
}

fn load_events_document(session: &TrajectorySession) -> Result<EventsDocument> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("create tokio runtime for Lance replay")?;
    rt.block_on(async {
        let lance = LanceTrajectoryStore;
        if !lance.exists(session).await? {
            bail!(
                "Lance event log missing at {}; --from events requires events.lance",
                lance.display_path(session)?
            );
        }
        let outcome = lance
            .replay(session, 0, None)
            .await
            .context("replay Lance for traj convert")?;
        let mut events = Vec::with_capacity(outcome.records.len());
        for (i, json) in outcome.records.iter().enumerate() {
            let rec: CaptureRecord =
                serde_json::from_str(json).with_context(|| format!("decode replay record[{i}]"))?;
            events.push(capture_to_event(rec));
        }
        Ok(EventsDocument {
            format: EventsDocument::FORMAT_NAME.into(),
            session_id: Some(session.session_id.clone()),
            agent_id: Some(session.agent_id.clone()),
            events,
        })
    })
}

fn write_events_lance(
    session: &TrajectorySession,
    story: &StorylineDocument,
    force: bool,
) -> Result<()> {
    let doc = storyline_to_events(story).map_err(|e| anyhow::anyhow!("{e}"))?;
    let lines: Vec<String> = doc
        .events
        .iter()
        .enumerate()
        .map(|(i, ev)| {
            let rec = event_to_capture(ev);
            record_to_engine_line(&rec).with_context(|| format!("encode event[{i}]"))
        })
        .collect::<Result<_>>()?;

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("create tokio runtime for Lance write")?;
    rt.block_on(async {
        let lance = LanceTrajectoryStore;
        if lance.exists(session).await? {
            if !force {
                bail!(
                    "Lance event log already exists at {}; pass --force to overwrite",
                    lance.display_path(session)?
                );
            }
            truncate_async(TrajectoryTruncateRequest {
                storage: session.storage.clone(),
                agent_id: session.agent_id.clone(),
                session_id: session.session_id.clone(),
                root_session_id: session.root_session_id.clone(),
                keep_rows: 0,
            })
            .await
            .context("truncate existing Lance before --force write")?;
        }
        if lines.is_empty() {
            // Ensure run dir exists even for empty conversion.
            let run = session.run_dir()?;
            fs::create_dir_all(&run)
                .with_context(|| format!("create run dir {}", run.display()))?;
            return Ok(());
        }
        lance
            .append(session, &lines)
            .await
            .context("append converted events to Lance")?;
        Ok(())
    })
}

fn capture_to_event(rec: CaptureRecord) -> EventRecord {
    EventRecord {
        seq: rec.seq,
        source: rec.source,
        kind: rec.kind,
        timestamp: rec.timestamp,
        session_id: rec.session_id,
        agent_id: rec.agent_id,
        parent_uuid: rec.parent_uuid,
        trace_id: rec.trace_id,
        call_id: rec.call_id,
        subagent_id: rec.subagent_id,
        parent_agent_id: rec.parent_agent_id,
        branch: rec.branch,
        parent_call_id: rec.parent_call_id,
        payload: rec.payload,
    }
}

fn event_to_capture(ev: &EventRecord) -> CaptureRecord {
    CaptureRecord {
        seq: ev.seq,
        source: ev.source.clone(),
        kind: ev.kind.clone(),
        timestamp: ev.timestamp.clone(),
        session_id: ev.session_id.clone(),
        agent_id: ev.agent_id.clone(),
        parent_uuid: ev.parent_uuid.clone(),
        trace_id: ev.trace_id.clone(),
        call_id: ev.call_id.clone(),
        subagent_id: ev.subagent_id.clone(),
        parent_agent_id: ev.parent_agent_id.clone(),
        branch: ev.branch.clone(),
        parent_call_id: ev.parent_call_id.clone(),
        payload: ev.payload.clone(),
    }
}

fn read_string_input(input: &str) -> Result<String> {
    if input == "-" {
        let mut buf = String::new();
        io::stdin().read_to_string(&mut buf).context("read stdin")?;
        return Ok(buf);
    }
    fs::read_to_string(input).with_context(|| format!("read input {input}"))
}

fn write_string_output(output: &str, text: &str) -> Result<()> {
    if output == "-" {
        let mut out = io::stdout().lock();
        out.write_all(text.as_bytes()).context("write stdout")?;
        if !text.ends_with('\n') {
            out.write_all(b"\n").ok();
        }
        return Ok(());
    }
    let path = Path::new(output);
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create parent dir {}", parent.display()))?;
        }
    }
    fs::write(path, text).with_context(|| format!("write output {output}"))
}

fn display_io(s: &str) -> &str {
    if s == "-" {
        "<stdin/stdout>"
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use persisting_pchronicle::OPENAI_MSG_FORMAT_VERSION;
    use tempfile::tempdir;

    fn sample_storyline() -> String {
        r#"{
  "spec": "storyline/v1",
  "session": "sess-cli",
  "agent": { "id": "agent-1", "name": "demo" },
  "turns": [
    {
      "id": 1,
      "src": "user",
      "msg": "hello"
    },
    {
      "id": 2,
      "src": "agent",
      "msg": "world",
      "latency_ms": 10
    }
  ]
}"#
        .to_string()
    }

    #[test]
    fn convert_storyline_to_agenticmd_and_back() {
        let dir = tempdir().unwrap();
        let input = dir.path().join("in.storyline.json");
        let md = dir.path().join("out.md");
        let back = dir.path().join("back.storyline.json");
        fs::write(&input, sample_storyline()).unwrap();

        run_traj_convert(&TrajectoryConvertArgs {
            input: input.to_string_lossy().into(),
            output: md.to_string_lossy().into(),
            fmt: ChronicleFormatCli::Agenticmd,
            from: Some(ChronicleFormatCli::Storyline),
            agent_id: None,
            session_id: None,
            root_session_id: None,
            force: false,
        })
        .unwrap();
        assert!(md.exists());
        let md_text = fs::read_to_string(&md).unwrap();
        assert!(md_text.contains("hello") || md_text.contains("world"));

        run_traj_convert(&TrajectoryConvertArgs {
            input: md.to_string_lossy().into(),
            output: back.to_string_lossy().into(),
            fmt: ChronicleFormatCli::Storyline,
            from: Some(ChronicleFormatCli::Agenticmd),
            agent_id: None,
            session_id: None,
            root_session_id: None,
            force: false,
        })
        .unwrap();
        let story: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&back).unwrap()).unwrap();
        assert_eq!(story["session"], "sess-cli");
        assert!(!story["turns"].as_array().unwrap().is_empty());
    }

    #[test]
    fn convert_atif_to_openai_msg() {
        let dir = tempdir().unwrap();
        let input = dir.path().join("t.atif.json");
        let output = dir.path().join("steps.json");
        let atif = r#"{
  "schema_version": "ATIF-v1.7",
  "session_id": "sess-1",
  "agent": { "name": "demo", "version": "0.1.0", "model_name": "m" },
  "steps": [
    { "step_id": 1, "source": "user", "message": "hi" },
    { "step_id": 2, "source": "agent", "message": "yo" }
  ]
}"#;
        fs::write(&input, atif).unwrap();
        run_traj_convert(&TrajectoryConvertArgs {
            input: input.to_string_lossy().into(),
            output: output.to_string_lossy().into(),
            fmt: ChronicleFormatCli::OpenaiMsg,
            from: Some(ChronicleFormatCli::Atif),
            agent_id: None,
            session_id: None,
            root_session_id: None,
            force: false,
        })
        .unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&output).unwrap()).unwrap();
        assert!(v.get("session_steps").is_some());
        assert_eq!(v["format_version"], OPENAI_MSG_FORMAT_VERSION);
    }

    #[test]
    fn reject_events_jsonl() {
        let dir = tempdir().unwrap();
        let input = dir.path().join("x.jsonl");
        fs::write(&input, "{}\n").unwrap();
        let err = run_traj_convert(&TrajectoryConvertArgs {
            input: input.to_string_lossy().into(),
            output: dir.path().join("out.json").to_string_lossy().into(),
            fmt: ChronicleFormatCli::Storyline,
            from: Some(ChronicleFormatCli::Events),
            agent_id: None,
            session_id: None,
            root_session_id: None,
            force: false,
        })
        .unwrap_err();
        assert!(err.to_string().contains("Lance-only"), "{err}");
    }

    fn args(
        input: impl Into<String>,
        output: impl Into<String>,
        fmt: ChronicleFormatCli,
        from: Option<ChronicleFormatCli>,
    ) -> TrajectoryConvertArgs {
        TrajectoryConvertArgs {
            input: input.into(),
            output: output.into(),
            fmt,
            from,
            agent_id: None,
            session_id: None,
            root_session_id: None,
            force: false,
        }
    }

    #[test]
    fn auto_detect_storyline_path_and_identity_copy() {
        let dir = tempdir().unwrap();
        let input = dir.path().join("storyline.json");
        let output = dir.path().join("copy.storyline.json");
        fs::write(&input, sample_storyline()).unwrap();
        run_traj_convert(&args(
            input.to_string_lossy(),
            output.to_string_lossy(),
            ChronicleFormatCli::Storyline,
            None,
        ))
        .unwrap();
        let out = fs::read_to_string(&output).unwrap();
        assert!(out.contains("sess-cli"));
        assert!(out.contains("hello"));
    }

    #[test]
    fn convert_storyline_to_atif_preserves_turns() {
        let dir = tempdir().unwrap();
        let input = dir.path().join("in.storyline.json");
        let output = dir.path().join("out.atif.json");
        fs::write(&input, sample_storyline()).unwrap();
        run_traj_convert(&args(
            input.to_string_lossy(),
            output.to_string_lossy(),
            ChronicleFormatCli::Atif,
            Some(ChronicleFormatCli::Storyline),
        ))
        .unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&output).unwrap()).unwrap();
        assert_eq!(v["session_id"], "sess-cli");
        let steps = v["steps"].as_array().unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0]["source"], "user");
        assert_eq!(steps[0]["message"], "hello");
        assert_eq!(steps[1]["source"], "agent");
        assert_eq!(steps[1]["message"], "world");
    }

    #[test]
    fn convert_creates_parent_dirs() {
        let dir = tempdir().unwrap();
        let input = dir.path().join("in.storyline.json");
        let output = dir.path().join("nested").join("deep").join("out.md");
        fs::write(&input, sample_storyline()).unwrap();
        run_traj_convert(&args(
            input.to_string_lossy(),
            output.to_string_lossy(),
            ChronicleFormatCli::Agenticmd,
            Some(ChronicleFormatCli::Storyline),
        ))
        .unwrap();
        assert!(output.exists());
    }

    #[test]
    fn convert_atif_openai_storyline_chain() {
        let dir = tempdir().unwrap();
        let atif_path = dir.path().join("t.atif.json");
        let openai_path = dir.path().join("steps.json");
        let story_path = dir.path().join("storyline.json");
        let atif = r#"{
  "schema_version": "ATIF-v1.7",
  "session_id": "chain-1",
  "agent": { "name": "demo", "version": "0.1.0", "model_name": "m" },
  "steps": [
    { "step_id": 1, "source": "user", "message": "alpha" },
    { "step_id": 2, "source": "agent", "message": "beta" }
  ]
}"#;
        fs::write(&atif_path, atif).unwrap();
        run_traj_convert(&args(
            atif_path.to_string_lossy(),
            openai_path.to_string_lossy(),
            ChronicleFormatCli::OpenaiMsg,
            Some(ChronicleFormatCli::Atif),
        ))
        .unwrap();
        run_traj_convert(&args(
            openai_path.to_string_lossy(),
            story_path.to_string_lossy(),
            ChronicleFormatCli::Storyline,
            Some(ChronicleFormatCli::OpenaiMsg),
        ))
        .unwrap();
        let story: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&story_path).unwrap()).unwrap();
        assert_eq!(story["session"], "chain-1");
        let turns = story["turns"].as_array().unwrap();
        assert!(turns.len() >= 2);
        assert!(turns
            .iter()
            .any(|t| t["msg"] == "alpha" || t["msg"] == "beta"));
    }

    #[test]
    fn convert_storyline_to_events_lance_and_back() {
        let dir = tempdir().unwrap();
        let input = dir.path().join("in.storyline.json");
        let store = dir.path().join("store");
        let out_story = dir.path().join("back.storyline.json");
        fs::write(&input, sample_storyline()).unwrap();

        run_traj_convert(&TrajectoryConvertArgs {
            input: input.to_string_lossy().into(),
            output: store.to_string_lossy().into(),
            fmt: ChronicleFormatCli::Events,
            from: Some(ChronicleFormatCli::Storyline),
            agent_id: Some("agent-1".into()),
            session_id: Some("sess-cli".into()),
            root_session_id: None,
            force: false,
        })
        .unwrap();

        // Second write without --force must fail.
        let err = run_traj_convert(&TrajectoryConvertArgs {
            input: input.to_string_lossy().into(),
            output: store.to_string_lossy().into(),
            fmt: ChronicleFormatCli::Events,
            from: Some(ChronicleFormatCli::Storyline),
            agent_id: Some("agent-1".into()),
            session_id: Some("sess-cli".into()),
            root_session_id: None,
            force: false,
        })
        .unwrap_err();
        assert!(err.to_string().contains("--force"), "{err}");

        run_traj_convert(&TrajectoryConvertArgs {
            input: store.to_string_lossy().into(),
            output: out_story.to_string_lossy().into(),
            fmt: ChronicleFormatCli::Storyline,
            from: Some(ChronicleFormatCli::Events),
            agent_id: Some("agent-1".into()),
            session_id: Some("sess-cli".into()),
            root_session_id: None,
            force: false,
        })
        .unwrap();
        let story: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&out_story).unwrap()).unwrap();
        assert_eq!(story["session"], "sess-cli");
        let turns = story["turns"].as_array().unwrap();
        assert!(!turns.is_empty());
        let texts: Vec<String> = turns
            .iter()
            .filter_map(|t| t.get("msg").and_then(|m| m.as_str()).map(str::to_string))
            .collect();
        assert!(
            texts
                .iter()
                .any(|t| t.contains("hello") || t.contains("world")),
            "unexpected turns: {texts:?}"
        );
    }

    #[test]
    fn convert_to_events_rejects_stdout() {
        let dir = tempdir().unwrap();
        let input = dir.path().join("in.storyline.json");
        fs::write(&input, sample_storyline()).unwrap();
        let err = run_traj_convert(&TrajectoryConvertArgs {
            input: input.to_string_lossy().into(),
            output: "-".into(),
            fmt: ChronicleFormatCli::Events,
            from: Some(ChronicleFormatCli::Storyline),
            agent_id: Some("a".into()),
            session_id: Some("s".into()),
            root_session_id: None,
            force: false,
        })
        .unwrap_err();
        assert!(err.to_string().contains("stdout"), "{err}");
    }

    #[test]
    fn stdin_requires_explicit_from() {
        // We cannot safely consume real stdin in unit tests; exercise the resolver
        // by constructing args that would hit the stdin branch.
        let err = resolve_from_format(&TrajectoryConvertArgs {
            input: "-".into(),
            output: "-".into(),
            fmt: ChronicleFormatCli::Storyline,
            from: None,
            agent_id: None,
            session_id: None,
            root_session_id: None,
            force: false,
        })
        .unwrap_err();
        assert!(err.to_string().contains("--from"), "{err}");
    }
}
