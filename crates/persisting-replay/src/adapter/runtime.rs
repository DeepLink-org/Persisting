use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

use crate::error::{ReplayError, ReplayErrorKind, ResultExt};
use crate::io::{canonicalize, read_regular_file};
use crate::model::{AgentKind, PlaybackRequest, ReplayMode};

#[derive(Debug, Clone)]
pub(crate) struct LaunchSpec {
    pub entrypoint: PathBuf,
    pub version: String,
    pub source: String,
    pub runtime_root: Option<PathBuf>,
}

pub(crate) fn resolve_launch_spec(
    request: &PlaybackRequest,
) -> Result<Option<LaunchSpec>, ReplayError> {
    if request.agent_entrypoint.is_some() && request.agent_runtime.is_some() {
        return Err(ReplayError::configuration(
            "agent entrypoint and agent runtime are mutually exclusive",
        ));
    }
    if request.mode == ReplayMode::PrepareOnly {
        return Ok(None);
    }
    let (entrypoint, source, runtime_root, declared_version) =
        if let Some(runtime_root) = &request.agent_runtime {
            let root = canonicalize(
                runtime_root,
                ReplayErrorKind::Configuration,
                "agent runtime",
            )?;
            let manifest_path = root.join("sandbox-playback-agent.json");
            let manifest: RuntimeManifest =
                serde_json::from_slice(&read_regular_file(&manifest_path)?).replay_context(
                    ReplayErrorKind::Configuration,
                    format!("parse agent runtime manifest {}", manifest_path.display()),
                )?;
            if manifest.schema_version != "sandbox-playback.agent-runtime/v1" {
                return Err(ReplayError::configuration(
                    "agent runtime schema_version must be sandbox-playback.agent-runtime/v1",
                ));
            }
            if manifest.agent != request.agent.as_str() {
                return Err(ReplayError::new(
                    ReplayErrorKind::UnsupportedAgent,
                    format!(
                        "agent runtime declares {:?}, requested {:?}",
                        manifest.agent,
                        request.agent.as_str()
                    ),
                ));
            }
            if manifest.version != request.agent.supported_version() {
                return Err(ReplayError::new(
                    ReplayErrorKind::UnsupportedVersion,
                    format!(
                        "agent runtime declares {:?}; profile requires {}",
                        manifest.version,
                        request.agent.supported_version()
                    ),
                ));
            }
            let relative = safe_relative(&manifest.entrypoint)?;
            (
                root.join(relative),
                "runtime_manifest".to_owned(),
                Some(root),
                Some(manifest.version),
            )
        } else {
            let entrypoint = request.agent_entrypoint.clone().ok_or_else(|| {
                ReplayError::configuration(
                    "replay and continuation modes require --agent-entrypoint or --agent-runtime",
                )
            })?;
            (entrypoint, "explicit_entrypoint".to_owned(), None, None)
        };
    if !entrypoint.is_absolute() {
        return Err(ReplayError::configuration(
            "agent entrypoint must be an absolute path",
        ));
    }
    let entrypoint = canonicalize(
        &entrypoint,
        ReplayErrorKind::Configuration,
        "agent entrypoint",
    )?;
    if !entrypoint.is_file() {
        return Err(ReplayError::configuration(format!(
            "agent entrypoint is not a regular file: {}",
            entrypoint.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if entrypoint
            .metadata()
            .map(|metadata| metadata.permissions().mode() & 0o111 == 0)
            .unwrap_or(true)
        {
            return Err(ReplayError::configuration(format!(
                "agent entrypoint is not executable: {}",
                entrypoint.display()
            )));
        }
    }
    let version = probe_version(request.agent, &entrypoint)?;
    if declared_version
        .as_deref()
        .is_some_and(|declared| declared != version)
    {
        return Err(ReplayError::new(
            ReplayErrorKind::UnsupportedVersion,
            "agent runtime manifest and executable versions differ",
        ));
    }
    Ok(Some(LaunchSpec {
        entrypoint,
        version,
        source,
        runtime_root,
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeManifest {
    schema_version: String,
    agent: String,
    version: String,
    entrypoint: PathBuf,
    #[serde(default, rename = "paths")]
    _paths: BTreeMap<String, PathBuf>,
}

pub(super) fn safe_relative(path: &Path) -> Result<PathBuf, ReplayError> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(ReplayError::configuration(
            "agent runtime entrypoint must be a non-empty relative path without '..'",
        ));
    }
    Ok(path.to_path_buf())
}

fn probe_version(agent: AgentKind, entrypoint: &Path) -> Result<String, ReplayError> {
    let expected = agent.supported_version();
    let mut command = Command::new(entrypoint);
    match agent {
        AgentKind::ClaudeCode | AgentKind::MiniSweAgent | AgentKind::PiAgent => {
            command.arg("--version");
        }
        AgentKind::Openhands => {
            command.args([
                "-c",
                "import importlib.metadata;print(importlib.metadata.version('openhands-ai'))",
            ]);
        }
        AgentKind::SweAgent => {
            command.args([
                "-c",
                "import importlib.metadata;print(importlib.metadata.version('sweagent'))",
            ]);
        }
    }
    command.env_remove("PYTHONHOME");
    command.env_remove("PYTHONPATH");
    command.env_remove("VIRTUAL_ENV");
    if agent == AgentKind::MiniSweAgent {
        let runtime = mini_python_runtime(entrypoint)?;
        configure_mini_python_environment(&mut command, &runtime)?;
    }
    let output = command.output().replay_context(
        ReplayErrorKind::UnsupportedVersion,
        format!(
            "probe {} version from {}",
            agent.as_str(),
            entrypoint.display()
        ),
    )?;
    let rendered = String::from_utf8_lossy(if output.stdout.is_empty() {
        &output.stderr
    } else {
        &output.stdout
    });
    let detected = parse_version(agent, &rendered);
    let status_is_acceptable =
        output.status.success() || (agent == AgentKind::MiniSweAgent && detected == Some(expected));
    if !status_is_acceptable || detected != Some(expected) {
        return Err(ReplayError::new(
            ReplayErrorKind::UnsupportedVersion,
            format!(
                "{} profile requires {}, got {:?} from {}",
                agent.as_str(),
                expected,
                rendered.trim(),
                entrypoint.display()
            ),
        ));
    }
    Ok(expected.to_owned())
}

fn parse_version(agent: AgentKind, rendered: &str) -> Option<&'static str> {
    let expected = agent.supported_version();
    match agent {
        AgentKind::ClaudeCode => {
            let mut lines = rendered.trim().lines();
            let first = lines.next()?.trim();
            let token = first.split_whitespace().next()?;
            (lines.next().is_none() && token == expected).then_some(expected)
        }
        AgentKind::MiniSweAgent => {
            const PREFIX: &str = "This is mini-swe-agent version ";
            let mut versions = rendered.lines().filter_map(|line| {
                line.trim()
                    .strip_prefix(PREFIX)?
                    .split_whitespace()
                    .next()
                    .map(|version| version.trim_end_matches('.'))
            });
            let version = versions.next()?;
            (versions.next().is_none() && version == expected).then_some(expected)
        }
        AgentKind::Openhands | AgentKind::PiAgent | AgentKind::SweAgent => {
            (rendered.trim() == expected).then_some(expected)
        }
    }
}

#[derive(Debug)]
pub(super) struct PiNodeRuntime {
    pub node: PathBuf,
    pub package_json: PathBuf,
}

pub(super) fn pi_node_runtime(entrypoint: &Path) -> Result<PiNodeRuntime, ReplayError> {
    let runtime_root = entrypoint
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| ReplayError::continuation("Pi entrypoint has no runtime root"))?;
    let node = runtime_root.join("node/bin/node");
    let package_json = runtime_root
        .join("npm-global/lib/node_modules/@earendil-works/pi-coding-agent/package.json");
    if !node.is_file() {
        return Err(ReplayError::continuation(format!(
            "Pi runtime Node executable does not exist: {}",
            node.display()
        )));
    }
    if !package_json.is_file() {
        return Err(ReplayError::continuation(format!(
            "Pi runtime package manifest does not exist: {}",
            package_json.display()
        )));
    }
    Ok(PiNodeRuntime { node, package_json })
}

#[derive(Debug)]
pub(super) struct MiniPythonRuntime {
    pub python: PathBuf,
    pub loader: Option<PathBuf>,
    pub python_home: Option<PathBuf>,
    pub virtual_env: Option<PathBuf>,
    library_paths: Vec<PathBuf>,
}

pub(super) fn mini_python_runtime(entrypoint: &Path) -> Result<MiniPythonRuntime, ReplayError> {
    if let Some(local_root) = entrypoint.parent().and_then(Path::parent) {
        let uv_root = local_root.join("share/uv");
        let virtual_env = uv_root.join("tools/mini-swe-agent");
        let python = virtual_env.join("bin/python");
        if python.is_file() {
            let python = fs::canonicalize(&python).replay_context(
                ReplayErrorKind::Continuation,
                format!(
                    "resolve bundled mini-swe-agent Python from {}",
                    python.display()
                ),
            )?;
            let python_home = python
                .parent()
                .and_then(Path::parent)
                .ok_or_else(|| ReplayError::continuation("bundled Python has no prefix"))?
                .to_path_buf();
            if !python_home.join("lib/python3.12/encodings").is_dir() {
                return Err(ReplayError::continuation(format!(
                    "bundled mini-swe-agent Python has no standard library below {}",
                    python_home.display()
                )));
            }
            let loader = uv_root.join("sweeval-system-libs/ld-linux-x86-64.so.2");
            if !loader.is_file() {
                return Err(ReplayError::continuation(format!(
                    "bundled mini-swe-agent Python loader does not exist: {}",
                    loader.display()
                )));
            }
            return Ok(MiniPythonRuntime {
                python,
                loader: Some(loader),
                python_home: Some(python_home.clone()),
                virtual_env: Some(virtual_env),
                library_paths: vec![uv_root.join("sweeval-system-libs"), python_home.join("lib")],
            });
        }
    }

    let prefix = read_regular_file(entrypoint)?;
    if let Some(first) = prefix.split(|byte| *byte == b'\n').next()
        && let Some(shebang) = first.strip_prefix(b"#!")
    {
        let rendered = String::from_utf8_lossy(shebang);
        let words: Vec<_> = rendered.split_whitespace().collect();
        if words.first() == Some(&"/usr/bin/env")
            && let Some(program) = words.get(1)
            && program.contains("python")
        {
            return Ok(MiniPythonRuntime {
                python: PathBuf::from(program),
                loader: None,
                python_home: None,
                virtual_env: None,
                library_paths: Vec::new(),
            });
        } else if let Some(program) = words.first()
            && program.contains("python")
        {
            return Ok(MiniPythonRuntime {
                python: PathBuf::from(program),
                loader: None,
                python_home: None,
                virtual_env: None,
                library_paths: Vec::new(),
            });
        }
    }
    for name in ["python3", "python"] {
        let candidate = entrypoint.parent().unwrap_or(Path::new("/")).join(name);
        if candidate.is_file() {
            return Ok(MiniPythonRuntime {
                python: candidate,
                loader: None,
                python_home: None,
                virtual_env: None,
                library_paths: Vec::new(),
            });
        }
    }
    Err(ReplayError::continuation(
        "mini-swe-agent entrypoint does not expose its Python interpreter",
    ))
}

pub(super) fn mini_python_library_path(
    runtime: &MiniPythonRuntime,
) -> Result<Option<OsString>, ReplayError> {
    let paths = runtime
        .library_paths
        .iter()
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    if paths.is_empty() {
        return Ok(None);
    }
    std::env::join_paths(paths).map(Some).map_err(|error| {
        ReplayError::configuration(format!(
            "cannot construct mini-swe-agent Python library path: {error}"
        ))
    })
}

pub(super) fn configure_mini_python_environment(
    command: &mut Command,
    runtime: &MiniPythonRuntime,
) -> Result<(), ReplayError> {
    if let Some(python_home) = &runtime.python_home {
        command.env("PYTHONHOME", python_home);
    }
    if let Some(virtual_env) = &runtime.virtual_env {
        command.env("VIRTUAL_ENV", virtual_env);
        command.env(
            "PYTHONPATH",
            virtual_env.join("lib/python3.12/site-packages"),
        );
        let current = std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into());
        let paths = std::iter::once(virtual_env.join("bin")).chain(std::env::split_paths(&current));
        let path = std::env::join_paths(paths).map_err(|error| {
            ReplayError::configuration(format!(
                "cannot prepend mini-swe-agent virtual environment to PATH: {error}"
            ))
        })?;
        command.env("PATH", path);
    }
    if let Some(library_path) = mini_python_library_path(runtime)? {
        command.env("LD_LIBRARY_PATH", library_path);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{fs, process::Command};

    use super::{
        configure_mini_python_environment, mini_python_runtime, parse_version, resolve_launch_spec,
    };
    use crate::model::{AgentKind, PlaybackRequest, ReplayMode};

    #[test]
    fn mini_version_probe_accepts_exact_banner_before_config_noise() {
        let output = "This is mini-swe-agent version 2.4.6.\n\
Check the v2 migration guide at https://example.invalid\n\
Loading global config from '/root/.config/mini-swe-agent/.env'";
        assert_eq!(
            parse_version(AgentKind::MiniSweAgent, output),
            Some("2.4.6")
        );
    }

    #[cfg(unix)]
    #[test]
    fn mini_python_runtime_finds_the_portable_uv_bundle() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let local = root.path().join(".local");
        let entrypoint = local.join("bin/mini-swe-agent");
        let virtual_env = local.join("share/uv/tools/mini-swe-agent");
        let python_home = local.join("share/uv/python/cpython-3.12.11");
        let python = python_home.join("bin/python3.12");
        fs::create_dir_all(entrypoint.parent().unwrap()).unwrap();
        fs::create_dir_all(virtual_env.join("bin")).unwrap();
        fs::create_dir_all(virtual_env.join("lib/python3.12/site-packages")).unwrap();
        fs::create_dir_all(python_home.join("lib/python3.12/encodings")).unwrap();
        fs::create_dir_all(python.parent().unwrap()).unwrap();
        let loader = local.join("share/uv/sweeval-system-libs/ld-linux-x86-64.so.2");
        fs::create_dir_all(loader.parent().unwrap()).unwrap();
        fs::write(&loader, "loader").unwrap();
        fs::write(&entrypoint, "#!/bin/sh\nexit 0\n").unwrap();
        fs::write(&python, "python").unwrap();
        symlink(&python, virtual_env.join("bin/python")).unwrap();

        let runtime = mini_python_runtime(&entrypoint).unwrap();
        let canonical_python_home = fs::canonicalize(&python_home).unwrap();
        assert_eq!(runtime.python, fs::canonicalize(&python).unwrap());
        assert_eq!(
            runtime.python_home.as_deref(),
            Some(canonical_python_home.as_path())
        );
        assert_eq!(runtime.loader.as_deref(), Some(loader.as_path()));
        assert_eq!(runtime.virtual_env.as_deref(), Some(virtual_env.as_path()));
        let mut command = Command::new(&runtime.python);
        configure_mini_python_environment(&mut command, &runtime).unwrap();
        assert!(command.get_envs().any(|(name, value)| {
            name == "PYTHONHOME" && value == Some(canonical_python_home.as_os_str())
        }));
        let path = command
            .get_envs()
            .find_map(|(name, value)| {
                (name == "PATH").then(|| value.expect("PATH value").to_os_string())
            })
            .expect("PATH override");
        assert_eq!(
            std::env::split_paths(&path).next().unwrap(),
            virtual_env.join("bin")
        );
    }

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn version_probes_require_exact_banners() {
        assert_eq!(
            parse_version(AgentKind::ClaudeCode, "2.1.220 (Claude Code)"),
            Some("2.1.220")
        );
        assert_eq!(
            parse_version(AgentKind::ClaudeCode, "12.1.220 (Claude Code)"),
            None
        );
        assert_eq!(
            parse_version(AgentKind::Openhands, "0.53.0\n"),
            Some("0.53.0")
        );
        assert_eq!(
            parse_version(
                AgentKind::Openhands,
                "warning about 0.53.0; actual runtime 0.54.0"
            ),
            None
        );
        assert_eq!(
            parse_version(
                AgentKind::MiniSweAgent,
                "This is mini-swe-agent version 2.4.6.\n"
            ),
            Some("2.4.6")
        );
        assert_eq!(parse_version(AgentKind::SweAgent, "1.1.0"), Some("1.1.0"));
        assert_eq!(parse_version(AgentKind::SweAgent, "swe-agent 1.1.0"), None);
    }

    #[cfg(unix)]
    #[test]
    fn prepare_only_never_starts_a_supplied_runtime() {
        let temporary = tempfile::tempdir().unwrap();
        let marker = temporary.path().join("started");
        let entrypoint = temporary.path().join("claude");
        fs::write(
            &entrypoint,
            format!(
                "#!/bin/sh\ntouch '{}'\nprintf '2.1.220 (Claude Code)\\n'\n",
                marker.display()
            ),
        )
        .unwrap();
        let mut permissions = fs::metadata(&entrypoint).unwrap().permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&entrypoint, permissions).unwrap();
        let request = PlaybackRequest {
            agent: AgentKind::ClaudeCode,
            trajectory: temporary.path().join("trajectory"),
            after_step: 1,
            workspace: temporary.path().to_path_buf(),
            state_dir: temporary.path().join("state"),
            output_dir: temporary.path().join("output"),
            agent_entrypoint: Some(entrypoint),
            agent_runtime: None,
            disallowed_tools: Vec::new(),
            trajectory_assets: None,
            session_id: None,
            max_steps: None,
            mode: ReplayMode::PrepareOnly,
            allow_stale_observations: false,
            run_id: None,
            disable_thinking: false,
            boundary_user_prompt: None,
        };

        assert!(resolve_launch_spec(&request).unwrap().is_none());
        assert!(!marker.exists());
    }
}
