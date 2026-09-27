use crate::{ToolFileChange, ToolResponse, ToolRuntimeEvent, ToolStatus};
use serde_json::{Value, json};
use std::path::Path;
use yunxi_agent_core::{AgentCancellationToken, AgentResult};
use yunxi_agent_exec::{ExecCommand, ExecManager};

pub const MAX_OUTPUT_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LinuxReadOperation {
    SystemdStatus,
    ManPage,
    ProcessList,
    NetworkSnapshot,
    FilesystemSummary,
    FilesystemList,
    DiskUsage,
    PacmanQuery,
}

impl LinuxReadOperation {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "systemd_status" | "systemd" => Some(Self::SystemdStatus),
            "man_page" | "man" => Some(Self::ManPage),
            "process_list" | "processes" | "process" => Some(Self::ProcessList),
            "network_snapshot" | "network" => Some(Self::NetworkSnapshot),
            "filesystem_summary" | "filesystem" => Some(Self::FilesystemSummary),
            "filesystem_list" | "directory_list" => Some(Self::FilesystemList),
            "disk_usage" | "disk" | "df" => Some(Self::DiskUsage),
            "pacman_query" | "pacman" => Some(Self::PacmanQuery),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::SystemdStatus => "systemd_status",
            Self::ManPage => "man_page",
            Self::ProcessList => "process_list",
            Self::NetworkSnapshot => "network_snapshot",
            Self::FilesystemSummary => "filesystem_summary",
            Self::FilesystemList => "filesystem_list",
            Self::DiskUsage => "disk_usage",
            Self::PacmanQuery => "pacman_query",
        }
    }
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn parameters_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "operation": {
                "type": "string",
                "enum": ["systemd_status", "man_page", "process_list", "network_snapshot", "filesystem_summary", "filesystem_list", "disk_usage", "pacman_query"]
            },
            "unit": {"type": "string", "description": "A systemd unit name; no paths or shell syntax."},
            "user": {"type": "boolean", "description": "Inspect the per-user systemd manager (default true)."},
            "topic": {"type": "string", "description": "A single man-page topic token."},
            "section": {"type": "string", "description": "Optional man section token."},
            "limit": {"type": "integer", "minimum": 1, "maximum": 200},
            "view": {"type": "string", "enum": ["addr", "route", "sockets"]},
            "path": {"type": "string", "description": "A local directory path; no shell expansion is performed."}
            ,"mode": {"type": "string", "enum": ["info", "search"]}
            ,"package": {"type": "string", "description": "A package name or search token; no shell syntax."}
        },
        "required": ["operation"],
        "additionalProperties": false
    })
}

pub async fn execute(
    id: Option<String>,
    cwd: &Path,
    operation: &str,
    arguments: &Value,
    policy: yunxi_agent_sandbox::ExecutionPolicy,
    cancellation: AgentCancellationToken,
) -> AgentResult<ToolResponse> {
    let operation = match LinuxReadOperation::parse(operation) {
        Some(operation) => operation,
        None => {
            return Ok(ToolResponse::declined(
                id,
                format!("unsupported linux_readonly operation: {operation}"),
            ));
        }
    };
    let argv = match fixed_argv(operation, arguments) {
        Ok(argv) => argv,
        Err(message) => return Ok(ToolResponse::declined(id, message)),
    };
    let command = canonical_command(&argv);
    let request = ExecCommand {
        id: id.clone(),
        cwd: cwd.to_path_buf(),
        command: command.clone(),
        argv,
        stdin: None,
        env: fixed_environment(operation),
        timeout_millis: Some(10_000),
        policy,
    };

    let trace = match ExecManager::default()
        .run_with_cancellation(request, cancellation)
        .await
    {
        Ok(trace) => trace,
        Err(error) => {
            let result = json!({
                "schema_version": 1,
                "tool": "linux_readonly",
                "operation": operation.as_str(),
                "status": "unavailable",
                "error": error.to_string(),
                "command": command,
            });
            return Ok(
                ToolResponse::failed(id, result.to_string(), None, Vec::new()).with_runtime_events(
                    vec![ToolRuntimeEvent::LinuxReadOnly {
                        operation: operation.as_str().to_string(),
                        status: "unavailable".to_string(),
                        command,
                        exit_code: None,
                        truncated: false,
                    }],
                ),
            );
        }
    };
    let (raw_stdout, raw_stderr) = trace
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
    let raw_stdout = match operation {
        LinuxReadOperation::ProcessList
        | LinuxReadOperation::FilesystemSummary
        | LinuxReadOperation::FilesystemList => {
            let default_limit = if operation == LinuxReadOperation::ProcessList {
                40
            } else {
                100
            };
            let limit = arguments
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(default_limit) as usize;
            limit_lines(&raw_stdout, limit)
        }
        _ => raw_stdout,
    };
    let stdout = bounded_output(&raw_stdout);
    let stderr = bounded_output(&raw_stderr);
    let status = if trace.summary.exit_code == Some(0) && !trace.summary.timed_out {
        ToolStatus::Completed
    } else {
        ToolStatus::Failed
    };
    let result = json!({
        "schema_version": 1,
        "tool": "linux_readonly",
        "operation": operation.as_str(),
        "status": if status == ToolStatus::Completed { "ok" } else { "failed" },
        "stdout": stdout.text,
        "stderr": stderr.text,
        "truncated": stdout.truncated || stderr.truncated,
        "exit_code": trace.summary.exit_code,
        "command": command,
    });
    let response = match status {
        ToolStatus::Completed => ToolResponse::completed(
            id,
            result.to_string(),
            trace.summary.exit_code,
            Vec::<ToolFileChange>::new(),
        ),
        _ => ToolResponse::failed(
            id,
            result.to_string(),
            trace.summary.exit_code,
            Vec::<ToolFileChange>::new(),
        ),
    };
    let truncated = stdout.truncated || stderr.truncated;
    Ok(response
        .with_lifecycle_events(trace.events)
        .with_runtime_events(vec![ToolRuntimeEvent::LinuxReadOnly {
            operation: operation.as_str().to_string(),
            status: if status == ToolStatus::Completed {
                "ok".to_string()
            } else {
                "failed".to_string()
            },
            command,
            exit_code: trace.summary.exit_code,
            truncated,
        }]))
}

fn fixed_argv(operation: LinuxReadOperation, arguments: &Value) -> Result<Vec<String>, String> {
    match operation {
        LinuxReadOperation::SystemdStatus => {
            let unit = required_token(arguments, "unit")?;
            let user = arguments
                .get("user")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            if !unit
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || ".@_:-".contains(c))
            {
                return Err("systemd unit contains unsupported characters".to_string());
            }
            let mut argv = vec!["systemctl".into()];
            if user {
                argv.push("--user".into());
            }
            argv.extend([
                "--no-pager".into(),
                "--plain".into(),
                "--full".into(),
                "show".into(),
                unit,
            ]);
            Ok(argv)
        }
        LinuxReadOperation::ManPage => {
            let topic = required_token(arguments, "topic")?;
            let section = arguments.get("section").and_then(Value::as_str);
            if !valid_token(&topic) || section.is_some_and(|value| !valid_token(value)) {
                return Err("man topic or section contains unsupported characters".to_string());
            }
            let mut argv = vec!["man".into(), "--locale=C".into(), "-P".into(), "cat".into()];
            if let Some(section) = section {
                argv.push(section.to_string());
            }
            argv.push(topic);
            Ok(argv)
        }
        LinuxReadOperation::ProcessList => {
            let _ = process_limit(arguments)?;
            Ok(vec![
                "ps".into(),
                "-eo".into(),
                "pid=,ppid=,user=,stat=,etime=,comm=".into(),
                "--sort=pid".into(),
            ])
        }
        LinuxReadOperation::NetworkSnapshot => {
            let view = arguments
                .get("view")
                .and_then(Value::as_str)
                .unwrap_or("addr");
            match view {
                "addr" => Ok(vec!["ip".into(), "-json".into(), "addr".into()]),
                "route" => Ok(vec!["ip".into(), "-json".into(), "route".into()]),
                "sockets" => Ok(vec!["ss".into(), "-H".into(), "-tun".into()]),
                _ => Err("network view must be addr, route, or sockets".to_string()),
            }
        }
        LinuxReadOperation::FilesystemSummary | LinuxReadOperation::FilesystemList => {
            let path = arguments
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or(".")
                .trim();
            validate_path(path)?;
            let limit = arguments
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(100);
            if !(1..=200).contains(&limit) {
                return Err("filesystem limit must be between 1 and 200".to_string());
            }
            let operation = match operation {
                LinuxReadOperation::FilesystemSummary => "summary",
                LinuxReadOperation::FilesystemList => "list",
                _ => unreachable!(),
            };
            Ok(vec![
                "find".into(),
                "--".into(),
                path.to_string(),
                "-mindepth".into(),
                "1".into(),
                "-maxdepth".into(),
                "1".into(),
                "-printf".into(),
                format!("%y\\t%s\\t%f\\t{operation}\\n"),
            ])
        }
        LinuxReadOperation::DiskUsage => {
            let path = arguments
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or(".")
                .trim();
            validate_path(path)?;
            Ok(vec![
                "df".into(),
                "-P".into(),
                "-k".into(),
                "--".into(),
                path.to_string(),
            ])
        }
        LinuxReadOperation::PacmanQuery => {
            let mode = arguments
                .get("mode")
                .and_then(Value::as_str)
                .unwrap_or("search");
            let package = required_token(arguments, "package")?;
            if !valid_pacman_token(&package) {
                return Err("pacman package contains unsupported characters".to_string());
            }
            let flag = match mode {
                "info" => "--info",
                "search" => "--search",
                _ => return Err("pacman mode must be info or search".to_string()),
            };
            Ok(vec!["pacman".into(), flag.into(), "--".into(), package])
        }
    }
}

fn process_limit(arguments: &Value) -> Result<u64, String> {
    let limit = arguments.get("limit").and_then(Value::as_u64).unwrap_or(40);
    if !(1..=200).contains(&limit) {
        return Err("process limit must be between 1 and 200".to_string());
    }
    Ok(limit)
}

fn limit_lines(value: &str, limit: usize) -> String {
    value.lines().take(limit).collect::<Vec<_>>().join("\n")
}

fn fixed_environment(operation: LinuxReadOperation) -> std::collections::BTreeMap<String, String> {
    let mut env = std::collections::BTreeMap::new();
    if matches!(operation, LinuxReadOperation::ManPage) {
        env.insert("MANPAGER".into(), "cat".into());
        env.insert("PAGER".into(), "cat".into());
    }
    env
}

fn required_token(arguments: &Value, key: &str) -> Result<String, String> {
    let value = arguments
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if value.is_empty() {
        Err(format!("linux_readonly requires {key}"))
    } else {
        Ok(value.to_string())
    }
}

fn valid_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || ".@_:+-".contains(c))
}

fn validate_path(value: &str) -> Result<(), String> {
    if value.is_empty() || value.len() > 4096 || value.chars().any(char::is_control) {
        return Err(
            "filesystem path must be non-empty, <= 4096 bytes, and contain no control characters"
                .to_string(),
        );
    }
    Ok(())
}

fn valid_pacman_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || ".@_:+-/*?".contains(c))
}

fn canonical_command(argv: &[String]) -> String {
    argv.iter()
        .map(|arg| {
            if arg
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || ".@_:+-=/".contains(c))
            {
                arg.clone()
            } else {
                format!("'{arg}'")
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

struct BoundedOutput {
    text: String,
    truncated: bool,
}
fn bounded_output(value: &str) -> BoundedOutput {
    if value.len() <= MAX_OUTPUT_BYTES {
        return BoundedOutput {
            text: value.to_string(),
            truncated: false,
        };
    }
    let mut end = MAX_OUTPUT_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    BoundedOutput {
        text: value[..end].to_string(),
        truncated: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_shell_syntax() {
        assert!(
            fixed_argv(
                LinuxReadOperation::SystemdStatus,
                &json!({"unit":"x;touch /tmp/y"})
            )
            .is_err()
        );
        assert!(fixed_argv(LinuxReadOperation::ManPage, &json!({"topic":"x|id"})).is_err());
        assert!(
            fixed_argv(
                LinuxReadOperation::FilesystemList,
                &json!({"path":"bad\npath"})
            )
            .is_err()
        );
        assert!(
            fixed_argv(
                LinuxReadOperation::PacmanQuery,
                &json!({"mode":"search", "package":"yunxi;touch"})
            )
            .is_err()
        );
    }
    #[test]
    fn command_is_fixed_argv() {
        let argv = fixed_argv(
            LinuxReadOperation::NetworkSnapshot,
            &json!({"view":"sockets"}),
        )
        .unwrap();
        assert!(!["sh", "bash", "zsh"].contains(&argv[0].as_str()));
        assert_eq!(argv, vec!["ss", "-H", "-tun"]);
    }
    #[test]
    fn all_operations_have_fixed_non_shell_argv() {
        let cases = [
            (
                LinuxReadOperation::SystemdStatus,
                json!({"unit":"yunxi-linux.service", "user": false}),
                vec![
                    "systemctl",
                    "--no-pager",
                    "--plain",
                    "--full",
                    "show",
                    "yunxi-linux.service",
                ],
            ),
            (
                LinuxReadOperation::ManPage,
                json!({"topic":"fish", "section":"1"}),
                vec!["man", "--locale=C", "-P", "cat", "1", "fish"],
            ),
            (
                LinuxReadOperation::ProcessList,
                json!({"limit": 3}),
                vec![
                    "ps",
                    "-eo",
                    "pid=,ppid=,user=,stat=,etime=,comm=",
                    "--sort=pid",
                ],
            ),
            (
                LinuxReadOperation::NetworkSnapshot,
                json!({"view":"route"}),
                vec!["ip", "-json", "route"],
            ),
            (
                LinuxReadOperation::FilesystemSummary,
                json!({"path":"/tmp/work", "limit": 3}),
                vec![
                    "find",
                    "--",
                    "/tmp/work",
                    "-mindepth",
                    "1",
                    "-maxdepth",
                    "1",
                    "-printf",
                    "%y\\t%s\\t%f\\tsummary\\n",
                ],
            ),
            (
                LinuxReadOperation::FilesystemList,
                json!({"path":".", "limit": 2}),
                vec![
                    "find",
                    "--",
                    ".",
                    "-mindepth",
                    "1",
                    "-maxdepth",
                    "1",
                    "-printf",
                    "%y\\t%s\\t%f\\tlist\\n",
                ],
            ),
            (
                LinuxReadOperation::DiskUsage,
                json!({"path":"/tmp/a path"}),
                vec!["df", "-P", "-k", "--", "/tmp/a path"],
            ),
            (
                LinuxReadOperation::PacmanQuery,
                json!({"mode":"info", "package":"yunxi-agent"}),
                vec!["pacman", "--info", "--", "yunxi-agent"],
            ),
        ];
        for (operation, arguments, expected) in cases {
            let argv = fixed_argv(operation, &arguments).expect("valid fixed argv");
            assert_eq!(argv, expected);
            assert!(!argv.iter().any(|argument| {
                matches!(argument.as_str(), "sh" | "bash" | "zsh" | "-c" | "-Command")
            }));
        }
    }
    #[test]
    fn process_limit_is_validated_and_applied_to_output() {
        assert!(fixed_argv(LinuxReadOperation::ProcessList, &json!({"limit": 0})).is_err());
        assert!(fixed_argv(LinuxReadOperation::ProcessList, &json!({"limit": 201})).is_err());
        assert_eq!(limit_lines("one\ntwo\nthree\nfour\n", 2), "one\ntwo");
        assert!(
            fixed_argv(
                LinuxReadOperation::FilesystemList,
                &json!({"path":".", "limit": 0})
            )
            .is_err()
        );
        assert!(
            fixed_argv(
                LinuxReadOperation::FilesystemSummary,
                &json!({"path":".", "limit": 201})
            )
            .is_err()
        );
        assert_eq!(
            fixed_argv(LinuxReadOperation::DiskUsage, &json!({"path":"."})).unwrap(),
            vec!["df", "-P", "-k", "--", "."]
        );
        assert!(fixed_argv(LinuxReadOperation::DiskUsage, &json!({"path":"bad\npath"})).is_err());
        assert!(
            fixed_argv(
                LinuxReadOperation::PacmanQuery,
                &json!({"mode":"search", "package":"yunxi-agent"})
            )
            .is_ok()
        );
    }
    #[test]
    fn output_is_bounded() {
        let bounded = bounded_output(&"x".repeat(MAX_OUTPUT_BYTES + 17));
        assert!(bounded.truncated);
        assert!(bounded.text.len() <= MAX_OUTPUT_BYTES);
    }
}
