//! Pure Linux operation planning.
//!
//! This module deliberately stops before execution.  It validates a fixed
//! argv allowlist or a typed mutation intent, classifies the operation, and
//! asks the existing sandbox policy what approval/escalation would be needed.
//! It never constructs an `ExecCommand`, spawns a process, reads command
//! output, or mutates the filesystem.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::{Path, PathBuf};
use yunxi_agent_sandbox::{CommandRisk, ExecutionPolicy, PolicyDecision, SandboxRequirement};

const MAX_ARGV_ITEMS: usize = 32;
const MAX_ARG_BYTES: usize = 1024;
const MAX_TOTAL_ARG_BYTES: usize = 4096;
const MAX_INTENT_TEXT_BYTES: usize = 4096;
const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum LinuxPreviewInput {
    /// A fixed, read-only argv shape.  This is display data, never a shell
    /// command and never executed by the planner.
    Argv { argv: Vec<String> },
    /// A typed mutation request.  Arbitrary shell strings are not accepted.
    Mutation { intent: LinuxMutationIntent },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LinuxMutationIntent {
    WriteFile { path: String, bytes: u64 },
    DeletePath { path: String },
    MovePath { from: String, to: String },
    SystemdAction { action: String, unit: String },
    PackageAction { action: String, package: String },
    NetworkAction { action: String, target: String },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinuxPreviewMode {
    Argv,
    Mutation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinuxPreviewStatus {
    Planned,
    ApprovalRequired,
    EscalationRequired,
    Rejected,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinuxPreviewTargetKind {
    Read,
    Write,
    Delete,
    Move,
    Process,
    Package,
    Network,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LinuxPreviewTarget {
    pub value: String,
    pub kind: LinuxPreviewTargetKind,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LinuxPreviewSideEffects {
    pub spawned: bool,
    pub files_changed: bool,
    pub system_modified: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LinuxPreviewPlan {
    pub schema_version: u32,
    pub status: LinuxPreviewStatus,
    pub mode: LinuxPreviewMode,
    pub normalized_argv: Vec<String>,
    /// Human-readable display text only.  It must never be passed to a shell.
    pub command: String,
    pub cwd: PathBuf,
    pub risk: CommandRisk,
    pub policy_decision: String,
    pub approval_required: bool,
    pub escalation_required: bool,
    pub requested_sandbox: SandboxRequirement,
    pub targets: Vec<LinuxPreviewTarget>,
    pub side_effects: LinuxPreviewSideEffects,
    pub runner: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LinuxPreviewError {
    message: String,
}

impl LinuxPreviewError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for LinuxPreviewError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for LinuxPreviewError {}

pub type LinuxPreviewResult<T> = Result<T, LinuxPreviewError>;

/// Build a plan without performing any external or filesystem operation.
pub fn plan(
    cwd: &Path,
    input: LinuxPreviewInput,
    policy: &ExecutionPolicy,
) -> LinuxPreviewResult<LinuxPreviewPlan> {
    let (mode, argv, command, risk, targets) = match input {
        LinuxPreviewInput::Argv { argv } => {
            validate_argv(&argv)?;
            let command = canonical_command(&argv);
            validate_read_only_argv(&argv)?;
            (
                LinuxPreviewMode::Argv,
                argv,
                command,
                CommandRisk::Low,
                Vec::new(),
            )
        }
        LinuxPreviewInput::Mutation { intent } => mutation_plan(intent)?,
    };

    let evaluation = policy.evaluate_with_risk(cwd, Some(&command), risk);
    let target_escape = !matches!(policy.sandbox, SandboxRequirement::DangerFullAccess)
        && targets.iter().any(|target| {
            matches!(
                target.kind,
                LinuxPreviewTargetKind::Write
                    | LinuxPreviewTargetKind::Delete
                    | LinuxPreviewTargetKind::Move
            ) && target_outside_workspace(cwd, &policy.workspace_root, &target.value)
        });
    let status = if target_escape {
        LinuxPreviewStatus::EscalationRequired
    } else if matches!(evaluation.decision, PolicyDecision::Allowed) {
        LinuxPreviewStatus::Planned
    } else if evaluation.approval_request.is_some() {
        LinuxPreviewStatus::ApprovalRequired
    } else if evaluation.escalation_request.is_some() {
        LinuxPreviewStatus::EscalationRequired
    } else {
        LinuxPreviewStatus::Rejected
    };

    Ok(LinuxPreviewPlan {
        schema_version: 1,
        status,
        mode,
        normalized_argv: argv,
        command,
        cwd: cwd.to_path_buf(),
        risk,
        policy_decision: match evaluation.decision {
            PolicyDecision::Allowed if !target_escape => "allowed".to_string(),
            PolicyDecision::Blocked { .. } => "blocked".to_string(),
            PolicyDecision::Allowed => "blocked".to_string(),
        },
        approval_required: evaluation.approval_request.is_some(),
        escalation_required: evaluation.escalation_request.is_some() || target_escape,
        requested_sandbox: policy.sandbox,
        targets,
        side_effects: LinuxPreviewSideEffects {
            spawned: false,
            files_changed: false,
            system_modified: false,
        },
        runner: "none",
    })
}

fn mutation_plan(
    intent: LinuxMutationIntent,
) -> LinuxPreviewResult<(
    LinuxPreviewMode,
    Vec<String>,
    String,
    CommandRisk,
    Vec<LinuxPreviewTarget>,
)> {
    let (command, risk, targets) = match intent {
        LinuxMutationIntent::WriteFile { path, bytes } => {
            validate_text(&path, "write path")?;
            if bytes > MAX_FILE_BYTES {
                return Err(LinuxPreviewError::new(
                    "write size exceeds 64 MiB preview limit",
                ));
            }
            (
                format!("write_file {} ({bytes} bytes)", display_token(&path)),
                CommandRisk::WritesWorkspace,
                vec![LinuxPreviewTarget {
                    value: path,
                    kind: LinuxPreviewTargetKind::Write,
                }],
            )
        }
        LinuxMutationIntent::DeletePath { path } => {
            validate_text(&path, "delete path")?;
            (
                format!("delete_path {}", display_token(&path)),
                CommandRisk::Destructive,
                vec![LinuxPreviewTarget {
                    value: path,
                    kind: LinuxPreviewTargetKind::Delete,
                }],
            )
        }
        LinuxMutationIntent::MovePath { from, to } => {
            validate_text(&from, "move source")?;
            validate_text(&to, "move destination")?;
            (
                format!(
                    "move_path {} -> {}",
                    display_token(&from),
                    display_token(&to)
                ),
                CommandRisk::WritesWorkspace,
                vec![
                    LinuxPreviewTarget {
                        value: from,
                        kind: LinuxPreviewTargetKind::Move,
                    },
                    LinuxPreviewTarget {
                        value: to,
                        kind: LinuxPreviewTargetKind::Move,
                    },
                ],
            )
        }
        LinuxMutationIntent::SystemdAction { action, unit } => {
            validate_token(&action, "systemd action")?;
            validate_token(&unit, "systemd unit")?;
            if !matches!(
                action.as_str(),
                "start" | "stop" | "restart" | "reload" | "enable" | "disable"
            ) {
                return Err(LinuxPreviewError::new(
                    "systemd action must be start, stop, restart, reload, enable, or disable",
                ));
            }
            (
                format!("systemd {} {}", action, unit),
                CommandRisk::ProcessControl,
                vec![LinuxPreviewTarget {
                    value: unit,
                    kind: LinuxPreviewTargetKind::Process,
                }],
            )
        }
        LinuxMutationIntent::PackageAction { action, package } => {
            validate_token(&action, "package action")?;
            validate_token(&package, "package name")?;
            if !matches!(
                action.as_str(),
                "install" | "remove" | "upgrade" | "refresh"
            ) {
                return Err(LinuxPreviewError::new(
                    "package action must be install, remove, upgrade, or refresh",
                ));
            }
            let risk = if action == "remove" {
                CommandRisk::Destructive
            } else {
                CommandRisk::Network
            };
            (
                format!("package {} {}", action, package),
                risk,
                vec![LinuxPreviewTarget {
                    value: package,
                    kind: LinuxPreviewTargetKind::Package,
                }],
            )
        }
        LinuxMutationIntent::NetworkAction { action, target } => {
            validate_token(&action, "network action")?;
            validate_text(&target, "network target")?;
            if !matches!(action.as_str(), "configure" | "route" | "firewall") {
                return Err(LinuxPreviewError::new(
                    "network action must be configure, route, or firewall",
                ));
            }
            (
                format!("network {} {}", action, display_token(&target)),
                CommandRisk::Network,
                vec![LinuxPreviewTarget {
                    value: target,
                    kind: LinuxPreviewTargetKind::Network,
                }],
            )
        }
    };
    Ok((
        LinuxPreviewMode::Mutation,
        Vec::new(),
        command,
        risk,
        targets,
    ))
}

fn validate_argv(argv: &[String]) -> LinuxPreviewResult<()> {
    if argv.is_empty() {
        return Err(LinuxPreviewError::new("preview argv cannot be empty"));
    }
    if argv.len() > MAX_ARGV_ITEMS {
        return Err(LinuxPreviewError::new(
            "preview argv has too many arguments",
        ));
    }
    let total = argv.iter().try_fold(0usize, |total, arg| {
        if arg.is_empty() || arg.len() > MAX_ARG_BYTES {
            return Err(LinuxPreviewError::new(
                "preview argv items must be non-empty and <= 1024 bytes",
            ));
        }
        if arg.chars().any(char::is_control) {
            return Err(LinuxPreviewError::new(
                "preview argv cannot contain control characters",
            ));
        }
        total
            .checked_add(arg.len())
            .and_then(|value| value.checked_add(1))
            .ok_or_else(|| LinuxPreviewError::new("preview argv size overflow"))
    })?;
    if total > MAX_TOTAL_ARG_BYTES {
        return Err(LinuxPreviewError::new(
            "preview argv exceeds 4096 bytes in total",
        ));
    }
    Ok(())
}

fn validate_read_only_argv(argv: &[String]) -> LinuxPreviewResult<()> {
    let executable = argv[0].rsplit('/').next().unwrap_or(&argv[0]);
    if !matches!(
        executable,
        "systemctl" | "man" | "ps" | "ip" | "ss" | "find" | "df" | "pacman"
    ) {
        return Err(LinuxPreviewError::new(
            "preview argv executable is outside the Linux read-only allowlist",
        ));
    }
    for argument in argv {
        if argument.contains(['|', '&', ';', '>', '<', '$', '`', '(', ')'])
            || matches!(
                argument.as_str(),
                "-c" | "--command" | "-exec" | "-execdir" | "-delete"
            )
        {
            return Err(LinuxPreviewError::new(
                "preview argv contains shell syntax or a mutating flag",
            ));
        }
    }
    let lower = argv
        .iter()
        .map(|arg| arg.to_ascii_lowercase())
        .collect::<Vec<_>>();
    if lower.iter().any(|argument| {
        matches!(
            argument.as_str(),
            "start"
                | "stop"
                | "restart"
                | "reload"
                | "enable"
                | "disable"
                | "link"
                | "set"
                | "flush"
        )
    }) {
        return Err(LinuxPreviewError::new(
            "preview argv contains a mutating Linux operation",
        ));
    }
    if executable == "pacman"
        && !lower
            .iter()
            .any(|argument| matches!(argument.as_str(), "--info" | "--search" | "-qi" | "-ss"))
    {
        return Err(LinuxPreviewError::new(
            "preview pacman argv must use --info, --search, -Qi, or -Ss",
        ));
    }
    Ok(())
}

fn validate_text(value: &str, label: &str) -> LinuxPreviewResult<()> {
    if value.is_empty()
        || value.len() > MAX_INTENT_TEXT_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(LinuxPreviewError::new(format!(
            "{label} must be non-empty, <= 4096 bytes, and contain no control characters"
        )));
    }
    Ok(())
}

fn validate_token(value: &str, label: &str) -> LinuxPreviewResult<()> {
    validate_text(value, label)?;
    if !value
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || ".@_:+-".contains(character))
    {
        return Err(LinuxPreviewError::new(format!(
            "{label} contains unsupported characters"
        )));
    }
    Ok(())
}

fn canonical_command(argv: &[String]) -> String {
    argv.iter()
        .map(|argument| display_token(argument))
        .collect::<Vec<_>>()
        .join(" ")
}

fn display_token(value: &str) -> String {
    if value
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || ".@_:+-=/%,".contains(character))
    {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

fn target_outside_workspace(cwd: &Path, workspace_root: &Path, target: &str) -> bool {
    let target = lexical_normalize(if Path::new(target).is_absolute() {
        PathBuf::from(target)
    } else {
        cwd.join(target)
    });
    !target.starts_with(lexical_normalize(workspace_root.to_path_buf()))
}

fn lexical_normalize(path: PathBuf) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn read_only_policy(root: &Path) -> ExecutionPolicy {
        ExecutionPolicy {
            approval: yunxi_agent_sandbox::ApprovalRequirement::PreApproved,
            sandbox: SandboxRequirement::ReadOnly,
            network: yunxi_agent_sandbox::NetworkPolicy::Disabled,
            workspace_root: root.to_path_buf(),
        }
    }

    #[test]
    fn argv_preview_is_pure_and_read_only() {
        let root = tempdir().expect("tempdir");
        let marker = root.path().join("marker");
        let plan = plan(
            root.path(),
            LinuxPreviewInput::Argv {
                argv: vec![
                    "df".into(),
                    "-P".into(),
                    "-k".into(),
                    "--".into(),
                    marker.display().to_string(),
                ],
            },
            &read_only_policy(root.path()),
        )
        .expect("preview");
        assert_eq!(plan.status, LinuxPreviewStatus::Planned);
        assert_eq!(plan.risk, CommandRisk::Low);
        assert_eq!(plan.runner, "none");
        assert!(!plan.side_effects.spawned);
        assert!(!plan.side_effects.files_changed);
        assert!(!marker.exists());
    }

    #[test]
    fn typed_write_preview_requires_escalation_in_read_only_policy() {
        let root = tempdir().expect("tempdir");
        let plan = plan(
            root.path(),
            LinuxPreviewInput::Mutation {
                intent: LinuxMutationIntent::WriteFile {
                    path: "notes.txt".into(),
                    bytes: 42,
                },
            },
            &read_only_policy(root.path()),
        )
        .expect("preview");
        assert_eq!(plan.status, LinuxPreviewStatus::EscalationRequired);
        assert!(plan.escalation_required);
        assert_eq!(plan.risk, CommandRisk::WritesWorkspace);
        assert_eq!(plan.targets[0].kind, LinuxPreviewTargetKind::Write);
    }

    #[test]
    fn rejects_shell_syntax_and_mutating_argv() {
        let root = tempdir().expect("tempdir");
        let policy = read_only_policy(root.path());
        for argv in [
            vec!["sh".into(), "-c".into(), "touch marker".into()],
            vec!["find".into(), ".".into(), "-delete".into()],
            vec!["systemctl".into(), "restart".into(), "yunxi.service".into()],
            vec!["df".into(), ">".into(), "marker".into()],
        ] {
            assert!(plan(root.path(), LinuxPreviewInput::Argv { argv }, &policy).is_err());
        }
    }

    #[test]
    fn package_and_network_preview_preserve_distinct_risks() {
        let root = tempdir().expect("tempdir");
        let policy = read_only_policy(root.path());
        let package = plan(
            root.path(),
            LinuxPreviewInput::Mutation {
                intent: LinuxMutationIntent::PackageAction {
                    action: "install".into(),
                    package: "ripgrep".into(),
                },
            },
            &policy,
        )
        .expect("package preview");
        assert_eq!(package.risk, CommandRisk::Network);
        assert_eq!(package.status, LinuxPreviewStatus::EscalationRequired);

        let network = plan(
            root.path(),
            LinuxPreviewInput::Mutation {
                intent: LinuxMutationIntent::NetworkAction {
                    action: "route".into(),
                    target: "default".into(),
                },
            },
            &policy,
        )
        .expect("network preview");
        assert_eq!(network.risk, CommandRisk::Network);
        assert_eq!(network.status, LinuxPreviewStatus::EscalationRequired);
    }

    #[test]
    fn workspace_write_preview_marks_absolute_escape_without_touching_disk() {
        let root = tempdir().expect("tempdir");
        let policy = ExecutionPolicy {
            approval: yunxi_agent_sandbox::ApprovalRequirement::PreApproved,
            sandbox: SandboxRequirement::WorkspaceWrite,
            network: yunxi_agent_sandbox::NetworkPolicy::Disabled,
            workspace_root: root.path().to_path_buf(),
        };
        let outside = root
            .path()
            .parent()
            .expect("parent")
            .join("yunxi-preview-outside.txt");
        let plan = plan(
            root.path(),
            LinuxPreviewInput::Mutation {
                intent: LinuxMutationIntent::WriteFile {
                    path: outside.display().to_string(),
                    bytes: 1,
                },
            },
            &policy,
        )
        .expect("preview");
        assert_eq!(plan.status, LinuxPreviewStatus::EscalationRequired);
        assert!(plan.escalation_required);
        assert_eq!(plan.policy_decision, "blocked");
        assert!(!outside.exists());
    }
}
