//! Files and result hand-off for a pVisor delegated through Docker or KVM.

use persisting_control::{AttemptId, RunInvocation, RunResult, RunSpec};
use std::path::{Path, PathBuf};

pub(crate) const SPEC_FILENAME: &str = "run-spec.json";
pub(crate) const RESULT_FILENAME: &str = "run-result.json";

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct DelegatedRunOutput {
    pub(crate) result: RunResult,
    pub(crate) agent_abi: crate::AgentAbiSnapshot,
}

pub(crate) struct DelegatedRunFiles {
    _temporary: tempfile::TempDir,
    pub(crate) spec_path: PathBuf,
    pub(crate) result_path: PathBuf,
}

impl DelegatedRunFiles {
    pub(crate) fn new(spec: &RunSpec) -> anyhow::Result<Self> {
        let temporary = tempfile::Builder::new()
            .prefix("pvisor-delegated-")
            .tempdir()?;
        let spec_path = temporary.path().join(SPEC_FILENAME);
        let result_path = temporary.path().join(RESULT_FILENAME);
        let mut delegated = spec.clone();
        delegated.metadata.remove("pvisor.executor");
        let RunInvocation::Process(process) = &mut delegated.invocation;
        process
            .env
            .retain(|key, _| !key.starts_with("PERSISTING_AGENT_ABI_"));
        write_private_json(&spec_path, &delegated)?;
        Ok(Self {
            _temporary: temporary,
            spec_path,
            result_path,
        })
    }

    pub(crate) fn read_result(
        &self,
        run_id: &persisting_control::RunId,
        attempt_id: &AttemptId,
        lease_epoch: u64,
    ) -> anyhow::Result<DelegatedRunOutput> {
        let mut output: DelegatedRunOutput =
            serde_json::from_slice(&std::fs::read(&self.result_path)?)?;
        output.result.run_id = run_id.clone();
        output.result.attempt_id = attempt_id.clone();
        output.result.lease_epoch = lease_epoch;
        output.agent_abi.run_id = run_id.to_string();
        output.agent_abi.attempt_id = attempt_id.to_string();
        Ok(output)
    }
}

pub(crate) fn write_result(path: &Path, output: &DelegatedRunOutput) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("result path has no parent: {}", path.display()))?;
    std::fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("run-result"),
        uuid::Uuid::new_v4().simple()
    ));
    write_private_json(&temporary, output)?;
    std::fs::rename(temporary, path)?;
    Ok(())
}

fn write_private_json(path: &Path, value: &impl serde::Serialize) -> anyhow::Result<()> {
    let body = serde_json::to_vec_pretty(value)?;
    std::fs::write(path, body)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delegated_spec_drops_host_agent_abi_and_normalizes_result_identity() {
        let mut spec = RunSpec::process("run-one", "agent", "true");
        let RunInvocation::Process(process) = &mut spec.invocation;
        process.env.insert(
            "PERSISTING_AGENT_ABI_ENDPOINT".into(),
            "/tmp/host.sock".into(),
        );
        process.env.insert("KEEP".into(), "yes".into());
        let files = DelegatedRunFiles::new(&spec).unwrap();
        let delegated: RunSpec =
            serde_json::from_slice(&std::fs::read(&files.spec_path).unwrap()).unwrap();
        let RunInvocation::Process(process) = delegated.invocation;
        assert!(!process.env.contains_key("PERSISTING_AGENT_ABI_ENDPOINT"));
        assert_eq!(process.env.get("KEEP").map(String::as_str), Some("yes"));
    }
}
