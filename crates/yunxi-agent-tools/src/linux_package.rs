//! Typed Arch/pacman package actions.
//!
//! Package mutations never accept shell text.  The action and package token
//! are validated before constructing a fixed pacman argv.  Installing,
//! removing, and upgrading are deliberately approval-gated by the request
//! constructor; search and info are read-only queries.

use crate::{ToolFileChange, ToolResponse, ToolRuntimeEvent};
use serde::{Deserialize, Serialize};
use std::path::Path;
use yunxi_agent_core::{AgentCancellationToken, AgentResult};
use yunxi_agent_exec::{ExecCommand, ExecManager};
use yunxi_agent_sandbox::ExecutionPolicy;

const MAX_PACKAGE_BYTES: usize = 256;
const MAX_OUTPUT_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageAction {
    Install,
    Remove,
    Upgrade,
    Search,
    Info,
}

impl PackageAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Install => "install",
            Self::Remove => "remove",
            Self::Upgrade => "upgrade",
            Self::Search => "search",
            Self::Info => "info",
        }
    }

    pub fn mutates(self) -> bool {
        matches!(self, Self::Install | Self::Remove | Self::Upgrade)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LinuxPackageInput {
    pub action: PackageAction,
    pub package: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LinuxPackageReport {
    pub schema_version: u32,
    pub tool: String,
    pub action: String,
    pub package: String,
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
    serde_json::json!({
        "type": "object",
        "properties": {
            "action": {"type": "string", "enum": ["install", "remove", "upgrade", "search", "info"]},
            "package": {"type": "string", "description": "One Arch package name or search token; shell syntax and extra arguments are rejected."}
        },
        "required": ["action", "package"],
        "additionalProperties": false
    })
}

pub fn fixed_argv(input: &LinuxPackageInput) -> Result<Vec<String>, String> {
    validate_package(&input.package)?;
    let package = input.package.clone();
    Ok(match input.action {
        PackageAction::Install => vec!["pacman".into(), "--sync".into(), "--".into(), package],
        PackageAction::Remove => vec!["pacman".into(), "--remove".into(), "--".into(), package],
        PackageAction::Upgrade => vec!["pacman".into(), "--sync".into(), "--".into(), package],
        PackageAction::Search => vec!["pacman".into(), "--search".into(), "--".into(), package],
        PackageAction::Info => vec!["pacman".into(), "--info".into(), "--".into(), package],
    })
}

pub async fn execute(
    id: Option<String>,
    cwd: &Path,
    input: LinuxPackageInput,
    policy: ExecutionPolicy,
    cancellation: AgentCancellationToken,
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
        env: std::collections::BTreeMap::new(),
        timeout_millis: Some(120_000),
        policy,
    };
    let trace = match ExecManager::default()
        .run_with_cancellation(request, cancellation)
        .await
    {
        Ok(trace) => trace,
        Err(error) => {
            let report = report(
                &input,
                argv,
                command,
                "unavailable",
                String::new(),
                error.to_string(),
                None,
                false,
                false,
            );
            return Ok(ToolResponse::failed(
                id,
                serde_json::to_string(&report).unwrap_or_else(|_| error.to_string()),
                None,
                Vec::new(),
            )
            .with_runtime_events(vec![package_event(&report)]));
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
    let (stdout, stdout_truncated) = bound(&stdout);
    let (stderr, stderr_truncated) = bound(&stderr);
    let cancelled = trace.summary.timed_out;
    let success = trace.summary.exit_code == Some(0) && !cancelled;
    let status = if success {
        "ok"
    } else if cancelled {
        "cancelled"
    } else {
        "failed"
    };
    let report = report(
        &input,
        argv,
        command,
        status,
        stdout,
        stderr,
        trace.summary.exit_code,
        stdout_truncated || stderr_truncated,
        cancelled,
    );
    let output =
        serde_json::to_string(&report).unwrap_or_else(|_| "{\"status\":\"failed\"}".into());
    let response = if success {
        ToolResponse::completed(id, output, report.exit_code, Vec::<ToolFileChange>::new())
    } else {
        ToolResponse::failed(id, output, report.exit_code, Vec::<ToolFileChange>::new())
    };
    Ok(response
        .with_lifecycle_events(trace.events)
        .with_runtime_events(vec![package_event(&report)]))
}

fn report(
    input: &LinuxPackageInput,
    argv: Vec<String>,
    command: String,
    status: &str,
    stdout: String,
    stderr: String,
    exit_code: Option<i32>,
    truncated: bool,
    cancelled: bool,
) -> LinuxPackageReport {
    LinuxPackageReport {
        schema_version: 1,
        tool: "linux_package".into(),
        action: input.action.as_str().into(),
        package: input.package.clone(),
        argv,
        command,
        status: status.into(),
        stdout,
        stderr,
        exit_code,
        truncated,
        cancelled,
        requires_root: input.action.mutates(),
    }
}

fn package_event(report: &LinuxPackageReport) -> ToolRuntimeEvent {
    ToolRuntimeEvent::LinuxPackage {
        action: report.action.clone(),
        package: report.package.clone(),
        status: report.status.clone(),
        command: report.command.clone(),
        exit_code: report.exit_code,
        truncated: report.truncated,
        mutation: report.action != "search" && report.action != "info",
    }
}

fn validate_package(package: &str) -> Result<(), String> {
    if package.is_empty()
        || package.len() > MAX_PACKAGE_BYTES
        || package.starts_with('-')
        || package.chars().any(char::is_control)
    {
        return Err("pacman package must be a non-empty token of at most 256 bytes".into());
    }
    if !package
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || ".@_:+-/*?=".contains(c))
    {
        return Err("pacman package contains unsupported shell or argument syntax".into());
    }
    Ok(())
}

fn bound(value: &str) -> (String, bool) {
    if value.len() <= MAX_OUTPUT_BYTES {
        return (value.to_string(), false);
    }
    let mut end = MAX_OUTPUT_BYTES;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    (value[..end].to_string(), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_argv_is_stable_and_rejects_injection() {
        let input = LinuxPackageInput {
            action: PackageAction::Install,
            package: "ripgrep".into(),
        };
        assert_eq!(
            fixed_argv(&input).unwrap(),
            ["pacman", "--sync", "--", "ripgrep"]
        );
        for package in [
            "--noconfirm",
            "foo;touch /tmp/x",
            "foo\nbar",
            "pacman --remove foo",
        ] {
            assert!(
                fixed_argv(&LinuxPackageInput {
                    action: PackageAction::Info,
                    package: package.into()
                })
                .is_err()
            );
        }
    }

    #[test]
    fn mutation_root_boundary_is_explicit() {
        assert!(PackageAction::Install.mutates());
        assert!(PackageAction::Remove.mutates());
        assert!(PackageAction::Upgrade.mutates());
        assert!(!PackageAction::Search.mutates());
        assert!(!PackageAction::Info.mutates());
    }
}
