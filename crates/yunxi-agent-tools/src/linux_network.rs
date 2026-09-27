//! Approval-gated, typed Linux network mutations.
//!
//! Only a small, explicit subset of `ip` is exposed. Every request becomes a
//! fixed argv vector; shell syntax, sudo, and arbitrary flags are impossible.

use crate::{ToolFileChange, ToolResponse, ToolRuntimeEvent};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::Path;
use yunxi_agent_core::{AgentCancellationToken, AgentResult};
use yunxi_agent_exec::{ExecCommand, ExecManager};
use yunxi_agent_sandbox::ExecutionPolicy;

const MAX_TOKEN_BYTES: usize = 128;
const MAX_OUTPUT_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum LinuxNetworkInput {
    LinkUp {
        interface: String,
    },
    LinkDown {
        interface: String,
    },
    AddrAdd {
        interface: String,
        address: String,
        prefix: u8,
    },
    AddrDel {
        interface: String,
        address: String,
        prefix: u8,
    },
    RouteAdd {
        destination: String,
        via: Option<String>,
        interface: Option<String>,
    },
    RouteDel {
        destination: String,
        via: Option<String>,
        interface: Option<String>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LinuxNetworkReport {
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
            "operation": {"type": "string", "enum": ["link_up", "link_down", "addr_add", "addr_del", "route_add", "route_del"]},
            "interface": {"type": "string", "description": "Network interface token."},
            "address": {"type": "string", "description": "IPv4/IPv6 address token without shell syntax."},
            "prefix": {"type": "integer", "minimum": 0, "maximum": 128},
            "destination": {"type": "string", "description": "Route destination token, for example default or 10.0.0.0/8."},
            "via": {"type": ["string", "null"], "description": "Optional gateway address."}
        },
        "required": ["operation"],
        "additionalProperties": false
    })
}

pub fn fixed_argv(input: &LinuxNetworkInput) -> Result<Vec<String>, String> {
    match input {
        LinuxNetworkInput::LinkUp { interface } => {
            validate_interface(interface)?;
            Ok(vec!["ip", "link", "set", "dev", interface, "up"]
                .into_iter()
                .map(String::from)
                .collect())
        }
        LinuxNetworkInput::LinkDown { interface } => {
            validate_interface(interface)?;
            Ok(vec!["ip", "link", "set", "dev", interface, "down"]
                .into_iter()
                .map(String::from)
                .collect())
        }
        LinuxNetworkInput::AddrAdd {
            interface,
            address,
            prefix,
        }
        | LinuxNetworkInput::AddrDel {
            interface,
            address,
            prefix,
        } => {
            validate_interface(interface)?;
            validate_token(address, "address")?;
            if *prefix > 128 {
                return Err("network prefix must be between 0 and 128".to_string());
            }
            let action = if matches!(input, LinuxNetworkInput::AddrAdd { .. }) {
                "add"
            } else {
                "del"
            };
            Ok(vec![
                "ip",
                "addr",
                action,
                &format!("{address}/{prefix}"),
                "dev",
                interface,
            ]
            .into_iter()
            .map(String::from)
            .collect())
        }
        LinuxNetworkInput::RouteAdd {
            destination,
            via,
            interface,
        }
        | LinuxNetworkInput::RouteDel {
            destination,
            via,
            interface,
        } => {
            validate_token(destination, "destination")?;
            if via.is_none() && interface.is_none() {
                return Err("route mutation requires via or interface".to_string());
            }
            if let Some(via) = via {
                validate_token(via, "gateway")?;
            }
            if let Some(interface) = interface {
                validate_interface(interface)?;
            }
            let action = if matches!(input, LinuxNetworkInput::RouteAdd { .. }) {
                "add"
            } else {
                "del"
            };
            let mut argv = vec![
                "ip".to_string(),
                "route".to_string(),
                action.to_string(),
                destination.clone(),
            ];
            if let Some(via) = via {
                argv.extend(["via".to_string(), via.clone()]);
            }
            if let Some(interface) = interface {
                argv.extend(["dev".to_string(), interface.clone()]);
            }
            Ok(argv)
        }
    }
}

pub async fn execute(
    id: Option<String>,
    cwd: &Path,
    input: LinuxNetworkInput,
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
    input: LinuxNetworkInput,
    policy: ExecutionPolicy,
    cancellation: AgentCancellationToken,
    env: std::collections::BTreeMap<String, String>,
) -> AgentResult<ToolResponse> {
    let operation = operation_name(&input);
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
            let report = LinuxNetworkReport {
                schema_version: 1,
                tool: "linux_network".to_string(),
                operation,
                argv,
                command,
                status: "unavailable".to_string(),
                stdout: String::new(),
                stderr: error.to_string(),
                exit_code: None,
                truncated: false,
                cancelled: false,
                requires_root: true,
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
    let report = LinuxNetworkReport {
        schema_version: 1,
        tool: "linux_network".to_string(),
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
        .to_string(),
        stdout: stdout.0,
        stderr: stderr.0,
        exit_code: trace.summary.exit_code,
        truncated: stdout.1 || stderr.1,
        cancelled,
        requires_root: true,
    };
    let output =
        serde_json::to_string(&report).unwrap_or_else(|_| "{\"status\":\"failed\"}".to_string());
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

fn operation_name(input: &LinuxNetworkInput) -> String {
    match input {
        LinuxNetworkInput::LinkUp { .. } => "link_up",
        LinuxNetworkInput::LinkDown { .. } => "link_down",
        LinuxNetworkInput::AddrAdd { .. } => "addr_add",
        LinuxNetworkInput::AddrDel { .. } => "addr_del",
        LinuxNetworkInput::RouteAdd { .. } => "route_add",
        LinuxNetworkInput::RouteDel { .. } => "route_del",
    }
    .to_string()
}

fn mutation_event(report: &LinuxNetworkReport) -> ToolRuntimeEvent {
    ToolRuntimeEvent::LinuxNetwork {
        operation: report.operation.clone(),
        status: report.status.clone(),
        command: report.command.clone(),
        exit_code: report.exit_code,
        truncated: report.truncated,
        mutation: true,
    }
}

fn validate_interface(value: &str) -> Result<(), String> {
    validate_token(value, "interface")
}
fn validate_token(value: &str, label: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > MAX_TOKEN_BYTES
        || value.starts_with('-')
        || value.chars().any(char::is_control)
    {
        return Err(format!(
            "{label} must be a non-empty token of at most 128 bytes"
        ));
    }
    if !value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || ".:/_-".contains(c))
    {
        return Err(format!("{label} contains unsupported characters"));
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
    #[test]
    fn fixed_argv_covers_only_typed_ip_forms() {
        let input = LinuxNetworkInput::AddrAdd {
            interface: "eth0".into(),
            address: "10.0.0.2".into(),
            prefix: 24,
        };
        assert_eq!(
            fixed_argv(&input).unwrap(),
            ["ip", "addr", "add", "10.0.0.2/24", "dev", "eth0"]
        );
        assert!(
            fixed_argv(&LinuxNetworkInput::LinkUp {
                interface: "eth0;down".into()
            })
            .is_err()
        );
        assert!(
            fixed_argv(&LinuxNetworkInput::RouteAdd {
                destination: "default".into(),
                via: None,
                interface: None
            })
            .is_err()
        );
    }
}
