//! Opt-in S3 contract test.
//!
//! Run with:
//! `PCHRONICLE_S3_TEST_URI=s3://bucket/test-prefix cargo test -p persisting-pchronicle --test s3_storage -- --ignored`

use anyhow::{Context, Result};
use opendal::Operator;
use persisting_pchronicle::document::{DocumentFormat, decode_json_storylines};
use persisting_pchronicle::model::StorylineDocument;
use persisting_pchronicle::storage::StorylineLanceStore;
use std::io::{Read, Write};
use std::process::{Command, Stdio};

mod support;

use support::fixture_path;

const REPLACEMENT_WORKER_ROOT_ENV: &str = "PCHRONICLE_S3_REPLACEMENT_WORKER_ROOT";
const REPLACEMENT_WORKER_SESSION_ENV: &str = "PCHRONICLE_S3_REPLACEMENT_WORKER_SESSION";
const REPLACEMENT_WORKER_BARRIER_ENV: &str = "PCHRONICLE_S3_REPLACEMENT_WORKER_BARRIER";

fn unique_root() -> Result<String> {
    let base = std::env::var("PCHRONICLE_S3_TEST_URI")
        .context("set PCHRONICLE_S3_TEST_URI to an isolated writable s3:// prefix")?;
    anyhow::ensure!(base.starts_with("s3://"), "test URI must use s3://");
    let suffix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    Ok(format!(
        "{}/pchronicle-contract-{}-{suffix}",
        base.trim_end_matches('/'),
        std::process::id()
    ))
}

fn fixture_storyline() -> Result<StorylineDocument> {
    let source = std::fs::read_to_string(fixture_path("atif/parallel_tools_14.json"))?;
    decode_json_storylines(DocumentFormat::Atif, &source, "fixture.json")?
        .pop()
        .context("missing fixture Storyline")
}

fn fixture_storyline_with_id(session_id: &str) -> Result<StorylineDocument> {
    let mut storyline = fixture_storyline()?;
    storyline.trajectory_id = Some(session_id.to_string());
    storyline.session_id = session_id.to_string();
    storyline.run_id = Some(session_id.to_string());
    Ok(storyline)
}

async fn write_replacement_outcome(root: &str, session_id: &str, outcome: &str) -> Result<()> {
    Operator::from_uri(root)?
        .write(
            &format!("replacement-outcomes/{session_id}"),
            outcome.as_bytes().to_vec(),
        )
        .await?;
    Ok(())
}

async fn read_replacement_outcome(root: &str, session_id: &str) -> Result<String> {
    let bytes = Operator::from_uri(root)?
        .read(&format!("replacement-outcomes/{session_id}"))
        .await?;
    Ok(std::str::from_utf8(&bytes.to_vec())?.to_string())
}

async fn cleanup(root: &str) -> Result<()> {
    if let Err(error) = Operator::from_uri(root)?
        .delete_with(".")
        .recursive(true)
        .await
    {
        eprintln!("S3 test cleanup skipped for {root}: {error}");
    }
    Ok(())
}

#[tokio::test]
#[ignore = "worker launched by s3_storyline_multiprocess_replacement_contract"]
async fn s3_storyline_replacement_worker() -> Result<()> {
    let root = match std::env::var(REPLACEMENT_WORKER_ROOT_ENV) {
        Ok(root) => root,
        Err(std::env::VarError::NotPresent) => return Ok(()),
        Err(error) => return Err(error).context("read S3 replacement worker root"),
    };
    let session_id = std::env::var(REPLACEMENT_WORKER_SESSION_ENV)
        .context("missing S3 replacement worker session")?;
    let barrier = std::env::var(REPLACEMENT_WORKER_BARRIER_ENV)
        .context("missing S3 replacement worker barrier")?;
    let mut stream = std::net::TcpStream::connect(&barrier)
        .with_context(|| format!("connect replacement barrier {barrier}"))?;
    stream.write_all(&[1])?;
    let mut release = [0_u8; 1];
    stream.read_exact(&mut release)?;
    anyhow::ensure!(release == [1], "invalid replacement barrier release");

    let store = StorylineLanceStore::open_uri(&root).await?;
    let storyline = fixture_storyline_with_id(&session_id)?;
    let outcome = match store.replace_storyline(&storyline).await {
        Ok(()) => "success",
        Err(error) if error.to_string().contains("commit conflict") => "conflict",
        Err(error) => return Err(error).context("unrecognized S3 replacement failure"),
    };
    write_replacement_outcome(&root, &session_id, outcome).await
}

#[tokio::test]
#[ignore = "requires PCHRONICLE_S3_TEST_URI and writable S3 credentials"]
async fn s3_storyline_multiprocess_replacement_contract() -> Result<()> {
    let contract_root = unique_root()?;
    let storyline_root = format!("{contract_root}/storyline-replacement");
    let contract_result = async {
        let baseline = fixture_storyline_with_id("multiprocess-baseline")?;
        let seed = StorylineLanceStore::open_uri(&storyline_root).await?;
        seed.replace_storyline(&baseline).await?;

        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let barrier_address = listener.local_addr()?.to_string();
        let executable = std::env::current_exe()?;
        let sessions = ["multiprocess-left", "multiprocess-right"];
        let mut children = sessions
            .iter()
            .map(|session_id| {
                Command::new(&executable)
                    .args([
                        "--exact",
                        "s3_storyline_replacement_worker",
                        "--ignored",
                        "--nocapture",
                    ])
                    .env(REPLACEMENT_WORKER_ROOT_ENV, &storyline_root)
                    .env(REPLACEMENT_WORKER_SESSION_ENV, session_id)
                    .env(REPLACEMENT_WORKER_BARRIER_ENV, &barrier_address)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .with_context(|| format!("spawn replacement worker {session_id}"))
            })
            .collect::<Result<Vec<_>>>()?;

        let mut waiters = Vec::with_capacity(sessions.len());
        for _ in &sessions {
            let (mut stream, _) = listener.accept()?;
            let mut ready = [0_u8; 1];
            stream.read_exact(&mut ready)?;
            anyhow::ensure!(ready == [1], "invalid replacement worker readiness");
            waiters.push(stream);
        }
        for stream in &mut waiters {
            stream.write_all(&[1])?;
        }
        drop(waiters);

        for (session_id, child) in sessions.iter().zip(children.drain(..)) {
            let output = child.wait_with_output()?;
            anyhow::ensure!(
                output.status.success(),
                "replacement worker {session_id} failed\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }

        let mut successful_sessions = Vec::new();
        for session_id in sessions {
            match read_replacement_outcome(&storyline_root, session_id)
                .await?
                .as_str()
            {
                "success" => successful_sessions.push(session_id.to_string()),
                "conflict" => {}
                outcome => anyhow::bail!(
                    "replacement worker {session_id} reported unrecognized outcome {outcome}"
                ),
            }
        }
        anyhow::ensure!(
            !successful_sessions.is_empty(),
            "all S3 replacement workers conflicted"
        );

        let reopened = StorylineLanceStore::open_uri(&storyline_root).await?;
        anyhow::ensure!(
            reopened
                .get_storyline_full(&baseline.session_id)
                .await?
                .as_ref()
                == Some(&baseline),
            "baseline Storyline was lost"
        );
        for session_id in successful_sessions {
            anyhow::ensure!(
                reopened.get_storyline_full(&session_id).await?.is_some(),
                "successful replacement {session_id} is absent from CURRENT"
            );
        }
        Ok::<_, anyhow::Error>(())
    }
    .await;
    let cleanup_result = cleanup(&contract_root).await;
    match (contract_result, cleanup_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => {
            Err(error).context("S3 replacement contract passed but cleanup failed")
        }
        (Err(error), Err(cleanup_error)) => Err(error).context(format!(
            "S3 replacement contract cleanup also failed: {cleanup_error:#}"
        )),
    }
}
