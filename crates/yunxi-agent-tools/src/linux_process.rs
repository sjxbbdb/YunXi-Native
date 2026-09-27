//! Approval-gated typed Linux process control.
//!
//! Only signal delivery and priority changes are exposed. The runner receives
//! a fixed argv vector and never invokes a shell or sudo.

use crate::{ToolFileChange, ToolResponse, ToolRuntimeEvent};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::Path;
use yunxi_agent_core::{AgentCancellationToken, AgentResult};
use yunxi_agent_exec::{ExecCommand, ExecManager};
use yunxi_agent_sandbox::ExecutionPolicy;

const MAX_OUTPUT_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessSignal {
    Term,
    Int,
    Hup,
    Kill,
    Stop,
    Cont,
}

impl ProcessSignal {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Term => "TERM",
            Self::Int => "INT",
            Self::Hup => "HUP",
            Self::Kill => "KILL",
            Self::Stop => "STOP",
            Self::Cont => "CONT",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum LinuxProcessInput {
    Signal { pid: u32, signal: ProcessSignal },
    Renice { pid: u32, priority: i8 },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LinuxProcessReport {
    pub schema_version: u32,
    pub tool: String,
    pub operation: String,
    pub argv: Vec<String>,
    pub command: String,
    pub status: String,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub truncated: bool,
    pub cancelled: bool,
    pub requires_root: bool,
}

pub fn parameters_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "operation": {"type": "string", "enum": ["signal", "renice"]},
            "pid": {"type": "integer", "minimum": 1, "maximum": 4194304},
            "signal": {"type": "string", "enum": ["term", "int", "hup", "kill", "stop", "cont"]},
            "priority": {"type": "integer", "minimum": -20, "maximum": 19}
        },
        "required": ["operation", "pid"],
        "additionalProperties": false
    })
}

pub fn fixed_argv(input: &LinuxProcessInput) -> Result<Vec<String>, String> {
    match input {
        LinuxProcessInput::Signal { pid, signal } => {
            validate_pid(*pid)?;
            Ok(vec![
                "kill".into(),
                format!("-{}", signal.as_str()),
                pid.to_string(),
            ])
        }
        LinuxProcessInput::Renice { pid, priority } => {
            validate_pid(*pid)?;
            if !(-20..=19).contains(priority) {
                return Err("process priority must be between -20 and 19".to_string());
            }
            Ok(vec![
                "renice".into(),
                "-n".into(),
                priority.to_string(),
                "-p".into(),
                pid.to_string(),
            ])
        }
    }
}

pub async fn execute(
    id: Option<String>,
    cwd: &Path,
    input: LinuxProcessInput,
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
    input: LinuxProcessInput,
    policy: ExecutionPolicy,
    cancellation: AgentCancellationToken,
    env: std::collections::BTreeMap<String, String>,
) -> AgentResult<ToolResponse> {
    let operation = operation_name(&input);
    let requires_root = requires_root(&input);
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
            let report = LinuxProcessReport {
                schema_version: 1,
                tool: "linux_process".into(),
                operation,
                argv,
                command,
                status: "unavailable".into(),
                stdout: String::new(),
                stderr: error.to_string(),
                exit_code: None,
                truncated: false,
                cancelled: false,
                requires_root,
            };
            let output = serde_json::to_string(&report).unwrap_or_else(|_| error.to_string());
            return Ok(ToolResponse::failed(id, output, None, Vec::new())
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
    let report = LinuxProcessReport {
        schema_version: 1,
        tool: "linux_process".into(),
        operation,
        argv,
        command,
        status: if success {
            "ok"
        } else if cancelled {
            "cancelled"
        } else {
            "failed"
        }
        .into(),
        stdout: stdout.0,
        stderr: stderr.0,
        exit_code: trace.summary.exit_code,
        truncated: stdout.1 || stderr.1,
        cancelled,
        requires_root,
    };
    let output =
        serde_json::to_string(&report).unwrap_or_else(|_| "{\"status\":\"failed\"}".into());
    let event = mutation_event(&report);
    let response = if success {
        ToolResponse::completed(id, output, report.exit_code, Vec::<ToolFileChange>::new())
    } else {
        ToolResponse::failed(id, output, report.exit_code, Vec::<ToolFileChange>::new())
    };
    Ok(response
        .with_lifecycle_events(trace.events)
        .with_runtime_events(vec![event]))
}

fn validate_pid(pid: u32) -> Result<(), String> {
    if pid == 0 || pid > 4_194_304 {
        Err("pid must be between 1 and 4194304".to_string())
    } else {
        Ok(())
    }
}
fn operation_name(input: &LinuxProcessInput) -> String {
    match input {
        LinuxProcessInput::Signal { .. } => "signal",
        LinuxProcessInput::Renice { .. } => "renice",
    }
    .into()
}
fn requires_root(input: &LinuxProcessInput) -> bool {
    match input {
        LinuxProcessInput::Signal { signal, .. } => {
            matches!(signal, ProcessSignal::Kill | ProcessSignal::Stop)
        }
        LinuxProcessInput::Renice { priority, .. } => *priority < 0,
    }
}
fn mutation_event(report: &LinuxProcessReport) -> ToolRuntimeEvent {
    ToolRuntimeEvent::LinuxProcess {
        operation: report.operation.clone(),
        status: report.status.clone(),
        command: report.command.clone(),
        exit_code: report.exit_code,
        truncated: report.truncated,
        requires_root: report.requires_root,
        mutation: true,
    }
}
fn bound(value: &str) -> (String, bool) {
    if value.len() <= MAX_OUTPUT_BYTES {
        return (value.into(), false);
    }
    let mut end = MAX_OUTPUT_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    (value[..end].into(), true)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixed_argv_rejects_pid_and_priority_injection() {
        assert_eq!(
            fixed_argv(&LinuxProcessInput::Signal {
                pid: 42,
                signal: ProcessSignal::Term
            })
            .unwrap(),
            ["kill", "-TERM", "42"]
        );
        assert_eq!(
            fixed_argv(&LinuxProcessInput::Renice {
                pid: 42,
                priority: -5
            })
            .unwrap(),
            ["renice", "-n", "-5", "-p", "42"]
        );
        assert!(validate_pid(0).is_err());
        assert!(
            fixed_argv(&LinuxProcessInput::Renice {
                pid: 42,
                priority: 20
            })
            .is_err()
        );
    }
}
