//! Exercise the actual exec boundary, not a router with a test-only worker.
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

#[tokio::test]
async fn catalog_server_authenticates_each_request_and_isolates_grants() -> Result<()> {
    let home = tempfile::tempdir()?;
    for name in ["left", "right"] {
        std::fs::create_dir(home.path().join(name))?;
        let mut trajectory: Value = serde_json::from_str(include_str!(
            "../../../examples/data/atif/support-ticket.json"
        ))?;
        trajectory["session_id"] = json!(format!("{name}-session"));
        std::fs::write(
            home.path().join(name).join("trajectory.atif.json"),
            serde_json::to_vec(&trajectory)?,
        )?;
    }
    let config = home.path().join("catalog.toml");
    std::fs::write(
        &config,
        format!(
            r#"
[datasets.left]
uri = "{}"
[datasets.right]
uri = "{}"
[users.alice]
access_key = "alice-ak"
secret_key = "alice-sk"
[users.bob]
access_key = "bob-ak"
secret_key = "bob-sk"
[[grants]]
user = "alice"
dataset = "left"
permissions = ["read"]
[[grants]]
user = "bob"
dataset = "right"
permissions = ["read"]
"#,
            home.path().join("left").display(),
            home.path().join("right").display()
        ),
    )?;
    let cache = home.path().join("cache");
    let server_log = home.path().join("server.stderr");
    let mut server = tokio::process::Command::new(env!("CARGO_BIN_EXE_pchronicle"))
        .args(["serve", "--catalog-config"])
        .arg(&config)
        .args(["--listen", "127.0.0.1:0"])
        .env("PCHRONICLE_CACHE_DIR", &cache)
        .env("AWS_ACCESS_KEY_ID", "ambient-must-not-be-used")
        .env("AWS_SECRET_ACCESS_KEY", "ambient-secret")
        .stdout(Stdio::piped())
        .stderr(std::fs::File::create(&server_log)?)
        .kill_on_drop(true)
        .spawn()?;
    let mut stdout = BufReader::new(server.stdout.take().context("server stdout")?);
    let mut line = String::new();
    tokio::time::timeout(Duration::from_secs(60), stdout.read_line(&mut line))
        .await
        .context("wait for catalog listener readiness")??;
    let ready: Value = serde_json::from_str(&line).with_context(|| {
        format!(
            "server readiness; stderr: {}",
            std::fs::read_to_string(&server_log).unwrap_or_default()
        )
    })?;
    let endpoint = ready["warehouse_endpoint"]
        .as_str()
        .context("warehouse endpoint")?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(70))
        .build()?;
    let url = format!("http://{endpoint}/api/explorer/tree");
    assert_anonymous_access_is_scoped(&client, endpoint).await?;
    // Same keep-alive client alternates identities; identity belongs to the
    // request, never to the TCP connection or the previous worker.
    for (user, dataset) in [("alice", "left"), ("bob", "right"), ("alice", "left")] {
        let response = client
            .get(&url)
            .header("x-pchronicle-access-key", format!("{user}-ak"))
            .header("x-pchronicle-secret-key", format!("{user}-sk"))
            .send()
            .await?;
        assert_eq!(response.status(), 200);
        let timings = response
            .headers()
            .get_all("server-timing")
            .iter()
            .map(|v| v.to_str().unwrap_or(""))
            .collect::<Vec<_>>()
            .join(",");
        assert!(timings.contains("total;"), "{timings}");
        let body: Value = response.json().await?;
        assert_eq!(body["children"].as_array().map(Vec::len), Some(1));
        assert_eq!(body["children"][0]["name"], dataset);
        let query = client
            .post(format!("http://{endpoint}/api/query/evidence"))
            .header("x-pchronicle-access-key", format!("{user}-ak"))
            .header("x-pchronicle-secret-key", format!("{user}-sk"))
            .json(&json!({"sql":"SELECT session_id FROM runs", "max_rows":10, "max_bytes":4096}))
            .send()
            .await?;
        assert_eq!(query.status(), 200);
        let timings = query
            .headers()
            .get_all("server-timing")
            .iter()
            .map(|v| v.to_str().unwrap_or(""))
            .collect::<Vec<_>>()
            .join(",");
        assert!(timings.contains("parent_total"), "{timings}");
        let query: Value = query.json().await?;
        assert_eq!(
            query["rows"],
            json!([{"session_id":format!("{dataset}-session")}]),
            "{query}"
        );
    }
    // Concurrent requests in one scope may now use different exec workers.
    // Each process must retain the same grants and storage identity.
    let mut concurrent = tokio::task::JoinSet::new();
    for user in ["alice", "bob", "alice", "bob"] {
        let client = client.clone();
        let endpoint = endpoint.to_owned();
        concurrent.spawn(async move {
            let response = client
                .post(format!("http://{endpoint}/api/query/evidence"))
                .header("x-pchronicle-access-key", format!("{user}-ak"))
                .header("x-pchronicle-secret-key", format!("{user}-sk"))
                .json(
                    &json!({"sql":"SELECT session_id FROM runs", "max_rows":10, "max_bytes":4096}),
                )
                .send()
                .await?;
            assert_eq!(response.status(), 200);
            let body: Value = response.json().await?;
            let expected = if user == "alice" {
                "left-session"
            } else {
                "right-session"
            };
            assert_eq!(body["rows"], json!([{"session_id":expected}]));
            Ok::<_, anyhow::Error>(())
        });
    }
    while let Some(result) = concurrent.join_next().await {
        result??;
    }
    let response = client
        .get(format!("{url}?dataset=right"))
        .header("x-pchronicle-access-key", "alice-ak")
        .header("x-pchronicle-secret-key", "alice-sk")
        .send()
        .await?;
    assert_eq!(response.status(), 404);
    assert_anonymous_access_is_scoped(&client, endpoint).await?;
    assert_eq!(std::fs::read_dir(cache.join("workers"))?.count(), 2);
    server.kill().await?;
    Ok(())
}

async fn assert_anonymous_access_is_scoped(client: &reqwest::Client, endpoint: &str) -> Result<()> {
    let tree = format!("http://{endpoint}/api/explorer/tree");
    let response = client.get(&tree).send().await?;
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await?;
    // This catalog has no wildcard grants: private mounts must stay hidden,
    // including after this keep-alive client has used an authenticated worker.
    assert_eq!(body["children"], json!([]), "{body}");
    for dataset in ["left", "right"] {
        assert_eq!(
            client
                .get(&tree)
                .query(&[("dataset", dataset)])
                .send()
                .await?
                .status(),
            401
        );
    }
    assert_eq!(
        client
            .get(&tree)
            .header("x-pchronicle-access-key", "alice-ak")
            .header("x-pchronicle-secret-key", "wrong-secret")
            .send()
            .await?
            .status(),
        401
    );
    assert_eq!(
        client
            .post(format!("http://{endpoint}/api/query/evidence"))
            .json(&json!({"sql":"SELECT session_id FROM runs", "max_rows":10, "max_bytes":4096}))
            .send()
            .await?
            .status(),
        401
    );
    Ok(())
}

async fn frame(input: &mut tokio::process::ChildStdin, value: Value) -> Result<()> {
    let bytes = serde_json::to_vec(&value)?;
    input.write_u32(bytes.len() as u32).await?;
    input.write_all(&bytes).await?;
    input.flush().await?;
    Ok(())
}

async fn read_frame(output: &mut tokio::process::ChildStdout) -> Result<Value> {
    let size = tokio::time::timeout(Duration::from_secs(30), output.read_u32()).await?? as usize;
    anyhow::ensure!(size < 1024 * 1024, "unexpected test response size");
    let mut bytes = vec![0; size];
    output.read_exact(&mut bytes).await?;
    Ok(serde_json::from_slice(&bytes)?)
}

#[tokio::test]
async fn exec_worker_handles_multiple_frames_and_exits_on_eof() -> Result<()> {
    let home = tempfile::tempdir()?;
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_pchronicle"))
        .args(["serve", "--catalog-query-worker"])
        .env_clear()
        .env("HOME", home.path())
        .env("PCHRONICLE_CACHE_DIR", home.path().join("cache"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let pid = child.id();
    let mut input = child.stdin.take().context("worker stdin")?;
    let mut output = child.stdout.take().context("worker stdout")?;
    frame(
        &mut input,
        json!({"mounts":[{"name":"local", "uri":home.path().join("dataset")}]}),
    )
    .await?;
    assert_eq!(read_frame(&mut output).await?, true);
    for _ in 0..2 {
        frame(
            &mut input,
            json!({"method":"GET", "uri":"/api/health", "headers":[], "body":[]}),
        )
        .await?;
        loop {
            let event = read_frame(&mut output).await?;
            match event["type"].as_str() {
                Some("Progress") => continue,
                Some("Response") => {
                    assert_eq!(event["value"]["status"], 200);
                    break;
                }
                _ => anyhow::bail!("unexpected worker frame: {event}"),
            }
        }
        assert_eq!(child.id(), pid);
        assert!(child.try_wait()?.is_none());
    }
    drop(input);
    assert!(
        tokio::time::timeout(Duration::from_secs(10), child.wait())
            .await??
            .success()
    );
    Ok(())
}
