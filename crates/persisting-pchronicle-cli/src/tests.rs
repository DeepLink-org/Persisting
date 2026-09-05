use super::*;

#[tokio::test]
async fn trajectory_append_uses_persisting_events_protocol_directly() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let request = persisting_events::TrajectoryAppendRequest {
        storage: temporary.path().to_string_lossy().into_owned(),
        agent_id: "agent".into(),
        session_id: "session".into(),
        format: Default::default(),
        root_session_id: None,
        records: vec![persisting_events::EventRecord {
            identity: persisting_events::EventIdentity::default(),
            seq: 1,
            source: "test".into(),
            kind: "note".into(),
            timestamp: None,
            session_id: Some("session".into()),
            agent_id: Some("agent".into()),
            parent_uuid: None,
            trace_id: None,
            call_id: None,
            subagent_id: None,
            parent_agent_id: None,
            branch: None,
            parent_call_id: None,
            payload: serde_json::json!({"content": "hello"}),
        }],
    };
    let response: persisting_events::TrajectoryAppendResponse =
        crate::control::append_trajectory(request).await?;
    assert_eq!(response.accepted_records, 1);
    assert_eq!(response.status, "ok");
    Ok(())
}

#[tokio::test]
async fn trajectory_append_can_persist_json_in_the_warehouse() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let request = persisting_events::TrajectoryAppendRequest {
        storage: temporary.path().to_string_lossy().into_owned(),
        agent_id: "agent".into(),
        session_id: "json-session".into(),
        format: persisting_events::TrajectoryFormat::Json,
        root_session_id: None,
        records: vec![persisting_events::EventRecord {
            identity: persisting_events::EventIdentity::default(),
            seq: 7,
            source: "gateway".into(),
            kind: "llm.request".into(),
            timestamp: None,
            session_id: Some("json-session".into()),
            agent_id: Some("agent".into()),
            parent_uuid: None,
            trace_id: None,
            call_id: None,
            subagent_id: None,
            parent_agent_id: None,
            branch: None,
            parent_call_id: None,
            payload: serde_json::json!({"http": {"request_body": {"raw": true}}}),
        }],
    };
    let response = crate::control::append_trajectory(request).await?;
    assert_eq!(response.status, "ok");
    let path = temporary
        .path()
        .join("json/agent/json-session/events.jsonl");
    let contents = std::fs::read_to_string(path)?;
    assert!(contents.contains("\"request_body\""));
    Ok(())
}
use clap::CommandFactory;
use serde_json::Value;
use std::ffi::{OsStr, OsString};
use std::fs;

static STATUS_REPORT_TRACING_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
static DATASET_ALIAS_ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct EnvGuard {
    key: &'static str,
    previous: Option<OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: impl AsRef<OsStr>) -> Self {
        let previous = std::env::var_os(key);
        unsafe { std::env::set_var(key, value) };
        Self { key, previous }
    }

    fn unset(key: &'static str) -> Self {
        let previous = std::env::var_os(key);
        unsafe { std::env::remove_var(key) };
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => unsafe { std::env::set_var(self.key, value) },
            None => unsafe { std::env::remove_var(self.key) },
        }
    }
}

fn atif_fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../persisting-pchronicle/tests/fixtures/atif/dialogue_10.json")
}

fn example_dataset(format: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/data")
        .join(format)
}

fn example_source(format: &str) -> PathBuf {
    let filename = match format {
        "atif" => "support-ticket.json",
        "openai-messages" => "training.json",
        "actf" => "code-repair.actf.json",
        other => panic!("unknown example format: {other}"),
    };
    example_dataset(format).join(filename)
}

fn atif_identity_document(document_id: &str, session_id: &str) -> Value {
    serde_json::json!({
        "schema_version": "ATIF-v1.7",
        "trajectory_id": document_id,
        "session_id": session_id,
        "agent": {"name": "agent", "version": "1"},
        "steps": []
    })
}

fn actf_with_unknown_step_fields(task_id: &str, users: &[&str]) -> Value {
    let steps = users
        .iter()
        .enumerate()
        .map(|(index, user)| {
            let step_id = i64::try_from(index).unwrap() + 1;
            serde_json::json!({
                "step_id": step_id,
                "assistant_content": {
                    "content": format!("reply-{step_id}"),
                    "reasoning_content": "",
                    "tool_calls": []
                },
                "metric": {
                    "prompt_tokens_len": 1,
                    "completion_tokens_len": 1,
                    "llm_infer_ms": 1.0,
                    "env_action_ms": null,
                    "stop_reason": "stop"
                },
                "system_prompt": "system",
                "user_content": user,
                "vendor_step": user,
                "tools": [],
                "observation": [],
                "started_at": "2026-08-01 10:00:00+00:00",
                "finished_at": "2026-08-01 10:00:01+00:00"
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "task_id": task_id,
        "category": "software-engineering",
        "k": 1,
        "correct": false,
        "attempts_tried": 1,
        "solved_at": null,
        "attempts": {
            "1": {
                "correct": false,
                "final_answer": null,
                "ground_truth": "expected",
                "trajectory": {
                    "schema_version": "ACTF_v1.0",
                    "steps": steps,
                    "started_at": "2026-08-01 10:00:00+00:00",
                    "finished_at": "2026-08-01 10:00:01+00:00"
                },
                "status": "completed",
                "score": null,
                "error": "",
                "artifacts": null,
                "extra": null,
                "analysis_result": null,
                "meta": null
            }
        }
    })
}

async fn append_canonical_note(storage: &std::path::Path) -> Result<()> {
    let coords = persisting_pchronicle::storage::StoryCoords::new(
        storage.to_string_lossy(),
        "agent",
        "session",
        None,
    );
    persisting_pchronicle::storage::RawEventLanceStore
        .append_events(
            &coords,
            &[persisting_pchronicle::model::EventRecord {
                identity: Default::default(),
                seq: 0,
                source: "test".into(),
                kind: "note".into(),
                timestamp: None,
                session_id: None,
                agent_id: None,
                parent_uuid: None,
                trace_id: None,
                call_id: None,
                subagent_id: None,
                parent_agent_id: None,
                branch: None,
                parent_call_id: None,
                payload: serde_json::json!({"content":"canonical"}),
            }],
        )
        .await?;
    Ok(())
}

fn canonical_source(storage: &std::path::Path) -> Result<PathBuf> {
    let coords = persisting_pchronicle::storage::StoryCoords::new(
        storage.to_string_lossy(),
        "agent",
        "session",
        None,
    );
    persisting_pchronicle::storage::raw_event_lance_path(&coords)
}

async fn append_canonical_run(storage: &Path, run_id: &str, seq: u64) -> Result<PathBuf> {
    let coords = persisting_pchronicle::storage::StoryCoords::new(
        storage.to_string_lossy(),
        "agent",
        run_id,
        Some(run_id.into()),
    );
    persisting_pchronicle::storage::RawEventLanceStore
        .append_events(
            &coords,
            &[persisting_pchronicle::model::EventRecord {
                identity: Default::default(),
                seq,
                source: "test".into(),
                kind: "note".into(),
                timestamp: None,
                session_id: Some(run_id.into()),
                agent_id: Some("agent".into()),
                parent_uuid: None,
                trace_id: None,
                call_id: None,
                subagent_id: None,
                parent_agent_id: None,
                branch: None,
                parent_call_id: None,
                payload: serde_json::json!({"content": format!("{run_id}-{seq}")}),
            }],
        )
        .await?;
    persisting_pchronicle::storage::raw_event_lance_path(&coords)
}

#[test]
fn command_tree_contains_the_product_commands() {
    let command = Cli::command();
    let names = command
        .get_subcommands()
        .map(|command| command.get_name())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        [
            "onboard", "default", "alias", "ls", "status", "query", "analysis", "agent", "find",
            "import", "drop", "export", "sync", "echo", "dev", "serve",
        ]
    );
    let ls = command
        .get_subcommands()
        .find(|command| command.get_name() == "ls")
        .unwrap();
    assert!(ls.get_all_aliases().any(|alias| alias == "list"));
    let import = command
        .get_subcommands()
        .find(|command| command.get_name() == "import")
        .unwrap();
    let output_format = import
        .get_arguments()
        .find(|argument| argument.get_id() == "output_format")
        .unwrap();
    assert!(
        output_format
            .get_help()
            .unwrap()
            .to_string()
            .contains("one squashed Storyline Lance Store at the Dataset root")
    );
    assert!(Cli::try_parse_from(["pchronicle", "project", "status"]).is_err());

    let serve = command
        .get_subcommands()
        .find(|command| command.get_name() == "serve")
        .unwrap();
    let catalog = serve
        .get_subcommands()
        .find(|command| command.get_name() == "catalog")
        .unwrap();
    let catalog_commands = catalog
        .get_subcommands()
        .map(|command| command.get_name())
        .collect::<Vec<_>>();
    assert_eq!(catalog_commands, ["issue", "grant", "revoke"]);
    let mut serve_command = Cli::command();
    let serve_help = serve_command.find_subcommand_mut("serve").unwrap();
    let mut help = Vec::new();
    serve_help.write_long_help(&mut help).unwrap();
    let help = String::from_utf8(help).unwrap();
    assert!(help.contains("pchronicle serve catalog"), "{help}");
}

#[test]
fn canonical_parser_surface_matches_the_cli_guide() -> Result<()> {
    let cli = Cli::try_parse_from([
        "pchronicle",
        "-c",
        "config.toml",
        "--log-level",
        "debug",
        "query",
        "./dataset",
        "--sql",
        "SELECT 1",
        "--timeout",
        "2m",
        "--max-output-bytes",
        "8MiB",
    ])?;
    assert_eq!(cli.config, Some(PathBuf::from("config.toml")));
    assert_eq!(cli.log_level, LogLevel::Debug);
    let Command::Query(query) = cli.command else {
        panic!("expected query command")
    };
    assert_eq!(query.dataset_uri.as_deref(), Some("./dataset"));
    assert_eq!(query.sql_option.as_deref(), Some("SELECT 1"));
    assert_eq!(query.timeout_seconds, 120);
    assert_eq!(query.max_output_bytes, 8 * 1024 * 1024);

    let cli = Cli::try_parse_from([
        "pchronicle",
        "query",
        "--mount",
        "live=./live",
        "--mount",
        "archive=@archive",
        "--file",
        "report.sql",
    ])?;
    let Command::Query(query) = cli.command else {
        panic!("expected query command")
    };
    assert_eq!(query.datasets.len(), 2);
    assert_eq!(query.file.as_deref(), Some("report.sql"));

    let cli = Cli::try_parse_from([
        "pchronicle",
        "import",
        "-f",
        "-",
        "-t",
        "./imported",
        "-i",
        "atif",
        "-o",
        "preserve",
    ])?;
    let Command::Import(import) = cli.command else {
        panic!("expected import command")
    };
    assert_eq!(import.output.as_deref(), Some("./imported"));
    assert_eq!(import.format, ExchangeFormat::Atif);
    assert_eq!(import.output_format, Some(ImportOutputFormat::Preserve));
    assert_eq!(import.mode, ImportMode::Create);
    assert_eq!(import.on_duplicate, None);
    assert!(!import.yes);

    let cli = Cli::try_parse_from([
        "pchronicle",
        "import",
        "-f",
        "input.json",
        "-t",
        "./imported",
        "--mode",
        "append",
        "--on-duplicate",
        "skip",
    ])?;
    let Command::Import(import) = cli.command else {
        panic!("expected import command")
    };
    assert_eq!(import.mode, ImportMode::Append);
    assert_eq!(import.on_duplicate, Some(DuplicateIdPolicy::Skip));

    let cli = Cli::try_parse_from(["pchronicle", "drop", "./imported", "--yes"])?;
    let Command::Drop(drop) = cli.command else {
        panic!("expected drop command")
    };
    assert_eq!(drop.dataset_uri, "./imported");
    assert!(drop.yes);

    assert!(Cli::try_parse_from(["pchronicle", "export", "-t", "-", "-o", "storyline"]).is_err());
    assert!(
        Cli::try_parse_from([
            "pchronicle",
            "serve",
            "--listen",
            "127.0.0.1:8080",
            "--gateway-config",
            "gateway.toml",
            "--gateway-dataset",
            "evals",
            "evals=./dataset"
        ])
        .is_ok()
    );
    Ok(())
}

#[tokio::test]
async fn alias_lifecycle_resolves_dataset_references_without_moving_data() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let config = temporary.path().join("config.toml");
    let config_arg = config.to_string_lossy().into_owned();
    let first = temporary.path().join("first");
    let second = temporary.path().join("second");
    fs::create_dir_all(&first)?;
    fs::create_dir_all(&second)?;
    let first_arg = first.to_string_lossy().into_owned();
    let second_arg = second.to_string_lossy().into_owned();

    for arguments in [
        vec!["-c", &config_arg, "alias", "add", "prod", &first_arg],
        vec!["-c", &config_arg, "alias", "add", "archive", &second_arg],
    ] {
        let cli =
            Cli::try_parse_from(std::iter::once("pchronicle").chain(arguments.iter().copied()))?;
        run(cli, false, &mut Vec::new(), &mut Vec::new()).await?;
    }

    assert_eq!(
        resolve_dataset_uri(Some("@prod"), Some(&config))?,
        fs::canonicalize(&first)?.to_string_lossy()
    );
    assert_eq!(
        resolve_dataset_uri(Some("@archive"), Some(&config))?,
        fs::canonicalize(&second)?.to_string_lossy()
    );

    let cli = Cli::try_parse_from([
        "pchronicle",
        "-c",
        &config_arg,
        "alias",
        "list",
        "--format",
        "json",
    ])?;
    let mut stdout = Vec::new();
    run(cli, false, &mut stdout, &mut Vec::new()).await?;
    let aliases: Value = serde_json::from_slice(&stdout)?;
    assert_eq!(aliases["schema_version"], "pchronicle-aliases/v1");
    let names = aliases["aliases"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|alias| alias["name"].as_str())
        .collect::<Vec<_>>();
    assert!(names.contains(&"@codex"));
    assert!(names.contains(&"@claude"));
    assert!(names.contains(&"@claude-code"));
    assert!(names.contains(&"archive"));
    assert!(names.contains(&"prod"));

    let cli = Cli::try_parse_from([
        "pchronicle",
        "-c",
        &config_arg,
        "alias",
        "rename",
        "prod",
        "production",
    ])?;
    run(cli, false, &mut Vec::new(), &mut Vec::new()).await?;
    assert!(resolve_dataset_uri(Some("@prod"), Some(&config)).is_err());
    assert!(resolve_dataset_uri(Some("@production/.."), Some(&config)).is_err());
    assert!(first.is_dir());
    Ok(())
}

#[tokio::test]
async fn alias_s3_credentials_are_stored_separately_and_applied_on_expansion() -> Result<()> {
    let _env_guard = DATASET_ALIAS_ENV_LOCK.lock().await;
    let temporary = tempfile::tempdir()?;
    let config = temporary.path().join("config.toml");
    let config_arg = config.to_string_lossy().into_owned();
    let cli = Cli::try_parse_from([
        "pchronicle",
        "-c",
        &config_arg,
        "alias",
        "add",
        "prod",
        "s3://example-bucket/evals",
        "--endpoint",
        "http://127.0.0.1:9000",
        "--region",
        "us-west-2",
        "--ak",
        "access-test",
        "--sk",
        "secret-test",
    ])?;
    run(cli, false, &mut Vec::new(), &mut Vec::new()).await?;

    let config_text = fs::read_to_string(&config)?;
    assert!(config_text.contains("[alias_credentials.prod]"));
    assert!(config_text.contains("alias_endpoints"));
    assert!(config_text.contains("prod = \"http://127.0.0.1:9000\""));
    assert!(config_text.contains("[alias_regions]"));
    assert!(config_text.contains("prod = \"us-west-2\""));
    assert!(config_text.contains("access_key = \"access-test\""));
    assert!(config_text.contains("secret_key = \"secret-test\""));

    let _access = EnvGuard::unset("AWS_ACCESS_KEY_ID");
    let _secret = EnvGuard::unset("AWS_SECRET_ACCESS_KEY");
    let _endpoint = EnvGuard::unset("AWS_ENDPOINT_URL_S3");
    let _generic_endpoint = EnvGuard::unset("AWS_ENDPOINT");
    let _allow_http = EnvGuard::unset("AWS_ALLOW_HTTP");
    let _region = EnvGuard::unset("AWS_REGION");
    assert_eq!(
        expand_dataset_reference("@prod", Some(&config), false)?,
        "s3://example-bucket/evals"
    );
    assert_eq!(
        std::env::var("AWS_ACCESS_KEY_ID").as_deref(),
        Ok("access-test")
    );
    assert_eq!(
        std::env::var("AWS_SECRET_ACCESS_KEY").as_deref(),
        Ok("secret-test")
    );
    assert_eq!(
        std::env::var("AWS_ENDPOINT_URL_S3").as_deref(),
        Ok("http://127.0.0.1:9000")
    );
    assert_eq!(
        std::env::var("AWS_ENDPOINT").as_deref(),
        Ok("http://127.0.0.1:9000")
    );
    assert_eq!(std::env::var("AWS_ALLOW_HTTP").as_deref(), Ok("true"));
    assert_eq!(std::env::var("AWS_REGION").as_deref(), Ok("us-west-2"));

    let cli = Cli::try_parse_from([
        "pchronicle",
        "-c",
        &config_arg,
        "alias",
        "list",
        "--format",
        "json",
    ])?;
    let mut stdout = Vec::new();
    run(cli, false, &mut stdout, &mut Vec::new()).await?;
    let output = String::from_utf8(stdout)?;
    assert!(!output.contains("access-test"));
    assert!(!output.contains("secret-test"));
    Ok(())
}

#[tokio::test]
async fn catalog_alias_stores_user_keys_and_rejects_endpoint() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let config = temporary.path().join("config.toml");
    let config_arg = config.to_string_lossy().into_owned();

    let missing_keys = Cli::try_parse_from([
        "pchronicle",
        "-c",
        &config_arg,
        "alias",
        "add",
        "team",
        "catalog://127.0.0.1:8081",
    ])?;
    let error = run(missing_keys, false, &mut Vec::new(), &mut Vec::new())
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("require --ak and --sk"), "{error}");

    let with_endpoint = Cli::try_parse_from([
        "pchronicle",
        "-c",
        &config_arg,
        "alias",
        "add",
        "team",
        "catalog://127.0.0.1:8081",
        "--endpoint",
        "http://127.0.0.1:9000",
        "--ak",
        "USER_AK",
        "--sk",
        "USER_SK",
    ])?;
    let error = run(with_endpoint, false, &mut Vec::new(), &mut Vec::new())
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("do not accept --endpoint"), "{error}");

    let cli = Cli::try_parse_from([
        "pchronicle",
        "-c",
        &config_arg,
        "alias",
        "add",
        "team",
        "catalog://127.0.0.1:8081",
        "--ak",
        "USER_AK",
        "--sk",
        "USER_SK",
    ])?;
    run(cli, false, &mut Vec::new(), &mut Vec::new()).await?;
    let config_text = fs::read_to_string(&config)?;
    assert!(config_text.contains("team = \"catalog://127.0.0.1:8081\""));
    assert!(config_text.contains("access_key = \"USER_AK\""));
    assert!(config_text.contains("secret_key = \"USER_SK\""));
    assert!(!config_text.contains("alias_endpoints"));
    assert!(!config_text.contains("BACKEND"));

    let error = expand_dataset_reference("@team", Some(&config), false)
        .unwrap_err()
        .to_string();
    assert!(error.contains("requires a dataset"), "{error}");
    Ok(())
}

#[tokio::test]
async fn serve_catalog_issue_grant_revoke_rewrites_config() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let catalog = temporary.path().join("catalog.toml");
    fs::write(
        &catalog,
        r#"
[libraries.prod]
uri = "s3://bucket/prod"
access_key = "BACKEND_AK"
secret_key = "BACKEND_SK"
"#,
    )?;
    let catalog_arg = catalog.to_string_lossy().into_owned();

    assert!(
        Cli::try_parse_from([
            "pchronicle",
            "serve",
            "catalog",
            "issue",
            "--catalog-config",
            &catalog_arg,
            "alice",
        ])
        .is_ok()
    );
    assert!(
        Cli::try_parse_from([
            "pchronicle",
            "serve",
            "--listen",
            "127.0.0.1:0",
            "catalog",
            "issue",
            "--catalog-config",
            &catalog_arg,
            "alice",
        ])
        .is_err()
    );

    let issue = Cli::try_parse_from([
        "pchronicle",
        "serve",
        "catalog",
        "issue",
        "--catalog-config",
        &catalog_arg,
        "alice",
        "--format",
        "json",
    ])?;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    run(issue, false, &mut stdout, &mut stderr).await?;
    let issued: Value = serde_json::from_slice(&stdout)?;
    let secret_key = issued["secret_key"].as_str().expect("secret_key");
    assert!(
        issued["access_key"]
            .as_str()
            .expect("access_key")
            .starts_with("pcak_")
    );
    let stderr_text = String::from_utf8(stderr)?;
    assert!(stderr_text.contains("updated=true"), "{stderr_text}");
    assert!(!stderr_text.contains(secret_key), "{stderr_text}");

    let grant = Cli::try_parse_from([
        "pchronicle",
        "serve",
        "catalog",
        "grant",
        "--catalog-config",
        &catalog_arg,
        "alice",
        "prod",
        "--format",
        "json",
    ])?;
    let mut stdout = Vec::new();
    run(grant, false, &mut stdout, &mut Vec::new()).await?;
    let granted: Value = serde_json::from_slice(&stdout)?;
    assert_eq!(granted["datasets"], serde_json::json!(["prod"]));

    let revoke = Cli::try_parse_from([
        "pchronicle",
        "serve",
        "catalog",
        "revoke",
        "--catalog-config",
        &catalog_arg,
        "alice",
        "prod",
        "--format",
        "json",
    ])?;
    let mut stdout = Vec::new();
    run(revoke, false, &mut stdout, &mut Vec::new()).await?;
    let revoked: Value = serde_json::from_slice(&stdout)?;
    assert_eq!(revoked["datasets"], serde_json::json!([]));
    Ok(())
}

#[test]
fn alias_rejects_markdown_endpoint_links() {
    let error = super::s3_endpoint_for(
        "s3://example-bucket/evals",
        Some("[http://127.0.0.1:9000](http://127.0.0.1:9000)".to_owned()),
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("plain URL"), "{error}");
}

#[tokio::test]
async fn log_level_changes_diagnostics_without_changing_results() -> Result<()> {
    let dataset = atif_fixture().to_string_lossy().into_owned();
    let info = Cli::try_parse_from([
        "pchronicle",
        "status",
        &dataset,
        "--format",
        "json",
        "--log-level",
        "info",
    ])?;
    let error = Cli::try_parse_from([
        "pchronicle",
        "status",
        &dataset,
        "--format",
        "json",
        "--log-level",
        "error",
    ])?;
    let mut info_stdout = Vec::new();
    let mut info_stderr = Vec::new();
    run(info, false, &mut info_stdout, &mut info_stderr).await?;
    let mut error_stdout = Vec::new();
    let mut error_stderr = Vec::new();
    run(error, false, &mut error_stdout, &mut error_stderr).await?;
    let mut info_result: Value = serde_json::from_slice(&info_stdout)?;
    let mut error_result: Value = serde_json::from_slice(&error_stdout)?;
    info_result.as_object_mut().unwrap().remove("created_at");
    error_result.as_object_mut().unwrap().remove("created_at");
    assert_eq!(info_result, error_result);
    assert!(!info_stderr.is_empty());
    assert!(error_stderr.is_empty());
    Ok(())
}

#[tokio::test]
async fn list_discovers_nested_sources_as_json() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::create_dir(temp.path().join("nested"))?;
    fs::write(
        temp.path().join("nested/trajectory.json"),
        r#"[{"session_id":"s1","step_id":0,"messages":[]}]"#,
    )?;
    fs::write(
        temp.path().join("trajectory.jsonl"),
        r#"{"schema_version":"ATIF-v1.4","session_id":"s2","steps":[],"agent":{"id":"a"}}"#,
    )?;
    let cli = Cli::try_parse_from([
        "pchronicle",
        "ls",
        temp.path().to_str().unwrap(),
        "--format",
        "json",
    ])?;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    run(cli, false, &mut stdout, &mut stderr).await?;

    let value: Value = serde_json::from_slice(&stdout)?;
    assert!(value.get("schema_version").is_none());
    assert_eq!(value["sources"].as_array().unwrap().len(), 2);
    assert_eq!(value["sources"][0]["source_path"], "nested/trajectory.json");
    assert_eq!(value["sources"][1]["source_path"], "trajectory.jsonl");
    assert!(String::from_utf8(stderr)?.contains("snapshot_id="));
    Ok(())
}

#[tokio::test]
async fn list_alias_and_table_output_work() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::write(temp.path().join("trajectory.json"), "[]")?;
    let cli = Cli::try_parse_from([
        "pchronicle",
        "list",
        temp.path().to_str().unwrap(),
        "--format",
        "table",
        "--physical",
    ])?;
    let mut stdout = Vec::new();
    run(cli, true, &mut stdout, &mut Vec::new()).await?;
    let output = String::from_utf8(stdout)?;
    assert!(output.contains("SOURCE"));
    assert!(output.contains("LAST MODIFIED"));
    assert!(output.contains("trajectory.json"));
    Ok(())
}

#[test]
fn list_source_status_does_not_serialize_catalog_diagnostics() -> Result<()> {
    let source = persisting_pchronicle::storage::DiscoveredSource {
        file: "broken.json".into(),
        format: None,
        kind: CatalogSourceKind::File,
        revision: None,
        projection_status: None,
        projection_generation: None,
        projection_candidates: 0,
        size_bytes: None,
        last_modified: None,
        status: CatalogSourceStatus::Error,
        error: Some("list-secret-sentinel /private/list/path".into()),
    };

    let output = serde_json::to_string(&source_response(&source))?;
    assert!(output.contains("Source discovery failed"), "{output}");
    assert!(!output.contains("list-secret-sentinel"), "{output}");
    assert!(!output.contains("/private/list/path"), "{output}");
    Ok(())
}

#[tokio::test]
async fn status_reports_exact_counts_as_json() -> Result<()> {
    let cli = Cli::try_parse_from([
        "pchronicle",
        "status",
        atif_fixture().to_str().unwrap(),
        "--format",
        "json",
    ])?;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    run(cli, false, &mut stdout, &mut stderr).await?;

    let value: Value = serde_json::from_slice(&stdout)?;
    assert!(value.get("schema_version").is_none());
    assert_eq!(value["status"], "ready");
    assert_eq!(value["counts_complete"], true);
    assert_eq!(value["sources"]["total"], 1);
    assert_eq!(value["sources"]["ready"], 1);
    assert_eq!(value["sources"]["error"], 0);
    assert_eq!(value["counts"]["runs"], 1);
    assert_eq!(value["counts"]["trajectories"], 1);
    assert_eq!(value["counts"]["steps"], 10);
    assert_eq!(value["counts"]["tool_calls"], 0);
    assert_eq!(value["counts"]["events"], 0);
    assert!(value["source_errors"].as_array().unwrap().is_empty());
    assert!(String::from_utf8(stderr)?.contains("counts_complete=true"));
    Ok(())
}

#[tokio::test]
async fn status_and_analysis_use_bounded_canonical_fallback() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let storage = temp.path().join("capture");
    append_canonical_note(&storage).await?;

    let status = Cli::try_parse_from([
        "pchronicle",
        "status",
        storage.to_str().unwrap(),
        "--format",
        "json",
        "--errors",
        "strict",
    ])?;
    let mut stdout = Vec::new();
    run(status, false, &mut stdout, &mut Vec::new()).await?;
    let status: Value = serde_json::from_slice(&stdout)?;
    assert_eq!(status["counts_complete"], true);
    assert_eq!(status["counts"]["trajectories"], 1);
    assert_eq!(status["counts"]["steps"], 1);
    assert_eq!(status["counts"]["events"], 1);

    let analysis = Cli::try_parse_from([
        "pchronicle",
        "analysis",
        "overview",
        storage.to_str().unwrap(),
        "--format",
        "jsonl",
    ])?;
    let mut stdout = Vec::new();
    run(analysis, false, &mut stdout, &mut Vec::new()).await?;
    let overview: Value = serde_json::from_slice(&stdout)?;
    assert_eq!(overview["trajectories"], 1);
    assert_eq!(overview["steps"], 1);
    Ok(())
}

#[tokio::test]
async fn status_reports_projection_fresh_and_missing_in_source_order() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let storage = temp.path().join("capture");
    let source_b = append_canonical_run(&storage, "b", 0).await?;
    let source_a = append_canonical_run(&storage, "a", 0).await?;
    let projection_a = storage.join("agent/a/storyline");
    persisting_pchronicle::storage::build_storyline_projection(
        source_a.to_string_lossy(),
        projection_a.to_string_lossy(),
        "a/events.lance",
    )
    .await?;
    let projection_b = storage.join("agent/b/storyline");
    assert!(!projection_b.exists());

    let cli = Cli::try_parse_from([
        "pchronicle",
        "status",
        storage.join("agent").to_str().unwrap(),
        "--format",
        "json",
    ])?;
    let mut stdout = Vec::new();
    run(cli, false, &mut stdout, &mut Vec::new()).await?;
    let response: Value = serde_json::from_slice(&stdout)?;
    let projections = response["projections"].as_array().unwrap();
    assert_eq!(projections.len(), 2);
    assert_eq!(projections[0]["source_path"], "a/events.lance");
    assert_eq!(projections[0]["projection_path"], "a/storyline");
    assert_eq!(projections[0]["status"], "fresh");
    assert_eq!(projections[0]["fact_version"], 1);
    assert_eq!(projections[0]["fact_rows"], 1);
    assert!(projections[0]["generation"].is_string());
    assert_eq!(projections[1]["source_path"], "b/events.lance");
    assert_eq!(projections[1]["projection_path"], "b/storyline");
    assert_eq!(projections[1]["status"], "missing");
    assert_eq!(projections[1]["fact_version"], 1);
    assert_eq!(projections[1]["fact_rows"], 1);
    assert!(projections[1].get("generation").is_none());
    assert_eq!(source_b.file_name().unwrap(), "events.lance");
    assert!(!projection_b.exists());

    let cli = Cli::try_parse_from([
        "pchronicle",
        "status",
        storage.join("agent").to_str().unwrap(),
        "--format",
        "table",
    ])?;
    let mut table = Vec::new();
    run(cli, true, &mut table, &mut Vec::new()).await?;
    let table = String::from_utf8(table)?;
    assert!(table.contains("PROJECTION"));
    assert!(table.contains("a/events.lance -> a/storyline"));
    assert!(table.contains("b/events.lance -> b/storyline"));
    Ok(())
}

#[tokio::test]
async fn status_reports_projection_stale_and_safe_errors() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let storage = temp.path().join("capture");
    for run_id in ["stale", "lineage-free", "malformed"] {
        let source = append_canonical_run(&storage, run_id, 0).await?;
        persisting_pchronicle::storage::build_storyline_projection(
            source.to_string_lossy(),
            storage
                .join(format!("agent/{run_id}/storyline"))
                .to_string_lossy(),
            format!("{run_id}/events.lance"),
        )
        .await?;
    }
    append_canonical_run(&storage, "stale", 1).await?;

    let lineage_free = storage.join("agent/lineage-free/storyline/CURRENT");
    let mut pointer: Value = serde_json::from_slice(&fs::read(&lineage_free)?)?;
    let removed = pointer
        .get_mut("committed")
        .and_then(Value::as_object_mut)
        .expect("CURRENT committed snapshot")
        .remove("projection");
    assert!(removed.is_some(), "projection lineage fixture");
    fs::write(&lineage_free, serde_json::to_vec(&pointer)?)?;
    let malformed = storage.join("agent/malformed/storyline/CURRENT");
    fs::write(&malformed, b"{broken")?;
    let lineage_free_before = fs::read(&lineage_free)?;
    let malformed_before = fs::read(&malformed)?;

    let cli = Cli::try_parse_from([
        "pchronicle",
        "status",
        storage.join("agent").to_str().unwrap(),
        "--format",
        "json",
        "--errors",
        "report",
    ])?;
    let mut stdout = Vec::new();
    run(cli, false, &mut stdout, &mut Vec::new()).await?;
    let response: Value = serde_json::from_slice(&stdout)?;
    let projections = response["projections"].as_array().unwrap();
    assert_eq!(projections.len(), 3);
    assert_eq!(projections[0]["source_path"], "lineage-free/events.lance");
    assert_eq!(projections[0]["status"], "error");
    assert!(projections[0].get("generation").is_none());
    assert!(projections[0].get("fact_version").is_none());
    assert!(projections[0].get("fact_rows").is_none());
    assert_eq!(projections[1]["source_path"], "malformed/events.lance");
    assert_eq!(projections[1]["status"], "error");
    assert_eq!(projections[2]["source_path"], "stale/events.lance");
    assert_eq!(projections[2]["status"], "stale");
    assert_eq!(fs::read(&lineage_free)?, lineage_free_before);
    assert_eq!(fs::read(&malformed)?, malformed_before);
    Ok(())
}

#[tokio::test]
async fn status_reports_partial_counts_for_bad_sources() -> Result<()> {
    let _tracing_guard = STATUS_REPORT_TRACING_LOCK.lock().await;
    let temp = tempfile::tempdir()?;
    fs::copy(atif_fixture(), temp.path().join("valid.json"))?;
    fs::write(temp.path().join("broken.json"), "{not-json")?;
    let cli = Cli::try_parse_from([
        "pchronicle",
        "status",
        temp.path().to_str().unwrap(),
        "--format",
        "json",
        "--errors",
        "report",
    ])?;
    let mut stdout = Vec::new();
    run(cli, false, &mut stdout, &mut Vec::new()).await?;

    let value: Value = serde_json::from_slice(&stdout)?;
    assert_eq!(value["status"], "degraded");
    assert_eq!(value["counts_complete"], false);
    assert_eq!(value["sources"]["total"], 2);
    assert_eq!(value["sources"]["ready"], 1);
    assert_eq!(value["sources"]["error"], 1);
    assert_eq!(value["counts"]["runs"], 1);
    assert_eq!(value["counts"]["steps"], 10);
    assert_eq!(value["source_errors"][0]["source_path"], "broken.json");
    Ok(())
}

#[tokio::test]
async fn status_strict_mode_rejects_bad_sources() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::write(temp.path().join("broken.json"), "{not-json")?;
    let cli = Cli::try_parse_from([
        "pchronicle",
        "status",
        temp.path().to_str().unwrap(),
        "--errors",
        "strict",
    ])?;

    assert!(
        run(cli, false, &mut Vec::new(), &mut Vec::new())
            .await
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn status_report_mode_marks_an_unreadable_dataset_as_error() -> Result<()> {
    let _tracing_guard = STATUS_REPORT_TRACING_LOCK.lock().await;
    let temp = tempfile::tempdir()?;
    fs::write(
        temp.path().join("broken.json"),
        "{status-secret-sentinel:/private/status/path",
    )?;
    let cli = Cli::try_parse_from([
        "pchronicle",
        "status",
        temp.path().to_str().unwrap(),
        "--format",
        "json",
        "--errors",
        "report",
    ])?;
    let mut stdout = Vec::new();
    run(cli, false, &mut stdout, &mut Vec::new()).await?;

    let value: Value = serde_json::from_slice(&stdout)?;
    assert_eq!(value["status"], "error");
    assert_eq!(value["sources"]["ready"], 0);
    assert_eq!(value["sources"]["error"], 1);
    let error = value["source_errors"][0]["error"].as_str().unwrap();
    assert_eq!(error, "Source status query failed");
    let output = String::from_utf8(stdout)?;
    assert!(!output.contains("status-secret-sentinel"), "{output}");
    assert!(!output.contains("/private/status/path"), "{output}");
    Ok(())
}

#[tokio::test]
async fn status_report_mode_logs_each_cached_source_failure_once() -> Result<()> {
    use std::fmt::Write as _;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tracing::field::{Field, Visit};
    use tracing::instrument::WithSubscriber as _;
    use tracing::{Event, Subscriber};
    use tracing_subscriber::Layer;
    use tracing_subscriber::layer::{Context as LayerContext, SubscriberExt};

    const SENTINEL: &str = "status-duplicate-log-sentinel";

    let _tracing_guard = STATUS_REPORT_TRACING_LOCK.lock().await;

    #[derive(Clone)]
    struct SentinelLayer {
        events: Arc<AtomicUsize>,
    }

    impl<S> Layer<S> for SentinelLayer
    where
        S: Subscriber,
    {
        fn on_event(&self, event: &Event<'_>, _context: LayerContext<'_, S>) {
            #[derive(Default)]
            struct Fields(String);

            impl Visit for Fields {
                fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
                    let _ = write!(self.0, "{}={value:?};", field.name());
                }
            }

            let mut fields = Fields::default();
            event.record(&mut fields);
            if fields.0.contains(SENTINEL) {
                self.events.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    let temp = tempfile::tempdir()?;
    fs::write(
        temp.path().join(format!("{SENTINEL}.json")),
        "{private-status-diagnostic:/private/status/path",
    )?;
    let cli = Cli::try_parse_from([
        "pchronicle",
        "status",
        temp.path().to_str().unwrap(),
        "--format",
        "json",
        "--errors",
        "report",
    ])?;
    let events = Arc::new(AtomicUsize::new(0));
    let subscriber = tracing_subscriber::registry().with(SentinelLayer {
        events: events.clone(),
    });
    let mut stdout = Vec::new();

    async {
        tracing::callsite::rebuild_interest_cache();
        run(cli, false, &mut stdout, &mut Vec::new()).await
    }
    .with_subscriber(subscriber)
    .await?;

    assert_eq!(events.load(Ordering::Relaxed), 1);
    let value: Value = serde_json::from_slice(&stdout)?;
    assert_eq!(
        value["source_errors"][0]["error"],
        "Source status query failed"
    );
    let public_error = value["source_errors"][0]["error"].as_str().unwrap();
    assert!(!public_error.contains("private-status-diagnostic"));
    assert!(!public_error.contains("/private/status/path"));
    Ok(())
}

#[test]
fn limited_buffer_labels_byte_exhaustion_without_swallowing_writer_errors() -> Result<()> {
    use persisting_pchronicle::query::QueryWriteOutcome;

    let mut buffer = LimitedBuffer::new(3);
    let write_error = buffer.write_all(b"four").unwrap_err();
    let outcome = buffer.finish(Err(anyhow::Error::new(write_error)))?;
    assert_eq!(outcome, QueryOutputBudgetOutcome::ByteLimitExceeded);

    let buffer = LimitedBuffer::new(3);
    let writer_error = anyhow::Error::new(std::io::Error::new(
        std::io::ErrorKind::BrokenPipe,
        "limited-writer-source-sentinel",
    ));
    let error = buffer
        .finish(Err(writer_error))
        .expect_err("ordinary writer errors must remain operational");
    let source = error
        .chain()
        .find_map(|source| source.downcast_ref::<std::io::Error>())
        .context("writer I/O source was not preserved")?;
    assert_eq!(source.kind(), std::io::ErrorKind::BrokenPipe);
    assert_eq!(source.to_string(), "limited-writer-source-sentinel");

    let buffer = LimitedBuffer::new(3);
    assert_eq!(
        buffer.finish(Ok(QueryWriteOutcome::LimitExceeded))?,
        QueryOutputBudgetOutcome::RowLimitExceeded
    );
    Ok(())
}

#[tokio::test]
async fn status_table_marks_counts_as_exact() -> Result<()> {
    let cli = Cli::try_parse_from([
        "pchronicle",
        "status",
        atif_fixture().to_str().unwrap(),
        "--format",
        "table",
    ])?;
    let mut stdout = Vec::new();
    run(cli, true, &mut stdout, &mut Vec::new()).await?;

    let output = String::from_utf8(stdout)?;
    assert!(output.contains("FIELD"));
    assert!(output.contains("ACCURACY"));
    assert!(output.contains("trajectories  1          exact"));
    assert!(output.contains("steps         10          exact"));
    Ok(())
}

#[tokio::test]
async fn status_rejects_zero_timeout() -> Result<()> {
    assert!(Cli::try_parse_from(["pchronicle", "status", ".", "--timeout", "0s"]).is_err());
    Ok(())
}

#[tokio::test]
async fn query_reads_all_example_dataset_formats_as_jsonl() -> Result<()> {
    for (format, expected_runs) in [("atif", 1), ("openai-messages", 2), ("actf", 1)] {
        let cli = Cli::try_parse_from([
            "pchronicle",
            "query",
            example_dataset(format).to_str().unwrap(),
            "SELECT COUNT(*) AS runs FROM dataset.runs",
            "--format",
            "jsonl",
        ])?;
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        run(cli, false, &mut stdout, &mut stderr).await?;

        let value: Value = serde_json::from_slice(&stdout)?;
        assert_eq!(value["runs"], expected_runs, "format={format}");
        assert!(String::from_utf8(stderr)?.contains("datasets=dataset"));
    }
    Ok(())
}

#[tokio::test]
async fn query_preserves_selected_column_order_in_table_and_csv() -> Result<()> {
    let dataset = example_dataset("atif");
    for format in ["table", "csv"] {
        let cli = Cli::try_parse_from([
            "pchronicle",
            "query",
            dataset.to_str().unwrap(),
            "SELECT session_id, step_id, source FROM dataset.steps ORDER BY step_id",
            "--format",
            format,
        ])?;
        let mut stdout = Vec::new();
        run(cli, format == "table", &mut stdout, &mut Vec::new()).await?;
        let output = String::from_utf8(stdout)?;
        let header = output.lines().next().unwrap();
        if format == "table" {
            assert_eq!(
                header.split_whitespace().collect::<Vec<_>>(),
                ["session_id", "step_id", "source"]
            );
        } else {
            assert_eq!(header, "session_id,step_id,source");
        }
        assert!(output.contains("support-001"));
    }
    Ok(())
}

#[tokio::test]
async fn query_supports_named_cross_dataset_sql() -> Result<()> {
    let atif = format!("atif={}", example_dataset("atif").display());
    let openai = format!("openai={}", example_dataset("openai-messages").display());
    let cli = Cli::try_parse_from([
        "pchronicle",
        "query",
        "--dataset",
        &atif,
        "--dataset",
        &openai,
        "SELECT (SELECT COUNT(*) FROM atif.runs) AS atif_runs, \
             (SELECT COUNT(*) FROM openai.runs) AS openai_runs",
        "--format",
        "jsonl",
    ])?;
    let mut stdout = Vec::new();
    run(cli, false, &mut stdout, &mut Vec::new()).await?;

    let value: Value = serde_json::from_slice(&stdout)?;
    assert_eq!(value["atif_runs"], 1);
    assert_eq!(value["openai_runs"], 2);
    Ok(())
}

#[tokio::test]
async fn query_writes_new_files_without_overwriting() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let output = temp.path().join("runs.csv");
    let dataset = example_dataset("actf");
    let args = [
        "pchronicle",
        "query",
        dataset.to_str().unwrap(),
        "SELECT session_id FROM dataset.runs",
        "--format",
        "csv",
        "--output",
        output.to_str().unwrap(),
    ];
    let cli = Cli::try_parse_from(args)?;
    let mut stdout = Vec::new();
    run(cli, false, &mut stdout, &mut Vec::new()).await?;
    assert!(stdout.is_empty());
    assert_eq!(
        fs::read_to_string(&output)?,
        "session_id\nexample-code-repair\n"
    );

    let cli = Cli::try_parse_from(args)?;
    let error = run(cli, false, &mut Vec::new(), &mut Vec::new())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("create query output file"));
    Ok(())
}

#[tokio::test]
async fn query_rejects_writes_and_bounded_output_without_partial_stdout() -> Result<()> {
    for (sql, limit_flag, limit, expected, boundary_code) in [
        (
            "DELETE FROM dataset.runs",
            "--max-output-rows",
            "100",
            "only accepts SELECT",
            None,
        ),
        (
            "SELECT * FROM dataset.steps",
            "--max-output-rows",
            "1",
            "max_output_rows",
            Some("resource_exhausted"),
        ),
        (
            "SELECT * FROM dataset.steps",
            "--max-output-bytes",
            "8",
            "max_output_bytes",
            Some("resource_exhausted"),
        ),
    ] {
        let cli = Cli::try_parse_from([
            "pchronicle",
            "query",
            example_dataset("atif").to_str().unwrap(),
            sql,
            limit_flag,
            limit,
        ])?;
        let mut stdout = Vec::new();
        let error = run(cli, false, &mut stdout, &mut Vec::new())
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains(expected), "{error:#}");
        if let Some(boundary_code) = boundary_code {
            assert!(error.to_string().starts_with(boundary_code), "{error:#}");
        }
        assert!(stdout.is_empty());
    }
    Ok(())
}

#[tokio::test]
async fn query_errors_preserve_the_operational_source_chain() -> Result<()> {
    let dataset = example_dataset("atif");
    let sql = "SELECT secret_column FROM dataset.runs";
    let cli = Cli::try_parse_from(["pchronicle", "query", dataset.to_str().unwrap(), sql])?;
    let error = run(cli, false, &mut Vec::new(), &mut Vec::new())
        .await
        .unwrap_err();
    assert!(
        error.chain().count() >= 2,
        "missing source chain: {error:#}"
    );
    Ok(())
}

#[tokio::test]
async fn query_rejects_malformed_named_dataset_mounts() -> Result<()> {
    let cli = Cli::try_parse_from([
        "pchronicle",
        "query",
        "--dataset",
        "missing-separator",
        "SELECT 1",
    ])?;
    let error = run(cli, false, &mut Vec::new(), &mut Vec::new())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("NAME=DATASET"));
    Ok(())
}

#[tokio::test]
async fn find_locates_runs_sessions_and_steps_in_example_datasets() -> Result<()> {
    for (format, flag, identity, expected_source) in [
        ("atif", "--session-id", "support-001", "support-ticket.json"),
        (
            "actf",
            "--run-id",
            "example-code-repair",
            "code-repair.actf.json",
        ),
        (
            "openai-messages",
            "--session-id",
            "training-002",
            "training.json",
        ),
    ] {
        let cli = Cli::try_parse_from([
            "pchronicle",
            "find",
            example_dataset(format).to_str().unwrap(),
            flag,
            identity,
            "--format",
            "json",
        ])?;
        let mut stdout = Vec::new();
        run(cli, false, &mut stdout, &mut Vec::new()).await?;

        let value: Value = serde_json::from_slice(&stdout)?;
        assert!(value.get("schema_version").is_none());
        assert_eq!(value["truncated"], false);
        assert_eq!(value["matches"][0]["source_path"], expected_source);
    }

    let cli = Cli::try_parse_from([
        "pchronicle",
        "find",
        example_dataset("atif").to_str().unwrap(),
        "--session-id",
        "support-001",
        "--step-id",
        "2",
        "--format",
        "json",
    ])?;
    let mut stdout = Vec::new();
    run(cli, false, &mut stdout, &mut Vec::new()).await?;
    let value: Value = serde_json::from_slice(&stdout)?;
    assert_eq!(value["matches"][0]["step_id"], 2);
    assert_eq!(value["matches"][0]["step_source"], "agent");
    assert_eq!(value["matches"][0]["effective_kind"], "autonomous");
    assert!(value["matches"][0]["preview"].as_str().is_some());
    Ok(())
}

#[test]
fn find_fts_predicates_are_combined_as_a_balanced_sql_tree() {
    let predicates = (0..16_384)
        .map(|index| format!("step_id = {index}"))
        .collect::<Vec<_>>();
    let sql = super::balanced_sql_group(&predicates, "OR");

    assert_eq!(sql.matches(" OR ").count(), predicates.len() - 1);
    let mut depth = 0usize;
    let mut max_depth = 0usize;
    for character in sql.chars() {
        match character {
            '(' => {
                depth += 1;
                max_depth = max_depth.max(depth);
            }
            ')' => depth -= 1,
            _ => {}
        }
    }
    assert_eq!(depth, 0);
    assert!(max_depth <= 16, "unexpectedly deep SQL tree: {max_depth}");
}

#[test]
fn fts_text_clause_is_false_only_after_a_successful_empty_search() {
    assert_eq!(
        super::compiled_text_predicate_sql(&[], true).expect("zero hits"),
        "FALSE"
    );
}

#[test]
fn fts_text_clause_is_not_false_when_search_never_ran() {
    let error = super::compiled_text_predicate_sql(&[], false)
        .expect_err("unsearched text must not compile to FALSE");
    let message = format!("{error:#}");
    assert!(
        message.contains("FTS unavailable"),
        "unexpected error: {message}"
    );
}

#[tokio::test]
async fn find_discovers_candidates_and_source_narrows_them() -> Result<()> {
    let temp = tempfile::tempdir()?;
    for file in ["first.json", "second.json"] {
        fs::write(
            temp.path().join(file),
            r#"[{"id":"event","session_id":"shared","step_id":1,"messages":[],"response":{"role":"assistant","content":"ok"}}]"#,
        )?;
    }

    let cli = Cli::try_parse_from([
        "pchronicle",
        "find",
        temp.path().to_str().unwrap(),
        "--session-id",
        "shared",
        "--format",
        "json",
    ])?;
    let mut stdout = Vec::new();
    run(cli, false, &mut stdout, &mut Vec::new()).await?;
    let value: Value = serde_json::from_slice(&stdout)?;
    assert_eq!(value["matches"].as_array().unwrap().len(), 2);

    let cli = Cli::try_parse_from([
        "pchronicle",
        "find",
        temp.path().to_str().unwrap(),
        "--source",
        "second.json",
        "--session-id",
        "shared",
        "--format",
        "json",
    ])?;
    let mut stdout = Vec::new();
    run(cli, false, &mut stdout, &mut Vec::new()).await?;
    let value: Value = serde_json::from_slice(&stdout)?;
    assert_eq!(value["matches"].as_array().unwrap().len(), 1);
    assert_eq!(value["matches"][0]["source_path"], "second.json");
    Ok(())
}

#[tokio::test]
async fn find_reports_truncation_and_empty_results() -> Result<()> {
    let cli = Cli::try_parse_from([
        "pchronicle",
        "find",
        example_dataset("openai-messages").to_str().unwrap(),
        "--document-id",
        "training-001",
        "--max-results",
        "1",
        "--format",
        "json",
    ])?;
    let mut stdout = Vec::new();
    run(cli, false, &mut stdout, &mut Vec::new()).await?;
    let value: Value = serde_json::from_slice(&stdout)?;
    assert_eq!(value["truncated"], false);
    assert_eq!(value["matches"].as_array().unwrap().len(), 1);

    let cli = Cli::try_parse_from([
        "pchronicle",
        "find",
        example_dataset("atif").to_str().unwrap(),
        "--session-id",
        "missing",
        "--format",
        "table",
    ])?;
    let mut stdout = Vec::new();
    run(cli, true, &mut stdout, &mut Vec::new()).await?;
    assert!(String::from_utf8(stdout)?.contains("(0 matches)"));
    Ok(())
}

#[tokio::test]
async fn find_truncates_ambiguous_candidates() -> Result<()> {
    let temp = tempfile::tempdir()?;
    for file in ["a.json", "b.json"] {
        fs::write(
            temp.path().join(file),
            r#"[{"id":"event","session_id":"shared","step_id":1,"messages":[],"response":{"role":"assistant","content":"ok"}}]"#,
        )?;
    }
    let cli = Cli::try_parse_from([
        "pchronicle",
        "find",
        temp.path().to_str().unwrap(),
        "--session-id",
        "shared",
        "--max-results",
        "1",
        "--format",
        "json",
    ])?;
    let mut stdout = Vec::new();
    run(cli, false, &mut stdout, &mut Vec::new()).await?;
    let value: Value = serde_json::from_slice(&stdout)?;
    assert_eq!(value["truncated"], true);
    assert_eq!(value["matches"].as_array().unwrap().len(), 1);
    Ok(())
}

#[tokio::test]
async fn find_validates_source_paths_and_escapes_quotes() -> Result<()> {
    for source in ["/absolute.json", "../outside.json", "s3://bucket/file"] {
        let cli = Cli::try_parse_from([
            "pchronicle",
            "find",
            example_dataset("atif").to_str().unwrap(),
            "--source",
            source,
            "--session-id",
            "support-001",
        ])?;
        assert!(
            run(cli, false, &mut Vec::new(), &mut Vec::new())
                .await
                .is_err()
        );
    }

    let temp = tempfile::tempdir()?;
    fs::write(
        temp.path().join("it's-valid.json"),
        r#"[{"id":"event","session_id":"quoted","step_id":1,"messages":[],"response":{"role":"assistant","content":"ok"}}]"#,
    )?;
    let cli = Cli::try_parse_from([
        "pchronicle",
        "find",
        temp.path().to_str().unwrap(),
        "--source",
        "it's-valid.json",
        "--session-id",
        "quoted",
        "--format",
        "json",
    ])?;
    let mut stdout = Vec::new();
    run(cli, false, &mut stdout, &mut Vec::new()).await?;
    let value: Value = serde_json::from_slice(&stdout)?;
    assert_eq!(value["matches"][0]["source_path"], "it's-valid.json");
    Ok(())
}

#[tokio::test]
async fn find_enforces_output_byte_limit_without_partial_stdout() -> Result<()> {
    let cli = Cli::try_parse_from([
        "pchronicle",
        "find",
        example_dataset("atif").to_str().unwrap(),
        "--session-id",
        "support-001",
        "--max-output-bytes",
        "8",
        "--format",
        "json",
    ])?;
    let mut stdout = Vec::new();
    let error = run(cli, false, &mut stdout, &mut Vec::new())
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("max_output_bytes"),
        "{error:#}"
    );
    assert!(stdout.is_empty());
    Ok(())
}

#[test]
fn find_cli_requires_one_identity_and_session_for_steps() {
    assert!(Cli::try_parse_from(["pchronicle", "find", ".", "--match", "needle"]).is_ok());
    assert!(Cli::try_parse_from(["pchronicle", "find", ".", "--match", "$.x=true"]).is_ok());
    assert!(Cli::try_parse_from(["pchronicle", "find", ".", "--json", "$.x=true"]).is_err());
    assert!(
        Cli::try_parse_from([
            "pchronicle",
            "find",
            ".",
            "--run-id",
            "r",
            "--session-id",
            "s"
        ])
        .is_err()
    );
    assert!(Cli::try_parse_from(["pchronicle", "find", ".", "--step-id", "1"]).is_err());
}

#[test]
fn find_preview_extracts_message_text_from_json_envelope() {
    assert_eq!(
        find_preview_text(r#"[{"type":"text","text":"hello"}]"#),
        "hello"
    );
    assert_eq!(
        find_preview_text(r#""[{\"text\":\"nested hello\"}]""#),
        "nested hello"
    );
    assert_eq!(find_preview_text("plain text"), "plain text");
}

#[tokio::test]
async fn find_rejects_empty_and_oversized_identities() -> Result<()> {
    for identity in ["", &"x".repeat(4097)] {
        let cli = Cli::try_parse_from([
            "pchronicle",
            "find",
            example_dataset("atif").to_str().unwrap(),
            "--session-id",
            identity,
        ])?;
        assert!(
            run(cli, false, &mut Vec::new(), &mut Vec::new())
                .await
                .is_err()
        );
    }
    Ok(())
}

#[tokio::test]
async fn import_warns_for_unmapped_and_vendor_residual_keys() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let input = temp.path().join("steps.actf.json");
    let output = temp.path().join("dataset");
    let step = |id: i64, user: &str| {
        serde_json::json!({
            "step_id": id,
            "assistant_content": {
                "content": format!("reply-{id}"),
                "reasoning_content": "",
                "tool_calls": []
            },
            "metric": {
                "prompt_tokens_len": 1,
                "completion_tokens_len": 1,
                "llm_infer_ms": 1.0,
                "env_action_ms": null,
                "stop_reason": "stop"
            },
            "system_prompt": "system",
            "user_content": user,
            "tools": [],
            "observation": [],
            "started_at": "2026-08-01 10:00:00+00:00",
            "finished_at": "2026-08-01 10:00:01+00:00",
            "vendor_step": format!("vendor-{id}")
        })
    };
    let document = serde_json::json!({
        "task_id": "warn-dedupe",
        "category": "software-engineering",
        "k": 1,
        "correct": false,
        "attempts_tried": 1,
        "solved_at": null,
        "attempts": {
            "1": {
                "correct": false,
                "final_answer": null,
                "ground_truth": "expected",
                "trajectory": {
                    "schema_version": "ACTF_v1.0",
                    "steps": [
                        step(1, "alpha"),
                        step(2, "beta"),
                        step(3, "gamma")
                    ],
                    "started_at": "2026-08-01 10:00:00+00:00",
                    "finished_at": "2026-08-01 10:00:01+00:00"
                },
                "status": "completed",
                "score": null,
                "error": "",
                "artifacts": null,
                "extra": null,
                "analysis_result": null,
                "meta": null
            }
        }
    });
    fs::write(&input, serde_json::to_vec_pretty(&document)?)?;

    let cli = Cli::try_parse_from([
        "pchronicle",
        "import",
        "--from",
        input.to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
        "--format",
        "actf",
    ])?;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    run(cli, false, &mut stdout, &mut stderr).await?;

    let stderr = String::from_utf8(stderr)?;
    let vendor_warnings: Vec<_> = stderr
        .lines()
        .filter(|line| line.contains("key=/attempts/1/trajectory/steps/*/vendor_step"))
        .collect();
    assert_eq!(
        vendor_warnings,
        [
            "warning: unknown field source=actf key=/attempts/1/trajectory/steps/*/vendor_step occurrences=3"
        ]
    );
    assert!(
        !stderr.contains("key=/attempts/1/trajectory/steps/*/user_content"),
        "mapped user_content must not warn: {stderr}"
    );
    assert!(
        !stderr.contains("key=/attempts/1/extra")
            && !stderr.contains("key=/attempts/1/meta")
            && !stderr.contains("key=/attempts/1/max_score"),
        "mapped attempt extra/meta/max_score must not warn: {stderr}"
    );
    assert!(!stderr.contains("/assistant_content/content"));
    assert!(
        !stderr.contains("vendor-1")
            && !stderr.contains("vendor-2")
            && !stderr.contains("vendor-3"),
        "warnings must not print unknown-field values: {stderr}"
    );
    Ok(())
}

#[tokio::test]
async fn import_counts_shared_actf_root_unknown_once_across_attempt_storylines() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let input = temp.path().join("multi-attempt.actf.json");
    let output = temp.path().join("dataset");
    let mut document = actf_with_unknown_step_fields("multi-attempt", &["first"]);
    document["vendor_root"] = serde_json::json!({"debug": true});
    document["k"] = serde_json::json!(2);
    document["attempts_tried"] = serde_json::json!(2);
    let mut second_attempt = document["attempts"]["1"].clone();
    second_attempt["trajectory"]["steps"][0]["user_content"] = serde_json::json!("second");
    document["attempts"]["2"] = second_attempt;
    fs::write(&input, serde_json::to_vec(&document)?)?;

    let cli = Cli::try_parse_from([
        "pchronicle",
        "import",
        "--format",
        "actf",
        "--from",
        input.to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
    ])?;
    let mut stderr = Vec::new();
    run(cli, false, &mut Vec::new(), &mut stderr).await?;

    let stderr = String::from_utf8(stderr)?;
    assert_eq!(
        stderr
            .lines()
            .filter(|line| line.contains("source=actf key=/vendor_root "))
            .collect::<Vec<_>>(),
        ["warning: unknown field source=actf key=/vendor_root occurrences=1"]
    );
    Ok(())
}

#[tokio::test]
async fn import_counts_shared_openai_root_unknown_once_across_sessions() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let input = temp.path().join("sessions.json");
    let output = temp.path().join("dataset");
    let row = |session_id: &str| {
        serde_json::json!({
            "id": format!("row-{session_id}"),
            "session_id": session_id,
            "step_id": 1,
            "agent_model": "model-1",
            "created_at": 1_785_578_400.25,
            "job_id": "job-1",
            "reward": 0.0,
            "step_reward": 0.0,
            "is_terminal": true,
            "is_truncated": false,
            "is_session_completed": true,
            "is_trainable": false,
            "messages": [{
                "role": "user",
                "content": "hello",
                "name": null,
                "refusal": null,
                "tool_call_id": null
            }],
            "response": {
                "role": "assistant",
                "content": "done",
                "name": null,
                "refusal": null,
                "tool_call_id": null
            },
            "vendor_row": {"kept": true}
        })
    };
    fs::write(
        &input,
        serde_json::to_vec(&serde_json::json!({
            "vendor_root": {"debug": true},
            "session_steps": [row("first"), row("second")]
        }))?,
    )?;

    let cli = Cli::try_parse_from([
        "pchronicle",
        "import",
        "--format",
        "openai-messages",
        "--from",
        input.to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
    ])?;
    let mut stderr = Vec::new();
    run(cli, false, &mut Vec::new(), &mut stderr).await?;

    let stderr = String::from_utf8(stderr)?;
    assert_eq!(
        stderr
            .lines()
            .filter(|line| line.contains("source=openai-msg key=/vendor_root "))
            .collect::<Vec<_>>(),
        ["warning: unknown field source=openai-msg key=/vendor_root occurrences=1"]
    );
    assert!(
        stderr.contains(
            "warning: unknown field source=openai-msg key=/session_steps/*/vendor_row occurrences=2"
        ),
        "non-empty unmapped OpenAI fields must still warn: {stderr}"
    );
    for mapped in [
        "/step_id",
        "/agent_model",
        "/created_at",
        "/job_id",
        "/reward",
        "/step_reward",
        "/is_terminal",
        "/is_truncated",
        "/is_session_completed",
        "/is_trainable",
        "/messages/*/role",
        "/messages/*/content",
        "/messages/*/name",
        "/messages/*/refusal",
        "/messages/*/tool_call_id",
        "/response/role",
        "/response/content",
        "/response/name",
        "/response/refusal",
        "/response/tool_call_id",
    ] {
        assert!(
            !stderr.contains(mapped),
            "mapped or known-empty OpenAI field must not warn ({mapped}): {stderr}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn directory_import_reads_atif_jsonl_and_ndjson_in_both_output_modes() -> Result<()> {
    let temp = tempfile::tempdir()?;
    for extension in ["jsonl", "ndjson"] {
        for output_format in [ImportOutputFormat::Preserve, ImportOutputFormat::Storyline] {
            let case = format!("{extension}-{}", output_format.response_name());
            let input_root = temp.path().join(format!("input-{case}"));
            let input = input_root.join(format!("nested/records.{extension}"));
            fs::create_dir_all(input.parent().unwrap())?;
            let record = |trajectory_id: &str| {
                serde_json::json!({
                    "schema_version": "ATIF-v1.7",
                    "trajectory_id": trajectory_id,
                    "agent": {"name": "agent", "version": "1"},
                    "steps": []
                })
            };
            let bytes = format!("{}\n\n{}\n", record("first"), record("second")).into_bytes();
            fs::write(&input, &bytes)?;
            let output = temp.path().join(format!("output-{case}"));

            let mut argv = vec![
                "pchronicle".to_owned(),
                "import".to_owned(),
                "--from".to_owned(),
                input_root.to_string_lossy().into_owned(),
                "--output".to_owned(),
                output.to_string_lossy().into_owned(),
            ];
            if output_format == ImportOutputFormat::Storyline {
                argv.extend(["--output-format".to_owned(), "storyline".to_owned()]);
            }
            let cli = Cli::try_parse_from(argv)?;
            let mut stdout = Vec::new();
            run(cli, false, &mut stdout, &mut Vec::new()).await?;

            let response: Value = serde_json::from_slice(&stdout)?;
            assert_eq!(response["sources"], 1, "case={case}");
            assert_eq!(response["trajectories"], 2, "case={case}");
            let staged = output.join(format!("nested/records.{extension}"));
            match output_format {
                ImportOutputFormat::Preserve => assert_eq!(fs::read(staged)?, bytes),
                ImportOutputFormat::Storyline => {
                    assert!(output.join("CURRENT").is_file());
                    assert!(!staged.exists());
                }
                ImportOutputFormat::CompactJsonl => unreachable!(),
            }
        }
    }
    Ok(())
}

#[tokio::test]
async fn single_file_preserve_import_keeps_atif_json_lines_queryable() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let record = |trajectory_id: &str| {
        serde_json::json!({
            "schema_version": "ATIF-v1.7",
            "trajectory_id": trajectory_id,
            "agent": {"name": "agent", "version": "1"},
            "steps": []
        })
    };
    let bytes = format!("{}\n{}\n", record("first"), record("second")).into_bytes();
    for extension in ["jsonl", "ndjson"] {
        let input = temp.path().join(format!("records.{extension}"));
        fs::write(&input, &bytes)?;
        let output = temp.path().join(format!("dataset-{extension}"));

        let cli = Cli::try_parse_from([
            "pchronicle",
            "import",
            "--from",
            input.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
        ])?;
        let mut stdout = Vec::new();
        run(cli, false, &mut stdout, &mut Vec::new()).await?;

        let source_path = format!("trajectories.atif.{extension}");
        let response: Value = serde_json::from_slice(&stdout)?;
        assert_eq!(response["source_path"], source_path);
        assert_eq!(response["trajectories"], 2);
        assert_eq!(fs::read(output.join(&source_path))?, bytes);

        let cli = Cli::try_parse_from([
            "pchronicle",
            "query",
            output.to_str().unwrap(),
            "SELECT COUNT(*) AS runs FROM dataset.runs",
            "--format",
            "jsonl",
        ])?;
        let mut stdout = Vec::new();
        run(cli, false, &mut stdout, &mut Vec::new()).await?;
        let count: Value = serde_json::from_slice(&stdout)?;
        assert_eq!(count["runs"], 2, "extension={extension}");
    }

    let invalid_input = temp.path().join("invalid.jsonl");
    fs::write(
        &invalid_input,
        format!("{}\n{{\"not\":\"atif\"}}\n", record("valid")),
    )?;
    let invalid_output = temp.path().join("invalid-output");
    let cli = Cli::try_parse_from([
        "pchronicle",
        "import",
        "--from",
        invalid_input.to_str().unwrap(),
        "--output",
        invalid_output.to_str().unwrap(),
    ])?;
    let error = run(cli, false, &mut Vec::new(), &mut Vec::new())
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("invalid.jsonl line 2"));
    assert!(!invalid_output.exists());
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn import_accepts_an_explicit_symlink_to_a_regular_file() -> Result<()> {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir()?;
    let input = temp.path().join("input.json");
    fs::copy(example_source("atif"), &input)?;
    let link = temp.path().join("input-link.json");
    symlink(&input, &link)?;
    let output = temp.path().join("dataset");

    let cli = Cli::try_parse_from([
        "pchronicle",
        "import",
        "--from",
        link.to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
    ])?;
    run(cli, false, &mut Vec::new(), &mut Vec::new()).await?;

    assert_eq!(
        fs::read(output.join("trajectories.atif.json"))?,
        fs::read(&input)?
    );
    Ok(())
}

#[tokio::test]
async fn import_recurses_directories_and_preserves_relative_source_paths() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let input = temp.path().join("input");
    let nested = input.join("nested/deeper");
    fs::create_dir_all(&nested)?;
    let atif = input.join("root.json");
    let openai = nested.join("training.json");
    fs::copy(example_source("atif"), &atif)?;
    fs::copy(example_source("openai-messages"), &openai)?;
    fs::write(input.join("README.txt"), "not an import source")?;
    let output = temp.path().join("dataset");

    let cli = Cli::try_parse_from([
        "pchronicle",
        "import",
        "--from",
        input.to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
    ])?;
    let mut stdout = Vec::new();
    run(cli, false, &mut stdout, &mut Vec::new()).await?;

    let response: Value = serde_json::from_slice(&stdout)?;
    assert_eq!(response["output_format"], "preserve");
    assert_eq!(response["sources"], 2);
    assert_eq!(response["trajectories"], 3);
    assert_eq!(
        response["input_bytes"],
        fs::metadata(&atif)?.len() + fs::metadata(&openai)?.len()
    );
    assert!(response.get("source_path").is_none());
    assert!(response.get("format").is_none());
    assert_eq!(fs::read(output.join("root.json"))?, fs::read(&atif)?);
    assert_eq!(
        fs::read(output.join("nested/deeper/training.json"))?,
        fs::read(&openai)?
    );
    assert!(!output.join("README.txt").exists());

    let cli = Cli::try_parse_from([
        "pchronicle",
        "query",
        output.to_str().unwrap(),
        "SELECT COUNT(*) AS runs FROM dataset.runs",
        "--format",
        "jsonl",
    ])?;
    let mut stdout = Vec::new();
    run(cli, false, &mut stdout, &mut Vec::new()).await?;
    let count: Value = serde_json::from_slice(&stdout)?;
    assert_eq!(count["runs"], 3);
    Ok(())
}

#[tokio::test]
async fn directory_import_auto_detects_each_file_and_skips_unknown_json() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let input = temp.path().join("input");
    fs::create_dir_all(input.join("details"))?;
    let atif = input.join("root.json");
    let openai = input.join("nested-training.json");
    let unknown = input.join("details/_error_gravitational-wave-detection_astronomy.json");
    fs::copy(example_source("atif"), &atif)?;
    fs::copy(example_source("openai-messages"), &openai)?;
    fs::write(&unknown, r#"{"error":"task failed","task_id":"astronomy"}"#)?;

    for output_format in [ImportOutputFormat::Preserve, ImportOutputFormat::Storyline] {
        let output = temp
            .path()
            .join(format!("dataset-{}", output_format.response_name()));
        let mut argv = vec![
            "pchronicle".to_owned(),
            "import".to_owned(),
            "--from".to_owned(),
            input.to_string_lossy().into_owned(),
            "--output".to_owned(),
            output.to_string_lossy().into_owned(),
        ];
        if output_format == ImportOutputFormat::Storyline {
            argv.extend(["--output-format".to_owned(), "storyline".to_owned()]);
        }
        let cli = Cli::try_parse_from(argv)?;
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        run(cli, false, &mut stdout, &mut stderr).await?;

        let response: Value = serde_json::from_slice(&stdout)?;
        assert_eq!(response["sources"], 2, "{output_format:?}: {response}");
        assert_eq!(response["trajectories"], 3, "{output_format:?}: {response}");
        let warnings = String::from_utf8(stderr)?;
        assert!(
            warnings.contains("import source=root.json status=processing"),
            "{output_format:?}: {warnings}"
        );
        assert!(
            warnings.contains("import source=root.json status=completed"),
            "{output_format:?}: {warnings}"
        );
        assert!(
            warnings.contains("_error_gravitational-wave-detection_astronomy.json"),
            "{output_format:?}: {warnings}"
        );
        assert!(
            warnings.contains("cannot detect import format"),
            "{output_format:?}: {warnings}"
        );
        if output_format == ImportOutputFormat::Preserve {
            assert!(!output.join(unknown.file_name().unwrap()).exists());
            assert!(
                !output
                    .join("details/_error_gravitational-wave-detection_astronomy.json")
                    .exists()
            );
        }
    }

    let single = temp.path().join("single-unknown");
    let cli = Cli::try_parse_from([
        "pchronicle",
        "import",
        "--from",
        unknown.to_str().unwrap(),
        "--output",
        single.to_str().unwrap(),
    ])?;
    let error = run(cli, false, &mut Vec::new(), &mut Vec::new())
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("cannot detect import format"),
        "{error:#}"
    );
    assert!(!single.exists());
    Ok(())
}

#[tokio::test]
async fn import_storyline_output_writes_one_root_lance_store() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let output = temp.path().join("dataset");
    let input = example_source("openai-messages");
    let cli = Cli::try_parse_from([
        "pchronicle",
        "import",
        "--from",
        input.to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
        "--output-format",
        "storyline",
    ])?;
    let mut stdout = Vec::new();
    run(cli, false, &mut stdout, &mut Vec::new()).await?;

    let response: Value = serde_json::from_slice(&stdout)?;
    assert_eq!(response["format"], "openai-msg");
    assert_eq!(response["source_path"], "session_steps.json");
    assert_eq!(response["output_format"], "storyline-lance");
    assert_eq!(response["sources"], 1);
    assert_eq!(response["trajectories"], 2);
    assert!(output.join("CURRENT").is_file());
    assert!(output.join("generations").is_dir());
    assert!(output.join("objects.lance").is_dir());
    assert!(!output.join("session_steps.json").exists());

    Ok(())
}

#[tokio::test]
async fn import_object_store_output_requires_storyline_format() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let input = temp.path().join("one.json");
    fs::write(
        &input,
        serde_json::to_vec(&atif_identity_document("document-one", "session-one"))?,
    )?;

    for extra in [vec![], vec!["--output-format", "preserve"]] {
        let mut args = vec![
            "pchronicle",
            "import",
            "--from",
            input.to_str().unwrap(),
            "--output",
            "s3://my-bucket/prefix",
        ];
        args.extend(extra);
        let cli = Cli::try_parse_from(args)?;
        let error = run(cli, false, &mut Vec::new(), &mut Vec::new())
            .await
            .unwrap_err();
        let message = format!("{error:#}");
        assert!(
            message.contains("object-store import requires --output-format storyline"),
            "unexpected import error: {message}"
        );
        assert!(
            !message.contains("only local output paths"),
            "object-store output still rejected as local-only: {message}"
        );
    }
    Ok(())
}

#[test]
fn import_output_normalizes_object_store_uri() -> Result<()> {
    let location = DatasetLocation::parse("s3://my-bucket/prefix/")?.into_create_target()?;
    assert!(location.is_object_store());
    assert_eq!(location.as_str(), "s3://my-bucket/prefix");
    let error = DatasetLocation::parse("s3://user:secret@my-bucket/prefix")
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("must not contain embedded credentials"),
        "{error}"
    );
    let error = DatasetLocation::parse("s3://dd/data/")
        .unwrap_err()
        .to_string();
    assert!(error.contains("must be 3-63 characters"), "{error}");
    Ok(())
}

#[tokio::test]
async fn import_storyline_output_query_uses_squashed_source() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let output = temp.path().join("dataset");
    let input = example_source("openai-messages");
    let cli = Cli::try_parse_from([
        "pchronicle",
        "import",
        "--from",
        input.to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
        "--output-format",
        "storyline",
    ])?;
    run(cli, false, &mut Vec::new(), &mut Vec::new()).await?;

    let cli = Cli::try_parse_from([
        "pchronicle",
        "query",
        output.to_str().unwrap(),
        "SELECT _file_ AS source_file, COUNT(*) AS runs FROM dataset.runs GROUP BY _file_",
        "--format",
        "jsonl",
    ])?;
    let mut stdout = Vec::new();
    run(cli, false, &mut stdout, &mut Vec::new()).await?;
    let row: Value = serde_json::from_slice(&stdout)?;
    assert_eq!(row["source_file"], ".");
    assert_eq!(row["runs"], 2);
    Ok(())
}

#[tokio::test]
async fn canonical_event_import_auto_detects_and_is_create_only() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let storage = temp.path().join("capture");
    append_canonical_note(&storage).await?;
    let source = canonical_source(&storage)?;
    let manifest = source.join("_manifest.json");
    let before = fs::read(&manifest)?;
    let output = temp.path().join("storyline");

    let cli = Cli::try_parse_from([
        "pchronicle",
        "import",
        "--from",
        source.to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
    ])?;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    run(cli, false, &mut stdout, &mut stderr).await?;

    let response: Value = serde_json::from_slice(&stdout)?;
    assert_eq!(response["format"], "events");
    assert_eq!(response["source_path"], "events.lance");
    assert_eq!(response["output_format"], "storyline-lance");
    assert_eq!(response["sources"], 1);
    assert_eq!(response["trajectories"], 1);
    assert_eq!(response["fact_rows"], 1);
    assert!(response.get("input_bytes").is_none());
    assert_eq!(fs::read(&manifest)?, before);
    assert!(output.join("CURRENT").is_file());
    let metadata = String::from_utf8(stderr)?;
    assert!(metadata.contains("fact_rows=1"));
    assert!(!metadata.contains("input_bytes="));

    let query = Cli::try_parse_from([
        "pchronicle",
        "query",
        output.to_str().unwrap(),
        "SELECT COUNT(*) AS runs FROM dataset.runs",
        "--format",
        "jsonl",
    ])?;
    let mut query_stdout = Vec::new();
    run(query, false, &mut query_stdout, &mut Vec::new()).await?;
    let count: Value = serde_json::from_slice(&query_stdout)?;
    assert_eq!(count["runs"], 1);

    let explicit = temp.path().join("explicit-storyline");
    let cli = Cli::try_parse_from([
        "pchronicle",
        "import",
        "--from",
        source.to_str().unwrap(),
        "--output",
        explicit.to_str().unwrap(),
        "--output-format",
        "storyline",
    ])?;
    run(cli, false, &mut Vec::new(), &mut Vec::new()).await?;
    assert!(explicit.join("CURRENT").is_file());

    let preserved = temp.path().join("preserved");
    let cli = Cli::try_parse_from([
        "pchronicle",
        "import",
        "--from",
        source.to_str().unwrap(),
        "--output",
        preserved.to_str().unwrap(),
        "--output-format",
        "preserve",
    ])?;
    let error = run(cli, false, &mut Vec::new(), &mut Vec::new())
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("cannot preserve an existing canonical event Store")
    );
    assert!(!preserved.exists());

    let existing = temp.path().join("existing");
    fs::create_dir(&existing)?;
    fs::write(existing.join("sentinel"), "keep")?;
    let cli = Cli::try_parse_from([
        "pchronicle",
        "import",
        "--from",
        source.to_str().unwrap(),
        "--output",
        existing.to_str().unwrap(),
    ])?;
    assert!(
        run(cli, false, &mut Vec::new(), &mut Vec::new())
            .await
            .is_err()
    );
    assert_eq!(fs::read_to_string(existing.join("sentinel"))?, "keep");
    Ok(())
}

#[tokio::test]
async fn canonical_event_import_supports_object_store_uris() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let storage = temp.path().join("capture");
    append_canonical_note(&storage).await?;
    let source = canonical_source(&storage)?;
    let output = format!(
        "shared-memory://pchronicle-canonical-import-{}/storyline",
        uuid::Uuid::new_v4().simple()
    );
    let cli = Cli::try_parse_from([
        "pchronicle",
        "import",
        "--from",
        source.to_str().unwrap(),
        "--output",
        &output,
    ])?;
    let mut stdout = Vec::new();
    run(cli, false, &mut stdout, &mut Vec::new()).await?;

    let response: Value = serde_json::from_slice(&stdout)?;
    assert_eq!(response["dataset_uri"], output);
    assert_eq!(response["fact_rows"], 1);
    let store = StorylineLanceStore::open_uri(&output).await?;
    assert!(store.current_table_paths().await?.is_some());
    Ok(())
}

#[tokio::test]
async fn events_lance_suffix_without_manifest_remains_an_ordinary_directory_import() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let input = temp.path().join("events.lance");
    fs::create_dir(&input)?;
    let output = temp.path().join("output");
    let cli = Cli::try_parse_from([
        "pchronicle",
        "import",
        "--from",
        input.to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
    ])?;
    let error = run(cli, false, &mut Vec::new(), &mut Vec::new())
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("contains no .json, .jsonl, or .ndjson files")
    );
    assert!(!output.exists());
    Ok(())
}

#[tokio::test]
async fn directory_storyline_output_squashes_sources_into_one_root_store() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let input = temp.path().join("input");
    fs::create_dir_all(input.join("first"))?;
    fs::create_dir_all(input.join("second"))?;
    fs::write(
        input.join("first/shared.json"),
        serde_json::to_vec(&atif_identity_document("document-first", "session-first"))?,
    )?;
    fs::write(
        input.join("second/shared.json"),
        serde_json::to_vec(&atif_identity_document("document-second", "session-second"))?,
    )?;
    let output = temp.path().join("dataset");

    let cli = Cli::try_parse_from([
        "pchronicle",
        "import",
        "--from",
        input.to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
        "--output-format",
        "storyline",
    ])?;
    let mut stdout = Vec::new();
    run(cli, false, &mut stdout, &mut Vec::new()).await?;

    let response: Value = serde_json::from_slice(&stdout)?;
    assert_eq!(response["output_format"], "storyline-lance");
    assert_eq!(response["sources"], 2);
    assert_eq!(response["trajectories"], 2);
    assert!(output.join("CURRENT").is_file());
    assert!(output.join("generations").is_dir());
    assert!(output.join("objects.lance").is_dir());
    assert!(!output.join("first/shared.json").exists());
    assert!(!output.join("second/shared.json").exists());

    let cli = Cli::try_parse_from([
        "pchronicle",
        "query",
        output.to_str().unwrap(),
        "SELECT _file_ AS source_file, COUNT(*) AS runs FROM dataset.runs GROUP BY _file_",
        "--format",
        "jsonl",
    ])?;
    let mut stdout = Vec::new();
    run(cli, false, &mut stdout, &mut Vec::new()).await?;
    let row: Value = serde_json::from_slice(&stdout)?;
    assert_eq!(row["source_file"], ".");
    assert_eq!(row["runs"], 2);
    Ok(())
}

#[tokio::test]
async fn directory_storyline_import_renames_duplicate_document_ids() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let input = temp.path().join("input");
    fs::create_dir_all(input.join("first"))?;
    fs::create_dir_all(input.join("second"))?;
    fs::write(
        input.join("first/Energy_001_Energy.json"),
        serde_json::to_vec(&atif_identity_document("Energy_001", "Energy_001"))?,
    )?;
    fs::write(
        input.join("second/Energy_001_Energy.json"),
        serde_json::to_vec(&atif_identity_document("Energy_001", "Energy_001"))?,
    )?;
    let output = temp.path().join("dataset");

    let cli = Cli::try_parse_from([
        "pchronicle",
        "import",
        "--from",
        input.to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
        "--output-format",
        "storyline",
    ])?;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    run(cli, false, &mut stdout, &mut stderr).await?;

    let response: Value = serde_json::from_slice(&stdout)?;
    assert_eq!(response["trajectories"], 2);
    let stderr = String::from_utf8(stderr)?;
    assert!(
        stderr.contains("duplicate document_id 'Energy_001' renamed to 'Energy_001#1'"),
        "{stderr}"
    );

    let cli = Cli::try_parse_from([
        "pchronicle",
        "query",
        output.to_str().unwrap(),
        "SELECT document_id FROM dataset.runs ORDER BY document_id",
        "--format",
        "jsonl",
    ])?;
    let mut stdout = Vec::new();
    run(cli, false, &mut stdout, &mut Vec::new()).await?;
    let rows = stdout
        .split(|&byte| byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(serde_json::from_slice::<Value>)
        .collect::<serde_json::Result<Vec<_>>>()?;
    let ids = rows
        .iter()
        .map(|row| row["document_id"].as_str().unwrap_or_default())
        .collect::<Vec<_>>();
    assert_eq!(ids, ["Energy_001", "Energy_001#1"]);
    Ok(())
}

#[tokio::test]
async fn append_storyline_import_suffixes_or_skips_existing_document_ids() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let initial = temp.path().join("initial.json");
    let duplicate = temp.path().join("duplicate.json");
    let output = temp.path().join("dataset");
    fs::write(
        &initial,
        serde_json::to_vec(&atif_identity_document("shared", "session-initial"))?,
    )?;
    fs::write(
        &duplicate,
        serde_json::to_vec(&atif_identity_document("shared", "session-duplicate"))?,
    )?;

    let create = Cli::try_parse_from([
        "pchronicle",
        "import",
        "--from",
        initial.to_str().unwrap(),
        "--to",
        output.to_str().unwrap(),
        "--output-format",
        "storyline",
    ])?;
    run(create, false, &mut Vec::new(), &mut Vec::new()).await?;

    let append = Cli::try_parse_from([
        "pchronicle",
        "import",
        "--from",
        duplicate.to_str().unwrap(),
        "--to",
        output.to_str().unwrap(),
        "--mode",
        "append",
    ])?;
    let mut append_stdout = Vec::new();
    let mut append_stderr = Vec::new();
    run(append, false, &mut append_stdout, &mut append_stderr).await?;
    assert_eq!(
        serde_json::from_slice::<Value>(&append_stdout)?["trajectories"],
        1
    );
    assert!(String::from_utf8(append_stderr)?.contains("renamed to 'shared#1'"));

    let skip = Cli::try_parse_from([
        "pchronicle",
        "import",
        "--from",
        duplicate.to_str().unwrap(),
        "--to",
        output.to_str().unwrap(),
        "--mode",
        "append",
        "--on-duplicate",
        "skip",
    ])?;
    let mut skip_stdout = Vec::new();
    let mut skip_stderr = Vec::new();
    run(skip, false, &mut skip_stdout, &mut skip_stderr).await?;
    assert_eq!(
        serde_json::from_slice::<Value>(&skip_stdout)?["trajectories"],
        0
    );
    assert!(String::from_utf8(skip_stderr)?.contains("document_id 'shared' skipped"));

    let query = Cli::try_parse_from([
        "pchronicle",
        "query",
        output.to_str().unwrap(),
        "SELECT document_id FROM dataset.runs ORDER BY document_id",
        "--format",
        "jsonl",
    ])?;
    let mut query_stdout = Vec::new();
    run(query, false, &mut query_stdout, &mut Vec::new()).await?;
    let ids = query_stdout
        .split(|&byte| byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(serde_json::from_slice::<Value>)
        .collect::<serde_json::Result<Vec<_>>>()?
        .into_iter()
        .map(|row| row["document_id"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert_eq!(ids, ["shared", "shared#1"]);
    Ok(())
}

#[tokio::test]
async fn replace_and_drop_require_confirmation_and_accept_yes() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let input = temp.path().join("replacement.json");
    let output = temp.path().join("dataset");
    fs::write(
        &input,
        serde_json::to_vec(&atif_identity_document(
            "replacement",
            "replacement-session",
        ))?,
    )?;
    fs::create_dir(&output)?;
    fs::write(output.join("old.marker"), "old")?;

    let replace_without_yes = Cli::try_parse_from([
        "pchronicle",
        "import",
        "--from",
        input.to_str().unwrap(),
        "--to",
        output.to_str().unwrap(),
        "--output-format",
        "storyline",
        "--mode",
        "replace",
    ])?;
    let error = run(replace_without_yes, false, &mut Vec::new(), &mut Vec::new())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("rerun with --yes"));
    assert!(output.join("old.marker").exists());

    fs::write(&input, b"not valid JSON")?;
    let failed_replace = Cli::try_parse_from([
        "pchronicle",
        "import",
        "--from",
        input.to_str().unwrap(),
        "--to",
        output.to_str().unwrap(),
        "--output-format",
        "storyline",
        "--mode",
        "replace",
        "--yes",
    ])?;
    assert!(
        run(failed_replace, false, &mut Vec::new(), &mut Vec::new())
            .await
            .is_err()
    );
    assert!(output.join("old.marker").exists());

    fs::write(
        &input,
        serde_json::to_vec(&atif_identity_document(
            "replacement",
            "replacement-session",
        ))?,
    )?;
    let replace = Cli::try_parse_from([
        "pchronicle",
        "import",
        "--from",
        input.to_str().unwrap(),
        "--to",
        output.to_str().unwrap(),
        "--output-format",
        "storyline",
        "--mode",
        "replace",
        "--yes",
    ])?;
    run(replace, false, &mut Vec::new(), &mut Vec::new()).await?;
    assert!(output.join("CURRENT").is_file());
    assert!(!output.join("old.marker").exists());
    assert!(!temp.path().read_dir()?.any(|entry| {
        entry.ok().is_some_and(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".pchronicle-replace-")
        })
    }));

    let drop_without_yes = Cli::try_parse_from(["pchronicle", "drop", output.to_str().unwrap()])?;
    let error = run(drop_without_yes, false, &mut Vec::new(), &mut Vec::new())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("rerun with --yes"));
    assert!(output.exists());

    let drop = Cli::try_parse_from(["pchronicle", "drop", output.to_str().unwrap(), "--yes"])?;
    let mut stdout = Vec::new();
    run(drop, false, &mut stdout, &mut Vec::new()).await?;
    assert_eq!(serde_json::from_slice::<Value>(&stdout)?["dropped"], true);
    assert!(!output.exists());

    fs::create_dir(&output)?;
    fs::write(output.join("interactive.marker"), "interactive")?;
    let interactive_drop = Cli::try_parse_from([
        "pchronicle",
        "--log-level",
        "error",
        "drop",
        output.to_str().unwrap(),
    ])?;
    let mut confirmation = std::io::Cursor::new(b"yes\n".to_vec());
    let mut prompt = Vec::new();
    run_with_stdio(
        interactive_drop,
        true,
        false,
        &mut confirmation,
        &mut Vec::new(),
        &mut prompt,
    )
    .await?;
    assert!(String::from_utf8(prompt)?.contains("Permanently drop Dataset"));
    assert!(!output.exists());
    Ok(())
}

#[tokio::test]
async fn directory_import_dedupes_unknown_warnings_across_sources() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let input = temp.path().join("input");
    fs::create_dir_all(input.join("nested"))?;
    fs::write(
        input.join("first.actf.json"),
        serde_json::to_vec(&actf_with_unknown_step_fields("first", &["alpha"]))?,
    )?;
    fs::write(
        input.join("nested/second.actf.json"),
        serde_json::to_vec(&actf_with_unknown_step_fields("second", &["beta", "gamma"]))?,
    )?;
    let output = temp.path().join("dataset");

    let cli = Cli::try_parse_from([
        "pchronicle",
        "import",
        "--from",
        input.to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
    ])?;
    let mut stderr = Vec::new();
    run(cli, false, &mut Vec::new(), &mut stderr).await?;

    let stderr = String::from_utf8(stderr)?;
    let warnings = stderr
        .lines()
        .filter(|line| line.contains("key=/attempts/1/trajectory/steps/*/vendor_step"))
        .collect::<Vec<_>>();
    assert_eq!(
        warnings,
        [
            "warning: unknown field source=actf key=/attempts/1/trajectory/steps/*/vendor_step occurrences=3"
        ]
    );
    assert!(!stderr.contains("key=/attempts/1/trajectory/steps/*/user_content"));
    assert!(!stderr.contains("key=/attempts/1/extra") && !stderr.contains("key=/attempts/1/meta"));
    assert!(!stderr.contains("alpha") && !stderr.contains("beta") && !stderr.contains("gamma"));
    Ok(())
}

#[tokio::test]
async fn directory_import_failure_does_not_publish_partial_output() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let input = temp.path().join("input");
    fs::create_dir_all(&input)?;
    fs::copy(example_source("atif"), input.join("a-valid.json"))?;
    fs::write(input.join("z-invalid.json"), "not json")?;

    for output_format in [ImportOutputFormat::Preserve, ImportOutputFormat::Storyline] {
        let output = temp
            .path()
            .join(format!("dataset-{}", output_format.response_name()));
        let mut argv = vec![
            "pchronicle".to_owned(),
            "import".to_owned(),
            "--from".to_owned(),
            input.to_string_lossy().into_owned(),
            "--output".to_owned(),
            output.to_string_lossy().into_owned(),
        ];
        if output_format == ImportOutputFormat::Storyline {
            argv.extend(["--output-format".to_owned(), "storyline".to_owned()]);
        }
        let cli = Cli::try_parse_from(argv)?;
        let error = run(cli, false, &mut Vec::new(), &mut Vec::new())
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("z-invalid.json"), "{error:#}");
        assert!(!output.exists());
    }
    assert!(!fs::read_dir(temp.path())?.any(|entry| {
        entry
            .ok()
            .and_then(|entry| entry.file_name().into_string().ok())
            .is_some_and(|name| name.starts_with(".pchronicle-import-"))
    }));
    Ok(())
}

#[tokio::test]
async fn storyline_squash_renames_duplicate_document_ids() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let input = temp.path().join("input");
    fs::create_dir_all(input.join("nested"))?;
    fs::write(
        input.join("first.json"),
        serde_json::to_vec(&atif_identity_document("shared-document", "session-first"))?,
    )?;
    fs::write(
        input.join("nested/second.json"),
        serde_json::to_vec(&atif_identity_document("shared-document", "session-second"))?,
    )?;
    let output = temp.path().join("output");
    let cli = Cli::try_parse_from([
        "pchronicle",
        "import",
        "--from",
        input.to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
        "--output-format",
        "storyline",
    ])?;
    let mut stderr = Vec::new();
    run(cli, false, &mut Vec::new(), &mut stderr).await?;
    let stderr = String::from_utf8(stderr)?;
    assert!(
        stderr.contains("duplicate document_id 'shared-document' renamed to 'shared-document#1'"),
        "{stderr}"
    );
    assert!(output.exists());

    let session_input = temp.path().join("sessions");
    fs::create_dir_all(&session_input)?;
    fs::write(
        session_input.join("first.json"),
        serde_json::to_vec(&atif_identity_document("document-first", "shared-session"))?,
    )?;
    fs::write(
        session_input.join("second.json"),
        serde_json::to_vec(&atif_identity_document("document-second", "shared-session"))?,
    )?;
    let session_output = temp.path().join("session-output");
    let cli = Cli::try_parse_from([
        "pchronicle",
        "import",
        "--from",
        session_input.to_str().unwrap(),
        "--output",
        session_output.to_str().unwrap(),
        "--output-format",
        "storyline",
    ])?;
    run(cli, false, &mut Vec::new(), &mut Vec::new()).await?;
    assert!(session_output.exists());

    let duplicate_input = temp.path().join("duplicates.json");
    fs::write(
        &duplicate_input,
        serde_json::to_vec(&serde_json::json!([
            atif_identity_document("same-document", "session-a"),
            atif_identity_document("same-document", "session-b")
        ]))?,
    )?;
    let duplicate_output = temp.path().join("duplicate-output");
    let cli = Cli::try_parse_from([
        "pchronicle",
        "import",
        "--from",
        duplicate_input.to_str().unwrap(),
        "--output",
        duplicate_output.to_str().unwrap(),
        "--format",
        "atif",
        "--output-format",
        "storyline",
    ])?;
    let mut stderr = Vec::new();
    run(cli, false, &mut Vec::new(), &mut stderr).await?;
    let stderr = String::from_utf8(stderr)?;
    assert!(
        stderr.contains("duplicate document_id 'same-document' renamed to 'same-document#1'"),
        "{stderr}"
    );
    assert!(duplicate_output.exists());
    Ok(())
}

#[tokio::test]
async fn storyline_squash_late_source_failure_removes_staging() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let input = temp.path().join("input");
    fs::create_dir_all(&input)?;
    let documents = (0..256)
        .map(|index| {
            atif_identity_document(&format!("document-{index}"), &format!("session-{index}"))
        })
        .collect::<Vec<_>>();
    fs::write(input.join("a-valid.json"), serde_json::to_vec(&documents)?)?;
    fs::write(input.join("z-invalid.json"), "not json")?;
    let output = temp.path().join("dataset");

    let cli = Cli::try_parse_from([
        "pchronicle",
        "import",
        "--from",
        input.to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
        "--output-format",
        "storyline",
    ])?;
    let error = run(cli, false, &mut Vec::new(), &mut Vec::new())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("z-invalid.json"), "{error:#}");
    assert!(!output.exists());
    assert!(!fs::read_dir(temp.path())?.any(|entry| {
        entry
            .ok()
            .and_then(|entry| entry.file_name().into_string().ok())
            .is_some_and(|name| name.starts_with(".pchronicle-import-"))
    }));
    Ok(())
}

#[tokio::test]
async fn import_creates_queryable_lossless_datasets_for_all_example_formats() -> Result<()> {
    let temp = tempfile::tempdir()?;
    for (format, expected_format, source_name, expected_runs) in [
        ("atif", "atif", "trajectories.atif.json", 1),
        ("openai-messages", "openai-msg", "session_steps.json", 2),
        ("actf", "actf", "trajectories.actf.json", 1),
    ] {
        let input = example_source(format);
        let output = temp.path().join(format);
        let cli = Cli::try_parse_from([
            "pchronicle",
            "import",
            "--from",
            input.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
        ])?;
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        run(cli, false, &mut stdout, &mut stderr).await?;

        let response: Value = serde_json::from_slice(&stdout)?;
        assert!(response.get("schema_version").is_none());
        assert_eq!(response["format"], expected_format);
        assert_eq!(response["source_path"], source_name);
        assert_eq!(response["trajectories"], expected_runs);
        assert_eq!(
            fs::read(output.join(source_name))?,
            fs::read(&input)?,
            "import must preserve the exchange document byte-for-byte"
        );
        assert!(String::from_utf8(stderr)?.contains("trajectories="));

        let cli = Cli::try_parse_from([
            "pchronicle",
            "query",
            output.to_str().unwrap(),
            "SELECT COUNT(*) AS runs FROM dataset.runs",
            "--format",
            "jsonl",
        ])?;
        let mut stdout = Vec::new();
        run(cli, false, &mut stdout, &mut Vec::new()).await?;
        let count: Value = serde_json::from_slice(&stdout)?;
        assert_eq!(count["runs"], expected_runs, "format={format}");
    }
    Ok(())
}

fn session_jsonl_fixture(kind: &str) -> &'static str {
    match kind {
        "codex" => {
            r#"{"timestamp":"2026-08-03T08:15:11.000Z","type":"session_meta","payload":{"id":"sess-cli","cwd":"/tmp/demo"}}
{"timestamp":"2026-08-03T08:15:12.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"hi"}]}}
{"timestamp":"2026-08-03T08:15:13.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"ok"}]}}
"#
        }
        "claude-code" => {
            r#"{"type":"user","sessionId":"claude-cli","cwd":"/tmp/app","timestamp":"2026-08-03T08:00:00.000Z","message":{"role":"user","content":"hi"}}
{"type":"assistant","sessionId":"claude-cli","timestamp":"2026-08-03T08:00:01.000Z","message":{"content":[{"type":"text","text":"ok"}]}}
"#
        }
        other => panic!("unknown session fixture {other}"),
    }
}

#[tokio::test]
async fn import_and_query_codex_session_jsonl() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let input = temp.path().join("rollout-sess-cli.jsonl");
    fs::write(&input, session_jsonl_fixture("codex"))?;
    let output = temp.path().join("imported");

    let cli = Cli::try_parse_from([
        "pchronicle",
        "import",
        "--from",
        input.to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
    ])?;
    let mut stdout = Vec::new();
    run(cli, false, &mut stdout, &mut Vec::new()).await?;
    let response: Value = serde_json::from_slice(&stdout)?;
    assert_eq!(response["format"], "codex");
    assert_eq!(response["trajectories"], 1);

    let cli = Cli::try_parse_from([
        "pchronicle",
        "query",
        output.to_str().unwrap(),
        "SELECT COUNT(*) AS runs FROM dataset.runs",
        "--format",
        "jsonl",
    ])?;
    let mut stdout = Vec::new();
    run(cli, false, &mut stdout, &mut Vec::new()).await?;
    let count: Value = serde_json::from_slice(&stdout)?;
    assert_eq!(count["runs"], 1);
    Ok(())
}

#[tokio::test]
async fn query_reads_codex_and_claude_code_session_directories() -> Result<()> {
    let temp = tempfile::tempdir()?;
    for (kind, filename, expected_session) in [
        ("codex", "rollout-direct.jsonl", "sess-cli"),
        ("claude-code", "claude-direct.jsonl", "claude-cli"),
    ] {
        let dir = temp.path().join(kind);
        fs::create_dir(&dir)?;
        fs::write(dir.join(filename), session_jsonl_fixture(kind))?;
        let cli = Cli::try_parse_from([
            "pchronicle",
            "query",
            dir.to_str().unwrap(),
            "SELECT session_id FROM dataset.runs",
            "--format",
            "jsonl",
        ])?;
        let mut stdout = Vec::new();
        run(cli, false, &mut stdout, &mut Vec::new()).await?;
        let row: Value = serde_json::from_slice(&stdout)?;
        assert_eq!(row["session_id"], expected_session, "kind={kind}");
    }
    Ok(())
}

#[tokio::test]
async fn query_expands_codex_and_claude_dataset_aliases() -> Result<()> {
    let _env_guard = DATASET_ALIAS_ENV_LOCK.lock().await;
    let temp = tempfile::tempdir()?;
    let codex_home = temp.path().join("codex-home");
    let claude_config = temp.path().join("claude-config");
    let codex_sessions = codex_home.join("sessions");
    let claude_projects = claude_config.join("projects");
    fs::create_dir_all(&codex_sessions)?;
    fs::create_dir_all(&claude_projects)?;
    fs::write(
        codex_sessions.join("rollout-alias.jsonl"),
        session_jsonl_fixture("codex"),
    )?;
    fs::write(
        claude_projects.join("claude-alias.jsonl"),
        session_jsonl_fixture("claude-code"),
    )?;
    let _codex_home = EnvGuard::set("CODEX_HOME", &codex_home);
    let _claude_config = EnvGuard::set("CLAUDE_CONFIG_DIR", &claude_config);

    for (alias, expected_session) in [("@codex", "sess-cli"), ("@claude", "claude-cli")] {
        let cli = Cli::try_parse_from([
            "pchronicle",
            "query",
            alias,
            "SELECT session_id FROM dataset.runs",
            "--format",
            "jsonl",
        ])?;
        let mut stdout = Vec::new();
        run(cli, false, &mut stdout, &mut Vec::new()).await?;
        let row: Value = serde_json::from_slice(&stdout)?;
        assert_eq!(row["session_id"], expected_session, "alias={alias}");
    }
    Ok(())
}

#[tokio::test]
async fn import_expands_codex_alias_from_path() -> Result<()> {
    let _env_guard = DATASET_ALIAS_ENV_LOCK.lock().await;
    let temp = tempfile::tempdir()?;
    let codex_home = temp.path().join("codex-home");
    let sessions = codex_home.join("sessions");
    fs::create_dir_all(&sessions)?;
    fs::write(
        sessions.join("rollout-import-alias.jsonl"),
        session_jsonl_fixture("codex"),
    )?;
    let output = temp.path().join("imported-alias");
    let _codex_home = EnvGuard::set("CODEX_HOME", &codex_home);

    let cli = Cli::try_parse_from([
        "pchronicle",
        "import",
        "--from",
        "@codex",
        "--output",
        output.to_str().unwrap(),
    ])?;
    let mut stdout = Vec::new();
    run(cli, false, &mut stdout, &mut Vec::new()).await?;
    let response: Value = serde_json::from_slice(&stdout)?;
    assert_eq!(response["sources"], 1);
    assert_eq!(response["trajectories"], 1);
    assert!(response.get("format").is_none());

    let cli = Cli::try_parse_from([
        "pchronicle",
        "query",
        output.to_str().unwrap(),
        "SELECT COUNT(*) AS runs FROM dataset.runs",
        "--format",
        "jsonl",
    ])?;
    let mut stdout = Vec::new();
    run(cli, false, &mut stdout, &mut Vec::new()).await?;
    let count: Value = serde_json::from_slice(&stdout)?;
    assert_eq!(count["runs"], 1);
    Ok(())
}

#[tokio::test]
async fn export_rejects_decode_only_session_formats() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let output = temp.path().join("out.json");
    for format in ["codex", "claude-code"] {
        assert!(
            Cli::try_parse_from([
                "pchronicle",
                "export",
                "--from",
                temp.path().to_str().unwrap(),
                "--output",
                output.to_str().unwrap(),
                "--format",
                format,
            ])
            .is_err()
        );
    }
    Ok(())
}

#[tokio::test]
async fn import_reads_a_bounded_explicit_stdin_stream() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let output = temp.path().join("streamed");
    let input = fs::read(example_source("atif"))?;
    let cli = Cli::try_parse_from([
        "pchronicle",
        "import",
        "--from",
        "-",
        "--input-format",
        "atif",
        "--to",
        output.to_str().unwrap(),
        "--max-input-bytes",
        &input.len().to_string(),
    ])?;
    let mut stdin = input.as_slice();
    let mut stdout = Vec::new();
    run_with_stdin(cli, false, &mut stdin, &mut stdout, &mut Vec::new()).await?;
    let response: Value = serde_json::from_slice(&stdout)?;
    assert_eq!(response["trajectories"], 1);
    assert_eq!(fs::read(output.join("trajectories.atif.json"))?, input);

    for args in [vec![
        "pchronicle",
        "import",
        "--from",
        "-",
        "--to",
        temp.path().join("missing-format").to_str().unwrap(),
    ]] {
        let cli = Cli::try_parse_from(args)?;
        let mut stdin = input.as_slice();
        assert!(
            run_with_stdin(cli, false, &mut stdin, &mut Vec::new(), &mut Vec::new())
                .await
                .is_err()
        );
    }
    Ok(())
}

#[tokio::test]
async fn storyline_import_from_stdin_writes_one_root_store() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let output = temp.path().join("streamed-storyline");
    let input = fs::read(example_source("atif"))?;
    let cli = Cli::try_parse_from([
        "pchronicle",
        "import",
        "--from",
        "-",
        "--stream",
        "--format",
        "atif",
        "--output",
        output.to_str().unwrap(),
        "--output-format",
        "storyline",
    ])?;
    let mut stdin = input.as_slice();
    run_with_stdin(cli, false, &mut stdin, &mut Vec::new(), &mut Vec::new()).await?;

    assert!(output.join("CURRENT").is_file());
    assert!(output.join("generations").is_dir());
    assert!(output.join("objects.lance").is_dir());
    assert!(!output.join("trajectories.atif.json").exists());
    Ok(())
}

#[tokio::test]
async fn import_rejects_invalid_oversized_and_unsupported_input_without_partial_output()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let invalid = temp.path().join("invalid.json");
    fs::write(&invalid, "not json")?;
    let unsupported = temp.path().join("unsupported.md");
    fs::write(&unsupported, "---\nformat: persisting\n---\n")?;

    for (name, input, extra, code) in [
        ("invalid", &invalid, vec![], "invalid_request"),
        (
            "oversized",
            &invalid,
            vec!["--max-input-bytes", "1"],
            "resource_exhausted",
        ),
        ("unsupported", &unsupported, vec![], "unsupported"),
    ] {
        let output = temp.path().join(name);
        let mut args = vec![
            "pchronicle",
            "import",
            "--from",
            input.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
        ];
        args.extend(extra);
        let cli = Cli::try_parse_from(args)?;
        let mut stdout = Vec::new();
        let error = run(cli, false, &mut stdout, &mut Vec::new())
            .await
            .unwrap_err();
        assert!(format!("{error:#}").starts_with(code), "{error:#}");
        assert!(stdout.is_empty());
        assert!(!output.exists());
    }
    assert!(!fs::read_dir(temp.path())?.any(|entry| {
        entry
            .ok()
            .and_then(|entry| entry.file_name().into_string().ok())
            .is_some_and(|name| name.starts_with(".pchronicle-import-"))
    }));
    Ok(())
}

#[tokio::test]
async fn import_is_create_only_and_keeps_duplicate_documents() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let output = temp.path().join("existing");
    fs::create_dir(&output)?;
    fs::write(output.join("sentinel"), "keep")?;
    let cli = Cli::try_parse_from([
        "pchronicle",
        "import",
        "--from",
        example_source("atif").to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
    ])?;
    assert!(
        run(cli, false, &mut Vec::new(), &mut Vec::new())
            .await
            .is_err()
    );
    assert_eq!(fs::read_to_string(output.join("sentinel"))?, "keep");

    let trajectory: Value = serde_json::from_slice(&fs::read(example_source("atif"))?)?;
    let duplicate_input = temp.path().join("duplicates.json");
    fs::write(
        &duplicate_input,
        serde_json::to_vec(&serde_json::json!([trajectory.clone(), trajectory]))?,
    )?;
    let duplicate_output = temp.path().join("duplicates");
    let cli = Cli::try_parse_from([
        "pchronicle",
        "import",
        "--from",
        duplicate_input.to_str().unwrap(),
        "--output",
        duplicate_output.to_str().unwrap(),
        "--format",
        "atif",
    ])?;
    run(cli, false, &mut Vec::new(), &mut Vec::new()).await?;
    assert!(duplicate_output.is_dir());
    assert!(!fs::read_dir(temp.path())?.any(|entry| {
        entry
            .ok()
            .and_then(|entry| entry.file_name().into_string().ok())
            .is_some_and(|name| name.starts_with(".pchronicle-import-"))
    }));

    let mut first: Value = serde_json::from_slice(&fs::read(example_source("atif"))?)?;
    first["trajectory_id"] = serde_json::json!("document-a");
    let mut second = first.clone();
    second["trajectory_id"] = serde_json::json!("document-b");
    let shared_input = temp.path().join("shared-session.json");
    fs::write(
        &shared_input,
        serde_json::to_vec(&serde_json::json!([first, second]))?,
    )?;
    let shared_output = temp.path().join("shared-session");
    let cli = Cli::try_parse_from([
        "pchronicle",
        "import",
        "--from",
        shared_input.to_str().unwrap(),
        "--output",
        shared_output.to_str().unwrap(),
        "--format",
        "atif",
    ])?;
    run(cli, false, &mut Vec::new(), &mut Vec::new()).await?;
    assert!(shared_output.is_dir());
    Ok(())
}

#[tokio::test]
async fn import_validation_does_not_inherit_the_materialization_row_limit() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let input = temp.path().join("large-valid.atif.json");
    let steps = (1..=10_000)
        .map(|step_id| {
            serde_json::json!({
                "step_id": step_id,
                "source": "agent",
                "message": "ok"
            })
        })
        .collect::<Vec<_>>();
    fs::write(
        &input,
        serde_json::to_vec(&serde_json::json!({
            "schema_version": "ATIF-v1.7",
            "trajectory_id": "large-document",
            "session_id": "large-session",
            "agent": {"name": "test", "version": "1"},
            "steps": steps
        }))?,
    )?;

    assert_eq!(
        exchange::validate_import_source(ExchangeFormat::Atif, &input).await?,
        1
    );
    Ok(())
}

#[test]
fn import_publish_primitive_never_replaces_an_existing_target() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let staged = temp.path().join("staged");
    let existing = temp.path().join("existing");
    fs::create_dir(&staged)?;
    fs::create_dir(&existing)?;
    fs::write(staged.join("new"), "new")?;
    fs::write(existing.join("sentinel"), "keep")?;

    assert!(rename_noreplace(&staged, &existing).is_err());
    assert_eq!(fs::read_to_string(existing.join("sentinel"))?, "keep");
    assert_eq!(fs::read_to_string(staged.join("new"))?, "new");
    Ok(())
}

#[tokio::test]
async fn export_filters_complete_trajectories_and_streams_finite_json() -> Result<()> {
    let dataset = example_dataset("openai-messages");
    let cli = Cli::try_parse_from([
        "pchronicle",
        "export",
        "--from",
        dataset.to_str().unwrap(),
        "--output",
        "-",
        "--stream",
        "--format",
        "openai-messages",
        "--session-id",
        "training-002",
        "--where",
        "step_count = 2",
    ])?;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    run(cli, false, &mut stdout, &mut stderr).await?;

    let document: Value = serde_json::from_slice(&stdout)?;
    let rows = document["session_steps"]
        .as_array()
        .context("OpenAI export must contain a session_steps array")?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["session_id"], "training-002");
    assert_eq!(rows[0]["id"], "example-openai-002");
    assert!(String::from_utf8(stderr)?.contains("trajectories=1"));
    Ok(())
}

#[tokio::test]
async fn export_converts_complete_trajectories_between_formats() -> Result<()> {
    let cli = Cli::try_parse_from([
        "pchronicle",
        "export",
        "--from",
        example_dataset("atif").to_str().unwrap(),
        "--output",
        "-",
        "--stream",
        "--format",
        "storyline",
        "--session-id",
        "support-001",
    ])?;
    let mut stdout = Vec::new();
    run(cli, false, &mut stdout, &mut Vec::new()).await?;
    let story: persisting_pchronicle::model::StorylineDocument = serde_json::from_slice(&stdout)?;
    assert_eq!(story.session_id, "support-001");
    assert_eq!(story.turns.len(), 3);
    Ok(())
}

#[tokio::test]
async fn export_is_bounded_create_only_and_has_no_partial_output() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let output = temp.path().join("export.json");
    let dataset = example_dataset("atif");
    fs::write(&output, "sentinel")?;
    let base = [
        "pchronicle",
        "export",
        "--from",
        dataset.to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
        "--format",
        "atif",
    ];
    let cli = Cli::try_parse_from(base)?;
    assert!(
        run(cli, false, &mut Vec::new(), &mut Vec::new())
            .await
            .is_err()
    );
    assert_eq!(fs::read_to_string(&output)?, "sentinel");

    let mut overwrite = base.to_vec();
    overwrite.push("--overwrite");
    let cli = Cli::try_parse_from(overwrite)?;
    run(cli, false, &mut Vec::new(), &mut Vec::new()).await?;
    assert!(fs::read_to_string(&output)?.contains("support-001"));

    let limited = temp.path().join("limited.json");
    let cli = Cli::try_parse_from([
        "pchronicle",
        "export",
        "--from",
        example_dataset("atif").to_str().unwrap(),
        "--output",
        limited.to_str().unwrap(),
        "--format",
        "atif",
        "--max-output-bytes",
        "8",
    ])?;
    let error = run(cli, false, &mut Vec::new(), &mut Vec::new())
        .await
        .unwrap_err();
    assert!(
        error.to_string().starts_with("resource_exhausted:"),
        "{error:#}"
    );
    assert!(error.to_string().contains("exact export"), "{error:#}");
    assert!(!limited.exists());
    assert!(!fs::read_dir(temp.path())?.any(|entry| {
        entry
            .ok()
            .and_then(|entry| entry.file_name().into_string().ok())
            .is_some_and(|name| name.starts_with(".pchronicle-export-"))
    }));
    Ok(())
}

#[tokio::test]
async fn export_stream_preserves_an_ordinary_writer_error_source() -> Result<()> {
    struct FailingWriter;

    impl Write for FailingWriter {
        fn write(&mut self, _buffer: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "export-writer-source-sentinel",
            ))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let cli = Cli::try_parse_from([
        "pchronicle",
        "export",
        "--from",
        example_dataset("atif").to_str().unwrap(),
        "--output",
        "-",
        "--stream",
        "--format",
        "atif",
    ])?;
    let error = run(cli, false, &mut FailingWriter, &mut Vec::new())
        .await
        .unwrap_err();
    let source = error
        .chain()
        .find_map(|source| source.downcast_ref::<std::io::Error>())
        .context("export stream failure did not retain its I/O source")?;
    assert_eq!(source.kind(), std::io::ErrorKind::BrokenPipe);
    assert_eq!(source.to_string(), "export-writer-source-sentinel");
    Ok(())
}

#[tokio::test]
async fn export_normalized_budgets_are_resource_exhausted_without_partial_output() -> Result<()> {
    let temp = tempfile::tempdir()?;

    let trajectory_limited = temp.path().join("trajectory-limited.json");
    let cli = Cli::try_parse_from([
        "pchronicle",
        "export",
        "--from",
        example_dataset("openai-messages").to_str().unwrap(),
        "--output",
        trajectory_limited.to_str().unwrap(),
        "--format",
        "openai-messages",
        "--where",
        "TRUE",
        "--max-trajectories",
        "1",
    ])?;
    let error = run(cli, false, &mut Vec::new(), &mut Vec::new())
        .await
        .unwrap_err();
    assert!(
        error.to_string().starts_with("resource_exhausted:"),
        "{error:#}"
    );
    assert!(error.to_string().contains("max_trajectories"), "{error:#}");
    assert!(!trajectory_limited.exists());

    let exact_trajectory_limited = temp.path().join("exact-trajectory-limited.json");
    let cli = Cli::try_parse_from([
        "pchronicle",
        "export",
        "--from",
        example_dataset("openai-messages").to_str().unwrap(),
        "--output",
        exact_trajectory_limited.to_str().unwrap(),
        "--format",
        "openai-messages",
        "--max-trajectories",
        "1",
    ])?;
    let error = run(cli, false, &mut Vec::new(), &mut Vec::new())
        .await
        .unwrap_err();
    assert!(
        error.to_string().starts_with("resource_exhausted:"),
        "{error:#}"
    );
    assert!(error.to_string().contains("max_trajectories"), "{error:#}");
    assert!(!exact_trajectory_limited.exists());

    let byte_limited = temp.path().join("normalized-byte-limited.json");
    let cli = Cli::try_parse_from([
        "pchronicle",
        "export",
        "--from",
        example_dataset("atif").to_str().unwrap(),
        "--output",
        byte_limited.to_str().unwrap(),
        "--format",
        "atif",
        "--where",
        "TRUE",
        "--max-output-bytes",
        "512",
    ])?;
    let error = run(cli, false, &mut Vec::new(), &mut Vec::new())
        .await
        .unwrap_err();
    assert!(
        error.to_string().starts_with("resource_exhausted:"),
        "{error:#}"
    );
    assert!(error.to_string().contains("normalized export"), "{error:#}");
    assert!(!byte_limited.exists());
    Ok(())
}

#[tokio::test]
async fn export_validates_stream_filters_and_strict_conversion() -> Result<()> {
    for args in [
        vec![
            "pchronicle",
            "export",
            "--from",
            example_dataset("atif").to_str().unwrap(),
            "--to",
            "-",
            "--output-format",
            "storyline",
            "--strict",
        ],
        vec![
            "pchronicle",
            "export",
            "--from",
            example_dataset("atif").to_str().unwrap(),
            "--to",
            "-",
            "--output-format",
            "atif",
            "--where",
            "DELETE FROM dataset.runs",
        ],
    ] {
        let cli = Cli::try_parse_from(args)?;
        let mut stdout = Vec::new();
        assert!(run(cli, false, &mut stdout, &mut Vec::new()).await.is_err());
        assert!(stdout.is_empty());
    }
    Ok(())
}

#[test]
fn rejects_credentials_and_signed_queries() {
    assert!(normalize_and_validate_dataset_uri("s3://user:secret@bucket/path").is_err());
    assert!(normalize_and_validate_dataset_uri("s3://bucket/path?X-Amz-Signature=secret").is_err());
    assert!(normalize_and_validate_dataset_uri("https://example.com/data").is_err());
}

#[test]
fn preserves_uri_roots_while_trimming_prefixes() {
    assert_eq!(
        normalize_and_validate_dataset_uri("file:///").unwrap(),
        "file:///"
    );
    assert_eq!(
        normalize_and_validate_dataset_uri("s3://bucket///").unwrap(),
        "s3://bucket"
    );
}

#[test]
fn expand_dataset_alias_maps_vendor_roots_and_suffixes() {
    let _env_guard = DATASET_ALIAS_ENV_LOCK.blocking_lock();
    let temp = tempfile::tempdir().unwrap();
    let codex_home = temp.path().join("codex-home");
    let claude_config = temp.path().join("claude-config");
    let home = temp.path().join("home");
    let _codex_home = EnvGuard::set("CODEX_HOME", &codex_home);
    let _claude_config = EnvGuard::set("CLAUDE_CONFIG_DIR", &claude_config);
    let _home = EnvGuard::set("HOME", &home);

    assert_eq!(
        expand_dataset_alias("@codex").unwrap(),
        codex_home.join("sessions").to_string_lossy()
    );
    assert_eq!(
        expand_dataset_alias("@codex/").unwrap(),
        codex_home.join("sessions").to_string_lossy()
    );
    assert_eq!(
        expand_dataset_alias("@codex/2026/05/29").unwrap(),
        codex_home
            .join("sessions")
            .join("2026")
            .join("05")
            .join("29")
            .to_string_lossy()
    );
    assert_eq!(
        expand_dataset_alias("@codex//etc").unwrap(),
        codex_home.join("sessions").join("etc").to_string_lossy()
    );
    assert_eq!(
        expand_dataset_alias("@claude").unwrap(),
        claude_config.join("projects").to_string_lossy()
    );
    assert_eq!(
        expand_dataset_alias("@claude-code").unwrap(),
        claude_config.join("projects").to_string_lossy()
    );
}

#[test]
fn expand_dataset_alias_treats_empty_env_as_unset_and_joins_relative_env() {
    let _env_guard = DATASET_ALIAS_ENV_LOCK.blocking_lock();
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let _home = EnvGuard::set("HOME", &home);
    let _codex_home = EnvGuard::set("CODEX_HOME", "");
    assert_eq!(
        expand_dataset_alias("@codex").unwrap(),
        home.join(".codex").join("sessions").to_string_lossy()
    );
    drop(_codex_home);
    let _codex_home = EnvGuard::unset("CODEX_HOME");
    assert_eq!(
        expand_dataset_alias("@codex").unwrap(),
        home.join(".codex").join("sessions").to_string_lossy()
    );

    let _codex_home = EnvGuard::set("CODEX_HOME", "relative-codex-home");
    let expected = std::env::current_dir()
        .unwrap()
        .join("relative-codex-home")
        .join("sessions");
    assert_eq!(
        expand_dataset_alias("@codex").unwrap(),
        expected.to_string_lossy()
    );
}

#[test]
fn expand_dataset_alias_rejects_unknown_parent_and_scheme_forms() {
    let error = expand_dataset_alias("@unknown").unwrap_err().to_string();
    assert!(
        error.contains("unknown dataset alias '@unknown'"),
        "{error}"
    );
    assert!(error.contains("expected @codex or @claude"), "{error}");
    assert!(expand_dataset_alias("@").is_err());
    assert!(expand_dataset_alias("@codex/../.ssh").is_err());
    assert!(expand_dataset_alias("@codex/foo/../bar").is_err());
    assert!(expand_dataset_alias("@codex://sessions").is_err());
}

#[test]
fn expand_dataset_alias_leaves_non_descriptor_paths_untouched() {
    assert_eq!(expand_dataset_alias("./@codex").unwrap(), "./@codex");
    assert_eq!(expand_dataset_alias("-").unwrap(), "-");
    assert_eq!(
        expand_dataset_alias("s3://bucket/@codex").unwrap(),
        "s3://bucket/@codex"
    );
}

#[test]
fn normalize_and_validate_dataset_uri_expands_alias_and_rejects_unknown() {
    let _env_guard = DATASET_ALIAS_ENV_LOCK.blocking_lock();
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    fs::create_dir(&sessions).unwrap();
    let _codex_home = EnvGuard::set("CODEX_HOME", temp.path());

    let normalized = normalize_and_validate_dataset_uri("@codex").unwrap();
    assert_eq!(
        normalized,
        sessions.canonicalize().unwrap().to_string_lossy()
    );

    let error = normalize_and_validate_dataset_uri("@foo")
        .unwrap_err()
        .to_string();
    assert!(error.contains("unknown dataset alias '@foo'"), "{error}");
}

fn serve_args_with_storage(storage: Vec<String>) -> ServeArgs {
    ServeArgs {
        command: None,
        config: None,
        storage,
        positional_storage: Vec::new(),
        listen: None,
        control: None,
        open: false,
        gateway: None,
        gateway_config: None,
        gateway_dataset: None,
        gateway_split: None,
        gateway_split_idle_seconds: 1800,
        gateway_state: None,
        gateway_object_store_manifest_mode: GatewayObjectStoreManifestMode::default(),
        gateway_stream_markdown: false,
        debug: false,
        catalog_config: None,
        catalog_query_worker: false,
    }
}

#[test]
fn serve_storage_expands_dataset_aliases() -> Result<()> {
    let _env_guard = DATASET_ALIAS_ENV_LOCK.blocking_lock();
    let temp = tempfile::tempdir()?;
    let sessions = temp.path().join("sessions");
    fs::create_dir(&sessions)?;
    let _codex_home = EnvGuard::set("CODEX_HOME", temp.path());

    let config = resolve_serve_config(&serve_args_with_storage(vec!["@codex".into()]))?;
    assert_eq!(config.datasets.len(), 1);
    assert_eq!(config.datasets[0].name, "default");
    assert_eq!(config.datasets[0].uri, sessions.to_string_lossy());

    let named = resolve_serve_config(&serve_args_with_storage(vec!["vendor=@codex".into()]))?;
    assert_eq!(named.datasets[0].name, "vendor");
    assert_eq!(named.datasets[0].uri, sessions.to_string_lossy());
    Ok(())
}

#[test]
fn serve_storage_config_uses_report_error_policy() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let dataset = temp.path().join("dataset");
    fs::create_dir(&dataset)?;

    let args = serve_args_with_storage(vec![dataset.to_string_lossy().into_owned()]);
    let config = resolve_serve_config(&args)?;
    assert_eq!(config.datasets.len(), 1);
    assert_eq!(config.datasets[0].name, "default");
    assert_eq!(config.default_dataset.as_deref(), Some("default"));
    assert_eq!(
        config.catalog_options.error_policy,
        CatalogErrorPolicy::Report
    );
    Ok(())
}

#[test]
fn serve_repeated_storage_mounts_basename_datasets() -> Result<()> {
    let args = serve_args_with_storage(vec!["./tmp".into(), "./data/evals".into()]);
    let config = resolve_serve_config(&args)?;
    let mounts: Vec<_> = config
        .datasets
        .iter()
        .map(|dataset| (dataset.name.as_str(), dataset.uri.as_str()))
        .collect();
    assert_eq!(mounts, vec![("tmp", "./tmp"), ("evals", "./data/evals")]);
    assert_eq!(config.default_dataset, None);
    assert_eq!(
        config.catalog_options.error_policy,
        CatalogErrorPolicy::Report
    );
    Ok(())
}

#[test]
fn serve_storage_name_uri_overrides_basename() -> Result<()> {
    let args = serve_args_with_storage(vec!["default=./tmp".into(), "archive=./data/evals".into()]);
    let config = resolve_serve_config(&args)?;
    let mounts: Vec<_> = config
        .datasets
        .iter()
        .map(|dataset| (dataset.name.as_str(), dataset.uri.as_str()))
        .collect();
    assert_eq!(
        mounts,
        vec![("default", "./tmp"), ("archive", "./data/evals")]
    );
    assert_eq!(config.default_dataset.as_deref(), Some("default"));
    Ok(())
}

#[test]
fn serve_single_named_storage_keeps_explicit_name() -> Result<()> {
    let args = serve_args_with_storage(vec!["evals=./data".into()]);
    let config = resolve_serve_config(&args)?;
    assert_eq!(config.datasets.len(), 1);
    assert_eq!(config.datasets[0].name, "evals");
    assert_eq!(config.datasets[0].uri, "./data");
    assert_eq!(config.default_dataset.as_deref(), Some("evals"));
    Ok(())
}

#[test]
fn serve_storage_sanitizes_hyphenated_basename() -> Result<()> {
    let args = serve_args_with_storage(vec!["./tmp".into(), "./trajectory-data".into()]);
    let config = resolve_serve_config(&args)?;
    let names: Vec<_> = config
        .datasets
        .iter()
        .map(|dataset| dataset.name.as_str())
        .collect();
    assert_eq!(names, vec!["tmp", "trajectory_data"]);
    Ok(())
}

#[test]
fn serve_storage_derives_object_uri_basename() -> Result<()> {
    let args = serve_args_with_storage(vec![
        "s3://bucket/archive".into(),
        "s3://other/evals".into(),
    ]);
    let config = resolve_serve_config(&args)?;
    let mounts: Vec<_> = config
        .datasets
        .iter()
        .map(|dataset| (dataset.name.as_str(), dataset.uri.as_str()))
        .collect();
    assert_eq!(
        mounts,
        vec![
            ("archive", "s3://bucket/archive"),
            ("evals", "s3://other/evals")
        ]
    );
    Ok(())
}

#[test]
fn serve_storage_keeps_object_uri_with_equals_in_path() -> Result<()> {
    let args = serve_args_with_storage(vec!["./tmp".into(), "s3://bucket/key=value/path".into()]);
    let config = resolve_serve_config(&args)?;
    assert_eq!(config.datasets[1].name, "path");
    assert_eq!(config.datasets[1].uri, "s3://bucket/key=value/path");
    Ok(())
}

#[test]
fn serve_storage_rejects_duplicate_names() {
    let error = resolve_serve_config(&serve_args_with_storage(vec![
        "./a/data".into(),
        "./b/data".into(),
    ]))
    .unwrap_err();
    let message = format!("{error:#}");
    assert!(
        message.contains("unique") || message.contains("duplicate"),
        "unexpected error: {message}"
    );
}

#[test]
fn serve_storage_rejects_underivable_basename() {
    let error = resolve_serve_config(&serve_args_with_storage(vec!["./tmp".into(), ".".into()]))
        .unwrap_err();
    let message = format!("{error:#}");
    assert!(
        message.contains("NAME=DATASET"),
        "unexpected error: {message}"
    );
}

#[test]
fn serve_control_storage_uses_default_mount() -> Result<()> {
    let config = resolve_serve_config(&serve_args_with_storage(vec![
        "default=./tmp".into(),
        "evals=./data".into(),
    ]))?;
    assert_eq!(control_storage_uri(&config)?, "./tmp");
    Ok(())
}

#[test]
fn serve_control_storage_requires_default_mount() {
    let config = resolve_serve_config(&serve_args_with_storage(vec![
        "./tmp".into(),
        "./data".into(),
    ]))
    .unwrap();
    let error = control_storage_uri(&config).unwrap_err();
    let message = format!("{error:#}");
    assert!(
        message.contains("default=DATASET"),
        "unexpected error: {message}"
    );
}

#[test]
fn warehouse_config_normalizes_mounts_and_selects_default() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let first = temp.path().join("first");
    let second = temp.path().join("second");
    fs::create_dir_all(&first)?;
    fs::create_dir_all(&second)?;
    let config_path = temp.path().join("warehouse.toml");
    fs::write(
        &config_path,
        format!(
            r#"
default_dataset = "archive"

[[datasets]]
name = "live"
uri = {first:?}

[[datasets]]
name = "archive"
uri = {second:?}
"#,
            first = first.to_string_lossy(),
            second = second.to_string_lossy(),
        ),
    )?;

    let config = load_warehouse_config(&config_path)?;
    assert_eq!(config.datasets.len(), 2);
    assert_eq!(config.default_dataset.as_deref(), Some("archive"));
    assert_eq!(
        config.catalog_options.error_policy,
        CatalogErrorPolicy::Report
    );
    assert_eq!(
        config.datasets[0].uri,
        fs::canonicalize(first)?.to_string_lossy()
    );
    assert_eq!(
        config.datasets[1].uri,
        fs::canonicalize(second)?.to_string_lossy()
    );
    Ok(())
}

#[test]
fn warehouse_config_expands_dataset_aliases() -> Result<()> {
    let _env_guard = DATASET_ALIAS_ENV_LOCK.blocking_lock();
    let temp = tempfile::tempdir()?;
    let sessions = temp.path().join("sessions");
    fs::create_dir(&sessions)?;
    let _codex_home = EnvGuard::set("CODEX_HOME", temp.path());
    let config_path = temp.path().join("warehouse.toml");
    fs::write(
        &config_path,
        r#"
[[datasets]]
name = "vendor"
uri = "@codex"
"#,
    )?;

    let config = load_warehouse_config(&config_path)?;
    assert_eq!(
        config.datasets[0].uri,
        fs::canonicalize(&sessions)?.to_string_lossy()
    );
    Ok(())
}

#[test]
fn warehouse_config_rejects_unsafe_or_ambiguous_mounts() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let dataset = temp.path().join("dataset");
    fs::create_dir(&dataset)?;

    for (name, body, expected) in [
        (
            "duplicate.toml",
            format!(
                "[[datasets]]\nname='live'\nuri={dataset:?}\n[[datasets]]\nname='live'\nuri={dataset:?}\n",
                dataset = dataset.to_string_lossy()
            ),
            "unique",
        ),
        (
            "missing-default.toml",
            format!(
                "default_dataset='missing'\n[[datasets]]\nname='live'\nuri={dataset:?}\n",
                dataset = dataset.to_string_lossy()
            ),
            "not mounted",
        ),
        (
            "credential.toml",
            "[[datasets]]\nname='live'\nuri='s3://user:secret@bucket/path'\n".into(),
            "credentials",
        ),
        (
            "unknown.toml",
            format!(
                "listen='0.0.0.0:80'\n[[datasets]]\nname='live'\nuri={dataset:?}\n",
                dataset = dataset.to_string_lossy()
            ),
            "unknown field",
        ),
    ] {
        let path = temp.path().join(name);
        fs::write(&path, body)?;
        let error = load_warehouse_config(&path).unwrap_err();
        assert!(
            format!("{error:#}").contains(expected),
            "unexpected error for {name}: {error:#}"
        );
    }
    Ok(())
}

#[test]
fn serve_cli_requires_one_dataset_source_and_an_explicit_service() -> Result<()> {
    for arguments in [
        vec![
            "pchronicle",
            "serve",
            "--storage",
            "/tmp/data",
            "--control",
            "127.0.0.1:0",
        ],
        vec![
            "pchronicle",
            "serve",
            "--storage",
            "/tmp/data",
            "--listen",
            "127.0.0.1:0",
        ],
        vec![
            "pchronicle",
            "serve",
            "--storage",
            "/tmp/data",
            "--gateway-config",
            "gateway.toml",
            "--gateway-dataset",
            "/tmp/data",
        ],
        vec![
            "pchronicle",
            "serve",
            "--warehouse-config",
            "warehouse.toml",
            "--listen",
            "127.0.0.1:0",
        ],
        vec![
            "pchronicle",
            "serve",
            "--storage",
            "./tmp",
            "--storage",
            "./data",
            "--listen",
            "127.0.0.1:0",
        ],
        vec!["pchronicle", "serve", "--storage", "/tmp/data"],
        vec![
            "pchronicle",
            "serve",
            "--warehouse-config",
            "warehouse.toml",
        ],
        vec!["pchronicle", "serve", "s3://my-bucket/prefix"],
        vec![
            "pchronicle",
            "serve",
            "s3://my-bucket/prefix",
            "--listen",
            "127.0.0.1:0",
        ],
        vec![
            "pchronicle",
            "serve",
            "s3://my-bucket/prefix",
            "--control",
            "127.0.0.1:0",
        ],
        vec![
            "pchronicle",
            "serve",
            "--storage",
            "./tmp",
            "s3://my-bucket/other",
            "--listen",
            "127.0.0.1:0",
        ],
    ] {
        assert!(
            Cli::try_parse_from(arguments.clone()).is_ok(),
            "valid serve arguments rejected: {arguments:?}"
        );
    }

    for arguments in [
        vec![
            "pchronicle",
            "serve",
            "--storage",
            "a",
            "--warehouse-config",
            "b",
            "--listen",
            "127.0.0.1:0",
        ],
        vec![
            "pchronicle",
            "serve",
            "--warehouse-config",
            "warehouse.toml",
            "--control",
            "127.0.0.1:0",
        ],
        vec![
            "pchronicle",
            "serve",
            "--storage",
            "/tmp/data",
            "--open",
            "--control",
            "127.0.0.1:0",
        ],
    ] {
        assert!(
            Cli::try_parse_from(arguments.clone()).is_err(),
            "invalid serve arguments accepted: {arguments:?}"
        );
    }
    Ok(())
}

#[test]
fn serve_positional_uri_is_equivalent_to_storage() -> Result<()> {
    let cli = Cli::try_parse_from(["pchronicle", "serve", "s3://my-bucket/prefix"])?;
    let Command::Serve(args) = cli.command else {
        unreachable!("serve command parsed as another variant")
    };
    let config = resolve_serve_config(&args)?;
    assert_eq!(config.datasets.len(), 1);
    assert_eq!(config.datasets[0].name, "default");
    assert_eq!(config.datasets[0].uri, "s3://my-bucket/prefix");
    assert_eq!(config.default_dataset.as_deref(), Some("default"));
    Ok(())
}

#[test]
fn serve_without_listen_defaults_warehouse_to_loopback_ephemeral_port() -> Result<()> {
    let cli = Cli::try_parse_from(["pchronicle", "serve", "--storage", "/tmp/data"])?;
    let Command::Serve(args) = cli.command else {
        unreachable!("serve command parsed as another variant")
    };
    assert_eq!(args.listen, None);
    assert_eq!(
        warehouse_listen(&args),
        Some("127.0.0.1:0".parse::<SocketAddr>()?)
    );

    let control_only = Cli::try_parse_from([
        "pchronicle",
        "serve",
        "--storage",
        "/tmp/data",
        "--control",
        "127.0.0.1:0",
    ])?;
    let Command::Serve(control_only) = control_only.command else {
        unreachable!("serve command parsed as another variant")
    };
    assert_eq!(warehouse_listen(&control_only), None);
    Ok(())
}

#[test]
fn echo_cli_uses_a_normal_loopback_default() -> Result<()> {
    let cli = Cli::try_parse_from(["pchronicle", "echo"])?;
    let Command::Echo(args) = cli.command else {
        unreachable!("echo command parsed as another variant")
    };
    assert_eq!(args.listen, "127.0.0.1:19080".parse::<SocketAddr>()?);
    assert_eq!(args.encoding, EchoEncoding::Plain);
    Ok(())
}

#[test]
fn serve_gateway_options_are_explicit_and_scoped() -> Result<()> {
    let ingest = Cli::try_parse_from([
        "pchronicle",
        "serve",
        "--gateway",
        "auto",
        "--gateway-dataset",
        "/tmp/captures",
        "--gateway-split",
        "{user}/{date}/{hour}",
    ])?;
    let Command::Serve(ingest) = ingest.command else {
        unreachable!("serve command parsed as another variant")
    };
    assert_eq!(ingest.gateway, Some("127.0.0.1:0".parse::<SocketAddr>()?));
    assert_eq!(ingest.gateway_config, None);
    assert_eq!(
        ingest.gateway_split.as_deref(),
        Some("{user}/{date}/{hour}")
    );
    assert_eq!(ingest.gateway_split_idle_seconds, 30 * 60);

    let idle_override = Cli::try_parse_from([
        "pchronicle",
        "serve",
        "--gateway",
        "auto",
        "--gateway-dataset",
        "/tmp/captures",
        "--gateway-split-idle",
        "45s",
    ])?;
    let Command::Serve(idle_override) = idle_override.command else {
        unreachable!("serve command parsed as another variant")
    };
    assert_eq!(idle_override.gateway_split_idle_seconds, 45);

    let cli = Cli::try_parse_from([
        "pchronicle",
        "serve",
        "--warehouse-config",
        "warehouse.toml",
        "--gateway-config",
        "gateway.toml",
        "--gateway-dataset",
        "s3://capture-bucket/events",
        "--gateway-state",
        ".gateway-state",
        "--gateway-object-store-manifest-mode",
        "single-writer",
        "--gateway-stream-markdown",
        "--gateway-debug",
    ])?;
    let Command::Serve(args) = cli.command else {
        unreachable!("serve command parsed as another variant")
    };
    assert_eq!(args.gateway, None);
    assert_eq!(args.gateway_config, Some(PathBuf::from("gateway.toml")));
    assert_eq!(
        args.gateway_dataset.as_deref(),
        Some("s3://capture-bucket/events")
    );
    assert_eq!(args.gateway_state, Some(PathBuf::from(".gateway-state")));
    assert_eq!(
        args.gateway_object_store_manifest_mode,
        GatewayObjectStoreManifestMode::SingleWriter
    );
    assert!(args.gateway_stream_markdown);
    assert!(args.debug);

    assert!(
        Cli::try_parse_from([
            "pchronicle",
            "serve",
            "--warehouse-config",
            "warehouse.toml",
            "--gateway-dataset",
            "captures",
        ])
        .is_err()
    );
    assert!(
        Cli::try_parse_from([
            "pchronicle",
            "serve",
            "--warehouse-config",
            "warehouse.toml",
            "--gateway-debug",
        ])
        .is_err()
    );

    let cli = Cli::try_parse_from([
        "pchronicle",
        "serve",
        "--warehouse-config",
        "warehouse.toml",
        "--gateway-config",
        "gateway.toml",
        "--gateway-dataset",
        "/tmp/captures",
        "--gateway-debug",
    ])?;
    let Command::Serve(args) = cli.command else {
        unreachable!("serve command parsed as another variant")
    };
    assert!(args.debug);
    Ok(())
}

#[test]
fn gateway_dataset_uri_is_auto_mounted_and_deduplicated() -> Result<()> {
    let mut config =
        server::ChronicleServerConfig::mounted(vec![DatasetMount::new("evals", "/tmp/evals")?])?;
    ensure_gateway_mount(&mut config, "/tmp/captures".into())?;
    assert_eq!(config.datasets.len(), 2);
    assert_eq!(config.datasets[1].name, "gateway");
    assert_eq!(config.datasets[1].uri, "/tmp/captures");
    ensure_gateway_mount(&mut config, "/tmp/captures".into())?;
    assert_eq!(config.datasets.len(), 2);
    Ok(())
}

#[test]
fn embedded_gateway_rejects_public_listeners() {
    let error = parse_gateway_listener("0.0.0.0:8787", "Gateway").unwrap_err();
    assert!(error.to_string().contains("loopback"));
    assert!(parse_gateway_listener("127.0.0.1:0", "Gateway").is_ok());
    assert!(parse_gateway_bind("0.0.0.0:0").is_err());
    assert_eq!(
        parse_gateway_bind("auto").unwrap(),
        "127.0.0.1:0".parse::<SocketAddr>().unwrap()
    );
}

#[tokio::test]
async fn embedded_gateway_forwards_and_persists_canonical_events() -> Result<()> {
    let dataset = tempfile::tempdir()?;
    let state = tempfile::tempdir()?;
    let upstream_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let upstream_addr = upstream_listener.local_addr()?;
    let (upstream_stop_tx, upstream_stop_rx) = tokio::sync::oneshot::channel::<()>();
    let upstream = tokio::spawn(async move {
        let app = axum::Router::new().route(
            "/v1/chat/completions",
            axum::routing::post(|| async {
                axum::Json(serde_json::json!({
                    "id": "chatcmpl-pchronicle",
                    "object": "chat.completion",
                    "choices": [{
                        "index": 0,
                        "message": {"role": "assistant", "content": "stored"},
                        "finish_reason": "stop"
                    }]
                }))
            }),
        );
        axum::serve(upstream_listener, app)
            .with_graceful_shutdown(async {
                let _ = upstream_stop_rx.await;
            })
            .await
    });

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let gateway_addr = listener.local_addr()?;
    let admin_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let admin_addr = admin_listener.local_addr()?;
    let mut config = persisting_gateway::config::ProxyConfig::from_toml_str(&format!(
        r#"
listen = "127.0.0.1:0"
admin_listen = "127.0.0.1:0"
agent_id = "test-agent"

[[models]]
name = "*"
upstream = "http://{upstream_addr}/v1"
"#
    ))?;
    config.listen = gateway_addr.to_string();
    config.admin_listen = admin_addr.to_string();
    let (sink, writer) =
        gateway_capture::gateway_capture_sink(&dataset.path().to_string_lossy(), &config.agent_id)?;
    let warehouse_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let warehouse_addr = warehouse_listener.local_addr()?;
    let warehouse_config = server::ChronicleServerConfig::mounted(vec![DatasetMount::default(
        dataset.path().to_string_lossy(),
    )?])?;
    let prepared_gateway = PreparedGateway::Proxy(Box::new(PreparedProxyGateway {
        config,
        state_dir: state.path().to_path_buf(),
        dataset_uri: dataset.path().to_string_lossy().into_owned(),
        split: None,
        stream_markdown: false,
        listener,
        admin_listener,
        sink,
        writer,
    }));
    let (serve_stop_tx, serve_stop_rx) = tokio::sync::oneshot::channel::<()>();
    let serve = tokio::spawn(async move {
        serve_warehouse_and_gateway(
            warehouse_config,
            warehouse_listener,
            prepared_gateway,
            async {
                let _ = serve_stop_rx.await;
            },
        )
        .await
    });

    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()?;
    let health = client
        .get(format!("http://{warehouse_addr}/api/health"))
        .send()
        .await?;
    assert_eq!(health.status(), reqwest::StatusCode::OK);
    assert_eq!(health.json::<Value>().await?["mode"], "read_only");
    let response = client
        .post(format!("http://{gateway_addr}/v1/chat/completions"))
        .header("content-type", "application/json")
        .header("x-persisting-session-id", "session-42")
        .body(r#"{"model":"test","messages":[{"role":"user","content":"keep me"}]}"#)
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(
        response.json::<Value>().await?["choices"][0]["message"]["content"],
        "stored"
    );

    let _ = serve_stop_tx.send(());
    tokio::time::timeout(Duration::from_secs(10), serve)
        .await
        .context("pChronicle serve shutdown timed out")???;
    let _ = upstream_stop_tx.send(());
    tokio::time::timeout(Duration::from_secs(10), upstream)
        .await
        .context("mock upstream shutdown timed out")???;

    let snapshot = Arc::new(
        DatasetCatalogSnapshot::discover(
            vec![DatasetMount::default(dataset.path().to_string_lossy())?],
            Some(DEFAULT_DATASET_NAME.into()),
            CatalogSnapshotOptions::default(),
        )
        .await?,
    );
    let engine = snapshot.query_engine(Default::default()).await?;
    let rows = engine
        .query_jsonl(
            "SELECT kind, COUNT(*) AS count FROM dataset.events GROUP BY kind ORDER BY kind",
        )
        .await?;
    let rows = rows
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(rows[0]["kind"], "llm.request");
    assert_eq!(rows[0]["count"], 1);
    assert_eq!(rows[1]["kind"], "llm.response");
    assert_eq!(rows[1]["count"], 1);
    Ok(())
}

#[test]
fn byte_units_are_stable() {
    assert_eq!(format_bytes(12), "12 B");
    assert_eq!(format_bytes(1536), "1.5 KiB");
}
