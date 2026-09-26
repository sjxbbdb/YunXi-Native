//! Read-only P0 knowledge collection for the Linux-native host.
//!
//! The collector deliberately owns only the source boundary: it validates a
//! man topic, runs a fixed argv through the existing execution policy, bounds
//! the output, and returns provenance. Persistence remains the storage layer's
//! responsibility, so collected text can never become an executable command.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;
use yunxi_agent_core::{AgentCancellationToken, AgentResult};
use yunxi_agent_exec::{ExecCommand, ExecLifecycleEvent, ExecManager};
use yunxi_agent_sandbox::{
    ApprovalRequirement, ExecutionPolicy, NetworkPolicy, SandboxRequirement,
};
use yunxi_agent_storage::{
    KnowledgeChunkingOptions, KnowledgeDocument, KnowledgeIngestSummary, SqliteKnowledgeStore,
};

pub const MAX_OUTPUT_BYTES: usize = 64 * 1024;
const MAX_TOKEN_CHARS: usize = 128;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManPageRequest {
    pub topic: String,
    pub section: Option<String>,
    pub source_version: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandHelpRequest {
    pub command: String,
    pub source_version: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CollectionStatus {
    Ok,
    Unavailable,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CollectedKnowledge {
    pub document: KnowledgeDocument,
    pub text: String,
    pub argv: Vec<String>,
    pub exit_code: Option<i32>,
    pub status: CollectionStatus,
    pub stderr: String,
    pub truncated: bool,
}

pub async fn collect_man_page(
    request: &ManPageRequest,
    cwd: &Path,
    cancellation: AgentCancellationToken,
) -> AgentResult<CollectedKnowledge> {
    validate_request(request)?;
    collect_fixed_command(
        document_for(request),
        man_argv(request),
        cwd,
        fixed_environment(),
        cancellation,
    )
    .await
}

pub async fn collect_help_command(
    request: &CommandHelpRequest,
    cwd: &Path,
    cancellation: AgentCancellationToken,
) -> AgentResult<CollectedKnowledge> {
    validate_help_request(request)?;
    collect_fixed_command(
        help_document_for(request),
        help_argv(request),
        cwd,
        fixed_environment(),
        cancellation,
    )
    .await
}

pub fn ingest_collected_knowledge(
    store: &SqliteKnowledgeStore,
    collected: &CollectedKnowledge,
    options: &KnowledgeChunkingOptions,
) -> AgentResult<KnowledgeIngestSummary> {
    if collected.status != CollectionStatus::Ok
        || collected.exit_code != Some(0)
        || collected.truncated
    {
        return Err(yunxi_agent_core::AgentError::Execution {
            message: "cannot ingest an unsuccessful or truncated knowledge collection".to_string(),
        });
    }
    if collected.text.trim().is_empty() {
        return Err(yunxi_agent_core::AgentError::Execution {
            message: "cannot ingest an empty man-page collection".to_string(),
        });
    }
    store.ingest_text(&collected.document, &collected.text, options)
}

pub fn ingest_collected_man_page(
    store: &SqliteKnowledgeStore,
    collected: &CollectedKnowledge,
    options: &KnowledgeChunkingOptions,
) -> AgentResult<KnowledgeIngestSummary> {
    ingest_collected_knowledge(store, collected, options)
}

pub fn ensure_system_space(store: &SqliteKnowledgeStore, source_version: &str) -> AgentResult<()> {
    store.upsert_space(&yunxi_agent_storage::KnowledgeSpaceSpec {
        space_id: "system-linux".to_string(),
        kind: yunxi_agent_storage::KnowledgeSpaceKind::System,
        owner: "system".to_string(),
        visibility: yunxi_agent_storage::KnowledgeVisibility::Public,
        source: "local-linux".to_string(),
        version: source_version.to_string(),
        generation: 1,
    })
}

pub fn document_for(request: &ManPageRequest) -> KnowledgeDocument {
    let section = request.section.as_deref().unwrap_or("default");
    KnowledgeDocument {
        document_id: format!("system-man:{section}:{}", request.topic),
        space_id: "system-linux".to_string(),
        title: match request.section.as_deref() {
            Some(section) => format!("man {section} {}", request.topic),
            None => format!("man {}", request.topic),
        },
        source: "local-linux".to_string(),
        version: request.source_version.clone(),
        generation: 1,
        owner: "system".to_string(),
        visibility: yunxi_agent_storage::KnowledgeVisibility::Public,
        metadata_json: serde_json::json!({
            "source_type": "man",
            "topic": request.topic,
            "section": request.section,
        })
        .to_string(),
    }
}

fn help_document_for(request: &CommandHelpRequest) -> KnowledgeDocument {
    KnowledgeDocument {
        document_id: format!("system-help:{}", request.command),
        space_id: "system-linux".to_string(),
        title: format!("{} --help", request.command),
        source: "local-linux".to_string(),
        version: request.source_version.clone(),
        generation: 1,
        owner: "system".to_string(),
        visibility: yunxi_agent_storage::KnowledgeVisibility::Public,
        metadata_json: serde_json::json!({
            "source_type": "command_help",
            "command": request.command,
        })
        .to_string(),
    }
}

pub fn man_argv(request: &ManPageRequest) -> Vec<String> {
    let mut argv = vec![
        "man".to_string(),
        "--locale=C".to_string(),
        "-P".to_string(),
        "cat".to_string(),
    ];
    if let Some(section) = &request.section {
        argv.push(section.clone());
    }
    argv.push(request.topic.clone());
    argv
}

pub fn help_argv(request: &CommandHelpRequest) -> Vec<String> {
    vec![request.command.clone(), "--help".to_string()]
}

fn validate_request(request: &ManPageRequest) -> AgentResult<()> {
    validate_token(&request.topic, "man topic")?;
    if let Some(section) = &request.section {
        validate_token(section, "man section")?;
    }
    if request.source_version.trim().is_empty()
        || request.source_version.chars().count() > MAX_TOKEN_CHARS
        || request.source_version.contains(['\r', '\n'])
    {
        return Err(yunxi_agent_core::AgentError::Execution {
            message: "man source version is invalid".to_string(),
        });
    }
    Ok(())
}

fn validate_help_request(request: &CommandHelpRequest) -> AgentResult<()> {
    const ALLOWED_COMMANDS: &[&str] = &["fish", "git", "systemctl", "pacman", "ip"];
    if !ALLOWED_COMMANDS.contains(&request.command.as_str()) {
        return Err(yunxi_agent_core::AgentError::Execution {
            message: format!("command help is not allowlisted: {}", request.command),
        });
    }
    if request.source_version.trim().is_empty()
        || request.source_version.chars().count() > MAX_TOKEN_CHARS
        || request.source_version.contains(['\r', '\n'])
    {
        return Err(yunxi_agent_core::AgentError::Execution {
            message: "command help source version is invalid".to_string(),
        });
    }
    Ok(())
}

fn validate_token(value: &str, label: &str) -> AgentResult<()> {
    if value.is_empty()
        || value.chars().count() > MAX_TOKEN_CHARS
        || !value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || ".@_:+-".contains(character))
    {
        return Err(yunxi_agent_core::AgentError::Execution {
            message: format!("{label} contains unsupported characters"),
        });
    }
    Ok(())
}

fn fixed_environment() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("MANPAGER".to_string(), "cat".to_string()),
        ("PAGER".to_string(), "cat".to_string()),
        ("TERM".to_string(), "dumb".to_string()),
    ])
}

fn canonical_command(argv: &[String]) -> String {
    argv.join(" ")
}

async fn collect_fixed_command(
    document: KnowledgeDocument,
    argv: Vec<String>,
    cwd: &Path,
    env: BTreeMap<String, String>,
    cancellation: AgentCancellationToken,
) -> AgentResult<CollectedKnowledge> {
    let command = canonical_command(&argv);
    let policy = ExecutionPolicy {
        approval: ApprovalRequirement::PreApproved,
        sandbox: SandboxRequirement::ReadOnly,
        network: NetworkPolicy::Disabled,
        workspace_root: cwd.to_path_buf(),
    };
    let trace = match ExecManager::default()
        .run_with_cancellation(
            ExecCommand {
                id: None,
                cwd: cwd.to_path_buf(),
                command,
                argv: argv.clone(),
                stdin: None,
                env,
                timeout_millis: Some(10_000),
                policy,
            },
            cancellation,
        )
        .await
    {
        Ok(trace) => trace,
        Err(error) => {
            return Ok(CollectedKnowledge {
                document,
                text: String::new(),
                argv,
                exit_code: None,
                status: CollectionStatus::Unavailable,
                stderr: error.to_string(),
                truncated: false,
            });
        }
    };
    let (stdout, stderr) = trace
        .events
        .iter()
        .rev()
        .find_map(|event| match event {
            ExecLifecycleEvent::Completed { output, .. } => {
                Some((output.stdout.clone(), output.stderr.clone()))
            }
            _ => None,
        })
        .unwrap_or_else(|| (trace.summary.aggregated_output.clone(), String::new()));
    let stdout = bounded_text(&stdout);
    let stderr = bounded_text(&stderr);
    let truncated = stdout.1 || stderr.1;
    let status = collection_status(trace.summary.exit_code, trace.summary.timed_out, truncated);
    Ok(CollectedKnowledge {
        document,
        text: stdout.0,
        argv,
        exit_code: trace.summary.exit_code,
        status,
        stderr: stderr.0,
        truncated,
    })
}

fn bounded_text(value: &str) -> (String, bool) {
    if value.len() <= MAX_OUTPUT_BYTES {
        return (value.to_string(), false);
    }
    let mut end = MAX_OUTPUT_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    (value[..end].to_string(), true)
}

fn collection_status(exit_code: Option<i32>, timed_out: bool, truncated: bool) -> CollectionStatus {
    if exit_code == Some(0) && !timed_out && !truncated {
        CollectionStatus::Ok
    } else {
        CollectionStatus::Failed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> ManPageRequest {
        ManPageRequest {
            topic: "fish".to_string(),
            section: Some("1".to_string()),
            source_version: "ubuntu-24.04".to_string(),
        }
    }

    #[test]
    fn man_argv_is_fixed_and_deterministic() {
        assert_eq!(
            man_argv(&request()),
            vec!["man", "--locale=C", "-P", "cat", "1", "fish"]
        );
        assert!(
            !man_argv(&request())
                .iter()
                .any(|arg| { matches!(arg.as_str(), "sh" | "bash" | "-c" | "-Command") })
        );
    }

    #[test]
    fn request_rejects_shell_syntax_paths_and_newlines() {
        let mut invalid = request();
        invalid.topic = "fish;id".to_string();
        assert!(validate_request(&invalid).is_err());
        invalid = request();
        invalid.section = Some("../1".to_string());
        assert!(validate_request(&invalid).is_err());
        invalid = request();
        invalid.source_version = "ubuntu\n24".to_string();
        assert!(validate_request(&invalid).is_err());
    }

    #[test]
    fn document_identity_and_provenance_are_stable() {
        let document = document_for(&request());
        assert_eq!(document.document_id, "system-man:1:fish");
        assert_eq!(document.source, "local-linux");
        assert_eq!(document.owner, "system");
        assert_eq!(
            document.visibility,
            yunxi_agent_storage::KnowledgeVisibility::Public
        );
        assert!(!document.metadata_json.contains("/"));
    }

    #[test]
    fn help_collector_is_allowlisted_and_uses_fixed_help_argv() {
        let request = CommandHelpRequest {
            command: "systemctl".to_string(),
            source_version: "ubuntu-24.04".to_string(),
        };
        assert_eq!(help_argv(&request), vec!["systemctl", "--help"]);
        assert_eq!(
            help_document_for(&request).document_id,
            "system-help:systemctl"
        );
        let invalid = CommandHelpRequest {
            command: "./script".to_string(),
            source_version: "ubuntu-24.04".to_string(),
        };
        assert!(validate_help_request(&invalid).is_err());
    }

    #[test]
    fn output_is_bounded_at_utf8_boundary() {
        let input = "鱼".repeat(MAX_OUTPUT_BYTES);
        let (output, truncated) = bounded_text(&input);
        assert!(truncated);
        assert!(output.len() <= MAX_OUTPUT_BYTES);
        assert!(output.is_char_boundary(output.len()));
    }

    #[test]
    fn truncated_success_is_not_ingestable() {
        assert_eq!(
            collection_status(Some(0), false, true),
            CollectionStatus::Failed
        );
        assert_eq!(
            collection_status(Some(0), true, false),
            CollectionStatus::Failed
        );
        assert_eq!(
            collection_status(Some(0), false, false),
            CollectionStatus::Ok
        );
    }
}
