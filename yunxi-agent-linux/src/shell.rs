//! Miyu-inspired fish integration for the YunXi Linux native host.
//!
//! The hook keeps fish's line editor in place, but YunXi owns every non-empty
//! submitted interactive buffer.  Fish never evaluates a submitted command
//! before YunXi receives it; command execution, if appropriate, happens later
//! through Runtime approval and sandbox policy.
//!
//! The fish hook lifecycle and PTY-facing shape are informed by Miyu Agent's
//! `crates/miyu-base/src/shell/fish.rs` (MIT License), while routing is fully
//! owned by YunXi Runtime.

use anyhow::{Context, Result, bail};
use clap::{ArgAction, Subcommand};
#[cfg(unix)]
use serde::{Deserialize, Serialize};
#[cfg(unix)]
use std::collections::{HashMap, VecDeque};
#[cfg(unix)]
use std::ffi::CStr;
use std::fs;
#[cfg(unix)]
use std::io::Write;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::sync::Arc;
#[cfg(unix)]
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;
#[cfg(unix)]
use std::time::{Duration, SystemTime, UNIX_EPOCH};
#[cfg(unix)]
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
#[cfg(unix)]
use tokio::net::{UnixListener, UnixStream};
#[cfg(unix)]
use tokio::signal::unix::{Signal, SignalKind, signal};
#[cfg(unix)]
use tokio::sync::{Mutex, broadcast, mpsc};
#[cfg(unix)]
use tokio::time::sleep;
#[cfg(unix)]
use yunxi_agent_core::{
    Agent, AgentConfig, AgentEvent, AgentInput, AgentRunApprovalDecision, AgentRunControl,
    AgentRunStatus, AgentRunUserInputResponse,
};
use yunxi_agent_persona::MemoryEmbeddingProvider;
#[cfg(unix)]
use yunxi_agent_protocol::linux_ipc::{
    LINUX_IPC_MAX_FRAME_BYTES, LINUX_IPC_PROTOCOL_VERSION, decode_json_frame, encode_json_frame,
    validate_frame_length,
};
#[cfg(unix)]
use yunxi_agent_runtime::YunXiRuntimeBackend;

#[path = "knowledge_collector.rs"]
mod knowledge_collector;
#[path = "knowledge_worker.rs"]
mod knowledge_worker;
#[path = "linux_tools.rs"]
mod linux_tools;
use linux_tools::LinuxToolCommand;

const HOOK_MARKER: &str = "# YunXi Agent fish hook";
const MAX_KNOWLEDGE_CATALOG_COMMANDS: usize = 32;
const MAX_KNOWLEDGE_MAN_TOPICS: usize = 32;
const KNOWLEDGE_IMPORT_DIRECTORY_MAX_DEPTH: usize = 8;
const KNOWLEDGE_IMPORT_DIRECTORY_MAX_FILES: usize = 512;
const KNOWLEDGE_IMPORT_DIRECTORY_MAX_TOTAL_BYTES: u64 = 64 * 1024 * 1024;
const KNOWLEDGE_IMPORT_DIRECTORY_MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;

#[cfg(unix)]
const MAX_TURN_PROMPT_BYTES: usize = 64 * 1024;
#[cfg(unix)]
const MAX_TURN_REQUEST_ID_BYTES: usize = 512;
#[cfg(unix)]
const MAX_TURN_CWD_BYTES: usize = 4096;
#[cfg(unix)]
const MAX_TURN_SESSION_ID_BYTES: usize = 512;
#[cfg(unix)]
const MAX_TURN_PROVIDER_BYTES: usize = 256;
#[cfg(unix)]
const MAX_TURN_MODEL_BYTES: usize = 256;
#[cfg(unix)]
const DAEMON_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Subcommand)]
pub(crate) enum LinuxShellCommand {
    /// Install the Miyu-style fish Enter hook.
    FishInit {
        /// Print the hook instead of writing it.
        #[arg(long)]
        print: bool,
        /// Deprecated compatibility flag; fish takeover is always enabled.
        #[arg(long, hide = true)]
        takeover: bool,
    },
    /// Remove a hook previously installed by `fish-init`.
    RemoveShellHook,
    /// Return exit 0 for a real shell command and exit 1 for prose.
    ShellClassify {
        #[arg(long, default_value = "fish")]
        shell: String,
        #[arg(long)]
        stdin: bool,
    },
    /// Route one line of prose through the resident daemon.
    ShellIntercept {
        #[arg(long, default_value = "fish")]
        shell: String,
        #[arg(long)]
        stdin: bool,
        /// Stable identity of the interactive shell that submitted this turn.
        #[arg(long)]
        session_id: Option<String>,
        #[arg(long, default_value = ".")]
        cwd: PathBuf,
        #[arg(long)]
        offline: bool,
        #[arg(long)]
        live: bool,
        #[arg(long)]
        provider: Option<String>,
        #[arg(long)]
        model: Option<String>,
    },
    /// Query the durable lifecycle state of one daemon run.
    RunStatus {
        /// Run id returned by the daemon in `run_accepted`.
        run_id: String,
    },
    /// Follow a detached run and stream its bounded JSON event frames.
    RunFollow {
        /// Run id returned by the daemon in `run_accepted`.
        run_id: String,
        /// Resume after this already-consumed event sequence number.
        #[arg(long, default_value_t = 0)]
        after_seq: u64,
    },
    /// Request cancellation of an active detached run.
    RunCancel {
        /// Run id returned by the daemon in `run_accepted`.
        run_id: String,
    },
    /// Start a detached run and print only its `run_accepted` JSON frame.
    RunDetached {
        /// Prompt submitted to the daemon; it is not persisted by the CLI.
        prompt: String,
        /// Workspace path sent to the daemon after canonicalization.
        #[arg(long, default_value = ".")]
        cwd: PathBuf,
        /// Stable session identity to resume from in the daemon.
        #[arg(long)]
        session_id: Option<String>,
        #[arg(long)]
        offline: bool,
        #[arg(long)]
        live: bool,
        #[arg(long)]
        provider: Option<String>,
        #[arg(long)]
        model: Option<String>,
    },
    /// Print the user-level systemd unit used to host the daemon.
    SystemdUnit,
    /// Run a bounded read-only Linux host probe.
    LinuxTool {
        #[command(subcommand)]
        command: LinuxToolCommand,
    },
    /// Collect one local man page into the isolated system knowledge space.
    KnowledgeMan {
        /// Man topic such as `fish` or `systemctl`.
        topic: String,
        /// Optional man section, for example `1` or `8`.
        #[arg(long)]
        section: Option<String>,
        /// Distro/runtime version recorded as provenance; `auto` reads os-release.
        #[arg(long, default_value = "auto")]
        source_version: String,
        /// Workspace whose `.yunxi/knowledge/knowledge.sqlite3` receives the document.
        #[arg(long, default_value = ".")]
        cwd: PathBuf,
    },
    /// Collect help text from an explicitly allowlisted local command.
    KnowledgeHelp {
        /// Allowlisted command: bash, fish, git, systemctl, pacman, ip, or a small
        /// fixed set of read-only shell/coreutils help commands.
        command: String,
        /// Distro/runtime version recorded as provenance; `auto` reads os-release.
        #[arg(long, default_value = "auto")]
        source_version: String,
        /// Workspace whose `.yunxi/knowledge/knowledge.sqlite3` receives the document.
        #[arg(long, default_value = ".")]
        cwd: PathBuf,
    },
    /// Search the isolated system knowledge space without executing results.
    KnowledgeSearch {
        /// FTS query text.
        query: String,
        /// Workspace whose `.yunxi/knowledge/knowledge.sqlite3` is queried.
        #[arg(long, default_value = ".")]
        cwd: PathBuf,
        /// Maximum number of matches to return.
        #[arg(long, default_value_t = 10)]
        limit: usize,
        /// Restrict results to one recorded distro/runtime version.
        #[arg(long)]
        source_version: Option<String>,
        /// Explicit knowledge space; defaults to the Linux system space.
        #[arg(long, default_value = "system-linux")]
        space_id: String,
        /// Owner used for the space access filter.
        #[arg(long, default_value = "system")]
        owner: String,
        /// Visibility used for the space access filter.
        #[arg(long, default_value = "public")]
        visibility: String,
        /// Include opt-in latency and result-count diagnostics in the JSON output.
        #[arg(long)]
        diagnostics: bool,
    },
    /// Build local embeddings for one already ingested knowledge document.
    KnowledgeIndex {
        /// Stable document id returned by knowledge-man/knowledge-help.
        document_id: String,
        /// Workspace whose `.yunxi/knowledge/knowledge.sqlite3` is updated.
        #[arg(long, default_value = ".")]
        cwd: PathBuf,
    },
    /// Enqueue the current generation of one document for local embedding.
    KnowledgeEnqueue {
        /// Stable document id returned by knowledge-man/knowledge-help.
        document_id: String,
        /// Embedding model requested by the worker.
        #[arg(long, default_value = yunxi_agent_persona::LOCAL_MEMORY_EMBEDDING_MODEL)]
        embedding_model: String,
        /// Workspace whose `.yunxi/knowledge/knowledge.sqlite3` receives the job.
        #[arg(long, default_value = ".")]
        cwd: PathBuf,
    },
    /// Explicitly retry one failed embedding job within its bounded budget.
    KnowledgeRetry {
        /// SQLite job id returned by knowledge-enqueue/knowledge-worker.
        job_id: i64,
        /// Workspace whose `.yunxi/knowledge/knowledge.sqlite3` is updated.
        #[arg(long, default_value = ".")]
        cwd: PathBuf,
    },
    /// Read-only diagnostic snapshot of active and staging embedding queues.
    KnowledgeWorkerStatus {
        /// Workspace whose `.yunxi/knowledge/knowledge.sqlite3` is observed.
        #[arg(long, default_value = ".")]
        cwd: PathBuf,
        /// Explicit workspaces for a bounded, read-only fleet snapshot.
        /// No directory discovery or recursive scanning is performed.
        #[arg(long = "workspace", action = ArgAction::Append, conflicts_with = "cwd")]
        workspaces: Vec<PathBuf>,
    },
    /// Read the scheduler state written by a knowledge worker without opening
    /// any knowledge or memory database.
    KnowledgeWorkerHealth {
        /// Stable worker identity; defaults to the daemon-owned worker.
        #[arg(long)]
        worker_id: Option<String>,
    },
    /// Process one pending local knowledge embedding job and exit.
    KnowledgeWorker {
        /// Stable worker identity used for the SQLite lease.
        #[arg(long)]
        worker_id: Option<String>,
        /// Maximum number of pending jobs to process in each invocation/round.
        #[arg(long, default_value_t = 1)]
        max_jobs: usize,
        /// Keep polling the selected workspace until Ctrl+C/SIGTERM.
        #[arg(long)]
        watch: bool,
        /// Delay between watch polling rounds.
        #[arg(long, default_value_t = 5)]
        interval_secs: u64,
        /// Workspace whose `.yunxi/knowledge/knowledge.sqlite3` is processed.
        #[arg(long, default_value = ".")]
        cwd: PathBuf,
        /// Explicit workspaces, instead of --cwd, for a bounded round-robin worker.
        /// No directory discovery or recursive scanning is performed.
        #[arg(long = "workspace", action = ArgAction::Append, conflicts_with = "cwd")]
        workspaces: Vec<PathBuf>,
    },
    /// Search the isolated knowledge vector index with the local provider.
    KnowledgeVectorSearch {
        /// Query text embedded by the local character n-gram provider.
        query: String,
        /// Workspace whose `.yunxi/knowledge/knowledge.sqlite3` is queried.
        #[arg(long, default_value = ".")]
        cwd: PathBuf,
        /// Maximum number of matches to return.
        #[arg(long, default_value_t = 10)]
        limit: usize,
        /// Restrict results to one recorded distro/runtime version.
        #[arg(long)]
        source_version: Option<String>,
        /// Explicit knowledge space; defaults to the Linux system space.
        #[arg(long, default_value = "system-linux")]
        space_id: String,
        /// Owner used for the space access filter.
        #[arg(long, default_value = "system")]
        owner: String,
        /// Visibility used for the space access filter.
        #[arg(long, default_value = "public")]
        visibility: String,
        /// Include opt-in latency and result-count diagnostics in the JSON output.
        #[arg(long)]
        diagnostics: bool,
    },
    /// Create an explicitly owned project or private knowledge space.
    KnowledgeSpaceInit {
        /// Stable space id; use letters, digits, '.', '_' or '-'.
        #[arg(long)]
        space_id: String,
        /// Space kind: project or private.
        #[arg(long)]
        kind: String,
        /// Visibility: owner or private. Project spaces currently require owner visibility.
        #[arg(long)]
        visibility: String,
        /// Stable owner id used by the access filter.
        #[arg(long)]
        owner: String,
        /// Provenance label for imported material.
        #[arg(long)]
        source: String,
        /// User-controlled source version/revision.
        #[arg(long)]
        version: String,
        /// Workspace whose knowledge database receives the space.
        #[arg(long, default_value = ".")]
        cwd: PathBuf,
    },
    /// List knowledge space metadata without reading indexed content.
    KnowledgeSpaceList {
        /// Workspace whose `.yunxi/knowledge/knowledge.sqlite3` is queried.
        #[arg(long, default_value = ".")]
        cwd: PathBuf,
    },
    /// Print the OS-bound knowledge access principal used by user-owned spaces.
    KnowledgePrincipal,
    /// Import one explicitly selected document from stdin into a project/private space.
    KnowledgeImportStdin {
        /// Existing project/private space id.
        #[arg(long)]
        space_id: String,
        /// Stable document id; use letters, digits, '.', '_' or '-'.
        #[arg(long)]
        document_id: String,
        /// Human-readable document title.
        #[arg(long)]
        title: String,
        /// Provenance label for this document.
        #[arg(long)]
        source: String,
        /// User-controlled source version/revision.
        #[arg(long)]
        version: String,
        /// Owner id; must match the existing space.
        #[arg(long)]
        owner: String,
        /// Visibility; must match the existing space.
        #[arg(long)]
        visibility: String,
        /// Workspace whose knowledge database receives the document.
        #[arg(long, default_value = ".")]
        cwd: PathBuf,
    },
    /// Import one explicitly selected UTF-8 text/Markdown file from inside the workspace.
    KnowledgeImportFile {
        /// File path, resolved and canonicalized relative to --cwd when relative.
        path: PathBuf,
        /// Existing project/private space id.
        #[arg(long)]
        space_id: String,
        /// Stable document id; use letters, digits, '.', '_' or '-'.
        #[arg(long)]
        document_id: String,
        /// Human-readable document title.
        #[arg(long)]
        title: String,
        /// Provenance label for this document.
        #[arg(long)]
        source: String,
        /// User-controlled source version/revision.
        #[arg(long)]
        version: String,
        /// Owner id; must match the existing space.
        #[arg(long)]
        owner: String,
        /// Visibility; must match the existing space.
        #[arg(long)]
        visibility: String,
        /// Workspace whose knowledge database receives the document.
        #[arg(long, default_value = ".")]
        cwd: PathBuf,
    },
    /// Import an explicitly selected directory of project/private knowledge.
    KnowledgeImportDirectory {
        /// Directory to traverse, resolved relative to --cwd when relative.
        directory: PathBuf,
        /// Existing project/private space id.
        #[arg(long)]
        space_id: String,
        /// Provenance label for the imported directory.
        #[arg(long)]
        source: String,
        /// User-controlled source version/revision.
        #[arg(long)]
        version: String,
        /// Owner id; must match the existing space.
        #[arg(long)]
        owner: String,
        /// Visibility; must match the existing space.
        #[arg(long)]
        visibility: String,
        /// Workspace whose knowledge database receives the documents.
        #[arg(long, default_value = ".")]
        cwd: PathBuf,
    },
    /// Retract one knowledge document and its derived index rows.
    KnowledgeRetract {
        /// Stable document id returned by a knowledge collector/import.
        document_id: String,
        /// Explicit knowledge space; defaults to the Linux system space.
        #[arg(long, default_value = "system-linux")]
        space_id: String,
        /// Owner used for the space access filter.
        #[arg(long, default_value = "system")]
        owner: String,
        /// Visibility used for the space access filter.
        #[arg(long, default_value = "public")]
        visibility: String,
        /// Workspace whose `.yunxi/knowledge/knowledge.sqlite3` is updated.
        #[arg(long, default_value = ".")]
        cwd: PathBuf,
    },
    /// Start a new system knowledge generation without changing the active one.
    KnowledgeGenerationBegin {
        /// Workspace whose knowledge database receives the candidate manifest.
        #[arg(long, default_value = ".")]
        cwd: PathBuf,
    },
    /// Collect allowlisted help text into a candidate generation.
    KnowledgeStageHelp {
        /// Allowlisted command: bash, fish, git, systemctl, pacman, ip, or a small
        /// fixed set of read-only shell/coreutils help commands.
        command: String,
        /// Candidate generation returned by knowledge-generation-begin.
        #[arg(long)]
        generation: i64,
        /// Distro/runtime version recorded as provenance; `auto` reads os-release.
        #[arg(long, default_value = "auto")]
        source_version: String,
        /// Workspace whose knowledge database receives the staging document.
        #[arg(long, default_value = ".")]
        cwd: PathBuf,
    },
    /// Collect an explicit batch of allowlisted help pages into a candidate generation.
    KnowledgeStageCatalog {
        /// Candidate generation returned by knowledge-generation-begin.
        #[arg(long)]
        generation: i64,
        /// Allowlisted command to collect; repeat for multiple commands. With no
        /// occurrences, use the fixed Linux P0 catalog.
        #[arg(long = "command", action = ArgAction::Append)]
        command: Vec<String>,
        /// Distro/runtime version recorded as provenance; `auto` reads os-release.
        #[arg(long, default_value = "auto")]
        source_version: String,
        /// Workspace whose knowledge database receives the staging documents.
        #[arg(long, default_value = ".")]
        cwd: PathBuf,
    },
    /// Collect one local man page into a candidate generation.
    KnowledgeStageMan {
        /// Man topic such as `fish` or `systemctl`.
        topic: String,
        /// Candidate generation returned by knowledge-generation-begin.
        #[arg(long)]
        generation: i64,
        /// Optional man section, for example `1` or `8`.
        #[arg(long)]
        section: Option<String>,
        /// Distro/runtime version recorded as provenance; `auto` reads os-release.
        #[arg(long, default_value = "auto")]
        source_version: String,
        /// Workspace whose knowledge database receives the staging document.
        #[arg(long, default_value = ".")]
        cwd: PathBuf,
    },
    /// Collect an explicit batch of local man pages into a candidate generation.
    KnowledgeStageManCatalog {
        /// Candidate generation returned by knowledge-generation-begin.
        #[arg(long)]
        generation: i64,
        /// Man topic to collect; repeat for multiple topics. With no occurrences,
        /// use the fixed Linux P1 topic catalog.
        #[arg(long = "topic", action = ArgAction::Append)]
        topics: Vec<String>,
        /// Optional common man section, for example 1 or 8.
        #[arg(long)]
        section: Option<String>,
        /// Distro/runtime version recorded as provenance; `auto` reads os-release.
        #[arg(long, default_value = "auto")]
        source_version: String,
        /// Workspace whose knowledge database receives the staging documents.
        #[arg(long, default_value = ".")]
        cwd: PathBuf,
    },
    /// Process one pending staging-generation embedding job and exit.
    KnowledgeGenerationWorker {
        /// Stable worker identity used for the SQLite lease.
        #[arg(long)]
        worker_id: Option<String>,
        /// Maximum number of pending staging jobs to process in this invocation.
        #[arg(long, default_value_t = 1)]
        max_jobs: usize,
        /// Workspace whose knowledge database is processed.
        #[arg(long, default_value = ".")]
        cwd: PathBuf,
    },
    /// Inspect whether a candidate generation is ready for explicit activation.
    KnowledgeGenerationReadiness {
        /// Candidate generation returned by knowledge-generation-begin.
        #[arg(long)]
        generation: i64,
        /// Workspace whose knowledge database is inspected.
        #[arg(long, default_value = ".")]
        cwd: PathBuf,
    },
    /// Seal a complete building generation as ready without changing active state.
    KnowledgeGenerationSeal {
        /// Candidate generation returned by knowledge-generation-begin.
        #[arg(long)]
        generation: i64,
        /// Workspace whose knowledge database is updated.
        #[arg(long, default_value = ".")]
        cwd: PathBuf,
    },
    /// Atomically activate a ready candidate generation.
    KnowledgeGenerationActivate {
        /// Candidate generation returned by knowledge-generation-begin.
        #[arg(long)]
        generation: i64,
        /// Workspace whose knowledge database is updated.
        #[arg(long, default_value = ".")]
        cwd: PathBuf,
    },
    /// Hidden long-lived process used by shell-intercept.
    #[command(hide = true)]
    Daemon {
        /// Explicit workspace whose knowledge embedding queue is serviced by
        /// this daemon. Repeat up to 32 times for an explicit fleet; omitting
        /// it keeps the knowledge worker disabled. No directory discovery is
        /// performed.
        #[arg(long = "knowledge-workspace", action = ArgAction::Append)]
        knowledge_workspaces: Vec<PathBuf>,
        /// Maximum jobs claimed by each knowledge worker polling round.
        #[arg(long, default_value_t = 1)]
        knowledge_max_jobs: usize,
        /// Delay between knowledge worker polling rounds.
        #[arg(long, default_value_t = 5)]
        knowledge_interval_secs: u64,
        /// Stable worker identity used for the SQLite lease.
        #[arg(long)]
        knowledge_worker_id: Option<String>,
    },
}

pub(crate) async fn run_command(command: LinuxShellCommand) -> Result<()> {
    match command {
        LinuxShellCommand::FishInit { print, takeover } => install_fish_hook(print, takeover),
        LinuxShellCommand::RemoveShellHook => remove_fish_hook(),
        LinuxShellCommand::ShellClassify { shell, stdin } => {
            let input = read_shell_input(stdin)?;
            if is_shell_command(&input, &shell) {
                Ok(())
            } else {
                std::process::exit(1);
            }
        }
        LinuxShellCommand::ShellIntercept {
            shell,
            stdin,
            session_id,
            cwd,
            offline,
            live,
            provider,
            model,
        } => {
            let input = read_shell_input(stdin)?;
            run_shell_intercept(
                shell, session_id, cwd, input, offline, live, provider, model,
            )
            .await
        }
        LinuxShellCommand::RunStatus { run_id } => run_status(run_id).await,
        LinuxShellCommand::RunFollow { run_id, after_seq } => run_follow(run_id, after_seq).await,
        LinuxShellCommand::RunCancel { run_id } => run_cancel(run_id).await,
        LinuxShellCommand::RunDetached {
            prompt,
            cwd,
            session_id,
            offline,
            live,
            provider,
            model,
        } => run_detached(prompt, cwd, session_id, offline, live, provider, model).await,
        LinuxShellCommand::SystemdUnit => {
            print!("{SYSTEMD_UNIT}");
            Ok(())
        }
        LinuxShellCommand::LinuxTool { command } => linux_tools::run(command),
        LinuxShellCommand::KnowledgeMan {
            topic,
            section,
            source_version,
            cwd,
        } => run_knowledge_man(topic, section, source_version, cwd).await,
        LinuxShellCommand::KnowledgeHelp {
            command,
            source_version,
            cwd,
        } => run_knowledge_help(command, source_version, cwd).await,
        LinuxShellCommand::KnowledgeSearch {
            query,
            cwd,
            limit,
            source_version,
            space_id,
            owner,
            visibility,
            diagnostics,
        } => run_knowledge_search(
            query,
            cwd,
            limit,
            source_version.as_deref(),
            space_id,
            owner,
            visibility,
            diagnostics,
        ),
        LinuxShellCommand::KnowledgeIndex { document_id, cwd } => {
            run_knowledge_index(document_id, cwd)
        }
        LinuxShellCommand::KnowledgeEnqueue {
            document_id,
            embedding_model,
            cwd,
        } => run_knowledge_enqueue(document_id, embedding_model, cwd),
        LinuxShellCommand::KnowledgeRetry { job_id, cwd } => run_knowledge_retry(job_id, cwd),
        LinuxShellCommand::KnowledgeWorkerStatus { cwd, workspaces } => {
            if workspaces.is_empty() {
                run_knowledge_worker_status(cwd)
            } else {
                knowledge_worker::run_status(workspaces)
            }
        }
        LinuxShellCommand::KnowledgeWorkerHealth { worker_id } => {
            knowledge_worker::run_health(worker_id)
        }
        LinuxShellCommand::KnowledgeWorker {
            worker_id,
            max_jobs,
            watch,
            interval_secs,
            cwd,
            workspaces,
        } => {
            if workspaces.is_empty() {
                if watch {
                    run_knowledge_worker_watch(worker_id, max_jobs, interval_secs, cwd).await
                } else {
                    run_knowledge_worker(worker_id, max_jobs, cwd)
                }
            } else {
                knowledge_worker::run(worker_id, max_jobs, interval_secs, workspaces, watch).await
            }
        }
        LinuxShellCommand::KnowledgeVectorSearch {
            query,
            cwd,
            limit,
            source_version,
            space_id,
            owner,
            visibility,
            diagnostics,
        } => run_knowledge_vector_search(
            query,
            cwd,
            limit,
            source_version.as_deref(),
            space_id,
            owner,
            visibility,
            diagnostics,
        ),
        LinuxShellCommand::KnowledgeSpaceInit {
            space_id,
            kind,
            visibility,
            owner,
            source,
            version,
            cwd,
        } => run_knowledge_space_init(space_id, kind, visibility, owner, source, version, cwd),
        LinuxShellCommand::KnowledgeSpaceList { cwd } => run_knowledge_space_list(cwd),
        LinuxShellCommand::KnowledgePrincipal => run_knowledge_principal(),
        LinuxShellCommand::KnowledgeImportStdin {
            space_id,
            document_id,
            title,
            source,
            version,
            owner,
            visibility,
            cwd,
        } => run_knowledge_import_stdin(
            space_id,
            document_id,
            title,
            source,
            version,
            owner,
            visibility,
            cwd,
        ),
        LinuxShellCommand::KnowledgeImportFile {
            path,
            space_id,
            document_id,
            title,
            source,
            version,
            owner,
            visibility,
            cwd,
        } => run_knowledge_import_file(
            path,
            space_id,
            document_id,
            title,
            source,
            version,
            owner,
            visibility,
            cwd,
        ),
        LinuxShellCommand::KnowledgeImportDirectory {
            directory,
            space_id,
            source,
            version,
            owner,
            visibility,
            cwd,
        } => run_knowledge_import_directory(
            directory, space_id, source, version, owner, visibility, cwd,
        ),
        LinuxShellCommand::KnowledgeRetract {
            document_id,
            space_id,
            owner,
            visibility,
            cwd,
        } => run_knowledge_retract(document_id, space_id, owner, visibility, cwd),
        LinuxShellCommand::KnowledgeGenerationBegin { cwd } => run_knowledge_generation_begin(cwd),
        LinuxShellCommand::KnowledgeStageHelp {
            command,
            generation,
            source_version,
            cwd,
        } => run_knowledge_stage_help(command, generation, source_version, cwd).await,
        LinuxShellCommand::KnowledgeStageCatalog {
            generation,
            command,
            source_version,
            cwd,
        } => run_knowledge_stage_catalog(generation, command, source_version, cwd).await,
        LinuxShellCommand::KnowledgeStageMan {
            topic,
            generation,
            section,
            source_version,
            cwd,
        } => run_knowledge_stage_man(topic, generation, section, source_version, cwd).await,
        LinuxShellCommand::KnowledgeStageManCatalog {
            generation,
            topics,
            section,
            source_version,
            cwd,
        } => {
            run_knowledge_stage_man_catalog(generation, topics, section, source_version, cwd).await
        }
        LinuxShellCommand::KnowledgeGenerationWorker {
            worker_id,
            max_jobs,
            cwd,
        } => run_knowledge_generation_worker(worker_id, max_jobs, cwd),
        LinuxShellCommand::KnowledgeGenerationReadiness { generation, cwd } => {
            run_knowledge_generation_readiness(generation, cwd)
        }
        LinuxShellCommand::KnowledgeGenerationSeal { generation, cwd } => {
            run_knowledge_generation_seal(generation, cwd)
        }
        LinuxShellCommand::KnowledgeGenerationActivate { generation, cwd } => {
            run_knowledge_generation_activate(generation, cwd)
        }
        LinuxShellCommand::Daemon {
            knowledge_workspaces,
            knowledge_max_jobs,
            knowledge_interval_secs,
            knowledge_worker_id,
        } => {
            run_daemon(
                knowledge_workspaces,
                knowledge_max_jobs,
                knowledge_interval_secs,
                knowledge_worker_id,
            )
            .await
        }
    }
}

fn run_knowledge_search(
    query: String,
    cwd: PathBuf,
    limit: usize,
    source_version: Option<&str>,
    space_id: String,
    owner: String,
    visibility: String,
    diagnostics: bool,
) -> Result<()> {
    let started = Instant::now();
    let cwd = std::fs::canonicalize(&cwd)
        .with_context(|| format!("无法访问知识工作区: {}", cwd.display()))?;
    let store = yunxi_agent_storage::SqliteKnowledgeStore::for_workspace(&cwd);
    let visibility = parse_knowledge_visibility(&visibility)?;
    let access = knowledge_access_context()?;
    authorize_cli_read_owner(&access, &owner, visibility)?;
    let Some(scope) = store.accessible_space_scope(&space_id, &access)? else {
        bail!("知识空间尚未初始化: {space_id}");
    };
    if scope.visibility != visibility {
        bail!("请求的知识空间 visibility 与存储 metadata 不一致");
    }
    let matches = store.search_versioned(&query, &scope, source_version, limit.min(50))?;
    let results = matches
        .into_iter()
        .map(|item| {
            serde_json::json!({
                "chunk_id": item.chunk_id,
                "document_id": item.document_id,
                "space_id": item.space_id,
                "title": item.title,
                "content": item.content,
                "metadata_json": item.metadata_json,
                "source": item.source,
                "version": item.version,
                "generation": item.generation,
                "rank": item.rank,
            })
        })
        .collect::<Vec<_>>();
    let result_count = results.len();
    let mut output = serde_json::json!({
        "schema_version": 1,
        "query": query,
        "source_version": source_version,
        "space_id": space_id,
        "active_generation": scope.generation,
        "results": results,
    });
    if diagnostics {
        output["diagnostics"] = serde_json::json!({
            "retrieval_latency_us": started.elapsed().as_micros() as u64,
            "result_count": result_count,
        });
    }
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

fn run_knowledge_generation_begin(cwd: PathBuf) -> Result<()> {
    let cwd = canonical_knowledge_cwd(cwd)?;
    let store = yunxi_agent_storage::SqliteKnowledgeStore::for_workspace(&cwd);
    knowledge_collector::ensure_system_space(&store, "mixed")?;
    let provider = yunxi_agent_persona::LocalChargramEmbedding::default();
    let generation = store.begin_generation_build(
        "system-linux",
        "system",
        yunxi_agent_storage::KnowledgeVisibility::Public,
        Some(provider.model_id()),
        Some(provider.dimensions()),
    )?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "schema_version": 1,
            "status": "building",
            "space_id": generation.space_id,
            "generation": generation.generation,
            "embedding_model": generation.embedding_model,
            "dimensions": generation.vector_dimensions,
            "active_generation_unchanged": true,
        }))?
    );
    Ok(())
}

async fn run_knowledge_stage_help(
    command: String,
    generation: i64,
    source_version: String,
    cwd: PathBuf,
) -> Result<()> {
    let cwd = canonical_knowledge_cwd(cwd)?;
    let request = knowledge_collector::CommandHelpRequest {
        command,
        source_version: resolve_source_version(&source_version),
    };
    let cancellation = yunxi_agent_core::AgentCancellationToken::new();
    let collected = knowledge_collector::collect_help_command(&request, &cwd, cancellation).await?;
    run_staged_collection_result(&cwd, generation, &collected, "linux.command_help")
}

async fn run_knowledge_stage_catalog(
    generation: i64,
    commands: Vec<String>,
    source_version: String,
    cwd: PathBuf,
) -> Result<()> {
    let cwd = canonical_knowledge_cwd(cwd)?;
    let commands = if commands.is_empty() {
        knowledge_collector::P0_HELP_COMMANDS
            .iter()
            .map(|command| (*command).to_string())
            .collect::<Vec<_>>()
    } else {
        commands
    };
    if commands.len() > MAX_KNOWLEDGE_CATALOG_COMMANDS {
        bail!("knowledge-stage-catalog accepts at most {MAX_KNOWLEDGE_CATALOG_COMMANDS} commands");
    }
    let requested_commands = commands.clone();
    let source_version = resolve_source_version(&source_version);
    let mut results = Vec::with_capacity(commands.len());
    for command in commands {
        let request = knowledge_collector::CommandHelpRequest {
            command: command.clone(),
            source_version: source_version.clone(),
        };
        let cancellation = yunxi_agent_core::AgentCancellationToken::new();
        let mut result =
            match knowledge_collector::collect_help_command(&request, &cwd, cancellation).await {
                Ok(collected) => {
                    staged_collection_result(&cwd, generation, &collected, "linux.command_help")
                        .unwrap_or_else(|error| {
                            serde_json::json!({
                                "schema_version": 1,
                                "collector": "linux.command_help",
                                "command": command.clone(),
                                "generation": generation,
                                "status": "failed",
                                "error": error.to_string(),
                                "active_generation_unchanged": true,
                            })
                        })
                }
                Err(error) => serde_json::json!({
                    "schema_version": 1,
                    "collector": "linux.command_help",
                    "command": command.clone(),
                    "generation": generation,
                    "status": "failed",
                    "error": error.to_string(),
                    "active_generation_unchanged": true,
                }),
            };
        result["command"] = serde_json::Value::String(command);
        results.push(result);
    }

    let ok = results
        .iter()
        .filter(|result| result["status"] == "ok")
        .count();
    let unavailable = results
        .iter()
        .filter(|result| result["status"] == "unavailable")
        .count();
    let failed = results
        .iter()
        .filter(|result| result["status"] == "failed")
        .count();
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "schema_version": 1,
            "collector": "linux.command_help",
            "generation": generation,
            "source_version": source_version,
            "commands": requested_commands,
            "results": results,
            "summary": {
                "total": ok + unavailable + failed,
                "ok": ok,
                "unavailable": unavailable,
                "failed": failed,
            },
            "active_generation_unchanged": true,
        }))?
    );
    Ok(())
}

async fn run_knowledge_stage_man(
    topic: String,
    generation: i64,
    section: Option<String>,
    source_version: String,
    cwd: PathBuf,
) -> Result<()> {
    let cwd = canonical_knowledge_cwd(cwd)?;
    let request = knowledge_collector::ManPageRequest {
        topic,
        section,
        source_version: resolve_source_version(&source_version),
    };
    let cancellation = yunxi_agent_core::AgentCancellationToken::new();
    let collected = knowledge_collector::collect_man_page(&request, &cwd, cancellation).await?;
    run_staged_collection_result(&cwd, generation, &collected, "linux.man")
}

async fn run_knowledge_stage_man_catalog(
    generation: i64,
    topics: Vec<String>,
    section: Option<String>,
    source_version: String,
    cwd: PathBuf,
) -> Result<()> {
    let cwd = canonical_knowledge_cwd(cwd)?;
    let topics = if topics.is_empty() {
        knowledge_collector::P1_MAN_TOPICS
            .iter()
            .map(|topic| (*topic).to_string())
            .collect::<Vec<_>>()
    } else {
        topics
    };
    if topics.len() > MAX_KNOWLEDGE_MAN_TOPICS {
        bail!("knowledge-stage-man-catalog accepts at most {MAX_KNOWLEDGE_MAN_TOPICS} topics");
    }
    let requested_topics = topics.clone();
    let source_version = resolve_source_version(&source_version);
    let mut results = Vec::with_capacity(topics.len());
    for topic in topics {
        let request = knowledge_collector::ManPageRequest {
            topic: topic.clone(),
            section: section.clone(),
            source_version: source_version.clone(),
        };
        let cancellation = yunxi_agent_core::AgentCancellationToken::new();
        let mut result = match knowledge_collector::collect_man_page(&request, &cwd, cancellation)
            .await
        {
            Ok(collected) => staged_collection_result(&cwd, generation, &collected, "linux.man")
                .unwrap_or_else(|error| {
                    serde_json::json!({
                        "schema_version": 1,
                        "collector": "linux.man",
                        "topic": topic.clone(),
                        "generation": generation,
                        "status": "failed",
                        "error": error.to_string(),
                        "active_generation_unchanged": true,
                    })
                }),
            Err(error) => serde_json::json!({
                "schema_version": 1,
                "collector": "linux.man",
                "topic": topic.clone(),
                "generation": generation,
                "status": "failed",
                "error": error.to_string(),
                "active_generation_unchanged": true,
            }),
        };
        result["topic"] = serde_json::Value::String(topic);
        results.push(result);
    }
    let ok = results
        .iter()
        .filter(|result| result["status"] == "ok")
        .count();
    let unavailable = results
        .iter()
        .filter(|result| result["status"] == "unavailable")
        .count();
    let failed = results
        .iter()
        .filter(|result| result["status"] == "failed")
        .count();
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "schema_version": 1,
            "collector": "linux.man",
            "generation": generation,
            "source_version": source_version,
            "section": section,
            "topics": requested_topics,
            "results": results,
            "summary": {
                "total": ok + unavailable + failed,
                "ok": ok,
                "unavailable": unavailable,
                "failed": failed,
            },
            "active_generation_unchanged": true,
        }))?
    );
    Ok(())
}

fn run_staged_collection_result(
    cwd: &Path,
    generation: i64,
    collected: &knowledge_collector::CollectedKnowledge,
    collector: &str,
) -> Result<()> {
    let result = staged_collection_result(cwd, generation, collected, collector)?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

fn staged_collection_result(
    cwd: &Path,
    generation: i64,
    collected: &knowledge_collector::CollectedKnowledge,
    collector: &str,
) -> Result<serde_json::Value> {
    let mut result = serde_json::json!({
        "schema_version": 1,
        "collector": collector,
        "status": collected.status,
        "document_id": collected.document.document_id,
        "generation": generation,
        "argv": collected.argv,
        "exit_code": collected.exit_code,
        "truncated": collected.truncated,
        "active_generation_unchanged": true,
    });
    if collected.status != knowledge_collector::CollectionStatus::Ok {
        result["stderr"] = serde_json::Value::String(collected.stderr.clone());
        return Ok(result);
    }

    let store = yunxi_agent_storage::SqliteKnowledgeStore::for_workspace(cwd);
    let provider = yunxi_agent_persona::LocalChargramEmbedding::default();
    let options = yunxi_agent_storage::KnowledgeChunkingOptions::default();
    let summary = if collector == "linux.man" {
        knowledge_collector::stage_collected_man_page(&store, collected, generation, &options)?
    } else {
        knowledge_collector::stage_collected_knowledge(&store, collected, generation, &options)?
    };
    let job = store.enqueue_staging_embedding_job(
        "system-linux",
        &collected.document.document_id,
        provider.model_id(),
        generation,
    )?;
    result["content_hash"] = serde_json::Value::String(summary.content_hash);
    result["chunks_written"] = serde_json::json!(summary.chunks_written);
    result["chunks_removed"] = serde_json::json!(summary.chunks_removed);
    result["embedding_job"] = serde_json::json!({
        "status": job.status.as_str(),
        "job_id": job.job_id,
        "embedding_model": job.embedding_model,
        "generation": job.generation,
        "attempts": job.attempts,
    });
    Ok(result)
}

fn run_knowledge_generation_worker(
    worker_id: Option<String>,
    max_jobs: usize,
    cwd: PathBuf,
) -> Result<()> {
    if max_jobs == 0 || max_jobs > 1_000 {
        bail!("--max-jobs 必须在 1 到 1000 之间");
    }
    let cwd = canonical_knowledge_cwd(cwd)?;
    let store = yunxi_agent_storage::SqliteKnowledgeStore::for_workspace(&cwd);
    let provider = yunxi_agent_persona::LocalChargramEmbedding::default();
    let worker_id =
        worker_id.unwrap_or_else(|| format!("yunxi-linux-staging-worker-{}", std::process::id()));
    let mut jobs = Vec::new();
    for _ in 0..max_jobs {
        let Some(result) = store.process_next_staging_embedding_job(&worker_id, &provider)? else {
            break;
        };
        jobs.push(serde_json::json!({
            "status": result.job.status.as_str(),
            "job_id": result.job.job_id,
            "space_id": result.job.space_id,
            "document_id": result.job.document_id,
            "embedding_model": result.job.embedding_model,
            "generation": result.job.generation,
            "attempts": result.job.attempts,
            "next_attempt_at_millis": result.job.next_attempt_at_millis,
            "chunks_indexed": result.chunks_indexed,
            "last_error": result.job.last_error,
        }));
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "schema_version": 1,
            "status": if jobs.is_empty() { "idle" } else { "processed" },
            "worker_id": worker_id,
            "embedding_model": provider.model_id(),
            "jobs": jobs,
            "active_generation_unchanged": true,
        }))?
    );
    Ok(())
}

fn run_knowledge_generation_readiness(generation: i64, cwd: PathBuf) -> Result<()> {
    let cwd = canonical_knowledge_cwd(cwd)?;
    let store = yunxi_agent_storage::SqliteKnowledgeStore::for_workspace(&cwd);
    let active = active_system_scope(&store)?
        .context("Linux 知识库尚未初始化，请先运行 knowledge-generation-begin")?;
    let scope = yunxi_agent_storage::KnowledgeSearchScope {
        generation,
        ..active
    };
    let provider = yunxi_agent_persona::LocalChargramEmbedding::default();
    let readiness =
        store.inspect_generation_readiness(&scope, provider.model_id(), provider.dimensions())?;
    let manifest = store.generation_manifest("system-linux", generation)?;
    let manifest_json = manifest.as_ref().map(|manifest| {
        serde_json::json!({
            "space_id": manifest.space_id,
            "generation": manifest.generation,
            "state": manifest.state.as_str(),
            "embedding_model": manifest.embedding_model,
            "vector_dimensions": manifest.vector_dimensions,
            "expected_documents": manifest.expected_documents,
            "indexed_documents": manifest.indexed_documents,
            "content_digest": manifest.content_digest,
            "created_at_millis": manifest.created_at_millis,
            "completed_at_millis": manifest.completed_at_millis,
        })
    });
    let readiness_json = serde_json::json!({
        "ready": readiness.ready,
        "expected_documents": readiness.expected_documents,
        "actual_documents": readiness.actual_documents,
        "chunks": readiness.chunks,
        "vectors": readiness.vectors,
        "pending_jobs": readiness.pending_jobs,
        "running_jobs": readiness.running_jobs,
        "failed_jobs": readiness.failed_jobs,
        "reasons": readiness.reasons,
    });
    let candidate_sealed = manifest
        .as_ref()
        .is_some_and(|manifest| manifest.state.as_str() == "ready");
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "schema_version": 1,
            "space_id": "system-linux",
            "generation": generation,
            "manifest": manifest_json,
            "readiness": readiness_json,
            "active_generation": active.generation,
            "candidate_sealed": candidate_sealed,
            "activation_required": candidate_sealed && active.generation != generation,
        }))?
    );
    Ok(())
}

fn run_knowledge_generation_seal(generation: i64, cwd: PathBuf) -> Result<()> {
    let cwd = canonical_knowledge_cwd(cwd)?;
    let store = yunxi_agent_storage::SqliteKnowledgeStore::for_workspace(&cwd);
    let active = active_system_scope(&store)?
        .context("Linux 知识库尚未初始化，请先运行 knowledge-generation-begin")?;
    let scope = yunxi_agent_storage::KnowledgeSearchScope {
        generation,
        ..active
    };
    let provider = yunxi_agent_persona::LocalChargramEmbedding::default();
    let generation =
        store.seal_generation_ready(&scope, provider.model_id(), provider.dimensions())?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "schema_version": 1,
            "status": "ready",
            "space_id": generation.space_id,
            "generation": generation.generation,
            "embedding_model": generation.embedding_model,
            "dimensions": generation.vector_dimensions,
            "expected_documents": generation.expected_documents,
            "indexed_documents": generation.indexed_documents,
            "content_digest": generation.content_digest,
            "active_generation_unchanged": true,
        }))?
    );
    Ok(())
}

fn run_knowledge_generation_activate(generation: i64, cwd: PathBuf) -> Result<()> {
    let cwd = canonical_knowledge_cwd(cwd)?;
    let store = yunxi_agent_storage::SqliteKnowledgeStore::for_workspace(&cwd);
    let active = active_system_scope(&store)?
        .context("Linux 知识库尚未初始化，请先运行 knowledge-generation-begin")?;
    let scope = yunxi_agent_storage::KnowledgeSearchScope {
        generation,
        ..active.clone()
    };
    let provider = yunxi_agent_persona::LocalChargramEmbedding::default();
    let generation =
        store.activate_generation(&scope, provider.model_id(), provider.dimensions())?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "schema_version": 1,
            "status": "activated",
            "space_id": generation.space_id,
            "generation": generation.generation,
            "embedding_model": generation.embedding_model,
            "dimensions": generation.vector_dimensions,
            "active_generation": generation.generation,
        }))?
    );
    Ok(())
}

fn canonical_knowledge_cwd(cwd: PathBuf) -> Result<PathBuf> {
    std::fs::canonicalize(&cwd).with_context(|| format!("无法访问知识工作区: {}", cwd.display()))
}

fn knowledge_access_context() -> Result<yunxi_agent_storage::KnowledgeAccessContext> {
    let (principal, _, _) = current_knowledge_principal()?;
    yunxi_agent_storage::KnowledgeAccessContext::new(principal)
        .map_err(|error| anyhow::anyhow!("生成知识访问主体失败: {error}"))
}

fn current_knowledge_principal() -> Result<(String, u64, String)> {
    #[cfg(unix)]
    let (uid, username) = {
        let uid = unsafe { libc::geteuid() } as u64;
        let username = unsafe {
            libc::getpwuid(uid as libc::uid_t)
                .as_ref()
                .and_then(|entry| {
                    if entry.pw_name.is_null() {
                        None
                    } else {
                        CStr::from_ptr(entry.pw_name)
                            .to_str()
                            .ok()
                            .map(str::to_owned)
                    }
                })
        }
        .or_else(|| std::env::var("USER").ok())
        .unwrap_or_else(|| "unknown".to_string());
        (uid, username)
    };
    #[cfg(not(unix))]
    let (uid, username) = {
        let username = std::env::var("USERNAME")
            .or_else(|_| std::env::var("USER"))
            .unwrap_or_else(|_| "unknown".to_string());
        (0, username)
    };
    let username = username
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || "._-".contains(character) {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    let username = if username.is_empty() {
        "unknown".to_string()
    } else {
        username
    };
    Ok((format!("uid:{uid}:user:{username}"), uid, username))
}

fn parse_knowledge_space_kind(value: &str) -> Result<yunxi_agent_storage::KnowledgeSpaceKind> {
    yunxi_agent_storage::KnowledgeSpaceKind::parse(value)
        .map_err(|error| anyhow::anyhow!("知识空间 kind 无效: {error}"))
}

fn parse_knowledge_visibility(value: &str) -> Result<yunxi_agent_storage::KnowledgeVisibility> {
    yunxi_agent_storage::KnowledgeVisibility::parse(value)
        .map_err(|error| anyhow::anyhow!("知识空间 visibility 无效: {error}"))
}

fn validate_knowledge_identifier(name: &str, value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "._-".contains(character))
    {
        bail!("{name} 必须是 1 到 128 字节的字母、数字、'.'、'_' 或 '-' 组合")
    }
    Ok(())
}

fn validate_knowledge_metadata(name: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() || value.chars().count() > 512 {
        bail!("{name} 不能为空且不能超过 512 个字符")
    }
    Ok(())
}

fn authorize_cli_mutation_owner(
    access: &yunxi_agent_storage::KnowledgeAccessContext,
    requested_owner: &str,
) -> Result<()> {
    if requested_owner != access.principal() {
        bail!(
            "owner 必须匹配当前 OS 主体 {}；不能通过修改 --owner 冒充其他主体",
            access.principal()
        )
    }
    Ok(())
}

fn authorize_cli_read_owner(
    access: &yunxi_agent_storage::KnowledgeAccessContext,
    requested_owner: &str,
    visibility: yunxi_agent_storage::KnowledgeVisibility,
) -> Result<()> {
    // The system collector's public space is intentionally addressed by its
    // reserved internal principal.  User-owned reads must use the OS-derived
    // principal even when the caller can otherwise read a public space.
    if requested_owner == access.principal()
        || (requested_owner == "system"
            && visibility == yunxi_agent_storage::KnowledgeVisibility::Public)
    {
        return Ok(());
    }
    bail!(
        "owner 必须匹配当前 OS 主体 {}；不能通过修改 --owner 冒充其他主体",
        access.principal()
    )
}

fn run_knowledge_space_init(
    space_id: String,
    kind: String,
    visibility: String,
    owner: String,
    source: String,
    version: String,
    cwd: PathBuf,
) -> Result<()> {
    validate_knowledge_identifier("space_id", &space_id)?;
    validate_knowledge_metadata("owner", &owner)?;
    validate_knowledge_metadata("source", &source)?;
    validate_knowledge_metadata("version", &version)?;
    let kind = parse_knowledge_space_kind(&kind)?;
    let visibility = parse_knowledge_visibility(&visibility)?;
    let access = knowledge_access_context()?;
    authorize_cli_mutation_owner(&access, &owner)?;
    if kind == yunxi_agent_storage::KnowledgeSpaceKind::System {
        bail!("knowledge-space-init 只允许创建 project 或 private 空间")
    }
    if kind == yunxi_agent_storage::KnowledgeSpaceKind::Project
        && visibility != yunxi_agent_storage::KnowledgeVisibility::Owner
    {
        bail!("project 空间当前只允许 owner visibility")
    }
    if kind == yunxi_agent_storage::KnowledgeSpaceKind::Private
        && visibility == yunxi_agent_storage::KnowledgeVisibility::Public
    {
        bail!("private 空间不能使用 public visibility")
    }
    let cwd = canonical_knowledge_cwd(cwd)?;
    let store = yunxi_agent_storage::SqliteKnowledgeStore::for_workspace(&cwd);
    let spec = yunxi_agent_storage::KnowledgeSpaceSpec {
        space_id,
        kind,
        owner,
        visibility,
        source,
        version,
        generation: 1,
    };
    access
        .authorize_mutation(&spec)
        .map_err(|error| anyhow::anyhow!("知识空间访问主体校验失败: {error}"))?;
    let status = match store.read_space(&spec.space_id)? {
        Some(existing) if existing == spec => "existing",
        Some(_) => bail!("知识空间已存在但 metadata 不一致；拒绝静默覆盖"),
        None => {
            store.upsert_space(&spec)?;
            "created"
        }
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "schema_version": 1,
            "status": status,
            "space_id": spec.space_id,
            "kind": spec.kind.as_str(),
            "owner": spec.owner,
            "visibility": spec.visibility.as_str(),
            "source": spec.source,
            "version": spec.version,
            "generation": spec.generation,
            "database": cwd.join(".yunxi/knowledge/knowledge.sqlite3"),
        }))?
    );
    Ok(())
}

fn run_knowledge_space_list(cwd: PathBuf) -> Result<()> {
    let cwd = canonical_knowledge_cwd(cwd)?;
    let store = yunxi_agent_storage::SqliteKnowledgeStore::for_workspace(&cwd);
    let access = knowledge_access_context()?;
    let spaces = store
        .list_accessible_spaces(&access)?
        .into_iter()
        .map(|space| {
            serde_json::json!({
                "space_id": space.space_id,
                "kind": space.kind.as_str(),
                "owner": space.owner,
                "visibility": space.visibility.as_str(),
                "source": space.source,
                "version": space.version,
                "generation": space.generation,
            })
        })
        .collect::<Vec<_>>();
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "schema_version": 1,
            "spaces": spaces,
            "database": cwd.join(".yunxi/knowledge/knowledge.sqlite3"),
        }))?
    );
    Ok(())
}

fn run_knowledge_principal() -> Result<()> {
    let (principal, uid, username) = current_knowledge_principal()?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "schema_version": 1,
            "principal": principal,
            "uid": uid,
            "username": username,
        }))?
    );
    Ok(())
}

fn run_knowledge_import_stdin(
    space_id: String,
    document_id: String,
    title: String,
    source: String,
    version: String,
    owner: String,
    visibility: String,
    cwd: PathBuf,
) -> Result<()> {
    let mut input = String::new();
    io::stdin()
        .read_to_string(&mut input)
        .context("读取知识导入 stdin 失败")?;
    run_knowledge_import_text(
        space_id,
        document_id,
        title,
        source,
        version,
        owner,
        visibility,
        cwd,
        input,
        "stdin",
    )
}

fn run_knowledge_import_file(
    path: PathBuf,
    space_id: String,
    document_id: String,
    title: String,
    source: String,
    version: String,
    owner: String,
    visibility: String,
    cwd: PathBuf,
) -> Result<()> {
    const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
    let cwd = canonical_knowledge_cwd(cwd)?;
    let path = if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    };
    let path = std::fs::canonicalize(&path)
        .with_context(|| "无法访问知识文件；文件必须位于工作区内".to_string())?;
    if !path.starts_with(&cwd) {
        bail!("知识文件必须位于工作区内")
    }
    let internal_state = cwd.join(".yunxi");
    if path.starts_with(&internal_state) {
        bail!("知识文件不能位于工作区的 .yunxi 状态目录内")
    }
    let metadata = std::fs::metadata(&path).context("读取知识文件元数据失败")?;
    if !metadata.is_file() {
        bail!("知识文件必须是普通文件")
    }
    if metadata.len() > MAX_FILE_BYTES {
        bail!("知识文件超过 8 MiB 大小上限")
    }
    let input =
        std::fs::read_to_string(&path).context("知识文件必须是 UTF-8 文本或 Markdown 文件")?;
    run_knowledge_import_text(
        space_id,
        document_id,
        title,
        source,
        version,
        owner,
        visibility,
        cwd,
        input,
        "file",
    )
}

fn run_knowledge_import_directory(
    directory: PathBuf,
    space_id: String,
    source: String,
    version: String,
    owner: String,
    visibility: String,
    cwd: PathBuf,
) -> Result<()> {
    validate_knowledge_identifier("space_id", &space_id)?;
    validate_knowledge_metadata("source", &source)?;
    validate_knowledge_metadata("version", &version)?;
    validate_knowledge_metadata("owner", &owner)?;
    let _visibility = parse_knowledge_visibility(&visibility)?;
    let access = knowledge_access_context()?;
    authorize_cli_mutation_owner(&access, &owner)?;
    let cwd = canonical_knowledge_cwd(cwd)?;
    let raw_directory = if directory.is_absolute() {
        directory
    } else {
        cwd.join(directory)
    };
    let metadata = std::fs::symlink_metadata(&raw_directory).context("无法读取知识目录元数据")?;
    if metadata.file_type().is_symlink() {
        bail!("知识目录不能是符号链接")
    }
    let directory =
        std::fs::canonicalize(&raw_directory).context("无法访问知识目录；目录必须位于工作区内")?;
    if !directory.starts_with(&cwd) {
        bail!("知识目录必须位于工作区内")
    }
    if directory == cwd.join(".yunxi") || directory.starts_with(cwd.join(".yunxi")) {
        bail!("知识目录不能位于工作区的 .yunxi 状态目录内")
    }
    if !std::fs::metadata(&directory)?.is_dir() {
        bail!("知识目录必须是目录")
    }

    let (files, skipped_during_walk) = collect_knowledge_directory_files(&directory)?;
    let mut imported = 0usize;
    let mut skipped = skipped_during_walk;
    let mut failed = 0usize;
    let mut total_bytes = 0u64;
    let mut documents = Vec::with_capacity(files.len());
    for path in files {
        let relative = path
            .strip_prefix(&directory)
            .unwrap_or(path.as_path())
            .to_string_lossy()
            .replace('\\', "/");
        let metadata = match std::fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(_) => {
                failed += 1;
                documents.push(serde_json::json!({
                    "status": "failed",
                    "relative_path": relative,
                    "error_code": "metadata_unavailable",
                }));
                continue;
            }
        };
        if metadata.len() > KNOWLEDGE_IMPORT_DIRECTORY_MAX_FILE_BYTES
            || total_bytes.saturating_add(metadata.len())
                > KNOWLEDGE_IMPORT_DIRECTORY_MAX_TOTAL_BYTES
        {
            skipped += 1;
            documents.push(serde_json::json!({
                "status": "skipped",
                "relative_path": relative,
                "reason": "size_limit",
            }));
            continue;
        }
        let input = match std::fs::read_to_string(&path) {
            Ok(input) => input,
            Err(_) => {
                skipped += 1;
                documents.push(serde_json::json!({
                    "status": "skipped",
                    "relative_path": relative,
                    "reason": "non_utf8_or_unreadable",
                }));
                continue;
            }
        };
        total_bytes = total_bytes.saturating_add(metadata.len());
        let document_id = format!("dir-{:016x}", stable_directory_hash(&relative));
        let title = relative.clone();
        match import_knowledge_text(
            space_id.clone(),
            document_id.clone(),
            title,
            source.clone(),
            version.clone(),
            owner.clone(),
            visibility.clone(),
            cwd.clone(),
            input,
            "directory",
        ) {
            Ok(result) => {
                imported += 1;
                documents.push(serde_json::json!({
                    "status": "imported",
                    "relative_path": relative,
                    "document_id": document_id,
                    "generation": result["generation"],
                    "chunks_written": result["chunks_written"],
                    "embedding_job": result["embedding_job"],
                }));
            }
            Err(_) => {
                failed += 1;
                documents.push(serde_json::json!({
                    "status": "failed",
                    "relative_path": relative,
                    "document_id": document_id,
                    "error_code": "knowledge_import_failed",
                }));
            }
        }
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "schema_version": 1,
            "status": if failed > 0 { "completed_with_errors" } else { "imported" },
            "space_id": space_id,
            "source": source,
            "version": version,
            "files_discovered": documents.len(),
            "imported": imported,
            "skipped": skipped,
            "failed": failed,
            "total_bytes": total_bytes,
            "documents": documents,
        }))?
    );
    Ok(())
}

fn collect_knowledge_directory_files(root: &Path) -> Result<(Vec<PathBuf>, usize)> {
    fn visit(
        directory: &Path,
        depth: usize,
        files: &mut Vec<PathBuf>,
        skipped: &mut usize,
    ) -> Result<()> {
        let mut entries = std::fs::read_dir(directory)
            .with_context(|| "读取知识目录失败".to_string())?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                *skipped += 1;
                continue;
            }
            if metadata.is_dir() {
                if entry.file_name() == ".yunxi" || depth >= KNOWLEDGE_IMPORT_DIRECTORY_MAX_DEPTH {
                    *skipped += 1;
                    continue;
                }
                visit(&path, depth + 1, files, skipped)?;
                continue;
            }
            if metadata.is_file() && is_knowledge_directory_text_file(&path) {
                if files.len() >= KNOWLEDGE_IMPORT_DIRECTORY_MAX_FILES {
                    bail!(
                        "知识目录超过 {} 个文件的上限",
                        KNOWLEDGE_IMPORT_DIRECTORY_MAX_FILES
                    )
                }
                files.push(path);
            } else {
                *skipped += 1;
            }
        }
        Ok(())
    }

    let mut files = Vec::new();
    let mut skipped = 0;
    visit(root, 0, &mut files, &mut skipped)?;
    Ok((files, skipped))
}

fn is_knowledge_directory_text_file(path: &Path) -> bool {
    const EXTENSIONS: &[&str] = &[
        "c", "cc", "cpp", "fish", "go", "h", "hpp", "java", "js", "json", "md", "py", "rs", "sh",
        "toml", "ts", "tsx", "txt", "yaml", "yml",
    ];
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    if matches!(
        file_name,
        "AGENTS.md" | "Dockerfile" | "LICENSE" | "Makefile" | "PKGBUILD" | "README"
    ) {
        return true;
    }
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|extension| {
            EXTENSIONS
                .iter()
                .any(|allowed| extension.eq_ignore_ascii_case(allowed))
        })
}

fn stable_directory_hash(value: &str) -> u64 {
    value.bytes().fold(0xcbf29ce484222325u64, |hash, byte| {
        hash.wrapping_mul(0x100000001b3)
            .wrapping_add(u64::from(byte))
    })
}

fn run_knowledge_import_text(
    space_id: String,
    document_id: String,
    title: String,
    source: String,
    version: String,
    owner: String,
    visibility: String,
    cwd: PathBuf,
    input: String,
    import_kind: &str,
) -> Result<()> {
    let result = import_knowledge_text(
        space_id,
        document_id,
        title,
        source,
        version,
        owner,
        visibility,
        cwd,
        input,
        import_kind,
    )?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

fn import_knowledge_text(
    space_id: String,
    document_id: String,
    title: String,
    source: String,
    version: String,
    owner: String,
    visibility: String,
    cwd: PathBuf,
    input: String,
    import_kind: &str,
) -> Result<serde_json::Value> {
    validate_knowledge_identifier("space_id", &space_id)?;
    validate_knowledge_identifier("document_id", &document_id)?;
    validate_knowledge_metadata("title", &title)?;
    validate_knowledge_metadata("source", &source)?;
    validate_knowledge_metadata("version", &version)?;
    validate_knowledge_metadata("owner", &owner)?;
    let visibility = parse_knowledge_visibility(&visibility)?;
    let access = knowledge_access_context()?;
    authorize_cli_mutation_owner(&access, &owner)?;
    let cwd = canonical_knowledge_cwd(cwd)?;
    let store = yunxi_agent_storage::SqliteKnowledgeStore::for_workspace(&cwd);
    let existing = store
        .read_space(&space_id)?
        .with_context(|| format!("知识空间不存在: {space_id}；请先运行 knowledge-space-init"))?;
    access
        .authorize_mutation(&existing)
        .map_err(|error| anyhow::anyhow!("知识空间访问主体校验失败: {error}"))?;
    if existing.kind == yunxi_agent_storage::KnowledgeSpaceKind::System {
        bail!("知识导入不允许写入 system 空间")
    }
    if existing.owner != owner || existing.visibility != visibility {
        bail!("导入 metadata 与知识空间 owner/visibility 不一致")
    }
    if existing.source != source || existing.version != version {
        bail!("导入 source/version 必须与知识空间 metadata 一致")
    }
    let scope = store
        .active_space_scope(&space_id, &owner, visibility)?
        .context("知识空间没有可用的 active generation")?;
    let normalized = yunxi_agent_storage::normalize_knowledge_text(
        &input,
        yunxi_agent_storage::KnowledgeChunkingOptions::default().max_input_chars,
    )?;
    if normalized.is_empty() {
        bail!("知识导入内容不能为空")
    }
    let document = yunxi_agent_storage::KnowledgeDocument {
        document_id,
        space_id: space_id.clone(),
        title,
        source,
        version,
        generation: scope.generation,
        owner,
        visibility,
        metadata_json: serde_json::json!({
            "import": import_kind,
            "space_kind": existing.kind.as_str(),
        })
        .to_string(),
    };
    let summary = store.ingest_text(
        &document,
        &normalized,
        &yunxi_agent_storage::KnowledgeChunkingOptions::default(),
    )?;
    let job = store.enqueue_current_document_embedding_job(
        &document.document_id,
        yunxi_agent_persona::LOCAL_MEMORY_EMBEDDING_MODEL,
    )?;
    Ok(serde_json::json!({
        "schema_version": 1,
        "status": "imported",
        "space_id": document.space_id,
        "document_id": document.document_id,
        "generation": document.generation,
        "content_hash": summary.content_hash,
        "chunks_written": summary.chunks_written,
        "chunks_removed": summary.chunks_removed,
        "embedding_job": {
            "job_id": job.job_id,
            "status": job.status.as_str(),
            "embedding_model": job.embedding_model,
            "generation": job.generation,
        },
    }))
}

fn run_knowledge_index(document_id: String, cwd: PathBuf) -> Result<()> {
    let cwd = std::fs::canonicalize(&cwd)
        .with_context(|| format!("无法访问知识工作区: {}", cwd.display()))?;
    let store = yunxi_agent_storage::SqliteKnowledgeStore::for_workspace(&cwd);
    let provider = yunxi_agent_persona::LocalChargramEmbedding::default();
    let summary = store.index_document_with_embeddings(&document_id, &provider)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "schema_version": 1,
            "status": "ok",
            "document_id": summary.document_id,
            "embedding_model": summary.embedding_model,
            "dimensions": summary.dimensions,
            "chunks_indexed": summary.chunks_indexed,
        }))?
    );
    Ok(())
}

fn run_knowledge_enqueue(document_id: String, embedding_model: String, cwd: PathBuf) -> Result<()> {
    let cwd = std::fs::canonicalize(&cwd)
        .with_context(|| format!("无法访问知识工作区: {}", cwd.display()))?;
    let store = yunxi_agent_storage::SqliteKnowledgeStore::for_workspace(&cwd);
    let job = store.enqueue_current_document_embedding_job(&document_id, &embedding_model)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "schema_version": 1,
            "status": job.status.as_str(),
            "job_id": job.job_id,
            "document_id": job.document_id,
            "embedding_model": job.embedding_model,
            "generation": job.generation,
            "attempts": job.attempts,
            "next_attempt_at_millis": job.next_attempt_at_millis,
        }))?
    );
    Ok(())
}

fn run_knowledge_retry(job_id: i64, cwd: PathBuf) -> Result<()> {
    let cwd = std::fs::canonicalize(&cwd)
        .with_context(|| format!("无法访问知识工作区: {}", cwd.display()))?;
    let store = yunxi_agent_storage::SqliteKnowledgeStore::for_workspace(&cwd);
    let job = store.retry_embedding_job(job_id)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "schema_version": 1,
            "status": job.status.as_str(),
            "job_id": job.job_id,
            "document_id": job.document_id,
            "embedding_model": job.embedding_model,
            "generation": job.generation,
            "attempts": job.attempts,
            "next_attempt_at_millis": job.next_attempt_at_millis,
            "last_error": job.last_error,
        }))?
    );
    Ok(())
}

fn run_knowledge_worker_status(cwd: PathBuf) -> Result<()> {
    let cwd = canonical_knowledge_cwd(cwd)?;
    let store = yunxi_agent_storage::SqliteKnowledgeStore::for_workspace(&cwd);
    let snapshot = store.embedding_queue_status()?;
    let status = knowledge_queue_status_label(&snapshot);
    let mut output = serde_json::to_value(&snapshot)?;
    let object = output
        .as_object_mut()
        .context("知识队列状态输出不是 JSON 对象")?;
    object.insert("schema_version".to_string(), serde_json::json!(1));
    object.insert("status".to_string(), serde_json::json!(status));
    object.insert(
        "embedding_model".to_string(),
        serde_json::json!(yunxi_agent_persona::LOCAL_MEMORY_EMBEDDING_MODEL),
    );
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

fn knowledge_queue_status_label(
    snapshot: &yunxi_agent_storage::KnowledgeEmbeddingQueueStatus,
) -> &'static str {
    let queues = [&snapshot.active, &snapshot.staging];
    let has_failed = queues
        .iter()
        .any(|queue| queue.failed > 0 || queue.expired_leases > 0 || queue.terminal_failed > 0);
    let has_ready = queues
        .iter()
        .any(|queue| queue.pending_ready > 0 || queue.retry_due > 0);
    let has_unfinished = queues
        .iter()
        .any(|queue| queue.pending > 0 || queue.running > 0 || queue.failed > 0);
    if has_failed {
        "degraded"
    } else if has_ready {
        "ready"
    } else if !has_unfinished && queues.iter().any(|queue| queue.completed > 0) {
        "complete"
    } else {
        "idle"
    }
}

fn run_knowledge_worker(worker_id: Option<String>, max_jobs: usize, cwd: PathBuf) -> Result<()> {
    validate_worker_max_jobs(max_jobs)?;
    let cwd = canonicalize_worker_workspace(&cwd)?;
    let worker_id =
        worker_id.unwrap_or_else(|| format!("yunxi-linux-embedding-worker-{}", std::process::id()));
    let jobs = process_knowledge_workspace(&worker_id, max_jobs, &cwd)?;
    let provider = yunxi_agent_persona::LocalChargramEmbedding::default();
    let output = serde_json::json!({
        "schema_version": 1,
        "status": if jobs.is_empty() { "idle" } else { "processed" },
        "worker_id": worker_id,
        "embedding_model": provider.model_id(),
        "jobs": jobs,
    });
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

fn validate_worker_max_jobs(max_jobs: usize) -> Result<()> {
    if max_jobs == 0 || max_jobs > 1_000 {
        bail!("--max-jobs 必须在 1 到 1000 之间");
    }
    Ok(())
}

fn canonicalize_worker_workspace(cwd: &Path) -> Result<PathBuf> {
    let canonical = std::fs::canonicalize(cwd)
        .with_context(|| format!("无法访问知识工作区: {}", cwd.display()))?;
    if !canonical.is_dir() {
        bail!("知识工作区不是目录: {}", canonical.display());
    }
    Ok(canonical)
}

fn process_knowledge_workspace(
    worker_id: &str,
    max_jobs: usize,
    cwd: &Path,
) -> Result<Vec<serde_json::Value>> {
    let store = yunxi_agent_storage::SqliteKnowledgeStore::for_workspace(cwd);
    let provider = yunxi_agent_persona::LocalChargramEmbedding::default();
    let mut jobs = Vec::new();
    for _ in 0..max_jobs {
        let Some(result) = store.process_next_embedding_job(worker_id, &provider)? else {
            break;
        };
        jobs.push(serde_json::json!({
            "status": result.job.status.as_str(),
            "job_id": result.job.job_id,
            "document_id": result.job.document_id,
            "embedding_model": result.job.embedding_model,
            "generation": result.job.generation,
            "attempts": result.job.attempts,
            "next_attempt_at_millis": result.job.next_attempt_at_millis,
            "chunks_indexed": result.chunks_indexed,
            "last_error": result.job.last_error,
        }));
    }
    Ok(jobs)
}

async fn run_knowledge_worker_watch(
    worker_id: Option<String>,
    max_jobs: usize,
    interval_secs: u64,
    cwd: PathBuf,
) -> Result<()> {
    if interval_secs == 0 || interval_secs > 3_600 {
        bail!("--interval-secs 必须在 1 到 3600 之间");
    }
    let worker_id =
        worker_id.unwrap_or_else(|| format!("yunxi-linux-embedding-watch-{}", std::process::id()));
    loop {
        run_knowledge_worker(Some(worker_id.clone()), max_jobs, cwd.clone())?;
        tokio::select! {
            stop_reason = wait_for_knowledge_worker_stop() => {
                let stop_reason = stop_reason?;
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "schema_version": 1,
                        "status": "stopped",
                        "worker_id": worker_id,
                        "reason": stop_reason,
                    }))?
                );
                return Ok(());
            }
            _ = tokio::time::sleep(std::time::Duration::from_secs(interval_secs)) => {}
        }
    }
}

async fn wait_for_knowledge_worker_stop() -> Result<&'static str> {
    let interrupt = tokio::signal::ctrl_c();
    tokio::pin!(interrupt);

    #[cfg(unix)]
    {
        use anyhow::Context;
        use tokio::signal::unix::{SignalKind, signal};

        let mut terminate = signal(SignalKind::terminate())
            .context("无法注册 knowledge worker 的 SIGTERM 处理器")?;
        tokio::select! {
            result = &mut interrupt => {
                result.context("等待 knowledge worker 的 Ctrl+C 信号失败")?;
                Ok("interrupt")
            }
            _ = terminate.recv() => Ok("terminate"),
        }
    }

    #[cfg(not(unix))]
    {
        interrupt
            .await
            .context("等待 knowledge worker 的 Ctrl+C 信号失败")?;
        Ok("interrupt")
    }
}

fn run_knowledge_vector_search(
    query: String,
    cwd: PathBuf,
    limit: usize,
    source_version: Option<&str>,
    space_id: String,
    owner: String,
    visibility: String,
    diagnostics: bool,
) -> Result<()> {
    let started = Instant::now();
    let cwd = std::fs::canonicalize(&cwd)
        .with_context(|| format!("无法访问知识工作区: {}", cwd.display()))?;
    let store = yunxi_agent_storage::SqliteKnowledgeStore::for_workspace(&cwd);
    let provider = yunxi_agent_persona::LocalChargramEmbedding::default();
    let embedding_started = Instant::now();
    let embedding = yunxi_agent_persona::MemoryEmbeddingProvider::embed(&provider, &query)
        .map_err(|error| anyhow::anyhow!("知识查询 embedding 失败: {error}"))?;
    let embedding_latency_us = embedding_started.elapsed().as_micros() as u64;
    let visibility = parse_knowledge_visibility(&visibility)?;
    let access = knowledge_access_context()?;
    authorize_cli_read_owner(&access, &owner, visibility)?;
    let Some(scope) = store.accessible_space_scope(&space_id, &access)? else {
        bail!("知识空间尚未初始化: {space_id}");
    };
    if scope.visibility != visibility {
        bail!("请求的知识空间 visibility 与存储 metadata 不一致");
    }
    let retrieval_started = Instant::now();
    let matches = store.search_vectors_versioned(
        &embedding.values,
        provider.model_id(),
        &scope,
        source_version,
        limit.min(50),
    )?;
    let retrieval_latency_us = retrieval_started.elapsed().as_micros() as u64;
    let results = matches
        .into_iter()
        .map(|item| {
            serde_json::json!({
                "chunk_id": item.chunk_id,
                "document_id": item.document_id,
                "space_id": item.space_id,
                "title": item.title,
                "content": item.content,
                "metadata_json": item.metadata_json,
                "source": item.source,
                "version": item.version,
                "generation": item.generation,
                "score": item.score,
                "embedding_model": item.embedding_model,
            })
        })
        .collect::<Vec<_>>();
    let result_count = results.len();
    let mut output = serde_json::json!({
        "schema_version": 1,
        "query": query,
        "source_version": source_version,
        "embedding_model": provider.model_id(),
        "space_id": space_id,
        "active_generation": scope.generation,
        "results": results,
    });
    if diagnostics {
        output["diagnostics"] = serde_json::json!({
            "embedding_latency_us": embedding_latency_us,
            "retrieval_latency_us": retrieval_latency_us,
            "total_latency_us": started.elapsed().as_micros() as u64,
            "result_count": result_count,
        });
    }
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

fn run_knowledge_retract(
    document_id: String,
    space_id: String,
    owner: String,
    visibility: String,
    cwd: PathBuf,
) -> Result<()> {
    validate_knowledge_identifier("document_id", &document_id)?;
    validate_knowledge_identifier("space_id", &space_id)?;
    validate_knowledge_metadata("owner", &owner)?;
    let visibility = parse_knowledge_visibility(&visibility)?;
    let access = knowledge_access_context()?;
    let cwd = std::fs::canonicalize(&cwd)
        .with_context(|| format!("无法访问知识工作区: {}", cwd.display()))?;
    let store = yunxi_agent_storage::SqliteKnowledgeStore::for_workspace(&cwd);
    let existing = store
        .read_space(&space_id)?
        .with_context(|| format!("知识空间尚未初始化: {space_id}"))?;
    let internal_system_retract = owner == "system"
        && visibility == yunxi_agent_storage::KnowledgeVisibility::Public
        && existing.kind == yunxi_agent_storage::KnowledgeSpaceKind::System
        && existing.owner == "system";
    if !internal_system_retract {
        authorize_cli_mutation_owner(&access, &owner)?;
        access
            .authorize_mutation(&existing)
            .map_err(|error| anyhow::anyhow!("知识空间访问主体校验失败: {error}"))?;
    }
    let Some(scope) = store.accessible_space_scope(&space_id, &access)? else {
        bail!("知识空间尚未初始化: {space_id}");
    };
    if scope.visibility != visibility {
        bail!("请求的知识空间 visibility 与存储 metadata 不一致");
    }
    let retracted = store.retract_document(&document_id, &scope)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "schema_version": 1,
            "status": if retracted { "retracted" } else { "not_found" },
            "document_id": document_id,
            "space_id": space_id,
        }))?
    );
    Ok(())
}

async fn run_knowledge_help(command: String, source_version: String, cwd: PathBuf) -> Result<()> {
    let cwd = std::fs::canonicalize(&cwd)
        .with_context(|| format!("无法访问知识工作区: {}", cwd.display()))?;
    let request = knowledge_collector::CommandHelpRequest {
        command,
        source_version: resolve_source_version(&source_version),
    };
    let cancellation = yunxi_agent_core::AgentCancellationToken::new();
    let mut collected =
        knowledge_collector::collect_help_command(&request, &cwd, cancellation).await?;
    let status = collected.status;
    let stderr = collected.stderr.clone();
    let mut result = serde_json::json!({
        "schema_version": 1,
        "collector": "linux.command_help",
        "status": status,
        "document_id": collected.document.document_id,
        "source_version": request.source_version,
        "argv": collected.argv,
        "exit_code": collected.exit_code,
        "truncated": collected.truncated,
    });
    if status != knowledge_collector::CollectionStatus::Ok {
        result["stderr"] = serde_json::Value::String(stderr);
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }

    let store = yunxi_agent_storage::SqliteKnowledgeStore::for_workspace(&cwd);
    knowledge_collector::ensure_system_space(&store, &request.source_version)?;
    let scope = active_system_scope(&store)?.context("Linux 知识库初始化后缺少当前生效代际")?;
    collected.document.generation = scope.generation;
    let summary = knowledge_collector::ingest_collected_knowledge(
        &store,
        &collected,
        &yunxi_agent_storage::KnowledgeChunkingOptions::default(),
    )?;
    result["content_hash"] = serde_json::Value::String(summary.content_hash);
    result["chunks_written"] = serde_json::json!(summary.chunks_written);
    result["chunks_removed"] = serde_json::json!(summary.chunks_removed);
    result["embedding_job"] = enqueue_collected_embedding(&store, &collected.document.document_id)?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

async fn run_knowledge_man(
    topic: String,
    section: Option<String>,
    source_version: String,
    cwd: PathBuf,
) -> Result<()> {
    let cwd = std::fs::canonicalize(&cwd)
        .with_context(|| format!("无法访问知识工作区: {}", cwd.display()))?;
    let request = knowledge_collector::ManPageRequest {
        topic,
        section,
        source_version: resolve_source_version(&source_version),
    };
    let cancellation = yunxi_agent_core::AgentCancellationToken::new();
    let mut collected = knowledge_collector::collect_man_page(&request, &cwd, cancellation).await?;
    let status = collected.status;
    let stderr = collected.stderr.clone();
    let mut result = serde_json::json!({
        "schema_version": 1,
        "collector": "linux.man",
        "status": status,
        "document_id": collected.document.document_id,
        "source_version": request.source_version,
        "argv": collected.argv,
        "exit_code": collected.exit_code,
        "truncated": collected.truncated,
    });
    if status != knowledge_collector::CollectionStatus::Ok {
        result["stderr"] = serde_json::Value::String(stderr);
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }

    let store = yunxi_agent_storage::SqliteKnowledgeStore::for_workspace(&cwd);
    knowledge_collector::ensure_system_space(&store, &request.source_version)?;
    let scope = active_system_scope(&store)?.context("Linux 知识库初始化后缺少当前生效代际")?;
    collected.document.generation = scope.generation;
    let summary = knowledge_collector::ingest_collected_man_page(
        &store,
        &collected,
        &yunxi_agent_storage::KnowledgeChunkingOptions::default(),
    )?;
    result["content_hash"] = serde_json::Value::String(summary.content_hash);
    result["chunks_written"] = serde_json::json!(summary.chunks_written);
    result["chunks_removed"] = serde_json::json!(summary.chunks_removed);
    result["embedding_job"] = enqueue_collected_embedding(&store, &collected.document.document_id)?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

fn active_system_scope(
    store: &yunxi_agent_storage::SqliteKnowledgeStore,
) -> Result<Option<yunxi_agent_storage::KnowledgeSearchScope>> {
    active_knowledge_scope(
        store,
        "system-linux",
        "system",
        yunxi_agent_storage::KnowledgeVisibility::Public,
    )
}

fn active_knowledge_scope(
    store: &yunxi_agent_storage::SqliteKnowledgeStore,
    space_id: &str,
    owner: &str,
    visibility: yunxi_agent_storage::KnowledgeVisibility,
) -> Result<Option<yunxi_agent_storage::KnowledgeSearchScope>> {
    store
        .active_space_scope(space_id, owner, visibility)
        .context("读取知识空间当前生效代际失败")
}

fn enqueue_collected_embedding(
    store: &yunxi_agent_storage::SqliteKnowledgeStore,
    document_id: &str,
) -> Result<serde_json::Value> {
    let job = store.enqueue_current_document_embedding_job(
        document_id,
        yunxi_agent_persona::LOCAL_MEMORY_EMBEDDING_MODEL,
    )?;
    Ok(serde_json::json!({
        "status": job.status.as_str(),
        "job_id": job.job_id,
        "embedding_model": job.embedding_model,
        "generation": job.generation,
        "attempts": job.attempts,
    }))
}

fn resolve_source_version(requested: &str) -> String {
    if requested != "auto" {
        return requested.to_string();
    }
    #[cfg(target_os = "linux")]
    {
        return yunxi_agent_runtime::detect_linux_source_version()
            .unwrap_or_else(|| "unknown".to_string());
    }
    #[cfg(not(target_os = "linux"))]
    {
        "unknown".to_string()
    }
}

const SYSTEMD_UNIT: &str = include_str!("../packaging/systemd/yunxi-linux.service");

fn read_shell_input(stdin: bool) -> Result<String> {
    if stdin {
        let mut input = String::new();
        io::stdin()
            .read_to_string(&mut input)
            .context("读取 shell 标准输入失败")?;
        return Ok(input.trim_end_matches(['\r', '\n']).to_string());
    }
    bail!("shell 命令必须使用 --stdin；这样可以避免参数重新解析和命令替换")
}

fn install_fish_hook(print: bool, _takeover: bool) -> Result<()> {
    let binary = std::env::var_os("YUNXI_LINUX_BINARY")
        .map(PathBuf::from)
        .or_else(|| std::env::current_exe().ok())
        .context("无法定位 yunxi-linux 可执行文件")?;
    let hook = fish_hook(&binary);
    if print {
        print!("{hook}");
        return Ok(());
    }

    let path = fish_hook_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("创建 fish 配置目录失败: {}", parent.display()))?;
    }
    if path.exists() {
        let existing = fs::read_to_string(&path).unwrap_or_default();
        if !existing.contains(HOOK_MARKER) {
            bail!(
                "拒绝覆盖非 YunXi 文件: {}；请先备份/移动它，或手动合并 fish hook",
                path.display()
            );
        }
    }
    fs::write(&path, hook).with_context(|| format!("写入 fish hook 失败: {}", path.display()))?;
    println!("已安装 fish hook: {}", path.display());
    println!("重启 fish，或执行: source {}", path.display());
    Ok(())
}

fn remove_fish_hook() -> Result<()> {
    let path = fish_hook_path()?;
    if !path.exists() {
        println!("fish hook 不存在: {}", path.display());
        return Ok(());
    }
    let existing = fs::read_to_string(&path).unwrap_or_default();
    if !existing.contains(HOOK_MARKER) {
        bail!("{} 不是 YunXi 生成的 hook，未删除", path.display());
    }
    fs::remove_file(&path).with_context(|| format!("删除 fish hook 失败: {}", path.display()))?;
    println!("已删除 fish hook: {}", path.display());
    Ok(())
}

fn fish_hook_path() -> Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("Linux fish 集成需要 HOME")?;
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));
    Ok(config.join("fish").join("conf.d").join("yunxi.fish"))
}

fn fish_quote_path(path: &Path) -> String {
    let value = path.to_string_lossy();
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('$', "\\$")
        .replace('`', "\\`")
}

fn fish_hook(binary: &Path) -> String {
    let binary = fish_quote_path(binary);
    format!(
        r#"{HOOK_MARKER}
# 由 `yunxi-linux fish-init` 生成。
# YunXi 接管每个非空提交；fish 只保留编辑、历史、补全和空输入。
set -g __yunxi_binary "{binary}"

function __yunxi_hand_to_ai
    set -g __yunxi_pending_buffer "$argv[1]"
    commandline -b -- ""
    commandline -f execute
end

function __yunxi_execute_or_continue
    commandline --is-valid
    set -l valid_status $status
    if test $valid_status -eq 2
        commandline -i \n
        commandline -f repaint
    else
        commandline -f execute
    end
end

function __yunxi_insert_newline
    commandline -f expand-abbr
    commandline -i \n
end

function __yunxi_accept_line
    status is-interactive; or return
    commandline -f expand-abbr
    set -l buffer (commandline -b | string collect)
    set -l trimmed (string trim -- "$buffer")
    if test -z "$trimmed"
        __yunxi_execute_or_continue
        return
    end
    __yunxi_hand_to_ai "$buffer"
end

bind enter __yunxi_accept_line
bind \r __yunxi_accept_line
bind -M insert enter __yunxi_accept_line
bind -M insert \r __yunxi_accept_line
bind ctrl-j __yunxi_insert_newline
bind \cj __yunxi_insert_newline
bind -M insert ctrl-j __yunxi_insert_newline
bind -M insert \cj __yunxi_insert_newline

function __yunxi_on_prompt --on-event fish_prompt
    set -q __yunxi_pending_buffer; or return
    set -l buffer $__yunxi_pending_buffer
    set -e __yunxi_pending_buffer
    printf '\n'
    printf '%s' "$buffer" | "$__yunxi_binary" shell-intercept --shell fish --session-id "fish-"$fish_pid --cwd "$PWD" --stdin
end

"#
    )
}

/// Conservative classifier modelled on Miyu's first-token decision.
pub(crate) fn is_shell_command(input: &str, _shell: &str) -> bool {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return true;
    }
    let first_line = trimmed.lines().next().unwrap_or_default().trim_start();
    if first_line.starts_with('#') {
        return true;
    }
    let tokens = raw_tokens(trimmed);
    let Some(mut head) = tokens.into_iter().next() else {
        return true;
    };
    let mut index = 1usize;
    let all_tokens = raw_tokens(trimmed);
    while is_assignment(&head) {
        if index >= all_tokens.len() {
            return true;
        }
        head = all_tokens[index].clone();
        index += 1;
    }
    if head.is_empty() || has_shell_syntax(&head) {
        return false;
    }
    let ambiguous = [
        "time", "test", "date", "which", "type", "command", "history",
    ];
    if ambiguous.contains(&head.as_str())
        && trimmed
            .chars()
            .any(|c| c == '?' || ('\u{4e00}'..='\u{9fff}').contains(&c))
    {
        return false;
    }
    if fish_builtin(&head) || executable_token(&head) {
        return true;
    }
    false
}

fn raw_tokens(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;
    for ch in input.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        if ch == '\\' && quote != Some('\'') {
            escaped = true;
            current.push(ch);
            continue;
        }
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            } else {
                current.push(ch);
            }
        } else if ch == '\'' || ch == '"' {
            quote = Some(ch);
        } else if ch.is_whitespace() {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
        } else {
            current.push(ch);
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

fn is_assignment(token: &str) -> bool {
    let Some((name, _)) = token.split_once('=') else {
        return false;
    };
    !name.is_empty()
        && name.chars().enumerate().all(|(i, c)| {
            c == '_' || c.is_ascii_alphanumeric() && (i > 0 || c.is_ascii_alphabetic())
        })
}

fn has_shell_syntax(token: &str) -> bool {
    token.chars().any(|ch| {
        matches!(
            ch,
            '$' | '('
                | ')'
                | '~'
                | '{'
                | '}'
                | '%'
                | ';'
                | '&'
                | '|'
                | '<'
                | '>'
                | '#'
                | '^'
                | '!'
                | '\\'
        )
    })
}

fn fish_builtin(token: &str) -> bool {
    matches!(
        token,
        "alias"
            | "and"
            | "argparse"
            | "abbr"
            | "begin"
            | "bind"
            | "break"
            | "builtin"
            | "case"
            | "cd"
            | "command"
            | "commandline"
            | "contains"
            | "continue"
            | "dirh"
            | "dirs"
            | "disown"
            | "echo"
            | "else"
            | "end"
            | "eval"
            | "exec"
            | "exit"
            | "false"
            | "fg"
            | "fish"
            | "for"
            | "function"
            | "functions"
            | "history"
            | "if"
            | "jobs"
            | "kill"
            | "math"
            | "not"
            | "printf"
            | "pwd"
            | "random"
            | "read"
            | "realpath"
            | "return"
            | "set"
            | "source"
            | "status"
            | "string"
            | "suspend"
            | "switch"
            | "test"
            | "time"
            | "true"
            | "type"
            | "ulimit"
            | "umask"
            | "vared"
            | "wait"
            | "while"
    )
}

fn executable_token(token: &str) -> bool {
    let path = Path::new(token);
    if path.components().count() > 1 {
        return is_executable(path);
    }
    let Some(path_var) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path_var).any(|dir| is_executable(&dir.join(token)))
}

fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        return metadata.permissions().mode() & 0o111 != 0;
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(unix)]
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ClientFrame {
    Hello {
        protocol_version: u16,
        client: String,
        capabilities: Vec<String>,
    },
    Ping {
        request_id: Option<String>,
    },
    /// Return the durable lifecycle state of a run, including one recovered
    /// after a daemon restart.
    Status {
        run_id: String,
    },
    Turn {
        request_id: String,
        cwd: String,
        prompt: String,
        session_id: Option<String>,
        offline: bool,
        live: bool,
        provider: Option<String>,
        model: Option<String>,
        /// The legacy/default delivery is attached: disconnect cancels the run.
        #[serde(default)]
        delivery: DeliveryMode,
    },
    Cancel {
        request_id: String,
        /// Detached clients cancel by the run id returned in RunAccepted.
        /// Attached clients keep using request_id for compatibility.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<String>,
    },
    Follow {
        /// New clients identify the completed turn directly.
        #[serde(default)]
        run_id: Option<String>,
        /// Legacy clients may still send their old session id field. The
        /// shell-intercept command never sends Follow, so this keeps the
        /// command-line path source-compatible while the replay contract
        /// moves to run ids.
        #[serde(default)]
        session_id: Option<String>,
        #[serde(default)]
        after_seq: u64,
    },
    ApprovalResponse {
        id: Option<String>,
        approved: bool,
        reason: Option<String>,
    },
    UserInputResponse {
        id: Option<String>,
        value: Option<String>,
    },
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum DeliveryMode {
    #[default]
    Attached,
    DetachedOutputOnly,
}

#[cfg(unix)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ServerFrame {
    HelloAck {
        protocol_version: u16,
        max_frame_bytes: usize,
        capabilities: Vec<String>,
    },
    Pong {
        request_id: Option<String>,
    },
    RunAccepted {
        run_id: String,
    },
    CancelAccepted {
        run_id: String,
    },
    RunStatus {
        run_id: String,
        status: RunStatus,
        next_seq: u64,
        recoverable: bool,
        created_at_unix_secs: u64,
    },
    /// Numbered output event. The wrapper lets a reconnecting client persist
    /// the exact cursor it has consumed without guessing from payloads.
    Event {
        run_id: String,
        seq: u64,
        frame: Box<ServerFrame>,
    },
    Thread {
        thread_id: String,
    },
    Message {
        content: String,
    },
    Approval {
        id: Option<String>,
        tool_name: String,
        reason: String,
        command: Option<String>,
        cwd: String,
    },
    UserInput {
        id: Option<String>,
        prompt: String,
    },
    Done {
        status: String,
    },
    ResyncRequired {
        run_id: String,
        reason: String,
    },
    Error {
        message: String,
    },
}

#[cfg(unix)]
const REPLAY_MAX_RUNS: usize = 128;

#[cfg(unix)]
const REPLAY_MAX_ACTIVE_RUNS: usize = 16;

#[cfg(unix)]
const REPLAY_MAX_EVENTS_PER_RUN: usize = 64;

#[cfg(unix)]
const REPLAY_MAX_BYTES_PER_RUN: usize = 2 * 1024 * 1024;
#[cfg(unix)]
const REPLAY_LIVE_CHANNEL_CAPACITY: usize = 128;
#[cfg(unix)]
const REPLAY_STATE_VERSION: u8 = 1;
#[cfg(unix)]
const REPLAY_STATE_MAX_BYTES: u64 = 8 * 1024 * 1024;

#[cfg(unix)]
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RunStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

#[cfg(unix)]
impl RunStatus {
    fn recoverable(self) -> bool {
        matches!(self, Self::Interrupted)
    }
}

/// An event retained for a completed run. Only user-visible output is kept;
/// approvals and user-input prompts are intentionally excluded because they
/// are tied to an active connection and cannot be safely replayed later.
#[cfg(unix)]
#[derive(Debug, Clone)]
struct ReplayEvent {
    seq: u64,
    bytes: usize,
    frame: ServerFrame,
}

#[cfg(unix)]
#[derive(Debug)]
struct ReplayRun {
    next_seq: u64,
    events: VecDeque<ReplayEvent>,
    event_bytes: usize,
    complete: bool,
    live_tx: broadcast::Sender<ReplayEvent>,
    active_follow_allowed: bool,
    status: RunStatus,
    request_id: Option<String>,
    cwd: Option<String>,
    session_id: Option<String>,
    created_at_unix_secs: u64,
}

#[cfg(unix)]
#[derive(Debug, Serialize, Deserialize)]
struct PersistedReplayRun {
    version: u8,
    run_id: String,
    status: RunStatus,
    next_seq: u64,
    active_follow_allowed: bool,
    request_id: Option<String>,
    cwd: Option<String>,
    session_id: Option<String>,
    created_at_unix_secs: u64,
    events: Vec<PersistedReplayEvent>,
}

#[cfg(unix)]
#[derive(Debug, Serialize, Deserialize)]
struct PersistedReplayEvent {
    seq: u64,
    frame: ServerFrame,
}

#[cfg(unix)]
#[derive(Debug, Clone)]
struct ReplayPersistence {
    directory: PathBuf,
}

#[cfg(unix)]
impl ReplayPersistence {
    fn new(directory: PathBuf) -> Result<Self> {
        fs::create_dir_all(&directory).with_context(|| {
            format!(
                "创建 YunXi daemon run state 目录失败: {}",
                directory.display()
            )
        })?;
        restrict_mode(&directory, 0o700)?;
        Ok(Self { directory })
    }

    fn path_for(&self, run_id: &str) -> PathBuf {
        self.directory
            .join(format!("{:016x}.json", stable_run_hash(run_id)))
    }

    fn write(&self, run_id: &str, run: &ReplayRun) -> Result<()> {
        let record = PersistedReplayRun {
            version: REPLAY_STATE_VERSION,
            run_id: run_id.to_string(),
            status: run.status,
            next_seq: run.next_seq,
            active_follow_allowed: run.active_follow_allowed,
            request_id: run.request_id.clone(),
            cwd: run.cwd.clone(),
            session_id: run.session_id.clone(),
            created_at_unix_secs: run.created_at_unix_secs,
            events: run
                .events
                .iter()
                .map(|event| PersistedReplayEvent {
                    seq: event.seq,
                    frame: event.frame.clone(),
                })
                .collect(),
        };
        let payload = serde_json::to_vec(&record).context("序列化 YunXi daemon run state 失败")?;
        if payload.len() as u64 > REPLAY_STATE_MAX_BYTES {
            bail!("YunXi daemon run state 超过大小上限")
        }
        let path = self.path_for(run_id);
        let temp = path.with_extension(format!("{}.tmp", std::process::id()));
        fs::write(&temp, payload)
            .with_context(|| format!("写入 run state 失败: {}", temp.display()))?;
        restrict_mode(&temp, 0o600)?;
        fs::rename(&temp, &path)
            .with_context(|| format!("提交 run state 失败: {}", path.display()))?;
        Ok(())
    }

    fn remove(&self, run_id: &str) {
        let _ = fs::remove_file(self.path_for(run_id));
    }

    fn load(&self) -> Result<Vec<PersistedReplayRun>> {
        let mut entries = fs::read_dir(&self.directory)
            .with_context(|| format!("读取 run state 目录失败: {}", self.directory.display()))?
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry
                    .path()
                    .extension()
                    .is_some_and(|value| value == "json")
            })
            .collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.file_name());
        let mut records = Vec::new();
        for entry in entries {
            let path = entry.path();
            let Ok(metadata) = fs::metadata(&path) else {
                continue;
            };
            if metadata.len() > REPLAY_STATE_MAX_BYTES {
                continue;
            }
            let Ok(payload) = fs::read(&path) else {
                continue;
            };
            let Ok(record) = serde_json::from_slice::<PersistedReplayRun>(&payload) else {
                continue;
            };
            if record.version == REPLAY_STATE_VERSION && !record.run_id.is_empty() {
                records.push(record);
            }
        }
        Ok(records)
    }
}

#[cfg(unix)]
fn stable_run_hash(value: &str) -> u64 {
    value
        .bytes()
        .fold(0xcbf29ce484222325, |hash, byte| hash ^ u64::from(byte))
        .wrapping_mul(0x100000001b3)
}

#[cfg(unix)]
#[derive(Debug)]
enum ReplayLookup {
    Unknown,
    Active {
        events: Vec<ReplayEvent>,
        receiver: broadcast::Receiver<ReplayEvent>,
    },
    ActiveUnavailable,
    Stale,
    Events(Vec<ReplayEvent>),
}

#[cfg(unix)]
#[derive(Debug)]
struct ReplayStore {
    runs: HashMap<String, ReplayRun>,
    completed_order: VecDeque<String>,
    max_runs: usize,
    max_active_runs: usize,
    max_events_per_run: usize,
    max_bytes_per_run: usize,
    persistence: Option<ReplayPersistence>,
}

#[cfg(unix)]
impl Default for ReplayStore {
    fn default() -> Self {
        Self {
            runs: HashMap::new(),
            completed_order: VecDeque::new(),
            max_runs: REPLAY_MAX_RUNS,
            max_active_runs: REPLAY_MAX_ACTIVE_RUNS,
            max_events_per_run: REPLAY_MAX_EVENTS_PER_RUN,
            max_bytes_per_run: REPLAY_MAX_BYTES_PER_RUN,
            persistence: None,
        }
    }
}

#[cfg(unix)]
impl ReplayStore {
    fn begin(&mut self, run_id: &str) -> bool {
        self.begin_with_delivery(run_id, false)
    }

    fn begin_with_delivery(&mut self, run_id: &str, active_follow_allowed: bool) -> bool {
        self.begin_with_metadata(run_id, active_follow_allowed, None, None, None)
    }

    fn begin_with_metadata(
        &mut self,
        run_id: &str,
        active_follow_allowed: bool,
        request_id: Option<String>,
        cwd: Option<String>,
        session_id: Option<String>,
    ) -> bool {
        if self.runs.values().filter(|run| !run.complete).count() >= self.max_active_runs {
            return false;
        }
        let created_at_unix_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let (live_tx, _) = broadcast::channel(REPLAY_LIVE_CHANNEL_CAPACITY);
        let run = ReplayRun {
            next_seq: 0,
            events: VecDeque::new(),
            event_bytes: 0,
            complete: false,
            live_tx,
            active_follow_allowed,
            status: RunStatus::Running,
            request_id,
            cwd,
            session_id,
            created_at_unix_secs,
        };
        self.runs.insert(run_id.to_string(), run);
        let _ = self.persist_run(run_id);
        true
    }

    fn discard(&mut self, run_id: &str) {
        self.runs.remove(run_id);
        self.completed_order.retain(|id| id != run_id);
        if let Some(persistence) = &self.persistence {
            persistence.remove(run_id);
        }
    }

    fn record(&mut self, run_id: &str, frame: &ServerFrame) -> Option<u64> {
        let run = self.runs.get_mut(run_id)?;
        if run.complete || !is_replayable_frame(frame) {
            return None;
        }
        let bytes = serde_json::to_vec(frame).ok()?.len();
        if self.max_events_per_run == 0 {
            return None;
        }
        run.next_seq = run.next_seq.saturating_add(1);
        let seq = run.next_seq;
        // Keep an oversized event as the sole ring entry. Dropping it and
        // sending an unnumbered frame would make completed-run replay
        // silently lossy.
        while run.events.len() >= self.max_events_per_run
            || (bytes <= self.max_bytes_per_run
                && run.event_bytes.saturating_add(bytes) > self.max_bytes_per_run)
        {
            let Some(evicted) = run.events.pop_front() else {
                break;
            };
            run.event_bytes = run.event_bytes.saturating_sub(evicted.bytes);
        }
        let event = ReplayEvent {
            seq,
            bytes,
            frame: frame.clone(),
        };
        run.events.push_back(event.clone());
        run.event_bytes = run.event_bytes.saturating_add(bytes);
        if let ServerFrame::Done { status } = frame {
            run.status = match status.as_str() {
                "completed" => RunStatus::Completed,
                "cancelled" => RunStatus::Cancelled,
                "interrupted" => RunStatus::Interrupted,
                _ => RunStatus::Failed,
            };
        }
        let _ = run.live_tx.send(event);
        let _ = self.persist_run(run_id);
        Some(seq)
    }

    fn finish(&mut self, run_id: &str) {
        let Some(run) = self.runs.get_mut(run_id) else {
            return;
        };
        run.complete = true;
        if run.status == RunStatus::Running {
            run.status = RunStatus::Completed;
        }
        self.completed_order.push_back(run_id.to_string());
        while self.completed_order.len() > self.max_runs {
            if let Some(evicted) = self.completed_order.pop_front() {
                self.runs.remove(&evicted);
            }
        }
        let _ = self.persist_run(run_id);
    }

    fn status(&self, run_id: &str) -> Option<(RunStatus, u64, bool, u64)> {
        self.runs.get(run_id).map(|run| {
            (
                run.status,
                run.next_seq,
                run.status.recoverable(),
                run.created_at_unix_secs,
            )
        })
    }

    fn persist_run(&self, run_id: &str) -> Result<()> {
        let Some(persistence) = &self.persistence else {
            return Ok(());
        };
        let Some(run) = self.runs.get(run_id) else {
            return Ok(());
        };
        persistence.write(run_id, run)
    }

    fn with_persistence(persistence: ReplayPersistence) -> Result<Self> {
        let mut store = Self {
            persistence: Some(persistence.clone()),
            ..Self::default()
        };
        for mut record in persistence.load()? {
            let (live_tx, _) = broadcast::channel(REPLAY_LIVE_CHANNEL_CAPACITY);
            let mut events = VecDeque::new();
            let mut event_bytes = 0usize;
            for event in record.events.drain(..) {
                if !is_replayable_frame(&event.frame) {
                    continue;
                }
                let bytes = serde_json::to_vec(&event.frame)
                    .map(|value| value.len())
                    .unwrap_or(0);
                events.push_back(ReplayEvent {
                    seq: event.seq,
                    bytes,
                    frame: event.frame,
                });
                event_bytes = event_bytes.saturating_add(bytes);
            }
            let was_running = record.status == RunStatus::Running;
            if was_running {
                record.status = RunStatus::Interrupted;
                record.active_follow_allowed = false;
                let done = ServerFrame::Done {
                    status: "interrupted".to_string(),
                };
                let seq = record.next_seq.saturating_add(1);
                let bytes = serde_json::to_vec(&done)
                    .map(|value| value.len())
                    .unwrap_or(0);
                events.push_back(ReplayEvent {
                    seq,
                    bytes,
                    frame: done,
                });
                event_bytes = event_bytes.saturating_add(bytes);
                record.next_seq = seq;
            }
            let run = ReplayRun {
                next_seq: record.next_seq,
                events,
                event_bytes,
                complete: true,
                live_tx,
                active_follow_allowed: false,
                status: record.status,
                request_id: record.request_id,
                cwd: record.cwd,
                session_id: record.session_id,
                created_at_unix_secs: record.created_at_unix_secs,
            };
            let run_id = record.run_id;
            store.runs.insert(run_id.clone(), run);
            store.completed_order.push_back(run_id.clone());
            if was_running {
                let _ = store.persist_run(&run_id);
            }
        }
        while store.completed_order.len() > store.max_runs {
            if let Some(evicted) = store.completed_order.pop_front() {
                store.runs.remove(&evicted);
                persistence.remove(&evicted);
            }
        }
        Ok(store)
    }

    fn lookup(&self, run_id: &str, after_seq: u64) -> ReplayLookup {
        let Some(run) = self.runs.get(run_id) else {
            return ReplayLookup::Unknown;
        };
        if let Some(oldest) = run.events.front().map(|event| event.seq)
            && after_seq < oldest.saturating_sub(1)
        {
            return ReplayLookup::Stale;
        }
        if !run.complete && !run.active_follow_allowed {
            return ReplayLookup::ActiveUnavailable;
        }
        if !run.complete {
            return ReplayLookup::Active {
                events: run.events.iter().cloned().collect(),
                receiver: run.live_tx.subscribe(),
            };
        }
        let Some(oldest) = run.events.front().map(|event| event.seq) else {
            return ReplayLookup::Events(Vec::new());
        };
        if after_seq < oldest.saturating_sub(1) {
            return ReplayLookup::Stale;
        }
        ReplayLookup::Events(
            run.events
                .iter()
                .filter(|event| event.seq > after_seq)
                .cloned()
                .collect(),
        )
    }
}

#[cfg(unix)]
struct ReplayRunGuard {
    replays: Arc<Mutex<ReplayStore>>,
    run_id: String,
}

#[cfg(unix)]
impl ReplayRunGuard {
    fn new(replays: Arc<Mutex<ReplayStore>>, run_id: String) -> Self {
        Self { replays, run_id }
    }

    async fn finish(self) {
        self.replays.lock().await.finish(&self.run_id);
    }

    async fn discard(self) {
        self.replays.lock().await.discard(&self.run_id);
    }
}

#[cfg(unix)]
static REQUEST_ID_COUNTER: AtomicU64 = AtomicU64::new(0);

#[cfg(unix)]
fn new_request_id() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let counter = REQUEST_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{}-{timestamp}-{counter}", std::process::id())
}

#[cfg(unix)]
fn is_replayable_frame(frame: &ServerFrame) -> bool {
    matches!(
        frame,
        ServerFrame::Thread { .. } | ServerFrame::Message { .. } | ServerFrame::Done { .. }
    )
}

#[cfg(unix)]
async fn write_frame<W: AsyncWrite + Unpin>(writer: &mut W, frame: &ServerFrame) -> Result<()> {
    let payload = encode_json_frame(frame).context("编码 YunXi shell IPC 消息失败")?;
    writer.write_all(&payload).await?;
    writer.flush().await?;
    Ok(())
}

#[cfg(unix)]
async fn send_client_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    frame: &ClientFrame,
) -> Result<()> {
    let payload = encode_json_frame(frame).context("编码 YunXi shell 请求失败")?;
    writer.write_all(&payload).await?;
    writer.flush().await?;
    Ok(())
}

#[cfg(unix)]
async fn read_frame<R: AsyncRead + Unpin, T: serde::de::DeserializeOwned>(
    reader: &mut R,
) -> Result<Option<T>> {
    let mut length_bytes = [0u8; 4];
    match reader.read_exact(&mut length_bytes).await {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    let length = validate_frame_length(u32::from_be_bytes(length_bytes))
        .context("YunXi shell IPC frame 长度无效")?;
    if length > LINUX_IPC_MAX_FRAME_BYTES {
        bail!(
            "YunXi shell IPC frame 超过 {} 字节上限",
            LINUX_IPC_MAX_FRAME_BYTES
        );
    }
    let mut payload = vec![0u8; length];
    reader.read_exact(&mut payload).await?;
    Ok(Some(
        decode_json_frame(&payload).context("解析 YunXi shell IPC JSON frame 失败")?,
    ))
}

#[cfg(unix)]
fn spawn_server_reader<R>(
    mut reader: R,
) -> (mpsc::Receiver<Result<Option<ServerFrame>>>, ReaderTaskGuard)
where
    R: AsyncRead + Unpin + Send + 'static,
{
    let (tx, rx) = mpsc::channel(16);
    let task = tokio::spawn(async move {
        loop {
            let frame = read_frame::<_, ServerFrame>(&mut reader).await;
            let end = matches!(&frame, Ok(None) | Err(_));
            if tx.send(frame).await.is_err() || end {
                break;
            }
        }
    });
    (rx, ReaderTaskGuard(task))
}

#[cfg(unix)]
async fn recv_server_frame(
    frames: &mut mpsc::Receiver<Result<Option<ServerFrame>>>,
) -> Result<Option<ServerFrame>> {
    frames
        .recv()
        .await
        .context("YunXi shell daemon reader task unexpectedly stopped")?
}

#[cfg(unix)]
async fn read_frame_with_timeout<R: AsyncRead + Unpin, T: serde::de::DeserializeOwned>(
    reader: &mut R,
    timeout: Duration,
    phase: &str,
) -> Result<Option<T>> {
    tokio::time::timeout(timeout, read_frame(reader))
        .await
        .with_context(|| format!("YunXi shell IPC {phase}超时"))?
}

#[cfg(unix)]
fn socket_path() -> Result<PathBuf> {
    let base = if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") {
        PathBuf::from(runtime).join("yunxi")
    } else {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| home.map(|p| p.join(".local").join("state")))
            .unwrap_or_else(|| PathBuf::from(".yunxi-state"))
            .join("yunxi")
            .join("run")
    };
    fs::create_dir_all(&base)?;
    restrict_mode(&base, 0o700)?;
    Ok(base.join("yunxi.sock"))
}

#[cfg(unix)]
fn daemon_lock_path(socket: &Path) -> PathBuf {
    socket.with_extension("lock")
}

#[cfg(unix)]
fn daemon_run_state_path() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| home.map(|path| path.join(".local").join("state")))
        .unwrap_or_else(|| PathBuf::from(".yunxi-state"));
    Ok(base.join("yunxi").join("runs"))
}

#[cfg(unix)]
struct DaemonLock {
    path: PathBuf,
    _file: fs::File,
}

#[cfg(unix)]
#[derive(Debug, Serialize, Deserialize)]
struct DaemonLockMetadata {
    pid: u32,
    start_time_ticks: Option<u64>,
}

#[cfg(unix)]
static DAEMON_LOCK_TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[cfg(unix)]
impl Drop for DaemonLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(unix)]
fn acquire_daemon_lock(path: &Path) -> Result<DaemonLock> {
    for _ in 0..2 {
        match try_create_daemon_lock(path) {
            Ok(lock) => return Ok(lock),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let value = fs::read_to_string(path).with_context(|| {
                    format!("读取 YunXi daemon lock metadata 失败: {}", path.display())
                })?;
                let (pid, start_time_ticks) = parse_daemon_lock_owner(&value).ok_or_else(|| {
                    anyhow::anyhow!(
                        "YunXi daemon lock metadata 无效，拒绝删除锁: {}",
                        path.display()
                    )
                })?;
                if process_alive(pid, start_time_ticks) {
                    bail!("YunXi shell daemon 已在运行（pid {pid}）");
                }
                fs::remove_file(path).with_context(|| {
                    format!("删除失效 YunXi daemon lock 失败: {}", path.display())
                })?;
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("创建 YunXi daemon lock 失败: {}", path.display()));
            }
        }
    }
    bail!("无法取得 YunXi shell daemon 单例锁: {}", path.display())
}

#[cfg(unix)]
fn try_create_daemon_lock(path: &Path) -> io::Result<DaemonLock> {
    let temp_path = daemon_lock_temp_path(path);
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)?;
        let metadata = DaemonLockMetadata {
            pid: std::process::id(),
            start_time_ticks: process_start_time(std::process::id()),
        };
        serde_json::to_writer(&mut file, &metadata).map_err(|error| {
            io::Error::other(format!("写入 daemon lock metadata 失败: {error}"))
        })?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        restrict_mode(&temp_path, 0o600)
            .map_err(|error| io::Error::other(format!("设置 daemon lock 权限失败: {error}")))?;

        // hard_link creates the final name without replacing an existing lock.
        // This makes the fully-written metadata visible at the same moment as
        // ownership is claimed, while preserving create-new semantics.
        fs::hard_link(&temp_path, path)?;
        let _ = fs::remove_file(&temp_path);
        Ok(DaemonLock {
            path: path.to_path_buf(),
            _file: file,
        })
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

#[cfg(unix)]
fn daemon_lock_temp_path(path: &Path) -> PathBuf {
    let counter = DAEMON_LOCK_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut name = path
        .file_name()
        .unwrap_or_else(|| std::ffi::OsStr::new("yunxi-daemon.lock"))
        .to_os_string();
    name.push(format!(".{}.{}.tmp", std::process::id(), counter));
    path.with_file_name(name)
}

#[cfg(unix)]
fn parse_daemon_lock_owner(value: &str) -> Option<(u32, Option<u64>)> {
    serde_json::from_str::<DaemonLockMetadata>(value)
        .map(|metadata| (metadata.pid, metadata.start_time_ticks))
        .or_else(|_| value.trim().parse::<u32>().map(|pid| (pid, None)))
        .ok()
}

#[cfg(unix)]
fn process_alive(pid: u32, expected_start_time: Option<u64>) -> bool {
    #[cfg(target_os = "linux")]
    {
        if !Path::new("/proc").join(pid.to_string()).exists() {
            return false;
        }
        return expected_start_time
            .map(|expected| process_start_time(pid) == Some(expected))
            .unwrap_or(true);
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (pid, expected_start_time);
        true
    }
}

#[cfg(unix)]
fn process_start_time(pid: u32) -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let (_, fields) = stat.rsplit_once(')')?;
        // `/proc/<pid>/stat` starts numbering at 3 after the command name;
        // starttime is field 22, hence index 19 in the remaining sequence.
        return fields
            .split_whitespace()
            .nth(19)
            .and_then(|value| value.parse::<u64>().ok());
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        None
    }
}

#[cfg(unix)]
fn restrict_mode(path: &Path, mode: u32) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    }
    let _ = (path, mode);
    Ok(())
}

#[cfg(unix)]
async fn run_shell_intercept(
    shell: String,
    session_id: Option<String>,
    cwd: PathBuf,
    prompt: String,
    offline: bool,
    live: bool,
    provider: Option<String>,
    model: Option<String>,
) -> Result<()> {
    if shell != "fish" {
        bail!("当前仅实现 Miyu 风格 fish 接管，收到 shell={shell}");
    }
    let cwd =
        fs::canonicalize(&cwd).with_context(|| format!("无法访问工作区: {}", cwd.display()))?;
    let socket = ensure_daemon().await?;
    let stream = UnixStream::connect(&socket).await?;
    let (mut reader, mut writer) = stream.into_split();
    send_client_frame(
        &mut writer,
        &ClientFrame::Hello {
            protocol_version: LINUX_IPC_PROTOCOL_VERSION,
            client: "yunxi-fish".to_string(),
            capabilities: vec![
                "turn".to_string(),
                "cancel".to_string(),
                "follow".to_string(),
            ],
        },
    )
    .await?;
    let Some(ServerFrame::HelloAck {
        protocol_version,
        max_frame_bytes: _,
        capabilities: _,
    }) = read_frame_with_timeout(&mut reader, DAEMON_HANDSHAKE_TIMEOUT, "握手").await?
    else {
        bail!("YunXi shell daemon 在握手时断开连接");
    };
    if protocol_version != LINUX_IPC_PROTOCOL_VERSION {
        bail!(
            "YunXi shell IPC 版本不兼容：daemon={} client={}",
            protocol_version,
            LINUX_IPC_PROTOCOL_VERSION
        );
    }
    let request_id = new_request_id();
    send_client_frame(
        &mut writer,
        &ClientFrame::Turn {
            request_id: request_id.clone(),
            cwd: cwd.display().to_string(),
            prompt,
            session_id: session_id.or_else(|| std::env::var("YUNXI_SHELL_SESSION").ok()),
            offline,
            live,
            provider,
            model,
            delivery: DeliveryMode::Attached,
        },
    )
    .await?;
    // Keep frame decoding in a task that is never cancelled by the Ctrl+C
    // select below. Cancelling read_frame halfway through a length prefix or
    // payload would otherwise lose bytes already consumed from the Unix
    // stream and desynchronize the next frame.
    let (mut server_frames, _reader_guard) = spawn_server_reader(reader);
    let mut interrupt =
        signal(SignalKind::interrupt()).context("注册 fish shell SIGINT handler 失败")?;
    let mut cancel_sent = false;
    loop {
        let frame = if cancel_sent {
            recv_server_frame(&mut server_frames).await?
        } else {
            tokio::select! {
                _ = interrupt.recv() => {
                    send_client_frame(
                        &mut writer,
                        &ClientFrame::Cancel {
                            request_id: request_id.clone(),
                            run_id: None,
                        },
                    ).await?;
                    cancel_sent = true;
                    continue;
                }
                frame = recv_server_frame(&mut server_frames) => frame?,
            }
        };
        let Some(frame) = frame else {
            bail!("YunXi shell daemon 在回合完成前断开连接");
        };
        match frame {
            ServerFrame::Message { content } => {
                println!("{content}");
            }
            ServerFrame::Thread { thread_id } => {
                // The daemon owns continuity. Keep this visible for diagnostics without
                // forcing fish to mutate the user's environment.
                eprintln!("[yunxi session {thread_id}]");
            }
            ServerFrame::Approval {
                id,
                tool_name,
                reason,
                command,
                cwd,
            } => {
                eprintln!(
                    "\n[YunXi 请求审批] tool={tool_name} cwd={cwd}\n{reason}{}\n允许? [y/N] ",
                    command
                        .map(|value| format!("\ncommand: {value}"))
                        .unwrap_or_default()
                );
                match read_yes_no_or_cancel(&mut interrupt).await? {
                    ShellPromptResult::Value(approved) => {
                        send_client_frame(
                            &mut writer,
                            &ClientFrame::ApprovalResponse {
                                id,
                                approved,
                                reason: Some(
                                    if approved {
                                        "fish shell user approved"
                                    } else {
                                        "fish shell user denied"
                                    }
                                    .to_string(),
                                ),
                            },
                        )
                        .await?;
                    }
                    ShellPromptResult::Cancelled => {
                        send_client_frame(
                            &mut writer,
                            &ClientFrame::Cancel {
                                request_id: request_id.clone(),
                                run_id: None,
                            },
                        )
                        .await?;
                        cancel_sent = true;
                    }
                }
            }
            ServerFrame::UserInput { id, prompt } => {
                match read_terminal_line_or_cancel(
                    &format!("\n[YunXi 需要输入] {prompt}\n> "),
                    &mut interrupt,
                )
                .await?
                {
                    ShellPromptResult::Value(value) => {
                        send_client_frame(
                            &mut writer,
                            &ClientFrame::UserInputResponse {
                                id,
                                value: Some(value),
                            },
                        )
                        .await?;
                    }
                    ShellPromptResult::Cancelled => {
                        send_client_frame(
                            &mut writer,
                            &ClientFrame::Cancel {
                                request_id: request_id.clone(),
                                run_id: None,
                            },
                        )
                        .await?;
                        cancel_sent = true;
                    }
                }
            }
            ServerFrame::Done { status } => {
                if cancel_sent && status == "cancelled" {
                    return Ok(());
                }
                if status != "completed" {
                    bail!("yunxi {status}");
                }
                return Ok(());
            }
            ServerFrame::Event { frame, .. } => match *frame {
                ServerFrame::Message { content } => println!("{content}"),
                ServerFrame::Thread { thread_id } => {
                    eprintln!("[yunxi session {thread_id}]");
                }
                ServerFrame::Done { status } => {
                    if cancel_sent && status == "cancelled" {
                        return Ok(());
                    }
                    if status != "completed" {
                        bail!("yunxi {status}");
                    }
                    return Ok(());
                }
                _ => {}
            },
            ServerFrame::Error { message } => bail!("{message}"),
            ServerFrame::HelloAck { .. } | ServerFrame::Pong { .. } => {}
            ServerFrame::ResyncRequired { run_id, reason } => {
                eprintln!("[yunxi resync required: {run_id}] {reason}");
            }
            ServerFrame::RunStatus {
                run_id,
                status,
                next_seq,
                recoverable,
                ..
            } => {
                eprintln!(
                    "[yunxi run status: {run_id}] {status:?} seq={next_seq} recoverable={recoverable}"
                );
            }
            ServerFrame::RunAccepted { .. } | ServerFrame::CancelAccepted { .. } => {}
        }
    }
}

#[cfg(unix)]
async fn read_yes_no_or_cancel(interrupt: &mut Signal) -> Result<ShellPromptResult<bool>> {
    match read_terminal_line_or_cancel("", interrupt).await? {
        ShellPromptResult::Value(answer) => Ok(ShellPromptResult::Value(matches!(
            answer.trim().to_ascii_lowercase().as_str(),
            "y" | "yes"
        ))),
        ShellPromptResult::Cancelled => Ok(ShellPromptResult::Cancelled),
    }
}

#[cfg(unix)]
async fn read_terminal_line_or_cancel(
    prompt: &str,
    interrupt: &mut Signal,
) -> Result<ShellPromptResult<String>> {
    let cancelled = Arc::new(AtomicBool::new(false));
    let worker_cancelled = Arc::clone(&cancelled);
    let prompt = prompt.to_string();
    let worker =
        tokio::task::spawn_blocking(move || read_terminal_line_polling(&prompt, &worker_cancelled));
    tokio::pin!(worker);

    tokio::select! {
        result = &mut worker => {
            Ok(ShellPromptResult::Value(result.context("终端输入任务失败")??))
        }
        signal = interrupt.recv() => {
            signal.context("等待 fish shell 审批输入的 Ctrl+C 信号失败")?;
            cancelled.store(true, Ordering::Release);
            // The worker polls the tty in bounded intervals. Join it before
            // sending Cancel so no detached thread can consume the next fish
            // prompt's input.
            let _ = (&mut worker).await;
            Ok(ShellPromptResult::Cancelled)
        }
    }
}

#[cfg(unix)]
fn read_terminal_line_polling(prompt: &str, cancelled: &AtomicBool) -> Result<String> {
    use std::fs::OpenOptions;
    use std::os::fd::AsRawFd;

    if cancelled.load(Ordering::Acquire) {
        return Ok(String::new());
    }
    let mut tty = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .context("无法打开 /dev/tty；fish shell 交互需要可用控制终端")?;
    tty.write_all(prompt.as_bytes())?;
    tty.flush()?;

    let fd = tty.as_raw_fd();
    let mut line = Vec::new();
    loop {
        if cancelled.load(Ordering::Acquire) {
            return Ok(String::new());
        }
        let mut poll_fd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&mut poll_fd, 1, 50) };
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error.into());
        }
        if ready == 0 {
            continue;
        }
        if cancelled.load(Ordering::Acquire) {
            return Ok(String::new());
        }
        if poll_fd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            bail!("/dev/tty 在等待输入时不可用");
        }
        let mut byte = [0u8; 1];
        let read = tty.read(&mut byte)?;
        if read == 0 {
            break;
        }
        match byte[0] {
            b'\n' | b'\r' => break,
            0x03 => return Ok(String::new()),
            byte => line.push(byte),
        }
    }
    Ok(String::from_utf8_lossy(&line).trim_end().to_string())
}

#[cfg(not(unix))]
async fn run_shell_intercept(
    _shell: String,
    _session_id: Option<String>,
    _cwd: PathBuf,
    _prompt: String,
    _offline: bool,
    _live: bool,
    _provider: Option<String>,
    _model: Option<String>,
) -> Result<()> {
    bail!("fish 接管仅支持 Unix/Linux；Linux 构建不会在 Windows 上启用它")
}

async fn run_status(run_id: String) -> Result<()> {
    #[cfg(unix)]
    {
        let socket = ensure_daemon().await?;
        let stream = UnixStream::connect(&socket)
            .await
            .with_context(|| format!("连接 YunXi shell daemon 失败: {}", socket.display()))?;
        let (mut reader, mut writer) = stream.into_split();
        send_client_frame(
            &mut writer,
            &ClientFrame::Hello {
                protocol_version: LINUX_IPC_PROTOCOL_VERSION,
                client: "yunxi-run-status".to_string(),
                capabilities: vec!["run_status".to_string()],
            },
        )
        .await?;
        let Some(ServerFrame::HelloAck {
            protocol_version, ..
        }) = read_frame_with_timeout(&mut reader, DAEMON_HANDSHAKE_TIMEOUT, "状态查询握手").await?
        else {
            bail!("YunXi shell daemon 在状态查询握手时断开连接");
        };
        if protocol_version != LINUX_IPC_PROTOCOL_VERSION {
            bail!(
                "YunXi shell IPC 版本不兼容：daemon={} client={}",
                protocol_version,
                LINUX_IPC_PROTOCOL_VERSION
            );
        }
        send_client_frame(&mut writer, &ClientFrame::Status { run_id }).await?;
        let Some(frame) =
            read_frame_with_timeout(&mut reader, DAEMON_HANDSHAKE_TIMEOUT, "状态查询").await?
        else {
            bail!("YunXi shell daemon 在状态查询时断开连接");
        };
        match frame {
            ServerFrame::RunStatus {
                run_id,
                status,
                next_seq,
                recoverable,
                created_at_unix_secs,
            } => println!(
                "{}",
                format_run_status_json(
                    run_id,
                    status,
                    next_seq,
                    recoverable,
                    created_at_unix_secs,
                )?
            ),
            ServerFrame::ResyncRequired { run_id, reason } => {
                bail!("run {run_id} 不可用: {reason}");
            }
            ServerFrame::Error { message } => bail!("{message}"),
            other => bail!("YunXi shell daemon 返回了意外的状态响应: {other:?}"),
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = run_id;
        bail!("run-status 仅支持 Unix/Linux")
    }
}

/// Follow is intentionally a streaming CLI: it never accumulates the replay
/// ring in memory and writes one protocol frame as one JSON line. The daemon
/// closes the connection after the terminal `Done` event, so a close before
/// that event is an actionable failure rather than a successful empty stream.
async fn run_follow(run_id: String, after_seq: u64) -> Result<()> {
    #[cfg(unix)]
    {
        let socket = ensure_daemon().await?;
        let stream = UnixStream::connect(&socket)
            .await
            .with_context(|| format!("连接 YunXi shell daemon 失败: {}", socket.display()))?;
        let (mut reader, mut writer) = stream.into_split();
        send_client_frame(
            &mut writer,
            &ClientFrame::Hello {
                protocol_version: LINUX_IPC_PROTOCOL_VERSION,
                client: "yunxi-run-follow".to_string(),
                capabilities: vec!["follow".to_string()],
            },
        )
        .await?;
        let Some(ServerFrame::HelloAck {
            protocol_version, ..
        }) = read_frame_with_timeout(&mut reader, DAEMON_HANDSHAKE_TIMEOUT, "Follow 握手").await?
        else {
            bail!("YunXi shell daemon 在 Follow 握手时断开连接");
        };
        if protocol_version != LINUX_IPC_PROTOCOL_VERSION {
            bail!(
                "YunXi shell IPC 版本不兼容：daemon={} client={}",
                protocol_version,
                LINUX_IPC_PROTOCOL_VERSION
            );
        }
        send_client_frame(
            &mut writer,
            &ClientFrame::Follow {
                run_id: Some(run_id.clone()),
                session_id: None,
                after_seq,
            },
        )
        .await?;

        loop {
            let Some(frame) = read_frame::<_, ServerFrame>(&mut reader)
                .await
                .context("读取 YunXi run-follow frame 失败")?
            else {
                bail!("YunXi shell daemon 在 run {run_id} 的 Follow 完成前断开连接");
            };
            let terminal_status = match &frame {
                ServerFrame::Event {
                    run_id: event_run_id,
                    frame,
                    ..
                } => {
                    if event_run_id != &run_id {
                        bail!(
                            "YunXi shell daemon 返回了错误的 run_id：收到={} 请求={}",
                            event_run_id,
                            run_id
                        );
                    }
                    match frame.as_ref() {
                        ServerFrame::Done { status } => Some(status.clone()),
                        _ => None,
                    }
                }
                ServerFrame::ResyncRequired {
                    run_id: response_run_id,
                    reason,
                } => {
                    bail!("run {response_run_id} 不可 Follow: {reason}");
                }
                ServerFrame::Error { message } => bail!("{message}"),
                _ => None,
            };
            print_json_frame(&frame)?;
            if let Some(status) = terminal_status {
                if matches!(status.as_str(), "completed" | "interrupted" | "cancelled") {
                    return Ok(());
                }
                bail!("run {run_id} 以非成功状态结束: {status}");
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (run_id, after_seq);
        bail!("run-follow 仅支持 Unix/Linux")
    }
}

/// Send a detached cancellation request over a fresh authenticated IPC
/// connection. The daemon acknowledges only an active detached run; unknown
/// or already terminal ids are returned as a non-zero CLI error.
async fn run_cancel(run_id: String) -> Result<()> {
    #[cfg(unix)]
    {
        let socket = ensure_daemon().await?;
        let stream = UnixStream::connect(&socket)
            .await
            .with_context(|| format!("连接 YunXi shell daemon 失败: {}", socket.display()))?;
        let (mut reader, mut writer) = stream.into_split();
        send_client_frame(
            &mut writer,
            &ClientFrame::Hello {
                protocol_version: LINUX_IPC_PROTOCOL_VERSION,
                client: "yunxi-run-cancel".to_string(),
                capabilities: vec!["cancel".to_string()],
            },
        )
        .await?;
        let Some(ServerFrame::HelloAck {
            protocol_version, ..
        }) = read_frame_with_timeout(&mut reader, DAEMON_HANDSHAKE_TIMEOUT, "Cancel 握手").await?
        else {
            bail!("YunXi shell daemon 在 Cancel 握手时断开连接");
        };
        if protocol_version != LINUX_IPC_PROTOCOL_VERSION {
            bail!(
                "YunXi shell IPC 版本不兼容：daemon={} client={}",
                protocol_version,
                LINUX_IPC_PROTOCOL_VERSION
            );
        }
        send_client_frame(
            &mut writer,
            &ClientFrame::Cancel {
                request_id: new_request_id(),
                run_id: Some(run_id.clone()),
            },
        )
        .await?;
        let Some(frame) =
            read_frame_with_timeout(&mut reader, DAEMON_HANDSHAKE_TIMEOUT, "Cancel 响应").await?
        else {
            bail!("YunXi shell daemon 在 run {run_id} 的 Cancel 响应前断开连接");
        };
        match frame {
            ServerFrame::CancelAccepted {
                run_id: accepted_run_id,
            } => {
                if accepted_run_id != run_id {
                    bail!(
                        "YunXi shell daemon 返回了错误的 run_id：收到={} 请求={}",
                        accepted_run_id,
                        run_id
                    );
                }
                print_json_frame(&ServerFrame::CancelAccepted {
                    run_id: accepted_run_id,
                })?;
                Ok(())
            }
            ServerFrame::Error { message } => bail!("{message}"),
            other => bail!("YunXi shell daemon 返回了意外的 Cancel 响应: {other:?}"),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = run_id;
        bail!("run-cancel 仅支持 Unix/Linux")
    }
}

/// Start a daemon-owned detached run. The CLI deliberately stops after the
/// acceptance frame; callers that want output must reconnect with `run-follow`.
async fn run_detached(
    prompt: String,
    cwd: PathBuf,
    session_id: Option<String>,
    offline: bool,
    live: bool,
    provider: Option<String>,
    model: Option<String>,
) -> Result<()> {
    #[cfg(unix)]
    {
        let cwd =
            fs::canonicalize(&cwd).with_context(|| format!("无法访问工作区: {}", cwd.display()))?;
        let socket = ensure_daemon().await?;
        let stream = UnixStream::connect(&socket)
            .await
            .with_context(|| format!("连接 YunXi shell daemon 失败: {}", socket.display()))?;
        let (mut reader, mut writer) = stream.into_split();
        send_client_frame(
            &mut writer,
            &ClientFrame::Hello {
                protocol_version: LINUX_IPC_PROTOCOL_VERSION,
                client: "yunxi-run-detached".to_string(),
                capabilities: vec!["turn".to_string(), "detached_output_only".to_string()],
            },
        )
        .await?;
        let Some(ServerFrame::HelloAck {
            protocol_version, ..
        }) =
            read_frame_with_timeout(&mut reader, DAEMON_HANDSHAKE_TIMEOUT, "detached 握手").await?
        else {
            bail!("YunXi shell daemon 在 detached 握手时断开连接");
        };
        if protocol_version != LINUX_IPC_PROTOCOL_VERSION {
            bail!(
                "YunXi shell IPC 版本不兼容：daemon={} client={}",
                protocol_version,
                LINUX_IPC_PROTOCOL_VERSION
            );
        }
        send_client_frame(
            &mut writer,
            &ClientFrame::Turn {
                request_id: new_request_id(),
                cwd: cwd.display().to_string(),
                prompt,
                session_id,
                offline,
                live,
                provider,
                model,
                delivery: DeliveryMode::DetachedOutputOnly,
            },
        )
        .await?;
        let Some(frame) =
            read_frame_with_timeout(&mut reader, DAEMON_HANDSHAKE_TIMEOUT, "detached 接受响应")
                .await?
        else {
            bail!("YunXi shell daemon 在 detached 回合接受前断开连接");
        };
        match frame {
            accepted @ ServerFrame::RunAccepted { .. } => print_json_frame(&accepted),
            ServerFrame::Error { message } => bail!("{message}"),
            other => bail!("YunXi shell daemon 返回了意外的 detached 响应: {other:?}"),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (prompt, cwd, session_id, offline, live, provider, model);
        bail!("run-detached 仅支持 Unix/Linux")
    }
}

#[cfg(unix)]
fn print_json_frame(frame: &ServerFrame) -> Result<()> {
    println!(
        "{}",
        serde_json::to_string(frame).context("序列化 YunXi IPC frame 失败")?
    );
    io::stdout()
        .flush()
        .context("刷新 YunXi IPC frame 输出失败")?;
    Ok(())
}

#[cfg(unix)]
fn format_run_status_json(
    run_id: String,
    status: RunStatus,
    next_seq: u64,
    recoverable: bool,
    created_at_unix_secs: u64,
) -> Result<String> {
    serde_json::to_string(&serde_json::json!({
        "run_id": run_id,
        "status": status,
        "next_seq": next_seq,
        "recoverable": recoverable,
        "created_at_unix_secs": created_at_unix_secs,
    }))
    .context("序列化 YunXi run status 失败")
}

#[cfg(unix)]
async fn ensure_daemon() -> Result<PathBuf> {
    let socket = socket_path()?;
    if daemon_is_ready(&socket).await {
        return Ok(socket);
    }
    let binary = std::env::var_os("YUNXI_LINUX_BINARY")
        .map(PathBuf::from)
        .or_else(|| std::env::current_exe().ok())
        .context("无法定位 yunxi-linux 可执行文件")?;
    let _ = std::process::Command::new(binary)
        .arg("daemon")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    for _ in 0..40 {
        sleep(Duration::from_millis(50)).await;
        if daemon_is_ready(&socket).await {
            return Ok(socket);
        }
    }
    bail!("YunXi shell daemon 未能在 2 秒内启动；检查 XDG_RUNTIME_DIR 与用户权限")
}

#[cfg(unix)]
async fn daemon_is_ready(socket: &Path) -> bool {
    let Ok(stream) = UnixStream::connect(socket).await else {
        return false;
    };
    let (mut reader, mut writer) = stream.into_split();
    if send_client_frame(
        &mut writer,
        &ClientFrame::Hello {
            protocol_version: LINUX_IPC_PROTOCOL_VERSION,
            client: "yunxi-probe".to_string(),
            capabilities: vec!["ping".to_string()],
        },
    )
    .await
    .is_err()
    {
        return false;
    }
    let Ok(Some(ServerFrame::HelloAck {
        protocol_version, ..
    })) = read_frame_with_timeout(&mut reader, DAEMON_HANDSHAKE_TIMEOUT, "探测握手").await
    else {
        return false;
    };
    if protocol_version != LINUX_IPC_PROTOCOL_VERSION {
        return false;
    }
    if send_client_frame(
        &mut writer,
        &ClientFrame::Ping {
            request_id: Some(new_request_id()),
        },
    )
    .await
    .is_err()
    {
        return false;
    }
    matches!(
        read_frame_with_timeout(&mut reader, DAEMON_HANDSHAKE_TIMEOUT, "探测 Ping",).await,
        Ok(Some(ServerFrame::Pong { .. }))
    )
}

#[cfg(not(unix))]
async fn run_daemon(
    _knowledge_workspaces: Vec<PathBuf>,
    _knowledge_max_jobs: usize,
    _knowledge_interval_secs: u64,
    _knowledge_worker_id: Option<String>,
) -> Result<()> {
    bail!("YunXi shell daemon 仅支持 Unix/Linux")
}

#[cfg(unix)]
async fn run_daemon(
    knowledge_workspaces: Vec<PathBuf>,
    knowledge_max_jobs: usize,
    knowledge_interval_secs: u64,
    knowledge_worker_id: Option<String>,
) -> Result<()> {
    validate_worker_max_jobs(knowledge_max_jobs)?;
    if knowledge_interval_secs == 0 || knowledge_interval_secs > 3600 {
        bail!("--knowledge-interval-secs 必须在 1 到 3600 之间");
    }
    if knowledge_workspaces.len() > 32 {
        bail!("--knowledge-workspace 最多重复 32 次");
    }
    let knowledge_workspaces = knowledge_workspaces
        .into_iter()
        .map(|workspace| canonicalize_worker_workspace(&workspace))
        .collect::<Result<Vec<_>>>()?;
    let socket = socket_path()?;
    if daemon_is_ready(&socket).await {
        if !knowledge_workspaces.is_empty() {
            bail!("YunXi shell daemon 已在运行；不能向活动 daemon 附加 knowledge worker");
        }
        return Ok(());
    }
    let lock_path = daemon_lock_path(&socket);
    let _lock = acquire_daemon_lock(&lock_path)?;
    if daemon_is_ready(&socket).await {
        if !knowledge_workspaces.is_empty() {
            bail!("YunXi shell daemon 已在运行；不能向活动 daemon 附加 knowledge worker");
        }
        return Ok(());
    }
    if !knowledge_workspaces.is_empty() {
        knowledge_worker::validate_daemon_workspaces(
            &knowledge_workspaces,
            knowledge_worker_id.as_deref(),
        )?;
    }
    if UnixStream::connect(&socket).await.is_ok() {
        bail!("YunXi shell socket 已被不兼容的 daemon 占用；拒绝覆盖活动进程");
    }
    if socket.exists() {
        fs::remove_file(&socket)
            .with_context(|| format!("删除失效 YunXi socket 失败: {}", socket.display()))?;
    }
    let listener = UnixListener::bind(&socket)
        .with_context(|| format!("绑定 YunXi shell socket 失败: {}", socket.display()))?;
    restrict_mode(&socket, 0o600)?;
    let sessions = Arc::new(Mutex::new(HashMap::<String, String>::new()));
    let replay_state = ReplayPersistence::new(daemon_run_state_path()?)?;
    let replays = Arc::new(Mutex::new(ReplayStore::with_persistence(replay_state)?));
    let cancellations = Arc::new(Mutex::new(HashMap::<String, AgentRunControl>::new()));
    let mut terminate = signal(SignalKind::terminate()).context("注册 SIGTERM handler 失败")?;
    let mut interrupt = signal(SignalKind::interrupt()).context("注册 SIGINT handler 失败")?;
    let (knowledge_shutdown_tx, knowledge_shutdown_rx) = tokio::sync::watch::channel(false);
    let knowledge_worker_handle = (!knowledge_workspaces.is_empty()).then(|| {
        let worker_id = knowledge_worker_id
            .clone()
            .unwrap_or_else(|| knowledge_worker::DEFAULT_DAEMON_WORKER_ID.to_string());
        tokio::spawn(async move {
            let result = knowledge_worker::run_daemon_worker(
                Some(worker_id),
                knowledge_max_jobs,
                knowledge_interval_secs,
                knowledge_workspaces,
                knowledge_shutdown_rx,
            )
            .await;
            if result.is_err() {
                eprintln!("yunxi daemon knowledge worker stopped: knowledge_worker_failed");
            }
            result
        })
    });
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                let sessions = Arc::clone(&sessions);
                let replays = Arc::clone(&replays);
                let cancellations = Arc::clone(&cancellations);
                tokio::spawn(async move {
                    if let Err(error) =
                        handle_connection(stream, sessions, replays, cancellations).await
                    {
                        eprintln!("yunxi daemon connection error: {error:#}");
                    }
                });
            }
            _ = terminate.recv() => break,
            _ = interrupt.recv() => break,
        }
    }
    let _ = knowledge_shutdown_tx.send(true);
    if let Some(worker) = knowledge_worker_handle {
        worker.await.context("daemon 知识 worker 任务异常退出")??;
    }
    let _ = fs::remove_file(&socket);
    Ok(())
}

#[cfg(unix)]
struct ReaderTaskGuard(tokio::task::JoinHandle<()>);

#[cfg(unix)]
impl Drop for ReaderTaskGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[cfg(unix)]
async fn handle_connection(
    stream: UnixStream,
    sessions: Arc<Mutex<HashMap<String, String>>>,
    replays: Arc<Mutex<ReplayStore>>,
    cancellations: Arc<Mutex<HashMap<String, AgentRunControl>>>,
) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let (tx, mut rx) = mpsc::channel::<ClientFrame>(16);
    let reader_task = tokio::spawn(async move {
        let mut reader = reader;
        while let Ok(Some(frame)) = read_frame::<_, ClientFrame>(&mut reader).await {
            if tx.send(frame).await.is_err() {
                break;
            }
        }
    });
    let _reader_task = ReaderTaskGuard(reader_task);
    let Some(ClientFrame::Hello {
        protocol_version,
        client: _,
        capabilities: _,
    }) = tokio::time::timeout(DAEMON_HANDSHAKE_TIMEOUT, rx.recv())
        .await
        .context("YunXi shell daemon 握手超时")?
    else {
        return Ok(());
    };
    if protocol_version != LINUX_IPC_PROTOCOL_VERSION {
        write_frame(
            &mut writer,
            &ServerFrame::Error {
                message: format!(
                    "不支持的 YunXi shell IPC 版本 {protocol_version}，当前版本为 {LINUX_IPC_PROTOCOL_VERSION}"
                ),
            },
        )
        .await?;
        return Ok(());
    }
    write_frame(
        &mut writer,
        &ServerFrame::HelloAck {
            protocol_version: LINUX_IPC_PROTOCOL_VERSION,
            max_frame_bytes: LINUX_IPC_MAX_FRAME_BYTES,
            capabilities: vec![
                "ping".to_string(),
                "turn".to_string(),
                "cancel".to_string(),
                "follow_resync".to_string(),
                "follow_replay".to_string(),
                "follow_active".to_string(),
                "detached_output_only".to_string(),
                "detached_cancel".to_string(),
                "run_status".to_string(),
                "run_recovery".to_string(),
            ],
        },
    )
    .await?;
    let Some(request) = tokio::time::timeout(DAEMON_HANDSHAKE_TIMEOUT, rx.recv())
        .await
        .context("YunXi shell daemon 首个请求超时")?
    else {
        return Ok(());
    };
    match request {
        ClientFrame::Ping { request_id } => {
            write_frame(&mut writer, &ServerFrame::Pong { request_id }).await?;
        }
        ClientFrame::Status { run_id } => {
            let status = replays.lock().await.status(&run_id);
            if let Some((status, next_seq, recoverable, created_at_unix_secs)) = status {
                write_frame(
                    &mut writer,
                    &ServerFrame::RunStatus {
                        run_id,
                        status,
                        next_seq,
                        recoverable,
                        created_at_unix_secs,
                    },
                )
                .await?;
            } else {
                write_frame(
                    &mut writer,
                    &ServerFrame::ResyncRequired {
                        run_id,
                        reason: "未知或已被回收的 run_id".to_string(),
                    },
                )
                .await?;
            }
        }
        ClientFrame::Turn {
            request_id,
            cwd,
            prompt,
            session_id,
            offline,
            live,
            provider,
            model,
            delivery,
        } => {
            if let Err(error) = validate_turn_request(
                &request_id,
                &cwd,
                &prompt,
                session_id.as_deref(),
                provider.as_deref(),
                model.as_deref(),
            ) {
                write_frame(
                    &mut writer,
                    &ServerFrame::Error {
                        message: error.to_string(),
                    },
                )
                .await?;
                return Ok(());
            }
            match delivery {
                DeliveryMode::Attached => {
                    run_daemon_turn(
                        &mut writer,
                        &mut rx,
                        sessions,
                        replays,
                        request_id,
                        cwd,
                        prompt,
                        session_id,
                        offline,
                        live,
                        provider,
                        model,
                    )
                    .await?;
                }
                DeliveryMode::DetachedOutputOnly => {
                    let run_id = begin_detached_run(
                        &replays,
                        request_id.clone(),
                        cwd.clone(),
                        session_id.clone(),
                    )
                    .await?;
                    let (control, _stream) = AgentRunControl::streaming_output_only();
                    cancellations
                        .lock()
                        .await
                        .insert(run_id.clone(), control.clone());
                    if let Err(error) = write_frame(
                        &mut writer,
                        &ServerFrame::RunAccepted {
                            run_id: run_id.clone(),
                        },
                    )
                    .await
                    {
                        cancellations.lock().await.remove(&run_id);
                        replays.lock().await.discard(&run_id);
                        return Err(error);
                    }
                    let task_replays = Arc::clone(&replays);
                    let task_cancellations = Arc::clone(&cancellations);
                    tokio::spawn(async move {
                        run_detached_turn(
                            task_replays,
                            task_cancellations,
                            sessions,
                            run_id,
                            control,
                            request_id,
                            cwd,
                            prompt,
                            session_id,
                            offline,
                            live,
                            provider,
                            model,
                        )
                        .await;
                    });
                }
            }
        }
        ClientFrame::Follow {
            run_id,
            session_id,
            after_seq,
        } => {
            let Some(run_id) = run_id.or(session_id) else {
                write_frame(
                    &mut writer,
                    &ServerFrame::ResyncRequired {
                        run_id: "".to_string(),
                        reason: "Follow 缺少 run_id".to_string(),
                    },
                )
                .await?;
                return Ok(());
            };
            let lookup = replays.lock().await.lookup(&run_id, after_seq);
            match lookup {
                ReplayLookup::Events(events) => {
                    for event in events {
                        write_frame(
                            &mut writer,
                            &numbered_replay_frame(&run_id, event.seq, event.frame),
                        )
                        .await?;
                    }
                }
                ReplayLookup::Unknown => {
                    write_frame(
                        &mut writer,
                        &ServerFrame::ResyncRequired {
                            run_id,
                            reason: "未知或已被回收的 run_id".to_string(),
                        },
                    )
                    .await?;
                }
                ReplayLookup::Active { events, receiver } => {
                    follow_active_run(&mut writer, &run_id, after_seq, events, receiver, replays)
                        .await?;
                }
                ReplayLookup::ActiveUnavailable => {
                    write_frame(
                        &mut writer,
                        &ServerFrame::ResyncRequired {
                            run_id,
                            reason: "attached 回合仍在运行；断线续跑仅适用于 detached_output_only"
                                .to_string(),
                        },
                    )
                    .await?;
                }
                ReplayLookup::Stale => {
                    write_frame(
                        &mut writer,
                        &ServerFrame::ResyncRequired {
                            run_id,
                            reason: "请求的 after_seq 已超出回放 ring，必须重新同步".to_string(),
                        },
                    )
                    .await?;
                }
            }
        }
        ClientFrame::Cancel { request_id, run_id } => {
            let Some(run_id) = run_id else {
                write_frame(
                    &mut writer,
                    &ServerFrame::Error {
                        message: "detached Cancel 必须提供 run_id；attached 回合应在原连接中取消"
                            .to_string(),
                    },
                )
                .await?;
                return Ok(());
            };
            if cancel_detached_run(&cancellations, &run_id).await {
                write_frame(&mut writer, &ServerFrame::CancelAccepted { run_id }).await?;
            } else {
                write_frame(
                    &mut writer,
                    &ServerFrame::Error {
                        message: format!(
                            "未知或已结束的 detached run_id: {run_id} (request_id={request_id})"
                        ),
                    },
                )
                .await?;
            }
        }
        ClientFrame::Hello { .. }
        | ClientFrame::ApprovalResponse { .. }
        | ClientFrame::UserInputResponse { .. } => {
            write_frame(
                &mut writer,
                &ServerFrame::Error {
                    message: "请求顺序无效".to_string(),
                },
            )
            .await?;
        }
    }
    Ok(())
}

#[cfg(unix)]
async fn cancel_detached_run(
    cancellations: &Arc<Mutex<HashMap<String, AgentRunControl>>>,
    run_id: &str,
) -> bool {
    let control = cancellations.lock().await.get(run_id).cloned();
    if let Some(control) = control {
        control.cancel();
        true
    } else {
        false
    }
}

#[cfg(unix)]
fn validate_turn_request(
    request_id: &str,
    cwd: &str,
    prompt: &str,
    session_id: Option<&str>,
    provider: Option<&str>,
    model: Option<&str>,
) -> Result<()> {
    validate_turn_field("request_id", request_id, MAX_TURN_REQUEST_ID_BYTES)?;
    validate_turn_field("cwd", cwd, MAX_TURN_CWD_BYTES)?;
    validate_turn_field("prompt", prompt, MAX_TURN_PROMPT_BYTES)?;
    if let Some(value) = session_id {
        validate_turn_field("session_id", value, MAX_TURN_SESSION_ID_BYTES)?;
    }
    if let Some(value) = provider {
        validate_turn_field("provider", value, MAX_TURN_PROVIDER_BYTES)?;
    }
    if let Some(value) = model {
        validate_turn_field("model", value, MAX_TURN_MODEL_BYTES)?;
    }
    Ok(())
}

#[cfg(unix)]
fn validate_turn_field(name: &str, value: &str, max_bytes: usize) -> Result<()> {
    if value.len() > max_bytes {
        bail!("YunXi shell {name} 超过 {max_bytes} 字节上限")
    }
    Ok(())
}

#[cfg(unix)]
async fn run_daemon_turn<W: tokio::io::AsyncWrite + Unpin>(
    writer: &mut W,
    rx: &mut mpsc::Receiver<ClientFrame>,
    sessions: Arc<Mutex<HashMap<String, String>>>,
    replays: Arc<Mutex<ReplayStore>>,
    request_id: String,
    cwd: String,
    prompt: String,
    requested_session: Option<String>,
    offline: bool,
    live: bool,
    provider: Option<String>,
    model: Option<String>,
) -> Result<()> {
    let run_id = new_request_id();
    if !replays.lock().await.begin(&run_id) {
        bail!("YunXi shell daemon 当前活动回合过多，请稍后重试");
    }
    let guard = ReplayRunGuard::new(Arc::clone(&replays), run_id.clone());
    let result = run_daemon_turn_inner(
        writer,
        rx,
        sessions,
        Arc::clone(&replays),
        run_id,
        request_id,
        cwd,
        prompt,
        requested_session,
        offline,
        live,
        provider,
        model,
    )
    .await;
    match result {
        Ok(()) => {
            guard.finish().await;
            Ok(())
        }
        Err(error) => {
            let _ = write_frame(
                writer,
                &ServerFrame::Error {
                    message: error.to_string(),
                },
            )
            .await;
            guard.discard().await;
            Err(error)
        }
    }
}

#[cfg(unix)]
async fn begin_detached_run(
    replays: &Arc<Mutex<ReplayStore>>,
    request_id: String,
    cwd: String,
    session_id: Option<String>,
) -> Result<String> {
    let run_id = new_request_id();
    if !replays.lock().await.begin_with_metadata(
        &run_id,
        true,
        Some(request_id),
        Some(cwd),
        session_id,
    ) {
        bail!("YunXi shell daemon 当前活动回合过多，请稍后重试");
    }
    Ok(run_id)
}

#[cfg(unix)]
async fn run_detached_turn(
    replays: Arc<Mutex<ReplayStore>>,
    cancellations: Arc<Mutex<HashMap<String, AgentRunControl>>>,
    sessions: Arc<Mutex<HashMap<String, String>>>,
    run_id: String,
    control: AgentRunControl,
    request_id: String,
    cwd: String,
    prompt: String,
    requested_session: Option<String>,
    offline: bool,
    live: bool,
    provider: Option<String>,
    model: Option<String>,
) {
    let result = run_detached_turn_inner(
        Arc::clone(&replays),
        control,
        sessions,
        run_id.clone(),
        request_id,
        cwd,
        prompt,
        requested_session,
        offline,
        live,
        provider,
        model,
    )
    .await;
    if result.is_err() {
        record_replay_frame(
            &replays,
            &run_id,
            ServerFrame::Done {
                status: "failed".to_string(),
            },
        )
        .await;
    }
    replays.lock().await.finish(&run_id);
    cancellations.lock().await.remove(&run_id);
}

#[cfg(unix)]
async fn run_detached_turn_inner(
    replays: Arc<Mutex<ReplayStore>>,
    control: AgentRunControl,
    sessions: Arc<Mutex<HashMap<String, String>>>,
    run_id: String,
    _request_id: String,
    cwd: String,
    prompt: String,
    requested_session: Option<String>,
    offline: bool,
    live: bool,
    provider: Option<String>,
    model: Option<String>,
) -> Result<()> {
    let cwd_path = PathBuf::from(&cwd);
    let mut config = AgentConfig::new(cwd_path.clone());
    if let Some(provider) = provider {
        config.provider = Some(provider);
    }
    if let Some(model) = model {
        config.model = Some(model);
    }
    let selection = crate::ProviderSelection::resolve(&config, offline, live)?;
    let backend = if selection.live {
        YunXiRuntimeBackend::for_workspace_with_live_provider(&cwd_path, &config)
    } else {
        YunXiRuntimeBackend::for_workspace(&cwd_path)
    };
    let session_key = requested_session
        .clone()
        .unwrap_or_else(|| format!("cwd:{cwd}"));
    let prior = if requested_session.is_some() {
        requested_session
    } else {
        sessions.lock().await.get(&session_key).cloned()
    };
    config.session_title = Some("YunXi Linux fish shell detached turn".to_string());
    config.parent_session_id = prior;
    let agent = Agent::new(config);
    let cancellation_token = control.cancellation_token();
    let (stream_control, mut stream) = AgentRunControl::streaming_output_only();
    let stream_control = stream_control.with_cancellation_token(cancellation_token);
    let mut turn =
        Box::pin(agent.run_with_backend_stream(&backend, AgentInput::text(prompt), stream_control));
    let mut thread_id = None;
    let mut message_sent = false;
    let result = loop {
        tokio::select! {
            turn_result = &mut turn => break turn_result.context("YunXi Runtime 执行失败")?,
            event = stream.events.recv() => if let Some(event) = event {
                record_detached_agent_event(&replays, &run_id, &mut thread_id, &mut message_sent, event).await?;
            },
        }
    };
    while let Ok(event) = stream.events.try_recv() {
        record_detached_agent_event(&replays, &run_id, &mut thread_id, &mut message_sent, event)
            .await?;
    }
    if !message_sent && let Some(content) = result.final_response {
        record_replay_frame(&replays, &run_id, ServerFrame::Message { content }).await;
    }
    let status = match result.status {
        AgentRunStatus::Completed => "completed",
        AgentRunStatus::Failed => "failed",
        AgentRunStatus::Cancelled => "cancelled",
    };
    if let Some(thread_id) = thread_id {
        sessions.lock().await.insert(session_key, thread_id);
    }
    record_replay_frame(
        &replays,
        &run_id,
        ServerFrame::Done {
            status: status.to_string(),
        },
    )
    .await;
    Ok(())
}

#[cfg(unix)]
async fn record_detached_agent_event(
    replays: &Arc<Mutex<ReplayStore>>,
    run_id: &str,
    thread_id: &mut Option<String>,
    message_sent: &mut bool,
    event: AgentEvent,
) -> Result<()> {
    match event {
        AgentEvent::ThreadStarted { thread_id: id } => {
            *thread_id = Some(id.clone());
            record_replay_frame(replays, run_id, ServerFrame::Thread { thread_id: id }).await;
        }
        AgentEvent::Message { content, .. } => {
            *message_sent = true;
            record_replay_frame(replays, run_id, ServerFrame::Message { content }).await;
        }
        _ => {}
    }
    Ok(())
}

#[cfg(unix)]
async fn record_replay_frame(
    replays: &Arc<Mutex<ReplayStore>>,
    run_id: &str,
    frame: ServerFrame,
) -> Option<u64> {
    replays.lock().await.record(run_id, &frame)
}

#[cfg(unix)]
async fn follow_active_run<W: tokio::io::AsyncWrite + Unpin>(
    writer: &mut W,
    run_id: &str,
    after_seq: u64,
    backlog: Vec<ReplayEvent>,
    mut receiver: broadcast::Receiver<ReplayEvent>,
    _replays: Arc<Mutex<ReplayStore>>,
) -> Result<()> {
    let mut cursor = after_seq;
    for event in backlog {
        if event.seq > cursor {
            write_frame(
                writer,
                &numbered_replay_frame(run_id, event.seq, event.frame.clone()),
            )
            .await?;
            cursor = event.seq;
        }
        // A client may present a cursor beyond the current sequence. Once
        // Done is already in the atomic backlog, never leave that client
        // waiting for a receiver event that can no longer arrive.
        if matches!(event.frame, ServerFrame::Done { .. }) {
            return Ok(());
        }
    }
    loop {
        match receiver.recv().await {
            Ok(event) => {
                if event.seq > cursor {
                    write_frame(
                        writer,
                        &numbered_replay_frame(run_id, event.seq, event.frame.clone()),
                    )
                    .await?;
                    cursor = event.seq;
                }
                // Check Done even when it is at/below the requested cursor;
                // this prevents an unbounded wait for an impossible future
                // event on a malformed or stale high cursor.
                if matches!(event.frame, ServerFrame::Done { .. }) {
                    return Ok(());
                }
            }
            Err(broadcast::error::RecvError::Lagged(_)) => {
                write_frame(
                    writer,
                    &ServerFrame::ResyncRequired {
                        run_id: run_id.to_string(),
                        reason: "active Follow 事件滞后，必须重新同步".to_string(),
                    },
                )
                .await?;
                return Ok(());
            }
            Err(broadcast::error::RecvError::Closed) => {
                write_frame(
                    writer,
                    &ServerFrame::ResyncRequired {
                        run_id: run_id.to_string(),
                        reason: "active Follow 对应回合不可用，必须重新同步".to_string(),
                    },
                )
                .await?;
                return Ok(());
            }
        }
    }
}

#[cfg(unix)]
async fn emit_agent_event<W: tokio::io::AsyncWrite + Unpin>(
    writer: &mut W,
    replays: &Arc<Mutex<ReplayStore>>,
    run_id: &str,
    thread_id: &mut Option<String>,
    message_sent: &mut bool,
    event: AgentEvent,
) -> Result<()> {
    match event {
        AgentEvent::ThreadStarted { thread_id: id } => {
            *thread_id = Some(id.clone());
            emit_replay_frame(
                writer,
                replays,
                run_id,
                ServerFrame::Thread { thread_id: id },
            )
            .await?;
        }
        AgentEvent::Message { content, .. } => {
            *message_sent = true;
            emit_replay_frame(writer, replays, run_id, ServerFrame::Message { content }).await?;
        }
        _ => {}
    }
    Ok(())
}

#[cfg(unix)]
async fn run_daemon_turn_inner<W: tokio::io::AsyncWrite + Unpin>(
    writer: &mut W,
    rx: &mut mpsc::Receiver<ClientFrame>,
    sessions: Arc<Mutex<HashMap<String, String>>>,
    replays: Arc<Mutex<ReplayStore>>,
    run_id: String,
    request_id: String,
    cwd: String,
    prompt: String,
    requested_session: Option<String>,
    offline: bool,
    live: bool,
    provider: Option<String>,
    model: Option<String>,
) -> Result<()> {
    write_frame(
        writer,
        &ServerFrame::RunAccepted {
            run_id: run_id.clone(),
        },
    )
    .await?;
    let cwd_path = PathBuf::from(&cwd);
    let mut config = AgentConfig::new(cwd_path.clone());
    if let Some(provider) = provider {
        config.provider = Some(provider);
    }
    if let Some(model) = model {
        config.model = Some(model);
    }
    let selection = crate::ProviderSelection::resolve(&config, offline, live)?;
    let backend = if selection.live {
        YunXiRuntimeBackend::for_workspace_with_live_provider(&cwd_path, &config)
    } else {
        YunXiRuntimeBackend::for_workspace(&cwd_path)
    };
    let session_key = requested_session
        .clone()
        .unwrap_or_else(|| format!("cwd:{cwd}"));
    let prior = if requested_session.is_some() {
        requested_session
    } else {
        sessions.lock().await.get(&session_key).cloned()
    };
    config.session_title = Some("YunXi Linux fish shell".to_string());
    config.parent_session_id = prior;
    let agent = Agent::new(config);
    let (control, mut stream) = AgentRunControl::streaming();
    let run_control = control.clone();
    let mut turn =
        Box::pin(agent.run_with_backend_stream(&backend, AgentInput::text(prompt), run_control));
    let mut thread_id = None;
    let mut result = None;
    let mut message_sent = false;
    loop {
        if result.is_some() {
            break;
        }
        tokio::select! {
            biased;
            turn_result = &mut turn => {
                result = Some(turn_result.context("YunXi Runtime 执行失败")?);
            }
            event = stream.events.recv() => if let Some(event) = event {
                emit_agent_event(
                    writer,
                    &replays,
                    &run_id,
                    &mut thread_id,
                    &mut message_sent,
                    event,
                ).await?;
            },
            request = stream.approvals.recv() => if let Some(request) = request {
                write_frame(writer, &ServerFrame::Approval {
                    id: request.id.clone(),
                    tool_name: request.tool_name.clone(),
                    reason: request.reason.clone(),
                    command: request.command.clone(),
                    cwd: request.cwd.clone(),
                }).await?;
                match await_approval(rx, request.id.as_deref(), &request_id).await? {
                    ShellPromptResult::Value(decision) => {
                        let _ = request.respond_to.send(AgentRunApprovalDecision {
                            approved: decision.0,
                            reason: decision.1,
                        });
                    }
                    ShellPromptResult::Cancelled => {
                        control.cancel();
                        let _ = request.respond_to.send(AgentRunApprovalDecision {
                            approved: false,
                            reason: Some("fish shell user cancelled".to_string()),
                        });
                    }
                }
            },
            request = stream.user_inputs.recv() => if let Some(request) = request {
                write_frame(writer, &ServerFrame::UserInput { id: request.id.clone(), prompt: request.prompt.clone() }).await?;
                match await_user_input(rx, request.id.as_deref(), &request_id).await? {
                    ShellPromptResult::Value(value) => {
                        let _ = request.respond_to.send(AgentRunUserInputResponse { value });
                    }
                    ShellPromptResult::Cancelled => {
                        control.cancel();
                        let _ = request.respond_to.send(AgentRunUserInputResponse { value: None });
                    }
                }
            },
            frame = rx.recv() => match frame {
                Some(frame) => match frame {
                    ClientFrame::Cancel { request_id: cancelled, run_id: None } if cancelled == request_id => {
                        control.cancel();
                    }
                    ClientFrame::Ping { request_id } => {
                        write_frame(writer, &ServerFrame::Pong { request_id }).await?;
                    }
                    ClientFrame::Follow {
                        run_id,
                        session_id,
                        ..
                    } => {
                        let run_id = run_id.or(session_id).unwrap_or_default();
                        write_frame(writer, &ServerFrame::ResyncRequired {
                            run_id,
                            reason: "当前回合仍在运行；不支持活动回合断线续跑".to_string(),
                        }).await?;
                    }
                    _ => {}
                }
                None => {
                    control.cancel();
                    bail!("shell client disconnected");
                }
            }
        }
    }
    let result = result.expect("turn result is set before leaving loop");
    // A completed runtime turn may win the biased select at the same time as
    // its final stream events. Drain those events before emitting Done so every
    // Message gets its replay sequence first.
    while let Ok(event) = stream.events.try_recv() {
        emit_agent_event(
            writer,
            &replays,
            &run_id,
            &mut thread_id,
            &mut message_sent,
            event,
        )
        .await?;
    }
    if !message_sent && let Some(content) = result.final_response {
        emit_replay_frame(writer, &replays, &run_id, ServerFrame::Message { content }).await?;
    }
    let status = match result.status {
        AgentRunStatus::Completed => "completed",
        AgentRunStatus::Failed => "failed",
        AgentRunStatus::Cancelled => "cancelled",
    };
    if let Some(thread_id) = thread_id {
        sessions.lock().await.insert(session_key, thread_id);
    }
    emit_replay_frame(
        writer,
        &replays,
        &run_id,
        ServerFrame::Done {
            status: status.to_string(),
        },
    )
    .await?;
    Ok(())
}

#[cfg(unix)]
async fn emit_replay_frame<W: tokio::io::AsyncWrite + Unpin>(
    writer: &mut W,
    replays: &Arc<Mutex<ReplayStore>>,
    run_id: &str,
    frame: ServerFrame,
) -> Result<()> {
    let seq = replays.lock().await.record(run_id, &frame);
    if let Some(seq) = seq {
        write_frame(writer, &numbered_replay_frame(run_id, seq, frame)).await
    } else {
        write_frame(writer, &frame).await
    }
}

#[cfg(unix)]
fn numbered_replay_frame(run_id: &str, seq: u64, frame: ServerFrame) -> ServerFrame {
    ServerFrame::Event {
        run_id: run_id.to_string(),
        seq,
        frame: Box::new(frame),
    }
}

#[cfg(unix)]
enum ShellPromptResult<T> {
    Value(T),
    Cancelled,
}

#[cfg(unix)]
async fn await_approval(
    rx: &mut mpsc::Receiver<ClientFrame>,
    expected: Option<&str>,
    request_id: &str,
) -> Result<ShellPromptResult<(bool, Option<String>)>> {
    loop {
        let Some(frame) = rx.recv().await else {
            bail!("shell client disconnected");
        };
        match frame {
            ClientFrame::Cancel {
                request_id: cancelled,
                run_id: None,
            } if cancelled == request_id => return Ok(ShellPromptResult::Cancelled),
            ClientFrame::ApprovalResponse {
                id,
                approved,
                reason,
            } if id.as_deref() == expected => {
                return Ok(ShellPromptResult::Value((approved, reason)));
            }
            _ => {}
        }
    }
}

#[cfg(unix)]
async fn await_user_input(
    rx: &mut mpsc::Receiver<ClientFrame>,
    expected: Option<&str>,
    request_id: &str,
) -> Result<ShellPromptResult<Option<String>>> {
    loop {
        let Some(frame) = rx.recv().await else {
            bail!("shell client disconnected");
        };
        match frame {
            ClientFrame::Cancel {
                request_id: cancelled,
                run_id: None,
            } if cancelled == request_id => return Ok(ShellPromptResult::Cancelled),
            ClientFrame::UserInputResponse { id, value } if id.as_deref() == expected => {
                return Ok(ShellPromptResult::Value(value));
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_commands_and_prose_like_miyu() {
        #[cfg(unix)]
        assert!(is_shell_command("ls -la", "fish"));
        assert!(is_shell_command("echo hi", "fish"));
        assert!(is_shell_command("cd /tmp", "fish"));
        assert!(is_shell_command("FOO=bar echo hi", "fish"));
        assert!(is_shell_command("for item in a b", "fish"));
        assert!(!is_shell_command("你好，帮我找一下项目文件", "fish"));
        assert!(!is_shell_command("time 是什么命令？", "fish"));
        assert!(!is_shell_command("第一行\n第二行", "fish"));
    }

    #[test]
    fn hook_routes_every_non_empty_submission() {
        let hook = fish_hook(Path::new("/home/user/.local/bin/yunxi-linux"));
        assert!(hook.contains("__yunxi_hand_to_ai \"$buffer\""));
        assert!(hook.contains("set -g __yunxi_binary"));
        assert!(!hook.contains("shell-classify --shell fish --stdin"));
        assert!(!hook.contains("__yunxi_takeover"));
        assert!(!hook.contains("conservative"));
        assert!(hook.contains(
            "shell-intercept --shell fish --session-id \"fish-\"$fish_pid --cwd \"$PWD\" --stdin"
        ));
        assert!(hook.contains("bind enter __yunxi_accept_line"));
        assert!(hook.contains("bind ctrl-j __yunxi_insert_newline"));
    }

    #[cfg(unix)]
    #[test]
    fn run_status_output_contains_only_lifecycle_fields() {
        let output =
            format_run_status_json("run-1".to_string(), RunStatus::Interrupted, 4, true, 123)
                .expect("run status JSON");
        let value: serde_json::Value = serde_json::from_str(&output).expect("valid JSON");
        assert_eq!(value["run_id"], "run-1");
        assert_eq!(value["status"], "interrupted");
        assert_eq!(value["next_seq"], 4);
        assert_eq!(value["recoverable"], true);
        assert_eq!(value["created_at_unix_secs"], 123);
        assert_eq!(value.as_object().expect("object").len(), 5);
        assert!(!output.contains("prompt"));
        assert!(!output.contains("cwd"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn approval_and_user_input_waits_honor_matching_cancel() {
        let (approval_tx, mut approval_rx) = mpsc::channel(2);
        approval_tx
            .send(ClientFrame::Cancel {
                request_id: "turn-1".to_string(),
                run_id: None,
            })
            .await
            .expect("cancel frame");
        assert!(matches!(
            await_approval(&mut approval_rx, Some("approval-1"), "turn-1")
                .await
                .expect("approval wait"),
            ShellPromptResult::Cancelled
        ));

        let (input_tx, mut input_rx) = mpsc::channel(2);
        input_tx
            .send(ClientFrame::Cancel {
                request_id: "other-turn".to_string(),
                run_id: None,
            })
            .await
            .expect("mismatched cancel frame");
        input_tx
            .send(ClientFrame::UserInputResponse {
                id: Some("input-1".to_string()),
                value: Some("answer".to_string()),
            })
            .await
            .expect("input response");
        assert!(matches!(
            await_user_input(&mut input_rx, Some("input-1"), "turn-1")
                .await
                .expect("input wait"),
            ShellPromptResult::Value(Some(value)) if value == "answer"
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn detached_cancel_registry_cancels_only_known_run() {
        let cancellations = Arc::new(Mutex::new(HashMap::new()));
        let (control, _stream) = AgentRunControl::streaming_output_only();
        let token = control.cancellation_token();
        cancellations
            .lock()
            .await
            .insert("detached-run".to_string(), control);

        assert!(cancel_detached_run(&cancellations, "detached-run").await);
        assert!(token.is_cancelled());
        assert!(!cancel_detached_run(&cancellations, "missing-run").await);
    }

    #[cfg(unix)]
    #[test]
    fn terminal_polling_exits_before_opening_tty_when_cancelled() {
        let cancelled = AtomicBool::new(true);
        assert_eq!(
            read_terminal_line_polling("unused", &cancelled).expect("cancelled input"),
            ""
        );
    }

    #[test]
    fn systemd_unit_is_user_scoped_and_daemon_only() {
        assert!(SYSTEMD_UNIT.contains("ExecStart=%h/.local/bin/yunxi-linux daemon"));
        assert!(SYSTEMD_UNIT.contains("WantedBy=default.target"));
        assert!(SYSTEMD_UNIT.contains("KillSignal=SIGTERM"));
        assert!(!SYSTEMD_UNIT.contains("User=root"));
        assert!(!SYSTEMD_UNIT.contains("ListenStream="));
    }

    #[cfg(unix)]
    #[test]
    fn completed_run_replays_with_monotonic_sequences() {
        let mut store = ReplayStore {
            max_runs: 2,
            max_events_per_run: 3,
            ..ReplayStore::default()
        };
        store.begin("run-1");
        store.record(
            "run-1",
            &ServerFrame::Thread {
                thread_id: "thread-1".to_string(),
            },
        );
        store.record(
            "run-1",
            &ServerFrame::Message {
                content: "hello".to_string(),
            },
        );
        store.record(
            "run-1",
            &ServerFrame::Done {
                status: "completed".to_string(),
            },
        );
        store.finish("run-1");

        let ReplayLookup::Events(events) = store.lookup("run-1", 0) else {
            panic!("completed run should be replayable");
        };
        assert_eq!(
            events.iter().map(|event| event.seq).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        let ReplayLookup::Events(events) = store.lookup("run-1", 1) else {
            panic!("after_seq should filter already received events");
        };
        assert_eq!(
            events.iter().map(|event| event.seq).collect::<Vec<_>>(),
            vec![2, 3]
        );
    }

    #[cfg(unix)]
    #[test]
    fn replay_ring_reports_stale_active_and_unknown_runs() {
        let mut store = ReplayStore {
            max_runs: 1,
            max_events_per_run: 2,
            ..ReplayStore::default()
        };
        store.begin("active");
        assert!(matches!(
            store.lookup("active", 0),
            ReplayLookup::ActiveUnavailable
        ));

        assert!(store.begin_with_delivery("detached-active", true));
        assert!(matches!(
            store.lookup("detached-active", 0),
            ReplayLookup::Active { .. }
        ));

        store.begin("run-1");
        for content in ["one", "two", "three"] {
            store.record(
                "run-1",
                &ServerFrame::Message {
                    content: content.to_string(),
                },
            );
        }
        store.finish("run-1");
        assert!(matches!(store.lookup("run-1", 0), ReplayLookup::Stale));
        assert!(matches!(store.lookup("missing", 0), ReplayLookup::Unknown));

        store.begin("run-2");
        store.record(
            "run-2",
            &ServerFrame::Done {
                status: "completed".to_string(),
            },
        );
        store.finish("run-2");
        assert!(matches!(store.lookup("run-1", 2), ReplayLookup::Unknown));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn detached_active_follow_keeps_backlog_and_live_order() {
        let mut store = ReplayStore::default();
        assert!(store.begin_with_delivery("detached", true));
        store.record(
            "detached",
            &ServerFrame::Thread {
                thread_id: "thread-1".to_string(),
            },
        );

        let ReplayLookup::Active {
            events: backlog,
            mut receiver,
        } = store.lookup("detached", 0)
        else {
            panic!("detached active run should provide backlog and live receiver");
        };
        assert_eq!(
            backlog.iter().map(|event| event.seq).collect::<Vec<_>>(),
            vec![1]
        );

        store.record(
            "detached",
            &ServerFrame::Message {
                content: "hello".to_string(),
            },
        );
        store.record(
            "detached",
            &ServerFrame::Done {
                status: "completed".to_string(),
            },
        );

        let message = receiver.recv().await.expect("live message event");
        let done = receiver.recv().await.expect("live done event");
        assert_eq!(message.seq, 2);
        assert!(matches!(message.frame, ServerFrame::Message { .. }));
        assert_eq!(done.seq, 3);
        assert!(matches!(done.frame, ServerFrame::Done { .. }));
    }

    #[cfg(unix)]
    #[test]
    fn replay_store_discards_aborted_runs() {
        let mut store = ReplayStore::default();
        store.begin("aborted");
        store.record(
            "aborted",
            &ServerFrame::Message {
                content: "partial".to_string(),
            },
        );
        store.discard("aborted");
        assert!(matches!(store.lookup("aborted", 0), ReplayLookup::Unknown));
    }

    #[cfg(unix)]
    #[test]
    fn durable_replay_marks_running_run_interrupted_after_restart() {
        let directory = std::env::temp_dir().join(format!(
            "yunxi-run-state-test-{}-{}",
            std::process::id(),
            REQUEST_ID_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let persistence = ReplayPersistence::new(directory.clone()).expect("state directory");
        let mut store = ReplayStore {
            persistence: Some(persistence.clone()),
            ..ReplayStore::default()
        };
        assert!(store.begin_with_metadata(
            "restart-run",
            true,
            Some("request-1".to_string()),
            Some("/workspace".to_string()),
            None,
        ));
        store.record(
            "restart-run",
            &ServerFrame::Message {
                content: "partial output".to_string(),
            },
        );
        drop(store);

        let restored = ReplayStore::with_persistence(persistence).expect("restore state");
        assert_eq!(
            restored.status("restart-run"),
            Some((
                RunStatus::Interrupted,
                2,
                true,
                restored.status("restart-run").unwrap().3
            ))
        );
        let ReplayLookup::Events(events) = restored.lookup("restart-run", 0) else {
            panic!("interrupted run should be replayable as a terminal result");
        };
        assert!(
            matches!(events.last().map(|event| &event.frame), Some(ServerFrame::Done { status }) if status == "interrupted")
        );
        fs::remove_dir_all(directory).expect("remove test state");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn daemon_lock_uses_process_start_time_to_avoid_pid_reuse() {
        let pid = std::process::id();
        let start_time = process_start_time(pid).expect("current process start time");
        assert!(process_alive(pid, Some(start_time)));
        assert!(!process_alive(pid, Some(start_time.saturating_add(1))));
    }

    #[cfg(unix)]
    #[test]
    fn daemon_lock_metadata_parser_rejects_partial_contents() {
        assert_eq!(
            parse_daemon_lock_owner(r#"{"pid":42,"start_time_ticks":7}"#),
            Some((42, Some(7)))
        );
        assert_eq!(parse_daemon_lock_owner("42"), Some((42, None)));
        assert_eq!(parse_daemon_lock_owner(""), None);
        assert_eq!(parse_daemon_lock_owner(r#"{"pid":"#), None);
    }

    #[test]
    fn source_version_resolution_preserves_explicit_values() {
        assert_eq!(resolve_source_version("arch-rolling"), "arch-rolling");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn source_version_resolution_auto_uses_a_safe_value() {
        let value = resolve_source_version("auto");
        assert!(!value.is_empty());
        assert!(
            value
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || ".@_:+-".contains(character))
        );
    }

    #[cfg(unix)]
    #[test]
    fn daemon_lock_does_not_remove_invalid_metadata() {
        let path = std::env::temp_dir().join(format!(
            "yunxi-daemon-lock-test-{}-{}",
            std::process::id(),
            DAEMON_LOCK_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::write(&path, b"{}").expect("write invalid lock metadata");

        let result = acquire_daemon_lock(&path);
        assert!(result.is_err());
        assert!(path.exists(), "invalid metadata must not be deleted");
        fs::remove_file(path).expect("remove test lock");
    }

    #[cfg(unix)]
    #[test]
    fn turn_request_limits_reject_oversized_semantic_fields() {
        let prompt = "x".repeat(MAX_TURN_PROMPT_BYTES);
        assert!(validate_turn_request("request", "/tmp", &prompt, None, None, None).is_ok());
        let oversized_prompt = "x".repeat(MAX_TURN_PROMPT_BYTES + 1);
        let error = validate_turn_request("request", "/tmp", &oversized_prompt, None, None, None)
            .expect_err("oversized prompt must be rejected");
        assert!(error.to_string().contains("prompt"));
        assert!(error.to_string().contains("65536"));

        let oversized_cwd = "x".repeat(MAX_TURN_CWD_BYTES + 1);
        let error = validate_turn_request("request", &oversized_cwd, "prompt", None, None, None)
            .expect_err("oversized cwd must be rejected");
        assert!(error.to_string().contains("cwd"));
        assert!(error.to_string().contains("4096"));

        let oversized_provider = "x".repeat(MAX_TURN_PROVIDER_BYTES + 1);
        let error = validate_turn_request(
            "request",
            "/tmp",
            "prompt",
            None,
            Some(&oversized_provider),
            None,
        )
        .expect_err("oversized provider must be rejected");
        assert!(error.to_string().contains("provider"));
        assert!(error.to_string().contains("256"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn daemon_turn_emits_structured_error_before_returning_failure() {
        let (mut server, mut client) = tokio::io::duplex(16 * 1024);
        let (_tx, mut rx) = mpsc::channel(1);
        let sessions = Arc::new(Mutex::new(HashMap::new()));
        let replays = Arc::new(Mutex::new(ReplayStore::default()));

        let error = run_daemon_turn(
            &mut server,
            &mut rx,
            sessions,
            replays,
            "request-1".to_string(),
            "/tmp".to_string(),
            "test".to_string(),
            None,
            true,
            true,
            None,
            None,
        )
        .await
        .expect_err("conflicting provider flags must fail");
        assert!(
            error
                .to_string()
                .contains("--offline 与 --live 不能同时使用")
        );

        assert!(matches!(
            read_frame::<_, ServerFrame>(&mut client).await,
            Ok(Some(ServerFrame::RunAccepted { .. }))
        ));
        let frame = read_frame::<_, ServerFrame>(&mut client)
            .await
            .expect("read daemon error frame")
            .expect("daemon error frame should be present");
        let ServerFrame::Error { message } = frame else {
            panic!("expected structured daemon error, got {frame:?}");
        };
        assert!(message.contains("--offline 与 --live 不能同时使用"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn server_reader_forwards_a_frame_written_in_segments() {
        let (mut writer, reader) = tokio::io::duplex(1024);
        let (mut frames, _reader_guard) = spawn_server_reader(reader);
        let expected = ServerFrame::Message {
            content: "分段 frame 仍应保持完整".to_string(),
        };
        let encoded = encode_json_frame(&expected).expect("encode test frame");

        for byte in encoded {
            writer
                .write_all(std::slice::from_ref(&byte))
                .await
                .expect("write one frame byte");
            tokio::task::yield_now().await;
        }

        let received = tokio::time::timeout(Duration::from_secs(1), frames.recv())
            .await
            .expect("reader should forward a segmented frame")
            .expect("reader channel should remain open")
            .expect("reader should decode the segmented frame")
            .expect("segmented frame should not be EOF");
        assert!(
            matches!(received, ServerFrame::Message { content } if content == "分段 frame 仍应保持完整")
        );
    }
}
