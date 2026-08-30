use std::process::{Command, Output};

#[cfg(unix)]
use std::fs::{self, File};
#[cfg(unix)]
use std::io::{self, Read};
#[cfg(unix)]
use std::os::fd::FromRawFd;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::path::Path;
#[cfg(unix)]
use std::process::Stdio;

use anyhow::{Context, Result};
use serde_json::Value;

fn pchronicle(args: &[&str]) -> Result<Output> {
    Command::new(env!("CARGO_BIN_EXE_pchronicle"))
        .args(args)
        .output()
        .context("execute pchronicle binary")
}

#[test]
fn version_reports_the_package_version() -> Result<()> {
    let output = pchronicle(&["--version"])?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    assert_eq!(
        String::from_utf8(output.stdout)?.trim(),
        format!("pchronicle {}", env!("CARGO_PKG_VERSION"))
    );
    Ok(())
}

#[test]
fn help_exposes_the_supported_product_surface() -> Result<()> {
    let output = pchronicle(&["--help"])?;
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout)?;
    for command in [
        "onboard", "default", "alias", "ls", "status", "query", "analysis", "agent", "find",
        "import", "export", "serve",
    ] {
        assert!(stdout.contains(command), "help omits {command}: {stdout}");
    }
    for command in ["control", "project", "search", "maintain"] {
        assert!(
            !stdout
                .lines()
                .any(|line| line.trim_start().starts_with(command)),
            "help exposes unsupported command {command}: {stdout}"
        );
    }
    Ok(())
}

#[test]
fn agent_help_explains_startup_controls() -> Result<()> {
    let output = pchronicle(&["agent", "--help"])?;
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout)?;
    for option in ["--ask", "--ask-file", "--no-overview", "--dry-run"] {
        assert!(
            stdout.contains(option),
            "agent help omits {option}: {stdout}"
        );
    }
    assert!(stdout.contains("bounded Dataset status check"), "{stdout}");
    assert!(
        stdout.contains("does not validate Agent installation"),
        "{stdout}"
    );
    for example in [
        "pchronicle agent codex ./dataset",
        "pchronicle agent claude @prod --ask \"Compare model latency\"",
        "pchronicle agent codex ./dataset --ask-file question.txt --no-overview",
        "pchronicle agent codex ./dataset --dry-run",
    ] {
        assert!(
            stdout.contains(example),
            "agent help omits example: {example}"
        );
    }
    assert!(stdout.contains("question text redacted"), "{stdout}");
    Ok(())
}

#[test]
fn serve_help_exposes_only_the_canonical_dataset_surface() -> Result<()> {
    let output = pchronicle(&["serve", "--help"])?;
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout)?;
    assert!(
        stdout.contains("Usage: pchronicle serve [OPTIONS] <[NAME=]DATASET>..."),
        "{stdout}"
    );
    for option in [
        "--listen",
        "--control",
        "--open",
        "--gateway",
        "--gateway-config",
        "--gateway-dataset",
        "--gateway-split",
        "--gateway-state",
        "--gateway-stream-markdown",
        "--gateway-debug",
    ] {
        assert!(
            stdout.contains(option),
            "serve help omits {option}: {stdout}"
        );
    }
    for legacy in [
        "--warehouse-config",
        "--storage",
        "--gateway-object-store-manifest-mode",
    ] {
        assert!(
            !stdout.contains(legacy),
            "serve help exposes compatibility option {legacy}: {stdout}"
        );
    }
    Ok(())
}

#[test]
fn piped_onboard_is_markdown_without_terminal_escapes() -> Result<()> {
    let output = pchronicle(&["onboard"])?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout)?;
    assert!(stdout.starts_with("# pChronicle Onboard\n"), "{stdout}");
    assert!(stdout.contains("## Inspect · 发现 Source"), "{stdout}");
    assert!(stdout.contains("## Query · 先看 Schema"), "{stdout}");
    assert!(stdout.contains("## Formats · 跨格式查询"), "{stdout}");
    assert!(
        stdout.contains("## Find · FTS、字段限定与 JSONB"),
        "{stdout}"
    );
    assert!(
        stdout.contains("--match '$.tags=\"important\"'"),
        "{stdout}"
    );
    assert!(stdout.contains("--output-format storyline"), "{stdout}");
    assert!(stdout.contains("## Exchange · 严格导出"), "{stdout}");
    assert!(stdout.contains("support-ticket.json"), "{stdout}");
    assert!(stdout.contains("support-001"), "{stdout}");
    assert!(stdout.contains("example-code-repair"), "{stdout}");
    assert!(stdout.contains("exact=true"), "{stdout}");
    assert!(stdout.contains("# 完成"), "{stdout}");
    assert!(!stdout.contains('\u{1b}'), "{stdout}");
    Ok(())
}

#[test]
fn onboard_accepts_an_explicit_dataset_read_only() -> Result<()> {
    let dataset = format!("{}/../../examples/data/actf", env!("CARGO_MANIFEST_DIR"));
    let output = pchronicle(&["onboard", &dataset])?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout)?;
    assert!(stdout.contains("code-repair.actf.json"), "{stdout}");
    assert!(stdout.contains("example-code-repair"), "{stdout}");
    assert!(stdout.contains("本次完整流程将实际读取"), "{stdout}");
    Ok(())
}

#[test]
fn onboard_subcommand_navigates_directly_to_one_section() -> Result<()> {
    let dataset = format!("{}/../../examples/data/atif", env!("CARGO_MANIFEST_DIR"));
    let output = pchronicle(&["onboard", "query", &dataset])?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout)?;
    assert!(stdout.starts_with("# pChronicle Onboard · Query"));
    assert!(stdout.contains("DESCRIBE dataset.steps"), "{stdout}");
    assert!(stdout.contains("dataset.tool_calls"), "{stdout}");
    assert!(!stdout.contains("## Inspect ·"), "{stdout}");
    assert!(!stdout.contains("## Exchange ·"), "{stdout}");
    Ok(())
}

#[test]
fn onboard_help_lists_every_section() -> Result<()> {
    let output = pchronicle(&["onboard", "--help"])?;
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout)?;
    for section in [
        "all", "concepts", "inspect", "analyze", "query", "formats", "find", "exchange", "serve",
    ] {
        assert!(
            stdout
                .lines()
                .any(|line| line.trim_start().starts_with(section)),
            "onboard help omits {section}: {stdout}"
        );
    }
    Ok(())
}

#[test]
fn clap_errors_use_exit_code_two_and_do_not_write_stdout() -> Result<()> {
    let output = pchronicle(&["unknown-command"])?;
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr)?;
    assert!(stderr.contains("unrecognized subcommand"), "{stderr}");
    Ok(())
}

#[test]
fn missing_dataset_uses_not_found_exit_code_and_does_not_write_stdout() -> Result<()> {
    let output = pchronicle(&["status", "/definitely/missing/pchronicle-dataset"])?;
    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8(output.stderr)?.starts_with("error[not_found]: "));
    Ok(())
}

#[test]
fn successful_query_keeps_machine_output_on_stdout() -> Result<()> {
    let dataset = format!("{}/../../examples/data/atif", env!("CARGO_MANIFEST_DIR"));
    let output = pchronicle(&[
        "query",
        &dataset,
        "SELECT COUNT(*) AS runs FROM dataset.runs",
        "--format",
        "jsonl",
    ])?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(value["runs"], 1);
    assert!(String::from_utf8(output.stderr)?.contains("datasets=dataset"));
    Ok(())
}

#[cfg(unix)]
#[test]
fn agent_skill_examples_match_the_live_cli_contract() -> Result<()> {
    const SKILL: &str = include_str!("../assets/agent/pchronicle-dataset/SKILL.md");

    let mut blocks = Vec::new();
    let mut current = None::<String>;
    for line in SKILL.lines() {
        match (current.as_mut(), line) {
            (None, "```bash") => current = Some(String::new()),
            (Some(_), "```") => blocks.push(current.take().expect("bash block is active")),
            (Some(block), line) => {
                block.push_str(line);
                block.push('\n');
            }
            (None, _) => {}
        }
    }
    assert!(current.is_none(), "unterminated bash block in Agent skill");
    assert_eq!(blocks.len(), 3, "Agent skill command examples changed");

    let dataset = format!("{}/../../examples/data/atif", env!("CARGO_MANIFEST_DIR"));
    for block in blocks {
        let output = Command::new("/bin/sh")
            .args(["-eu", "-c", &block])
            .env("PCHRONICLE_BIN", env!("CARGO_BIN_EXE_pchronicle"))
            .env("PCHRONICLE_DATASET_URI", &dataset)
            .output()?;
        assert!(
            output.status.success(),
            "Agent skill command block failed:\n{block}\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

#[tokio::test]
async fn canonical_event_import_is_queryable_in_release() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let storage = temp.path().join("capture");
    let coords = persisting_pchronicle::storage::StoryCoords::new(
        storage.to_string_lossy(),
        "agent",
        "run",
        Some("run".into()),
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
                session_id: Some("run".into()),
                agent_id: Some("agent".into()),
                parent_uuid: None,
                trace_id: None,
                call_id: None,
                subagent_id: None,
                parent_agent_id: None,
                branch: None,
                parent_call_id: None,
                payload: serde_json::json!({"content":"release-smoke"}),
            }],
        )
        .await?;
    let source = persisting_pchronicle::storage::raw_event_lance_path(&coords)?;
    let output = temp.path().join("storyline");
    let imported = pchronicle(&[
        "import",
        "--from",
        source.to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
    ])?;
    assert!(
        imported.status.success(),
        "{}",
        String::from_utf8_lossy(&imported.stderr)
    );
    let response: Value = serde_json::from_slice(&imported.stdout)?;
    assert_eq!(response["format"], "events");
    assert_eq!(response["output_format"], "storyline-lance");
    assert_eq!(response["fact_rows"], 1);
    assert!(response.get("input_bytes").is_none());

    let queried = pchronicle(&[
        "query",
        output.to_str().unwrap(),
        "SELECT COUNT(*) AS runs FROM dataset.runs",
        "--format",
        "jsonl",
    ])?;
    assert!(
        queried.status.success(),
        "{}",
        String::from_utf8_lossy(&queried.stderr)
    );
    let row: Value = serde_json::from_slice(&queried.stdout)?;
    assert_eq!(row["runs"], 1);
    Ok(())
}

#[test]
fn default_warehouse_is_persistent_across_cli_processes() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let settings = temp.path().join("settings.toml");
    let warehouse = temp.path().join("warehouse");
    let settings = settings.to_string_lossy();
    let warehouse = warehouse.to_string_lossy();

    let configured = pchronicle(&["--settings", &settings, "default", &warehouse])?;
    assert!(
        configured.status.success(),
        "{}",
        String::from_utf8_lossy(&configured.stderr)
    );
    let expected = std::fs::canonicalize(warehouse.as_ref())?;
    assert_eq!(
        String::from_utf8(configured.stdout)?.trim(),
        expected.to_string_lossy()
    );

    let queried = pchronicle(&[
        "--settings",
        &settings,
        "query",
        "SELECT COUNT(*) AS runs FROM dataset.runs",
        "--format",
        "jsonl",
    ])?;
    assert!(
        queried.status.success(),
        "{}",
        String::from_utf8_lossy(&queried.stderr)
    );
    let value: Value = serde_json::from_slice(&queried.stdout)?;
    assert_eq!(value["runs"], 0);
    Ok(())
}

#[test]
fn relative_settings_file_works_from_the_process_directory() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let output = Command::new(env!("CARGO_BIN_EXE_pchronicle"))
        .current_dir(temp.path())
        .args(["--settings", "settings.toml", "default", "warehouse"])
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(temp.path().join("settings.toml").is_file());
    assert!(temp.path().join("warehouse").is_dir());
    Ok(())
}

#[cfg(unix)]
#[test]
fn agent_launches_codex_and_claude_with_normalized_context() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let bin_dir = temp.path().join("bin");
    let codex_home = temp.path().join("codex-home");
    install_fake_agents(&bin_dir)?;
    let dataset = temp.path().join("dataset");
    let caller = temp.path().join("caller");
    fs::create_dir_all(&dataset)?;
    fs::create_dir_all(&caller)?;
    let expected_dataset = fs::canonicalize(&dataset)?;
    let expected_caller = fs::canonicalize(&caller)?;
    let expected_pchronicle = fs::canonicalize(env!("CARGO_BIN_EXE_pchronicle"))?;

    for target in ["codex", "claude"] {
        let question = "Compare customer-42 failure modes";
        let record = temp.path().join(format!("record-{target}"));
        fs::create_dir(&record)?;
        let mut command = Command::new(env!("CARGO_BIN_EXE_pchronicle"));
        command
            .current_dir(&caller)
            .env("PATH", &bin_dir)
            .env("CODEX_HOME", &codex_home)
            .env("PCHRONICLE_TEST_RECORD", &record)
            .args([
                "agent",
                "--dataset",
                "../dataset",
                "--ask",
                question,
                "--no-overview",
                target,
            ]);
        let output = output_with_terminal(command)?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8(output.stdout)?.trim(), "fake-agent-ok");
        let stderr = String::from_utf8(output.stderr)?;
        assert!(stderr.contains(&format!("target={target}")), "{stderr}");
        assert!(stderr.contains(&format!("launching {target}")), "{stderr}");
        assert!(
            stderr.contains("bootstrap=status question=provided"),
            "{stderr}"
        );
        assert!(
            stderr.contains("not a filesystem or network sandbox"),
            "{stderr}"
        );
        assert!(
            stderr.contains("tool permissions are unchanged"),
            "{stderr}"
        );
        assert!(
            stderr.contains("other environment variables are inherited"),
            "{stderr}"
        );
        assert!(
            stderr.contains("no persistent Agent config file is changed"),
            "{stderr}"
        );
        assert!(
            !stderr.contains(question),
            "question leaked to stderr: {stderr}"
        );
        assert_eq!(
            fs::canonicalize(read_record(&record, "cwd")?)?,
            expected_caller
        );
        assert_eq!(
            Path::new(&read_record(&record, "dataset")?),
            expected_dataset
        );
        assert_eq!(
            fs::canonicalize(read_record(&record, "pchronicle-bin")?)?,
            expected_pchronicle
        );

        let args = fs::read_to_string(record.join("args"))?;
        assert!(args.contains(question), "{args}");
        assert!(
            args.contains(r#"{"run_status":true,"run_overview":false}"#),
            "{args}"
        );
        match target {
            "codex" => {
                assert!(args.contains("skills.config="), "{args}");
                assert!(args.contains("$pchronicle-dataset-"), "{args}");
                assert!(!args.contains("developer_instructions="), "{args}");
                assert!(args.contains("read-only"), "{args}");
                let config = args
                    .lines()
                    .find(|argument| argument.starts_with("skills.config="))
                    .context("fake Codex did not record its skill config")?;
                let config: toml::Value = toml::from_str(config)?;
                let selectors = config["skills"]["config"]
                    .as_array()
                    .context("Codex skill config is not an array")?;
                assert_eq!(selectors.len(), 2, "{selectors:?}");
                let skill_file = selectors[0]["path"]
                    .as_str()
                    .context("Codex file selector is missing")?;
                let skill_directory = selectors[1]["path"]
                    .as_str()
                    .context("Codex directory selector is missing")?;
                assert_eq!(selectors[0]["enabled"].as_bool(), Some(true));
                assert_eq!(selectors[1]["enabled"].as_bool(), Some(true));
                assert_eq!(
                    Path::new(skill_file).parent(),
                    Some(Path::new(skill_directory))
                );
            }
            "claude" => {
                assert!(args.contains("--plugin-dir"), "{args}");
                assert!(args.contains("--append-system-prompt"), "{args}");
                assert!(args.contains("/pchronicle:pchronicle-dataset"), "{args}");
            }
            _ => unreachable!(),
        }

        let staged_path = read_record(&record, "staged-path")?;
        if target == "codex" {
            assert!(
                Path::new(&staged_path).starts_with(fs::canonicalize(codex_home.join("skills"))?),
                "Codex skill was not staged in a discovery root: {staged_path}"
            );
        }
        assert!(
            !Path::new(&staged_path).exists(),
            "temporary Agent bundle was not removed: {staged_path}"
        );
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn agent_dry_run_is_inspectable_and_has_no_launch_side_effects() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let empty_path = temp.path().join("empty-path");
    let codex_home = temp.path().join("codex-home");
    let dataset = temp.path().join("dataset");
    let caller = temp.path().join("caller");
    fs::create_dir(&empty_path)?;
    fs::create_dir(&dataset)?;
    fs::create_dir(&caller)?;
    let question = "Compare confidential-customer failure modes";

    let output = Command::new(env!("CARGO_BIN_EXE_pchronicle"))
        .current_dir(&caller)
        .env("PATH", &empty_path)
        .env("CODEX_HOME", &codex_home)
        .args([
            "agent",
            "--dataset",
            "../dataset",
            "--ask",
            question,
            "--no-overview",
            "--dry-run",
            "codex",
        ])
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout)?;
    assert!(
        !stdout.contains(question),
        "question leaked from dry-run: {stdout}"
    );
    let plan: Value = serde_json::from_str(&stdout)?;
    assert_eq!(plan["schema_version"], "pchronicle-agent-plan/v1");
    assert_eq!(plan["agent"], "codex");
    assert_eq!(plan["executable_candidate"], "codex");
    assert_eq!(
        Path::new(plan["dataset_uri"].as_str().unwrap()),
        fs::canonicalize(&dataset)?
    );
    assert_eq!(
        Path::new(plan["working_directory"].as_str().unwrap()),
        fs::canonicalize(&caller)?
    );
    assert_eq!(plan["startup_mode"], "health_then_answer");
    assert_eq!(plan["bootstrap"]["run_status"], true);
    assert_eq!(plan["bootstrap"]["run_overview"], false);
    assert_eq!(plan["initial_question"]["provided"], true);
    assert_eq!(
        plan["initial_question"]["utf8_bytes"],
        question.len() as u64
    );
    assert_eq!(plan["initial_question"]["redacted"], true);
    assert_eq!(
        plan["dataset_access_guidance"],
        "read_only_pchronicle_commands"
    );
    assert_eq!(plan["target_permissions"], "unchanged");
    assert_eq!(
        plan["target_launch_injections"],
        serde_json::json!(["temporary_skill", "session_only_skills_config"])
    );
    assert_eq!(
        plan["set_environment_variables"],
        serde_json::json!(["PCHRONICLE_DATASET_URI", "PCHRONICLE_BIN"])
    );
    assert_eq!(
        plan["pchronicle_supplied_initial_model_context"],
        serde_json::json!([
            "analysis_guidance",
            "dataset_uri",
            "pchronicle_bin",
            "initial_question_when_provided"
        ])
    );
    assert_eq!(
        plan["model_visible_after_tool_use"],
        serde_json::json!(["pchronicle_command_results"])
    );
    assert_eq!(plan["temporary_injection_created"], false);
    assert_eq!(plan["will_launch"], false);
    assert!(!codex_home.exists(), "dry-run staged a Codex skill");
    Ok(())
}

#[cfg(unix)]
#[test]
fn agent_propagates_a_nonzero_child_exit_as_a_runtime_error() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let bin_dir = temp.path().join("bin");
    let codex_home = temp.path().join("codex-home");
    let record = temp.path().join("record");
    let dataset = temp.path().join("dataset");
    install_fake_agents(&bin_dir)?;
    fs::create_dir(&record)?;
    fs::create_dir(&dataset)?;

    let mut command = Command::new(env!("CARGO_BIN_EXE_pchronicle"));
    command
        .env("PATH", &bin_dir)
        .env("CODEX_HOME", &codex_home)
        .env("PCHRONICLE_TEST_RECORD", &record)
        .env("PCHRONICLE_TEST_EXIT", "7")
        .args(["agent", "--dataset"])
        .arg(&dataset)
        .arg("codex");
    let output = output_with_terminal(command)?;
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr)?;
    assert!(
        stderr.contains("codex exited with exit status: 7"),
        "{stderr}"
    );
    let staged_path = read_record(&record, "staged-path")?;
    assert!(!Path::new(&staged_path).exists());
    Ok(())
}

#[cfg(unix)]
#[test]
fn agent_uses_the_default_warehouse_when_dataset_is_omitted() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let bin_dir = temp.path().join("bin");
    let codex_home = temp.path().join("codex-home");
    let record = temp.path().join("record");
    let settings = temp.path().join("settings.toml");
    let warehouse = temp.path().join("warehouse");
    install_fake_agents(&bin_dir)?;
    fs::create_dir(&record)?;

    let configured = Command::new(env!("CARGO_BIN_EXE_pchronicle"))
        .args(["--settings"])
        .arg(&settings)
        .arg("default")
        .arg(&warehouse)
        .output()?;
    assert!(
        configured.status.success(),
        "{}",
        String::from_utf8_lossy(&configured.stderr)
    );

    let mut command = Command::new(env!("CARGO_BIN_EXE_pchronicle"));
    command
        .env("PATH", &bin_dir)
        .env("CODEX_HOME", &codex_home)
        .env("PCHRONICLE_TEST_RECORD", &record)
        .args(["--settings"])
        .arg(&settings)
        .args(["agent", "codex"]);
    let output = output_with_terminal(command)?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        Path::new(&read_record(&record, "dataset")?),
        fs::canonicalize(&warehouse)?
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn agent_reports_a_missing_executable_without_writing_stdout() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let empty_path = temp.path().join("empty-path");
    let codex_home = temp.path().join("codex-home");
    let dataset = temp.path().join("dataset");
    fs::create_dir(&empty_path)?;

    let missing_dataset = temp.path().join("missing-dataset");
    let output = Command::new(env!("CARGO_BIN_EXE_pchronicle"))
        .env("PATH", &empty_path)
        .env("CODEX_HOME", &codex_home)
        .args(["agent", "--dataset"])
        .arg(&missing_dataset)
        .args(["--dry-run", "codex"])
        .output()?;
    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr)?;
    assert!(stderr.contains(&format!("{missing_dataset:?}")), "{stderr}");
    assert!(
        stderr.contains("verify it exists and is accessible"),
        "{stderr}"
    );

    fs::create_dir(&dataset)?;

    let output = Command::new(env!("CARGO_BIN_EXE_pchronicle"))
        .env("PATH", &empty_path)
        .env("CODEX_HOME", &codex_home)
        .args(["agent", "--dataset"])
        .arg(&dataset)
        .arg("codex")
        .output()?;
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr)?;
    assert!(
        stderr.contains("interactive Agent launch requires terminal stdin and stdout"),
        "{stderr}"
    );
    assert!(!codex_home.exists(), "non-TTY launch staged a Codex skill");

    let mut command = Command::new(env!("CARGO_BIN_EXE_pchronicle"));
    command
        .env("PATH", &empty_path)
        .env("CODEX_HOME", &codex_home)
        .args(["agent", "--dataset"])
        .arg(&dataset)
        .arg("codex");
    let output = output_with_terminal(command)?;
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr)?;
    assert!(
        stderr.contains("codex executable was not found in PATH"),
        "{stderr}"
    );
    assert_eq!(fs::read_dir(codex_home.join("skills"))?.count(), 0);
    Ok(())
}

#[cfg(unix)]
fn output_with_terminal(mut command: Command) -> Result<Output> {
    let mut master_fd = -1;
    let mut slave_fd = -1;
    // SAFETY: openpty initializes both file descriptors on success. The null
    // pointers request default terminal attributes and no device-name buffer.
    let result = unsafe {
        libc::openpty(
            &mut master_fd,
            &mut slave_fd,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    anyhow::ensure!(
        result == 0,
        "open pseudo-terminal: {}",
        io::Error::last_os_error()
    );

    // SAFETY: openpty returned two distinct, owned descriptors. Each is
    // converted exactly once and subsequently closed by File/Stdio.
    let mut master = unsafe { File::from_raw_fd(master_fd) };
    let slave = unsafe { File::from_raw_fd(slave_fd) };
    command
        .stdin(Stdio::from(slave.try_clone()?))
        .stdout(Stdio::from(slave))
        .stderr(Stdio::piped());

    let child = command
        .spawn()
        .context("spawn pchronicle with a pseudo-terminal")?;
    drop(command);
    let reader = std::thread::spawn(move || {
        let mut stdout = Vec::new();
        if let Err(error) = master.read_to_end(&mut stdout) {
            // Linux PTY masters commonly report EIO after the final slave
            // closes; macOS normally reports EOF. Both mean capture is done.
            if error.raw_os_error() != Some(libc::EIO) {
                return Err(error);
            }
        }
        Ok(stdout)
    });
    let output = child
        .wait_with_output()
        .context("wait for pchronicle with a pseudo-terminal")?;
    let stdout = reader
        .join()
        .map_err(|_| anyhow::anyhow!("pchronicle pseudo-terminal reader panicked"))?
        .context("read pchronicle pseudo-terminal output")?;
    Ok(Output {
        status: output.status,
        stdout,
        stderr: output.stderr,
    })
}

#[cfg(unix)]
fn install_fake_agents(bin_dir: &Path) -> Result<()> {
    const SCRIPT: &str = r#"#!/bin/sh
set -eu

record=$PCHRONICLE_TEST_RECORD
printf '%s\n' "$PCHRONICLE_DATASET_URI" > "$record/dataset"
printf '%s\n' "$PCHRONICLE_BIN" > "$record/pchronicle-bin"
pwd > "$record/cwd"
: > "$record/args"

previous=
staged=
for argument in "$@"; do
  printf '%s\n' "$argument" >> "$record/args"
  if [ "$previous" = "--plugin-dir" ]; then
    staged=$argument
    test -f "$staged/.claude-plugin/plugin.json"
    test -f "$staged/skills/pchronicle-dataset/SKILL.md"
  fi
  case "$argument" in
    skills.config=*)
      remainder=${argument#*path}
      remainder=${remainder#*\"}
      staged=${remainder%%\"*}
      test -f "$staged"
      skill_dir=${staged%/SKILL.md}
      test -f "$skill_dir/agents/openai.yaml"
      ;;
  esac
  previous=$argument
done

test -n "$staged"
printf '%s\n' "$staged" > "$record/staged-path"
exit_code=${PCHRONICLE_TEST_EXIT:-0}
if [ "$exit_code" -ne 0 ]; then
  exit "$exit_code"
fi
printf '%s\n' fake-agent-ok
"#;

    fs::create_dir(bin_dir)?;
    for agent in ["codex", "claude"] {
        let path = bin_dir.join(agent);
        fs::write(&path, SCRIPT)?;
        let mut permissions = fs::metadata(&path)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions)?;
    }
    Ok(())
}

#[cfg(unix)]
fn read_record(record: &Path, name: &str) -> Result<String> {
    Ok(fs::read_to_string(record.join(name))?.trim_end().to_owned())
}
