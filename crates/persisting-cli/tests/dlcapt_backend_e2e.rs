use std::fs;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use tempfile::TempDir;

const ATTEMPTS: usize = 3;

fn reserve_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn reserve_distinct_ports() -> (u16, u16) {
    let public = reserve_port();
    loop {
        let admin = reserve_port();
        if admin != public {
            return (public, admin);
        }
    }
}

fn write_config(dir: &TempDir, public_port: u16, admin_port: u16) -> PathBuf {
    let path = dir.path().join("proxy.toml");
    fs::write(
        &path,
        format!(
            r#"
listen = "127.0.0.1:{public_port}"
admin_listen = "127.0.0.1:{admin_port}"
store_dir = "{}"
agent_id = "cli-e2e"
session_header = "x-persisting-session-id"
default_session_id = "default"
preserve_raw = false
base_session_path = "/v1/sessions"

[storage]
authoritative = "json_file"
also = ["md"]

[[models]]
name = "*"
provider = "openai"
upstream_base_url = "https://example.invalid/v1"
api_key = ""
"#,
            dir.path().join("store").display(),
        ),
    )
    .unwrap();
    path
}

fn wait_for_ok(url: &str) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let client = reqwest::blocking::Client::new();

    loop {
        let last_error = match client.get(url).send() {
            Ok(response) if response.status().is_success() => return Ok(()),
            Ok(response) => format!("received HTTP {}", response.status()),
            Err(error) => error.to_string(),
        };

        if Instant::now() >= deadline {
            return Err(format!(
                "service did not become ready: {url}; last error: {last_error}"
            ));
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn stop(child: Child) {
    let mut child = child;
    let _ = child.kill();
    let _ = child.wait_with_output();
}

fn failed_attempt(
    attempt: usize,
    public_url: &str,
    admin_url: &str,
    reason: impl std::fmt::Display,
    child: Option<Child>,
) -> String {
    let (status, stdout, stderr) = match child {
        Some(mut child) => {
            let _ = child.kill();
            match child.wait_with_output() {
                Ok(output) => (
                    output.status.to_string(),
                    String::from_utf8_lossy(&output.stdout).into_owned(),
                    String::from_utf8_lossy(&output.stderr).into_owned(),
                ),
                Err(error) => (
                    "<unavailable>".into(),
                    String::new(),
                    format!("failed collecting child output: {error}"),
                ),
            }
        }
        None => ("<not spawned>".into(), String::new(), String::new()),
    };

    format!(
        "attempt {attempt}/{ATTEMPTS}\n\
         public URL: {public_url}\n\
         admin URL: {admin_url}\n\
         readiness failure: {reason}\n\
         exit status: {status}\n\
         stdout:\n{stdout}\n\
         stderr:\n{stderr}"
    )
}

fn run_case_with_retry<F, V>(case_name: &str, mut start: F, mut verify: V)
where
    F: FnMut(&TempDir, &Path) -> Result<Child, String>,
    V: FnMut(&TempDir) -> Result<(), String>,
{
    let mut failures = Vec::new();

    for attempt in 1..=ATTEMPTS {
        let dir = tempfile::tempdir().unwrap();
        let (public, admin) = reserve_distinct_ports();
        let config = write_config(&dir, public, admin);
        let public_url = format!("http://127.0.0.1:{public}/healthz");
        let admin_url = format!("http://127.0.0.1:{admin}/admin/sessions");

        match start(&dir, &config) {
            Ok(child) => match wait_for_ok(&public_url)
                .and_then(|_| wait_for_ok(&admin_url))
                .and_then(|_| verify(&dir))
            {
                Ok(()) => {
                    stop(child);
                    return;
                }
                Err(reason) => failures.push(failed_attempt(
                    attempt,
                    &public_url,
                    &admin_url,
                    reason,
                    Some(child),
                )),
            },
            Err(reason) => failures.push(failed_attempt(
                attempt,
                &public_url,
                &admin_url,
                reason,
                None,
            )),
        }
    }

    panic!(
        "{case_name} failed after {ATTEMPTS} attempts:\n{}",
        failures.join("\n\n")
    );
}

fn dlcapt_bin() -> PathBuf {
    std::env::var_os("DLCAPT_BIN")
        .map(PathBuf::from)
        .filter(|path| path.is_file())
        .expect("DLCAPT_BIN must point to a prebuilt dlcapt binary")
}

#[test]
fn standalone_dlcapt_serves_health_and_admin() {
    run_case_with_retry(
        "standalone dlcapt",
        |_, config| {
            Command::new(dlcapt_bin())
                .arg(config)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|error| error.to_string())
        },
        |_| Ok(()),
    );
}

#[test]
fn cli_dlcapt_backend_ignores_capture_storage_environment() {
    run_case_with_retry(
        "persisting-cli dlcapt backend",
        |dir, config| {
            Command::new(env!("CARGO_BIN_EXE_persisting"))
                .args([
                    "gateway",
                    "serve",
                    "--backend",
                    "dlcapt",
                    "-c",
                    config.to_str().unwrap(),
                ])
                .env(
                    "PERSISTING_CAPTURE_STORAGE",
                    dir.path().join("must-not-be-used"),
                )
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|error| error.to_string())
        },
        |dir| {
            let capture_store = dir.path().join("must-not-be-used");
            (!capture_store.exists())
                .then_some(())
                .ok_or_else(|| format!("dlcapt unexpectedly created {}", capture_store.display()))
        },
    );
}
