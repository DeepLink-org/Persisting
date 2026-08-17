//! Fault probes used by the Langfuse backend feasibility review.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result};
use persisting_pchronicle::model::{EventIdentity, EventRecord};
use persisting_pchronicle::storage::{RawEventLanceStore, StoryCoords};

const CHILD_MODE_ENV: &str = "PCHRONICLE_LANGFUSE_CRASH_CHILD";
const STORAGE_ENV: &str = "PCHRONICLE_LANGFUSE_CRASH_STORAGE";
const ACK_MARKER: &str = "PCHRONICLE_LANGFUSE_ACKNOWLEDGED";

fn session(storage: &str) -> StoryCoords {
    StoryCoords::new(
        storage,
        "project-a",
        "kill-after-ack-trace",
        Some("kill-after-ack-trace".into()),
    )
}

fn event() -> EventRecord {
    EventRecord {
        identity: EventIdentity {
            event_id: Some("kill-after-ack-event".into()),
            timestamp_unix_ms: Some(1_767_225_600_000),
            ..Default::default()
        },
        seq: 0,
        source: "langfuse-backend-fault-probe".into(),
        kind: "event".into(),
        timestamp: Some("2026-01-01T00:00:00.000Z".into()),
        session_id: None,
        agent_id: Some("project-a".into()),
        parent_uuid: None,
        trace_id: Some("kill-after-ack-trace".into()),
        call_id: Some("kill-after-ack-span".into()),
        subagent_id: None,
        parent_agent_id: None,
        branch: None,
        parent_call_id: None,
        payload: serde_json::json!({"logical_id": "kill-after-ack-event"}),
    }
}

#[test]
fn crash_writer_child() -> Result<()> {
    if std::env::var_os(CHILD_MODE_ENV).is_none() {
        return Ok(());
    }
    let storage = std::env::var(STORAGE_ENV).context("missing crash child storage")?;
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async {
        let outcome = RawEventLanceStore
            .append_events(&session(&storage), &[event()])
            .await?;
        anyhow::ensure!(outcome.accepted_records == 1 && outcome.persisted_units == 1);
        Ok::<_, anyhow::Error>(())
    })?;
    println!("{ACK_MARKER}");
    std::io::stdout().flush()?;
    loop {
        std::thread::sleep(Duration::from_secs(60));
    }
}

#[test]
fn kill_after_ack_preserves_acknowledged_row() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let storage = temp.path().join("store").to_string_lossy().into_owned();
    let executable = std::env::current_exe()?;
    let mut child = Command::new(executable)
        .args(["--exact", "crash_writer_child", "--nocapture"])
        .env(CHILD_MODE_ENV, "1")
        .env(STORAGE_ENV, &storage)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("spawn crash writer child")?;
    let stdout = child.stdout.take().context("capture crash child stdout")?;
    let mut acknowledged = false;
    for line in BufReader::new(stdout).lines() {
        if line?.contains(ACK_MARKER) {
            acknowledged = true;
            break;
        }
    }
    anyhow::ensure!(acknowledged, "child exited before acknowledging its event");
    child.kill().context("SIGKILL acknowledged writer")?;
    let status = child.wait()?;
    anyhow::ensure!(!status.success(), "crash child unexpectedly exited cleanly");

    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async {
        let stored = RawEventLanceStore
            .read_events(&session(&storage), 0, None)
            .await?;
        anyhow::ensure!(stored.len() == 1, "acknowledged row was lost after SIGKILL");
        anyhow::ensure!(
            stored[0].identity.event_id.as_deref() == Some("kill-after-ack-event"),
            "unexpected row survived SIGKILL"
        );
        Ok::<_, anyhow::Error>(())
    })
}
