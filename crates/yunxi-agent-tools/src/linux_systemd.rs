//! Approval-gated, user-scoped systemd mutations.
//!
//! This module deliberately accepts only a typed action and a token-safe unit.
//! It never invokes a shell and always constructs the exact argv
//! `systemctl --user <action> <unit>`.

use crate::{ToolFileChange, ToolResponse, ToolRuntimeEvent};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::Path;
use yunxi_agent_core::{AgentCancellationToken, AgentResult};
use yunxi_agent_exec::{ExecCommand, ExecManager};
use yunxi_agent_sandbox::ExecutionPolicy;

const MAX_UNIT_BYTES: usize = 256;
const MAX_OUTPUT_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SystemdAction {
    Start,
    Stop,
    Restart,
    Reload,
    Enable,
    Disable,
}

impl SystemdAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Restart => "restart",
            Self::Reload => "reload",
            Self::Enable => "enable",
            Self::Disable => "disable",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LinuxSystemdInput {
    pub action: SystemdAction,
    pub unit: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LinuxSystemdReport {
    pub schema_version: u32,
    pub tool: String,
    pub action: String,
    pub unit: String,
    pub argv: Vec<String>,
    pub command: String,
    pub status: String,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub truncated: bool,
    pub cancelled: bool,
}

pub fn parameters_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "action": {"type": "string", "enum": ["start", "stop", "restart", "reload", "enable", "disable"]},
            "unit": {"type": "string", "description": "A user systemd unit token; paths and shell syntax are rejected."}
        },
        "required": ["action", "unit"],
        "additionalProperties": false
    })
}

pub fn fixed_argv(input: &LinuxSystemdInput) -> Result<Vec<String>, String> {
    validate_unit(&input.unit)?;
    Ok(vec![
        "systemctl".to_string(),
        "--user".to_string(),
        input.action.as_str().to_string(),
        input.unit.clone(),
    ])
}

pub async fn execute(
    id: Option<String>,
    cwd: &Path,
    input: LinuxSystemdInput,
    policy: ExecutionPolicy,
    cancellation: AgentCancellationToken,
) -> AgentResult<ToolResponse> {
    execute_with_env(
        id,
        cwd,
        input,
        policy,
        cancellation,
        std::collections::BTreeMap::new(),
    )
    .await
}

pub async fn execute_with_env(
    id: Option<String>,
    cwd: &Path,
    input: LinuxSystemdInput,
    policy: ExecutionPolicy,
    cancellation: AgentCancellationToken,
    env: std::collections::BTreeMap<String, String>,
) -> AgentResult<ToolResponse> {
    let argv = match fixed_argv(&input) {
        Ok(argv) => argv,
        Err(message) => return Ok(ToolResponse::declined(id, message)),
    };
    let command = argv.join(" ");
    let request = ExecCommand {
        id: id.clone(),
        cwd: cwd.to_path_buf(),
        command: command.clone(),
        argv: argv.clone(),
        stdin: None,
        env,
        timeout_millis: Some(30_000),
        policy,
    };
    let trace = match ExecManager::default()
        .run_with_cancellation(request, cancellation)
        .await
    {
        Ok(trace) => trace,
        Err(error) => {
            let report = LinuxSystemdReport {
                schema_version: 1,
                tool: "linux_systemd".to_string(),
                action: input.action.as_str().to_string(),
                unit: input.unit,
                argv,
                command: command.clone(),
                status: "unavailable".to_string(),
                stdout: String::new(),
                stderr: error.to_string(),
                exit_code: None,
                truncated: false,
                cancelled: false,
            };
            return Ok(ToolResponse::failed(
                id,
                serde_json::to_string(&report).unwrap_or_else(|_| error.to_string()),
                None,
                Vec::new(),
            )
            .with_runtime_events(vec![mutation_event(&report)]));
        }
    };
    let (stdout, stderr) = trace
        .events
        .iter()
        .rev()
        .find_map(|event| match event {
            yunxi_agent_exec::ExecLifecycleEvent::Completed { output, .. } => {
                Some((output.stdout.clone(), output.stderr.clone()))
            }
            _ => None,
        })
        .unwrap_or_else(|| (trace.summary.aggregated_output.clone(), String::new()));
    let stdout = bound(&stdout);
    let stderr = bound(&stderr);
    let cancelled = trace.summary.timed_out;
    let success = trace.summary.exit_code == Some(0) && !cancelled;
    let report = LinuxSystemdReport {
        schema_version: 1,
        tool: "linux_systemd".to_string(),
        action: input.action.as_str().to_string(),
        unit: input.unit,
        argv,
        command,
        status: if success {
            "ok"
        } else if cancelled {
            "cancelled"
        } else {
            "failed"
        }
        .to_string(),
        stdout: stdout.0,
        stderr: stderr.0,
        exit_code: trace.summary.exit_code,
        truncated: stdout.1 || stderr.1,
        cancelled,
    };
    let runtime_event = mutation_event(&report);
    let output =
        serde_json::to_string(&report).unwrap_or_else(|_| "{\"status\":\"failed\"}".to_string());
    let response = if success {
        ToolResponse::completed(id, output, report.exit_code, Vec::<ToolFileChange>::new())
    } else {
        ToolResponse::failed(id, output, report.exit_code, Vec::<ToolFileChange>::new())
    };
    Ok(response
        .with_lifecycle_events(trace.events)
        .with_runtime_events(vec![runtime_event]))
}

fn mutation_event(report: &LinuxSystemdReport) -> ToolRuntimeEvent {
    ToolRuntimeEvent::LinuxSystemd {
        action: report.action.clone(),
        unit: report.unit.clone(),
        status: report.status.clone(),
        command: report.command.clone(),
        exit_code: report.exit_code,
        truncated: report.truncated,
        mutation: true,
    }
}

fn validate_unit(unit: &str) -> Result<(), String> {
    if unit.is_empty() {
        return Err("systemd unit must not be empty".to_string());
    }
    if unit.len() > MAX_UNIT_BYTES {
        return Err("systemd unit must be at most 256 bytes".to_string());
    }
    if unit.starts_with('-') {
        return Err("systemd unit must not start with '-'".to_string());
    }
    if unit.chars().any(char::is_control) {
        return Err("systemd unit must not contain control characters".to_string());
    }
    if !unit
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || ".@_:-".contains(c))
    {
        return Err("systemd unit contains unsupported characters".to_string());
    }
    Ok(())
}

fn bound(value: &str) -> (String, bool) {
    if value.len() <= MAX_OUTPUT_BYTES {
        return (value.to_string(), false);
    }
    let mut end = MAX_OUTPUT_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    (value[..end].to_string(), true)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use crate::ToolStatus;
    #[cfg(unix)]
    use std::sync::{Mutex, OnceLock};
    #[test]
    fn fixed_argv_is_user_scoped_and_rejects_injection() {
        let input = LinuxSystemdInput {
            action: SystemdAction::Restart,
            unit: "yunxi-linux.service".into(),
        };
        assert_eq!(
            fixed_argv(&input).unwrap(),
            ["systemctl", "--user", "restart", "yunxi-linux.service"]
        );
        for unit in ["--now", "foo/bar", "foo;restart", "foo\nbar"] {
            assert!(
                fixed_argv(&LinuxSystemdInput {
                    action: SystemdAction::Start,
                    unit: unit.into()
                })
                .is_err()
            );
        }
    }

    #[test]
    fn systemd_unit_validation_reports_the_rejected_condition() {
        assert_eq!(
            validate_unit(""),
            Err("systemd unit must not be empty".to_string())
        );
        assert_eq!(
            validate_unit("--now"),
            Err("systemd unit must not start with '-'".to_string())
        );
        assert_eq!(
            validate_unit("foo\nbar"),
            Err("systemd unit must not contain control characters".to_string())
        );
        assert_eq!(
            validate_unit(&"a".repeat(MAX_UNIT_BYTES + 1)),
            Err("systemd unit must be at most 256 bytes".to_string())
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fake_systemctl_receives_fixed_user_argv() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;
        use tempfile::tempdir;
        use yunxi_agent_sandbox::{ApprovalRequirement, NetworkPolicy, SandboxRequirement};

        static PATH_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let _path_guard = PATH_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .expect("path lock");
        let dir = tempdir().expect("tempdir");
        let script = dir.path().join("systemctl");
        let marker = dir.path().join("argv");
        fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\n",
                marker.display()
            ),
        )
        .expect("script");
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("chmod");
        let old_path = std::env::var_os("PATH");
        let path = format!(
            "{}:{}",
            dir.path().display(),
            old_path.as_deref().unwrap_or_default().to_string_lossy()
        );
        // SAFETY: this test is the only PATH-mutating test in this module and
        // restores the process environment before returning.
        unsafe {
            std::env::set_var("PATH", path);
        }
        let policy = ExecutionPolicy {
            approval: ApprovalRequirement::PreApproved,
            sandbox: SandboxRequirement::DangerFullAccess,
            network: NetworkPolicy::Inherit,
            workspace_root: dir.path().to_path_buf(),
        };
        let response = execute(
            Some("fake".into()),
            dir.path(),
            LinuxSystemdInput {
                action: SystemdAction::Restart,
                unit: "demo.service".into(),
            },
            policy,
            AgentCancellationToken::new(),
        )
        .await
        .expect("execute");
        if let Some(path) = old_path {
            unsafe {
                std::env::set_var("PATH", path);
            }
        } else {
            unsafe {
                std::env::remove_var("PATH");
            }
        }
        assert_eq!(response.status, ToolStatus::Completed);
        assert_eq!(
            fs::read_to_string(marker).expect("argv"),
            "--user\nrestart\ndemo.service\n"
        );
    }
}
