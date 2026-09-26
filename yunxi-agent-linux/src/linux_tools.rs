//! Read-only Linux host probes used by the native tool layer.
//!
//! This module intentionally does not execute through a shell. Every probe
//! uses a fixed executable and validated positional arguments, returns bounded
//! JSON, and treats a missing Linux utility as an explicit `unavailable`
//! result. Mutating operations (install, restart, write, sudo) are not part of
//! this first tool-layer slice.

use anyhow::{Result, bail};
use clap::Subcommand;
use serde::Serialize;
use std::process::{Command, Output, Stdio};

const MAX_OUTPUT_BYTES: usize = 64 * 1024;
const DEFAULT_PROCESS_LIMIT: usize = 40;
const MAX_PROCESS_LIMIT: usize = 200;

#[derive(Debug, Clone, Subcommand)]
pub(crate) enum LinuxToolCommand {
    /// Print the stable read-only Linux tool catalog.
    Describe,
    /// Read user/system systemd status without changing service state.
    SystemdStatus {
        /// Optional unit name, for example `yunxi-linux.service`.
        #[arg(long)]
        unit: Option<String>,
    },
    /// Read one local manual page through a non-interactive pager.
    Man {
        /// Man topic such as `systemctl` or `fish`.
        topic: String,
    },
    /// Read a bounded process snapshot.
    Processes {
        /// Maximum number of process rows returned.
        #[arg(long, default_value_t = DEFAULT_PROCESS_LIMIT)]
        limit: usize,
    },
    /// Read local interface and listening-socket state.
    Network,
    /// Query the local pacman database or the configured sync databases.
    ///
    /// This intentionally exposes only pacman's read-only `--info` and
    /// `--search` modes.  Package installation, removal, upgrade, database
    /// refresh, and every other mutating mode stay outside this command.
    Pacman {
        /// Query metadata for an installed package (`pacman --info`).
        #[arg(long, conflicts_with = "search", required_unless_present = "search")]
        info: Option<String>,
        /// Search configured package databases (`pacman --search`).
        #[arg(long, conflicts_with = "info", required_unless_present = "info")]
        search: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize)]
struct LinuxToolSpec {
    name: &'static str,
    description: &'static str,
    risk_class: &'static str,
    requires_root: bool,
    mutates_system: bool,
}

#[derive(Debug, Serialize)]
struct LinuxToolResult {
    tool: &'static str,
    status: &'static str,
    risk_class: &'static str,
    requires_root: bool,
    mutates_system: bool,
    available: bool,
    exit_code: Option<i32>,
    command: Vec<String>,
    stdout: String,
    stderr: String,
}

pub(crate) fn run(command: LinuxToolCommand) -> Result<()> {
    let value = match command {
        LinuxToolCommand::Describe => serde_json::to_value(specs())?,
        LinuxToolCommand::SystemdStatus { unit } => {
            if let Some(unit) = &unit {
                validate_token(unit, "systemd unit")?;
            }
            serde_json::to_value(run_systemd_status(unit.as_deref()))?
        }
        LinuxToolCommand::Man { topic } => {
            validate_token(&topic, "man topic")?;
            serde_json::to_value(run_man(&topic))?
        }
        LinuxToolCommand::Processes { limit } => {
            let limit = limit.clamp(1, MAX_PROCESS_LIMIT);
            serde_json::to_value(run_processes(limit))?
        }
        LinuxToolCommand::Network => serde_json::to_value(run_network())?,
        LinuxToolCommand::Pacman { info, search } => {
            if let Some(package) = info.as_deref() {
                validate_pacman_token(package, "pacman package")?;
                serde_json::to_value(run_pacman("--info", package))?
            } else if let Some(query) = search.as_deref() {
                validate_pacman_token(query, "pacman search")?;
                serde_json::to_value(run_pacman("--search", query))?
            } else {
                bail!("pacman requires exactly one of --info or --search")
            }
        }
    };
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

fn specs() -> Vec<LinuxToolSpec> {
    vec![
        LinuxToolSpec {
            name: "linux.systemd_status",
            description: "Read systemd user/system status without changing service state.",
            risk_class: "read_only",
            requires_root: false,
            mutates_system: false,
        },
        LinuxToolSpec {
            name: "linux.man",
            description: "Read a local man page without opening an interactive pager.",
            risk_class: "read_only",
            requires_root: false,
            mutates_system: false,
        },
        LinuxToolSpec {
            name: "linux.processes",
            description: "Read a bounded process snapshot from the local host.",
            risk_class: "read_only",
            requires_root: false,
            mutates_system: false,
        },
        LinuxToolSpec {
            name: "linux.network",
            description: "Read local interface and listening-socket state.",
            risk_class: "read_only",
            requires_root: false,
            mutates_system: false,
        },
        LinuxToolSpec {
            name: "linux.pacman",
            description: "Read installed package metadata or search Arch package databases.",
            risk_class: "read_only",
            requires_root: false,
            mutates_system: false,
        },
    ]
}

fn run_systemd_status(unit: Option<&str>) -> LinuxToolResult {
    let mut args = vec!["--user".to_string(), "--no-pager".to_string()];
    if let Some(unit) = unit {
        args.extend([
            "status".to_string(),
            "--full".to_string(),
            "--plain".to_string(),
            "--".to_string(),
            unit.to_string(),
        ]);
    } else {
        args.push("is-system-running".to_string());
    }
    run_fixed_command("systemctl", args, "linux.systemd_status")
}

fn run_man(topic: &str) -> LinuxToolResult {
    let mut result = run_fixed_command(
        "man",
        vec![
            "-P".to_string(),
            "cat".to_string(),
            "--".to_string(),
            topic.to_string(),
        ],
        "linux.man",
    );
    // Man exits non-zero for an unavailable page; keep that fact in JSON but
    // make the user-facing error explicit and bounded.
    if result.status == "failed" && result.stderr.is_empty() {
        result.stderr = format!("man page not available: {topic}");
    }
    result
}

fn run_processes(limit: usize) -> LinuxToolResult {
    let mut result = run_fixed_command(
        "ps",
        vec![
            "-eo".to_string(),
            "pid=,ppid=,stat=,comm=,args=".to_string(),
            "--sort=-%cpu".to_string(),
        ],
        "linux.processes",
    );
    if result.available {
        result.stdout = result
            .stdout
            .lines()
            .take(limit)
            .collect::<Vec<_>>()
            .join("\n");
        if !result.stdout.is_empty() {
            result.stdout.push('\n');
        }
    }
    result
}

fn run_network() -> LinuxToolResult {
    let first = run_fixed_command(
        "ip",
        vec!["-brief".to_string(), "address".to_string()],
        "linux.network",
    );
    if first.available {
        return first;
    }
    run_fixed_command("ss", vec!["-tuln".to_string()], "linux.network")
}

fn run_pacman(mode: &'static str, value: &str) -> LinuxToolResult {
    run_fixed_command(
        "pacman",
        vec![mode.to_string(), "--".to_string(), value.to_string()],
        "linux.pacman",
    )
}

fn run_fixed_command(program: &str, args: Vec<String>, tool: &'static str) -> LinuxToolResult {
    let mut command = vec![program.to_string()];
    command.extend(args.iter().cloned());
    let output = Command::new(program)
        .args(&args)
        .stdin(Stdio::null())
        .output();
    match output {
        Ok(output) => result_from_output(tool, command, output),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => LinuxToolResult {
            tool,
            status: "unavailable",
            risk_class: "read_only",
            requires_root: false,
            mutates_system: false,
            available: false,
            exit_code: None,
            command,
            stdout: String::new(),
            stderr: format!("required Linux utility is unavailable: {program}"),
        },
        Err(error) => LinuxToolResult {
            tool,
            status: "failed",
            risk_class: "read_only",
            requires_root: false,
            mutates_system: false,
            available: true,
            exit_code: None,
            command,
            stdout: String::new(),
            stderr: format!("failed to start {program}: {error}"),
        },
    }
}

fn result_from_output(tool: &'static str, command: Vec<String>, output: Output) -> LinuxToolResult {
    let exit_code = output.status.code();
    LinuxToolResult {
        tool,
        status: if output.status.success() {
            "ok"
        } else {
            "failed"
        },
        risk_class: "read_only",
        requires_root: false,
        mutates_system: false,
        available: true,
        exit_code,
        command,
        stdout: bounded_text(&output.stdout),
        stderr: bounded_text(&output.stderr),
    }
}

fn bounded_text(bytes: &[u8]) -> String {
    let bytes = if bytes.len() > MAX_OUTPUT_BYTES {
        &bytes[..MAX_OUTPUT_BYTES]
    } else {
        bytes
    };
    String::from_utf8_lossy(bytes).into_owned()
}

fn validate_token(value: &str, label: &str) -> Result<()> {
    if value.is_empty()
        || !value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '@' | '_' | ':' | '-')
        })
    {
        bail!("{label} contains unsupported characters")
    }
    Ok(())
}

fn validate_pacman_token(value: &str, label: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 128
        || !value.chars().all(|character| {
            character.is_ascii_alphanumeric()
                || matches!(
                    character,
                    '.' | '@' | '_' | ':' | '+' | '-' | '/' | '*' | '?'
                )
        })
    {
        bail!("{label} contains unsupported characters")
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_is_read_only_and_non_root() {
        let specs = specs();
        assert_eq!(specs.len(), 5);
        assert!(specs.iter().all(|spec| {
            spec.risk_class == "read_only" && !spec.requires_root && !spec.mutates_system
        }));
    }

    #[test]
    fn tokens_reject_shell_syntax_and_whitespace() {
        assert!(validate_token("yunxi-linux.service", "unit").is_ok());
        assert!(validate_token("systemctl; rm -rf /", "unit").is_err());
        assert!(validate_token("systemctl --user", "topic").is_err());
    }

    #[test]
    fn pacman_queries_are_fixed_and_reject_shell_syntax() {
        assert!(validate_pacman_token("yunxi-agent", "package").is_ok());
        assert!(validate_pacman_token("linux-firmware*", "search").is_ok());
        assert!(validate_pacman_token("foo; touch /tmp/yunxi", "search").is_err());
        assert!(validate_pacman_token("pacman --info", "package").is_err());

        let result = run_pacman("--info", "yunxi-agent");
        assert_eq!(result.tool, "linux.pacman");
        assert_eq!(result.command, ["pacman", "--info", "--", "yunxi-agent"]);
        assert_eq!(result.risk_class, "read_only");
        assert!(!result.requires_root);
        assert!(!result.mutates_system);
    }

    #[test]
    fn output_is_bounded() {
        let output = bounded_text(&vec![b'a'; MAX_OUTPUT_BYTES + 10]);
        assert_eq!(output.len(), MAX_OUTPUT_BYTES);
    }
}
