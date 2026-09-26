//! Miyu-inspired fish integration for the YunXi Linux native host.
//!
//! The hook deliberately makes one conservative decision before fish expands a
//! command line: a real command remains fish's responsibility, while prose is
//! handed to the YunXi daemon.  This is the important boundary; the hook never
//! evaluates command substitutions or globs merely to classify a line.
//!
//! The classification and fallback behavior are adapted from Miyu Agent's
//! `crates/miyu-base/src/shell/fish.rs` (MIT License).  This module routes the
//! accepted prose into YunXi Runtime instead of Miyu's own engine.

use anyhow::{Context, Result, bail};
use clap::Subcommand;
#[cfg(unix)]
use serde::{Deserialize, Serialize};
#[cfg(unix)]
use std::collections::{HashMap, VecDeque};
use std::fs;
#[cfg(unix)]
use std::io::Write;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::sync::Arc;
#[cfg(unix)]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(unix)]
use std::time::{Duration, SystemTime, UNIX_EPOCH};
#[cfg(unix)]
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
#[cfg(unix)]
use tokio::net::{UnixListener, UnixStream};
#[cfg(unix)]
use tokio::signal::unix::{SignalKind, signal};
#[cfg(unix)]
use tokio::sync::{Mutex, mpsc};
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
#[path = "linux_tools.rs"]
mod linux_tools;
use linux_tools::LinuxToolCommand;

const HOOK_MARKER: &str = "# YunXi Agent fish hook";

#[derive(Debug, Clone, Subcommand)]
pub(crate) enum LinuxShellCommand {
    /// Install the Miyu-style fish Enter hook.
    FishInit {
        /// Print the hook instead of writing it.
        #[arg(long)]
        print: bool,
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
        /// Allowlisted command: fish, git, systemctl, pacman, or ip.
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
    /// Process one pending local knowledge embedding job and exit.
    KnowledgeWorker {
        /// Stable worker identity used for the SQLite lease.
        #[arg(long)]
        worker_id: Option<String>,
        /// Maximum number of pending jobs to process in this invocation.
        #[arg(long, default_value_t = 1)]
        max_jobs: usize,
        /// Workspace whose `.yunxi/knowledge/knowledge.sqlite3` is processed.
        #[arg(long, default_value = ".")]
        cwd: PathBuf,
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
    },
    /// Retract one system knowledge document and its derived index rows.
    KnowledgeRetract {
        /// Stable document id returned by knowledge-man/knowledge-help.
        document_id: String,
        /// Workspace whose `.yunxi/knowledge/knowledge.sqlite3` is updated.
        #[arg(long, default_value = ".")]
        cwd: PathBuf,
    },
    /// Hidden long-lived process used by shell-intercept.
    #[command(hide = true)]
    Daemon,
}

pub(crate) async fn run_command(command: LinuxShellCommand) -> Result<()> {
    match command {
        LinuxShellCommand::FishInit { print } => install_fish_hook(print),
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
        } => run_knowledge_search(query, cwd, limit, source_version.as_deref()),
        LinuxShellCommand::KnowledgeIndex { document_id, cwd } => {
            run_knowledge_index(document_id, cwd)
        }
        LinuxShellCommand::KnowledgeEnqueue {
            document_id,
            embedding_model,
            cwd,
        } => run_knowledge_enqueue(document_id, embedding_model, cwd),
        LinuxShellCommand::KnowledgeRetry { job_id, cwd } => run_knowledge_retry(job_id, cwd),
        LinuxShellCommand::KnowledgeWorker {
            worker_id,
            max_jobs,
            cwd,
        } => run_knowledge_worker(worker_id, max_jobs, cwd),
        LinuxShellCommand::KnowledgeVectorSearch {
            query,
            cwd,
            limit,
            source_version,
        } => run_knowledge_vector_search(query, cwd, limit, source_version.as_deref()),
        LinuxShellCommand::KnowledgeRetract { document_id, cwd } => {
            run_knowledge_retract(document_id, cwd)
        }
        LinuxShellCommand::Daemon => run_daemon().await,
    }
}

fn run_knowledge_search(
    query: String,
    cwd: PathBuf,
    limit: usize,
    source_version: Option<&str>,
) -> Result<()> {
    let cwd = std::fs::canonicalize(&cwd)
        .with_context(|| format!("无法访问知识工作区: {}", cwd.display()))?;
    let store = yunxi_agent_storage::SqliteKnowledgeStore::for_workspace(&cwd);
    let Some(scope) = active_system_scope(&store)? else {
        bail!("Linux 知识库尚未初始化，请先运行 knowledge-help 或 knowledge-man");
    };
    let matches = store.search_versioned(&query, &scope, source_version, limit.min(50))?;
    let results = matches
        .into_iter()
        .map(|item| {
            serde_json::json!({
                "chunk_id": item.chunk_id,
                "document_id": item.document_id,
                "title": item.title,
                "content": item.content,
                "metadata_json": item.metadata_json,
                "source": item.source,
                "version": item.version,
                "rank": item.rank,
            })
        })
        .collect::<Vec<_>>();
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "schema_version": 1,
            "query": query,
            "source_version": source_version,
            "space_id": "system-linux",
            "results": results,
        }))?
    );
    Ok(())
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
            "last_error": job.last_error,
        }))?
    );
    Ok(())
}

fn run_knowledge_worker(worker_id: Option<String>, max_jobs: usize, cwd: PathBuf) -> Result<()> {
    if max_jobs == 0 || max_jobs > 1_000 {
        bail!("--max-jobs 必须在 1 到 1000 之间");
    }
    let cwd = std::fs::canonicalize(&cwd)
        .with_context(|| format!("无法访问知识工作区: {}", cwd.display()))?;
    let store = yunxi_agent_storage::SqliteKnowledgeStore::for_workspace(&cwd);
    let provider = yunxi_agent_persona::LocalChargramEmbedding::default();
    let worker_id =
        worker_id.unwrap_or_else(|| format!("yunxi-linux-embedding-worker-{}", std::process::id()));
    let mut jobs = Vec::new();
    for _ in 0..max_jobs {
        let Some(result) = store.process_next_embedding_job(&worker_id, &provider)? else {
            break;
        };
        jobs.push(serde_json::json!({
            "status": result.job.status.as_str(),
            "job_id": result.job.job_id,
            "document_id": result.job.document_id,
            "embedding_model": result.job.embedding_model,
            "generation": result.job.generation,
            "attempts": result.job.attempts,
            "chunks_indexed": result.chunks_indexed,
            "last_error": result.job.last_error,
        }));
    }
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

fn run_knowledge_vector_search(
    query: String,
    cwd: PathBuf,
    limit: usize,
    source_version: Option<&str>,
) -> Result<()> {
    let cwd = std::fs::canonicalize(&cwd)
        .with_context(|| format!("无法访问知识工作区: {}", cwd.display()))?;
    let store = yunxi_agent_storage::SqliteKnowledgeStore::for_workspace(&cwd);
    let provider = yunxi_agent_persona::LocalChargramEmbedding::default();
    let embedding = yunxi_agent_persona::MemoryEmbeddingProvider::embed(&provider, &query)
        .map_err(|error| anyhow::anyhow!("知识查询 embedding 失败: {error}"))?;
    let Some(scope) = active_system_scope(&store)? else {
        bail!("Linux 知识库尚未初始化，请先运行 knowledge-help 或 knowledge-man");
    };
    let matches = store.search_vectors_versioned(
        &embedding.values,
        provider.model_id(),
        &scope,
        source_version,
        limit.min(50),
    )?;
    let results = matches
        .into_iter()
        .map(|item| {
            serde_json::json!({
                "chunk_id": item.chunk_id,
                "document_id": item.document_id,
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
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "schema_version": 1,
            "query": query,
            "source_version": source_version,
            "embedding_model": provider.model_id(),
            "space_id": "system-linux",
            "results": results,
        }))?
    );
    Ok(())
}

fn run_knowledge_retract(document_id: String, cwd: PathBuf) -> Result<()> {
    let cwd = std::fs::canonicalize(&cwd)
        .with_context(|| format!("无法访问知识工作区: {}", cwd.display()))?;
    let store = yunxi_agent_storage::SqliteKnowledgeStore::for_workspace(&cwd);
    let Some(scope) = active_system_scope(&store)? else {
        bail!("Linux 知识库尚未初始化，请先运行 knowledge-help 或 knowledge-man");
    };
    let retracted = store.retract_document(&document_id, &scope)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "schema_version": 1,
            "status": if retracted { "retracted" } else { "not_found" },
            "document_id": document_id,
            "space_id": "system-linux",
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
    store
        .active_space_scope(
            "system-linux",
            "system",
            yunxi_agent_storage::KnowledgeVisibility::Public,
        )
        .context("读取 Linux 知识库当前生效代际失败")
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

fn install_fish_hook(print: bool) -> Result<()> {
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
# 由 `yunxi-linux fish-init` 生成；普通 shell 命令始终交回 fish。
set -g __yunxi_binary "{binary}"

function __yunxi_head_is_plain_word
    test -n "$argv[1]"; or return 1
    string match -qr '[\x27\x22$()~{{}}%;&|<>#^!\x5c[:space:]]' -- "$argv[1]"; and return 1
    return 0
end

function __yunxi_line_is_plain_prose
    test -n "$argv[1]"; or return 1
    string match -qr '[\x27\x22$()~{{}}%;&|<>#^!\x5c]' -- "$argv[1]"; and return 1
    return 0
end

function __yunxi_line_has_cjk
    string match -qr '[\x{{4e00}}-\x{{9fff}}]' -- "$argv[1]"
end

function __yunxi_fish_knows_head
    __yunxi_head_is_plain_word "$argv[1]"; or return 1
    functions -q -- "$argv[1]"; and return 0
    type -q -- "$argv[1]"
end

function __yunxi_first_token_raw
    set -l tokens (commandline --input="$argv[1]" --tokens-raw 2>/dev/null)
    while test (count $tokens) -gt 0
        set -l token $tokens[1]
        if string match -qr '^[A-Za-z_][A-Za-z0-9_]*=' -- "$token"
            set -e tokens[1]
            continue
        end
        printf '%s' "$token"
        return 0
    end
    # Fish's token parser can return no token for a runtime-defined alias or
    # function in an interactive commandline. Fall back to the first lexical
    # word without evaluating expansions so those commands still stay in fish.
    set -l fallback (string replace -r '^[[:space:]]*([^[:space:]]+).*' '$1' -- "$argv[1]")
    if test -n "$fallback"
        printf '%s' "$fallback"
        return 0
    end
    return 1
end

function __yunxi_buffer_is_multiline
    test (string split \n -- "$argv[1]" | count) -gt 1
end

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
    if not __yunxi_buffer_is_multiline "$buffer"
        set -l head (__yunxi_first_token_raw "$buffer")
        if test (count $head) -eq 0
            if __yunxi_line_is_plain_prose "$buffer"
                __yunxi_hand_to_ai "$buffer"
                return
            end
        end
        if __yunxi_fish_knows_head "$head"
            __yunxi_execute_or_continue
            return
        end
        if __yunxi_line_has_cjk "$buffer"; and __yunxi_line_is_plain_prose "$buffer"
            __yunxi_hand_to_ai "$buffer"
            return
        end
        if __yunxi_head_is_plain_word "$head"
            printf '%s' "$buffer" | "$__yunxi_binary" shell-classify --shell fish --stdin >/dev/null 2>/dev/null
            if test $status -eq 1
                __yunxi_hand_to_ai "$buffer"
                return
            end
        end
        __yunxi_execute_or_continue
        return
    end
    set -l head (__yunxi_first_token_raw "$buffer")
    if __yunxi_fish_knows_head "$head"
        __yunxi_execute_or_continue
        return
    end
    printf '%s' "$buffer" | "$__yunxi_binary" shell-classify --shell fish --stdin >/dev/null 2>/dev/null
    if test $status -eq 1
        __yunxi_hand_to_ai "$buffer"
    else
        __yunxi_execute_or_continue
    end
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

function fish_command_not_found
    status is-interactive; or return 127
    set -l current_line (status current-commandline 2>/dev/null | string collect)
    test -n "$current_line"; or return 127
    __yunxi_buffer_is_multiline "$current_line"; and return 127
    set -l head (__yunxi_first_token_raw "$current_line")
    if test (count $head) -eq 0
        __yunxi_line_is_plain_prose "$current_line"; or return 127
        printf '\n'
        printf '%s' "$current_line" | "$__yunxi_binary" shell-intercept --shell fish --session-id "fish-"$fish_pid --cwd "$PWD" --stdin
        return 127
    end
    __yunxi_fish_knows_head "$head"; and return 127
    __yunxi_head_is_plain_word "$head"; or return 127
    printf '\n'
    printf '%s' "$current_line" | "$__yunxi_binary" shell-intercept --shell fish --session-id "fish-"$fish_pid --cwd "$PWD" --stdin
    return 127
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
    Turn {
        request_id: String,
        cwd: String,
        prompt: String,
        session_id: Option<String>,
        offline: bool,
        live: bool,
        provider: Option<String>,
        model: Option<String>,
    },
    Cancel {
        request_id: String,
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
#[derive(Debug, Default)]
struct ReplayRun {
    next_seq: u64,
    events: VecDeque<ReplayEvent>,
    event_bytes: usize,
    complete: bool,
}

#[cfg(unix)]
#[derive(Debug)]
enum ReplayLookup {
    Unknown,
    Active,
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
        }
    }
}

#[cfg(unix)]
impl ReplayStore {
    fn begin(&mut self, run_id: &str) -> bool {
        if self.runs.values().filter(|run| !run.complete).count() >= self.max_active_runs {
            return false;
        }
        self.runs.insert(run_id.to_string(), ReplayRun::default());
        true
    }

    fn discard(&mut self, run_id: &str) {
        self.runs.remove(run_id);
        self.completed_order.retain(|id| id != run_id);
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
        run.events.push_back(ReplayEvent {
            seq,
            bytes,
            frame: frame.clone(),
        });
        run.event_bytes = run.event_bytes.saturating_add(bytes);
        Some(seq)
    }

    fn finish(&mut self, run_id: &str) {
        let Some(run) = self.runs.get_mut(run_id) else {
            return;
        };
        run.complete = true;
        self.completed_order.push_back(run_id.to_string());
        while self.completed_order.len() > self.max_runs {
            if let Some(evicted) = self.completed_order.pop_front() {
                self.runs.remove(&evicted);
            }
        }
    }

    fn lookup(&self, run_id: &str, after_seq: u64) -> ReplayLookup {
        let Some(run) = self.runs.get(run_id) else {
            return ReplayLookup::Unknown;
        };
        if !run.complete {
            return ReplayLookup::Active;
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
    let (reader, mut writer) = stream.into_split();
    let mut reader = reader;
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
    }) = read_frame(&mut reader).await?
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
            request_id,
            cwd: cwd.display().to_string(),
            prompt,
            session_id: session_id.or_else(|| std::env::var("YUNXI_SHELL_SESSION").ok()),
            offline,
            live,
            provider,
            model,
        },
    )
    .await?;
    loop {
        let Some(frame) = read_frame::<_, ServerFrame>(&mut reader).await? else {
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
                let approved = read_yes_no()?;
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
            ServerFrame::UserInput { id, prompt } => {
                let value = read_terminal_line(&format!("\n[YunXi 需要输入] {prompt}\n> "))?;
                send_client_frame(
                    &mut writer,
                    &ClientFrame::UserInputResponse {
                        id,
                        value: Some(value.trim_end().to_string()),
                    },
                )
                .await?;
            }
            ServerFrame::Done { status } => {
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
            ServerFrame::RunAccepted { .. } => {}
        }
    }
}

#[cfg(unix)]
fn read_yes_no() -> Result<bool> {
    let answer = read_terminal_line("")?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

#[cfg(unix)]
fn read_terminal_line(prompt: &str) -> Result<String> {
    use std::fs::OpenOptions;
    use std::io::BufRead;

    if let Ok(mut tty) = OpenOptions::new().read(true).write(true).open("/dev/tty") {
        tty.write_all(prompt.as_bytes())?;
        tty.flush()?;
        let mut reader = io::BufReader::new(tty);
        let mut value = String::new();
        reader.read_line(&mut value)?;
        return Ok(value.trim_end().to_string());
    }
    eprint!("{prompt}");
    io::stderr().flush()?;
    let mut value = String::new();
    io::stdin().read_line(&mut value)?;
    Ok(value.trim_end().to_string())
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
    })) = read_frame(&mut reader).await
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
        read_frame::<_, ServerFrame>(&mut reader).await,
        Ok(Some(ServerFrame::Pong { .. }))
    )
}

#[cfg(not(unix))]
async fn run_daemon() -> Result<()> {
    bail!("YunXi shell daemon 仅支持 Unix/Linux")
}

#[cfg(unix)]
async fn run_daemon() -> Result<()> {
    let socket = socket_path()?;
    if daemon_is_ready(&socket).await {
        return Ok(());
    }
    let lock_path = daemon_lock_path(&socket);
    let _lock = acquire_daemon_lock(&lock_path)?;
    if daemon_is_ready(&socket).await {
        return Ok(());
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
    let replays = Arc::new(Mutex::new(ReplayStore::default()));
    let mut terminate = signal(SignalKind::terminate()).context("注册 SIGTERM handler 失败")?;
    let mut interrupt = signal(SignalKind::interrupt()).context("注册 SIGINT handler 失败")?;
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                let sessions = Arc::clone(&sessions);
                let replays = Arc::clone(&replays);
                tokio::spawn(async move {
                    if let Err(error) = handle_connection(stream, sessions, replays).await {
                        eprintln!("yunxi daemon connection error: {error:#}");
                    }
                });
            }
            _ = terminate.recv() => break,
            _ = interrupt.recv() => break,
        }
    }
    let _ = fs::remove_file(&socket);
    Ok(())
}

#[cfg(unix)]
async fn handle_connection(
    stream: UnixStream,
    sessions: Arc<Mutex<HashMap<String, String>>>,
    replays: Arc<Mutex<ReplayStore>>,
) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let (tx, mut rx) = mpsc::channel::<ClientFrame>(16);
    tokio::spawn(async move {
        let mut reader = reader;
        while let Ok(Some(frame)) = read_frame::<_, ClientFrame>(&mut reader).await {
            if tx.send(frame).await.is_err() {
                break;
            }
        }
    });
    let Some(ClientFrame::Hello {
        protocol_version,
        client: _,
        capabilities: _,
    }) = rx.recv().await
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
            ],
        },
    )
    .await?;
    let Some(request) = rx.recv().await else {
        return Ok(());
    };
    match request {
        ClientFrame::Ping { request_id } => {
            write_frame(&mut writer, &ServerFrame::Pong { request_id }).await?;
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
        } => {
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
                ReplayLookup::Active => {
                    write_frame(
                        &mut writer,
                        &ServerFrame::ResyncRequired {
                            run_id,
                            reason: "当前回合仍在运行；不支持活动回合断线续跑".to_string(),
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
        ClientFrame::Hello { .. }
        | ClientFrame::Cancel { .. }
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
            guard.discard().await;
            Err(error)
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
                let decision = await_approval(rx, request.id.as_deref()).await;
                if decision.is_err() {
                    control.cancel();
                }
                let decision = decision?;
                let _ = request.respond_to.send(AgentRunApprovalDecision { approved: decision.0, reason: decision.1 });
            },
            request = stream.user_inputs.recv() => if let Some(request) = request {
                write_frame(writer, &ServerFrame::UserInput { id: request.id.clone(), prompt: request.prompt.clone() }).await?;
                let value = await_user_input(rx, request.id.as_deref()).await;
                if value.is_err() {
                    control.cancel();
                }
                let value = value?;
                let _ = request.respond_to.send(AgentRunUserInputResponse { value });
            },
            frame = rx.recv() => match frame {
                Some(frame) => match frame {
                    ClientFrame::Cancel { request_id: cancelled } if cancelled == request_id => {
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
async fn await_approval(
    rx: &mut mpsc::Receiver<ClientFrame>,
    expected: Option<&str>,
) -> Result<(bool, Option<String>)> {
    loop {
        let Some(frame) = rx.recv().await else {
            bail!("shell client disconnected");
        };
        if let ClientFrame::ApprovalResponse {
            id,
            approved,
            reason,
        } = frame
            && id.as_deref() == expected
        {
            return Ok((approved, reason));
        }
    }
}

#[cfg(unix)]
async fn await_user_input(
    rx: &mut mpsc::Receiver<ClientFrame>,
    expected: Option<&str>,
) -> Result<Option<String>> {
    loop {
        let Some(frame) = rx.recv().await else {
            bail!("shell client disconnected");
        };
        if let ClientFrame::UserInputResponse { id, value } = frame
            && id.as_deref() == expected
        {
            return Ok(value);
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
    fn hook_keeps_the_real_miyu_boundaries() {
        let hook = fish_hook(Path::new("/home/user/.local/bin/yunxi-linux"));
        assert!(hook.contains("commandline --input=\"$argv[1]\" --tokens-raw"));
        assert!(hook.contains("shell-classify --shell fish --stdin"));
        assert!(hook.contains("type -q -- \"$argv[1]\""));
        assert!(hook.contains("functions -q -- \"$argv[1]\""));
        assert!(hook.contains("string replace -r '^[[:space:]]*([^[:space:]]+).*'"));
        assert!(hook.contains(
            "shell-intercept --shell fish --session-id \"fish-\"$fish_pid --cwd \"$PWD\" --stdin"
        ));
        assert!(hook.contains("fish_command_not_found"));
        assert!(hook.contains("bind enter __yunxi_accept_line"));
        assert!(hook.contains("bind ctrl-j __yunxi_insert_newline"));
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
        assert!(matches!(store.lookup("active", 0), ReplayLookup::Active));

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
}
