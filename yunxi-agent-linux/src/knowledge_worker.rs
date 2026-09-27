//! Explicit multi-workspace scheduling over the existing knowledge job lease.
//!
//! No directory discovery, generation activation, memory indexing, or daemon
//! registration happens here. A cursor survives polling rounds; each visit
//! claims at most one job so a busy workspace cannot starve another one.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
#[cfg(unix)]
use tokio::sync::watch;

const STATE_SCHEMA_VERSION: u32 = 2;
const LEGACY_STATE_SCHEMA_VERSION: u32 = 1;
const DEFAULT_FLEET_WORKER_ID: &str = "yunxi-linux-embedding-fleet";
pub(super) const DEFAULT_DAEMON_WORKER_ID: &str = "yunxi-linux-daemon-knowledge";
const MAX_STATUS_WORKSPACES: usize = 32;
const MAX_AGGREGATE_WARNINGS: usize = 8;
const MAX_RETRY_FAILURES: u32 = 6;
const MAX_RETRY_DELAY_SECS: u64 = 60;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SchedulerState {
    schema_version: u32,
    worker_id: String,
    workspace_fingerprints: Vec<String>,
    next_workspace_index: usize,
    next_workspace_fingerprint: Option<String>,
    rounds: u64,
    jobs_processed: u64,
    last_status: String,
    last_time_millis: u64,
    /// Per-workspace scheduler failures are persisted separately from the
    /// SQLite embedding job attempts. This is only for store-level failures
    /// (for example a corrupt/busy database), not provider job retries.
    #[serde(default)]
    workspace_backoff: Vec<WorkspaceBackoffState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WorkspaceBackoffState {
    fingerprint: String,
    failures: u32,
    retry_at_millis: Option<u64>,
}

#[derive(Debug, Clone)]
struct StateWarning {
    code: &'static str,
    message: String,
}

impl StateWarning {
    fn json(&self) -> Value {
        json!({"code": self.code, "message": self.message})
    }
}

struct Workspace {
    path: PathBuf,
    fingerprint: String,
    failures: u32,
    retry_at_millis: Option<u64>,
}

struct Fleet {
    workspaces: Vec<Workspace>,
    worker_id: String,
    next: usize,
    state_path: PathBuf,
    workspace_fingerprints: Vec<String>,
    rounds: u64,
    jobs_processed: u64,
    state_warnings: Vec<StateWarning>,
}

impl Fleet {
    fn new(workspaces: Vec<PathBuf>, worker_id: Option<String>) -> Result<Self> {
        if workspaces.is_empty() || workspaces.len() > 32 {
            bail!("--workspace 必须显式指定 1 到 32 个工作区");
        }
        let worker_id = worker_id.unwrap_or_else(|| DEFAULT_FLEET_WORKER_ID.to_string());
        if worker_id.trim().is_empty() || worker_id.len() > 256 {
            bail!("--worker-id 必须为 1 到 256 字节的非空标识");
        }
        // Validate the entire list before opening any database. Preserve the
        // first occurrence order so workspace_index remains deterministic.
        let mut selected: Vec<Workspace> = Vec::new();
        for (index, path) in workspaces.iter().enumerate() {
            let path = super::canonicalize_worker_workspace(path)
                .map_err(|_| anyhow::anyhow!("工作区参数 #{index} 无法访问或不是目录"))?;
            if !selected.iter().any(|selected| selected.path == path) {
                selected.push(Workspace {
                    fingerprint: workspace_fingerprint(&path),
                    path,
                    failures: 0,
                    retry_at_millis: None,
                });
            }
        }
        let workspace_fingerprints = selected
            .iter()
            .map(|workspace| workspace.fingerprint.clone())
            .collect::<Vec<_>>();
        let state_path = scheduler_state_path(&worker_id)?;
        let (next, rounds, jobs_processed, backoff, state_warnings) =
            restore_state(&state_path, &worker_id, &selected);
        for workspace in &mut selected {
            if let Some(saved) = backoff
                .iter()
                .find(|saved| saved.fingerprint == workspace.fingerprint)
            {
                workspace.failures = saved.failures.min(MAX_RETRY_FAILURES);
                workspace.retry_at_millis = saved.retry_at_millis;
            }
        }
        Ok(Self {
            workspaces: selected,
            worker_id,
            next,
            state_path,
            workspace_fingerprints,
            rounds,
            jobs_processed,
            state_warnings,
        })
    }

    fn round(&mut self, max_jobs: usize, cancelled: &AtomicBool) -> Value {
        let count = self.workspaces.len();
        let mut jobs: Vec<Vec<Value>> = vec![Vec::new(); count];
        let mut errors = vec![false; count];
        let mut eligible = vec![true; count];
        let mut processed = 0;
        while processed < max_jobs && eligible.iter().any(|value| *value) {
            if cancelled.load(Ordering::Acquire) {
                break;
            }
            let index = self.next;
            self.next = (index + 1) % count;
            if !eligible[index] {
                continue;
            }
            let workspace = &mut self.workspaces[index];
            let now_millis = unix_time_millis();
            if workspace
                .retry_at_millis
                .is_some_and(|when| when > now_millis)
            {
                errors[index] = true;
                eligible[index] = false;
                continue;
            }
            // A missing/corrupt/busy database must not stop other workspaces.
            // Avoid echoing raw storage errors, document identifiers or paths
            // into a log shared by several project/private knowledge spaces.
            match super::process_knowledge_workspace(&self.worker_id, 1, &workspace.path) {
                Ok(values) => {
                    workspace.failures = 0;
                    workspace.retry_at_millis = None;
                    if let Some(mut job) = values.into_iter().next() {
                        let error = !job["last_error"].is_null();
                        if let Some(fields) = job.as_object_mut() {
                            fields.remove("document_id");
                            fields.remove("last_error");
                            if error {
                                fields.insert("error_code".into(), json!("embedding_job_failed"));
                            }
                        }
                        jobs[index].push(job);
                        processed += 1;
                    } else {
                        eligible[index] = false;
                    }
                }
                Err(_) => {
                    errors[index] = true;
                    eligible[index] = false;
                    workspace.failures =
                        workspace.failures.saturating_add(1).min(MAX_RETRY_FAILURES);
                    let delay = retry_delay_secs(workspace.failures);
                    workspace.retry_at_millis =
                        Some(now_millis.saturating_add(delay.saturating_mul(1_000)));
                }
            }
        }
        let results: Vec<_> = jobs.into_iter().enumerate().map(|(index, jobs)| {
            let mut result = json!({
                "workspace_index": index,
                "status": if errors[index] { "error" } else if jobs.is_empty() { "idle" } else { "processed" },
                "jobs": jobs,
            });
            if errors[index] {
                result["error_code"] = json!("knowledge_store_unavailable");
                let remaining = self.workspaces[index]
                    .retry_at_millis
                    .map(|when| when.saturating_sub(unix_time_millis()))
                    .unwrap_or(0);
                result["retry_after_secs"] = json!(remaining.div_ceil(1_000));
                result["retry_failures"] = json!(self.workspaces[index].failures);
            }
            result
        }).collect();
        self.rounds = self.rounds.saturating_add(1);
        self.jobs_processed = self.jobs_processed.saturating_add(processed as u64);
        let mut record = json!({
            "schema_version": 1,
            "status": if errors.iter().any(|error| *error) { "degraded" } else if processed == 0 { "idle" } else { "processed" },
            "worker_id": self.worker_id,
            "embedding_model": yunxi_agent_persona::LOCAL_MEMORY_EMBEDDING_MODEL,
            "scheduler": "explicit_round_robin",
            "workspace_count": count,
            "jobs_processed": processed,
            "max_jobs": max_jobs,
            "next_workspace_index": self.next,
            "workspaces": results,
            "scheduler_round": self.rounds,
            "jobs_processed_total": self.jobs_processed,
        });
        let status = record["status"].as_str().unwrap_or("unknown").to_string();
        self.persist_state(&status, &mut record);
        record
    }

    fn stopped(&mut self, reason: &str) -> Value {
        let mut record = json!({
            "schema_version": 1, "status": "stopped", "reason": reason,
            "worker_id": self.worker_id, "scheduler": "explicit_round_robin",
            "workspace_count": self.workspaces.len(), "next_workspace_index": self.next,
        });
        self.persist_state("stopped", &mut record);
        record
    }

    fn persist_state(&mut self, status: &str, record: &mut Value) {
        let state = SchedulerState {
            schema_version: STATE_SCHEMA_VERSION,
            worker_id: self.worker_id.clone(),
            workspace_fingerprints: self.workspace_fingerprints.clone(),
            next_workspace_index: self.next,
            next_workspace_fingerprint: self
                .workspaces
                .get(self.next)
                .map(|workspace| workspace.fingerprint.clone()),
            rounds: self.rounds,
            jobs_processed: self.jobs_processed,
            last_status: status.to_string(),
            last_time_millis: unix_time_millis(),
            workspace_backoff: self
                .workspaces
                .iter()
                .map(|workspace| WorkspaceBackoffState {
                    fingerprint: workspace.fingerprint.clone(),
                    failures: workspace.failures.min(MAX_RETRY_FAILURES),
                    retry_at_millis: workspace.retry_at_millis,
                })
                .collect(),
        };
        if write_scheduler_state(&self.state_path, &state).is_err() {
            self.state_warnings.push(StateWarning {
                code: "scheduler_state_write_failed",
                message: "无法写入调度状态，当前回合仍已完成".to_string(),
            });
        }
        if !self.state_warnings.is_empty() {
            record["warnings"] =
                Value::Array(self.state_warnings.iter().map(StateWarning::json).collect());
            self.state_warnings.clear();
        }
    }
}

/// Validate explicit workspaces and worker identity before a daemon binds its
/// socket. This keeps configuration errors synchronous instead of silently
/// leaving a background task that failed during startup.
#[cfg(unix)]
pub(super) fn validate_daemon_workspaces(paths: &[PathBuf], worker_id: Option<&str>) -> Result<()> {
    if paths.is_empty() || paths.len() > 32 {
        bail!("--knowledge-workspace 必须显式指定 1 到 32 个工作区");
    }
    let worker_id = worker_id
        .map(ToOwned::to_owned)
        .or_else(|| Some(DEFAULT_DAEMON_WORKER_ID.to_string()));
    let _ = Fleet::new(paths.to_vec(), worker_id)?;
    Ok(())
}

fn stable_fingerprint(value: &str) -> String {
    // FNV-1a is used only as a stable, non-secret filename/key. The original
    // path is never persisted in scheduler state or emitted in diagnostics.
    let mut hash = 0xcbf29ce484222325u64;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn workspace_fingerprint(path: &Path) -> String {
    stable_fingerprint(&path.to_string_lossy())
}

fn scheduler_state_path(worker_id: &str) -> Result<PathBuf> {
    let root = scheduler_state_root();
    fs::create_dir_all(&root).with_context(|| "无法创建知识 worker 调度状态目录".to_string())?;
    restrict_state_directory(&root)?;
    Ok(root.join(format!("{}.json", stable_fingerprint(worker_id))))
}

fn scheduler_state_root() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| home.map(|path| path.join(".local").join("state")))
        .unwrap_or_else(|| PathBuf::from(".yunxi-state"))
        .join("yunxi")
        .join("knowledge-worker")
}

fn scheduler_state_read_path(worker_id: &str) -> PathBuf {
    scheduler_state_root().join(format!("{}.json", stable_fingerprint(worker_id)))
}

/// Read scheduler state only. This deliberately does not create the state
/// directory and never opens either SQLite database.
pub(super) fn run_health(worker_id: Option<String>) -> Result<()> {
    let worker_id = worker_id.unwrap_or_else(|| DEFAULT_DAEMON_WORKER_ID.to_string());
    if worker_id.trim().is_empty() || worker_id.len() > 256 {
        bail!("--worker-id 必须为 1 到 256 字节的非空标识");
    }
    let path = scheduler_state_read_path(&worker_id);
    let output = match fs::metadata(&path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => json!({
            "schema_version": 1,
            "scheduler": "read_only_state",
            "status": "missing",
            "worker_id": worker_id,
        }),
        Err(_) => json!({
            "schema_version": 1,
            "scheduler": "read_only_state",
            "status": "error",
            "error_code": "scheduler_state_unavailable",
            "worker_id": worker_id,
        }),
        Ok(metadata) if metadata.len() > 64 * 1024 => json!({
            "schema_version": 1,
            "scheduler": "read_only_state",
            "status": "error",
            "error_code": "scheduler_state_oversized",
            "worker_id": worker_id,
        }),
        Ok(_) => match fs::read_to_string(&path)
            .ok()
            .and_then(|value| serde_json::from_str::<SchedulerState>(&value).ok())
        {
            Some(state)
                if (state.schema_version == STATE_SCHEMA_VERSION
                    || state.schema_version == LEGACY_STATE_SCHEMA_VERSION)
                    && state.worker_id == worker_id =>
            {
                let now = unix_time_millis();
                let retrying = state
                    .workspace_backoff
                    .iter()
                    .filter(|item| item.retry_at_millis.is_some_and(|when| when > now))
                    .count();
                let max_retry_after_secs = state
                    .workspace_backoff
                    .iter()
                    .filter_map(|item| item.retry_at_millis)
                    .map(|when| when.saturating_sub(now).div_ceil(1_000))
                    .max()
                    .unwrap_or(0);
                json!({
                    "schema_version": 1,
                    "scheduler": "read_only_state",
                    "status": "ok",
                    "worker_id": worker_id,
                    "last_status": state.last_status,
                    "rounds": state.rounds,
                    "jobs_processed": state.jobs_processed,
                    "last_time_millis": state.last_time_millis,
                    "workspace_count": state.workspace_fingerprints.len(),
                    "next_workspace_index": state.next_workspace_index,
                    "retrying_workspace_count": retrying,
                    "max_retry_after_secs": max_retry_after_secs,
                })
            }
            _ => json!({
                "schema_version": 1,
                "scheduler": "read_only_state",
                "status": "error",
                "error_code": "scheduler_state_invalid",
                "worker_id": worker_id,
            }),
        },
    };
    print_record(&output)
}

fn restore_state(
    path: &Path,
    worker_id: &str,
    workspaces: &[Workspace],
) -> (
    usize,
    u64,
    u64,
    Vec<WorkspaceBackoffState>,
    Vec<StateWarning>,
) {
    let fingerprints = workspaces
        .iter()
        .map(|workspace| workspace.fingerprint.as_str())
        .collect::<Vec<_>>();
    let reset = |code: &'static str, message: &'static str| {
        (
            0,
            0,
            0,
            Vec::new(),
            vec![StateWarning {
                code,
                message: message.to_string(),
            }],
        )
    };
    if !path.exists() {
        return (0, 0, 0, Vec::new(), Vec::new());
    }
    let Ok(metadata) = fs::metadata(path) else {
        return reset(
            "scheduler_state_reset_unreadable",
            "调度状态不可读取，已重置轮询游标",
        );
    };
    if metadata.len() > 64 * 1024 {
        return reset(
            "scheduler_state_reset_oversized",
            "调度状态超过固定大小上限，已重置轮询游标",
        );
    }
    let Ok(value) = fs::read_to_string(path) else {
        return reset(
            "scheduler_state_reset_unreadable",
            "调度状态不可读取，已重置轮询游标",
        );
    };
    let Ok(state) = serde_json::from_str::<SchedulerState>(&value) else {
        return reset(
            "scheduler_state_reset_invalid_json",
            "调度状态不是有效 JSON，已重置轮询游标",
        );
    };
    if (state.schema_version != STATE_SCHEMA_VERSION
        && state.schema_version != LEGACY_STATE_SCHEMA_VERSION)
        || state.worker_id != worker_id
    {
        return reset(
            "scheduler_state_reset_incompatible",
            "调度状态版本或 worker 身份不匹配，已重置轮询游标",
        );
    }
    let next = if state.workspace_fingerprints == fingerprints {
        state.next_workspace_index % workspaces.len()
    } else {
        state
            .next_workspace_fingerprint
            .as_ref()
            .and_then(|fingerprint| fingerprints.iter().position(|item| item == fingerprint))
            .unwrap_or(0)
    };
    let backoff = state
        .workspace_backoff
        .into_iter()
        .filter(|item| !item.fingerprint.trim().is_empty())
        .map(|mut item| {
            item.failures = item.failures.min(MAX_RETRY_FAILURES);
            item
        })
        .collect();
    (
        next,
        state.rounds,
        state.jobs_processed,
        backoff,
        Vec::new(),
    )
}

/// Deterministic fleet-level backoff for workspace/store failures.
///
/// Provider embedding failures already have their own durable queue retry
/// schedule. This separate bounded schedule prevents one unavailable
/// workspace database from being hammered while leaving other workspaces
/// eligible. No random jitter is used so the result is reproducible across
/// daemon restarts and diagnostics remain easy to reason about.
fn retry_delay_secs(failures: u32) -> u64 {
    let exponent = failures.saturating_sub(1).min(5);
    (1_u64 << exponent).min(MAX_RETRY_DELAY_SECS)
}

fn write_scheduler_state(path: &Path, state: &SchedulerState) -> Result<()> {
    let value = serde_json::to_vec_pretty(state)?;
    if value.len() > 64 * 1024 {
        bail!("知识 worker 调度状态超过固定大小上限");
    }
    let parent = path
        .parent()
        .context("知识 worker 调度状态路径缺少父目录")?;
    let temp = parent.join(format!(
        ".{}.tmp-{}-{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("state"),
        std::process::id(),
        unix_time_millis()
    ));
    let _ = fs::remove_file(&temp);
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .context("创建调度状态临时文件失败")?;
    let result = (|| -> Result<()> {
        restrict_state_file(&file)?;
        file.write_all(&value)?;
        file.flush()?;
        file.sync_all()?;
        drop(file);
        #[cfg(unix)]
        {
            fs::rename(&temp, path)?;
        }
        #[cfg(not(unix))]
        {
            if path.exists() {
                fs::remove_file(path)?;
            }
            fs::rename(&temp, path)?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

fn unix_time_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn restrict_state_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    let _ = path;
    Ok(())
}

fn restrict_state_file(file: &fs::File) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    let _ = file;
    Ok(())
}

// Register Unix handlers before the first database visit, not after the first
// polling round. The SQLite/embedding work runs off the current-thread runtime
// so signal reception remains responsive even while a store is busy.
struct StopSignals {
    #[cfg(unix)]
    interrupt: tokio::signal::unix::Signal,
    #[cfg(unix)]
    terminate: tokio::signal::unix::Signal,
}

impl StopSignals {
    fn new() -> Result<Self> {
        Ok(Self {
            #[cfg(unix)]
            interrupt: tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?,
            #[cfg(unix)]
            terminate: tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?,
        })
    }

    async fn recv(&mut self) -> Result<&'static str> {
        #[cfg(unix)]
        {
            tokio::select! {
                _ = self.interrupt.recv() => Ok("interrupt"),
                _ = self.terminate.recv() => Ok("terminate"),
            }
        }
        #[cfg(not(unix))]
        {
            tokio::signal::ctrl_c().await?;
            Ok("interrupt")
        }
    }
}

fn print_record(record: &Value) -> Result<()> {
    let mut stdout = io::stdout().lock();
    serde_json::to_writer(&mut stdout, record)?;
    writeln!(stdout)?;
    stdout.flush()?;
    Ok(())
}

/// Build a bounded, path-free operational summary from read-only workspace
/// snapshots.  The per-workspace records remain the source of detail; this
/// aggregate is deliberately small and stable enough for a daemon/monitor to
/// consume without parsing paths or storage error text.
fn aggregate_status(results: &[Value]) -> Value {
    let mut workspace_status_counts = BTreeMap::from([
        ("idle".to_string(), 0_u64),
        ("ready".to_string(), 0_u64),
        ("complete".to_string(), 0_u64),
        ("degraded".to_string(), 0_u64),
        ("error".to_string(), 0_u64),
        ("other".to_string(), 0_u64),
    ]);
    let mut pending = 0_u64;
    let mut running = 0_u64;
    let mut failed = 0_u64;
    let mut retry = 0_u64;
    let mut warnings = Vec::new();

    for (workspace_index, result) in results.iter().enumerate() {
        let status = result
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("other");
        let count_key = match status {
            "idle" | "ready" | "complete" | "degraded" | "error" => status,
            _ => "other",
        };
        *workspace_status_counts
            .entry(count_key.to_string())
            .or_default() += 1;

        if status == "error" || status == "degraded" {
            warnings.push(json!({
                "code": if status == "error" {
                    "knowledge_store_unavailable"
                } else {
                    "knowledge_queue_degraded"
                },
                "workspace_index": workspace_index,
            }));
        }

        for queue_name in ["active", "staging"] {
            let Some(queue) = result.get(queue_name).and_then(Value::as_object) else {
                continue;
            };
            let read_count =
                |name: &str| queue.get(name).and_then(Value::as_u64).unwrap_or_default();
            pending = pending.saturating_add(read_count("pending"));
            running = running.saturating_add(read_count("running"));
            failed = failed.saturating_add(read_count("failed"));
            retry = retry
                .saturating_add(read_count("retry_due"))
                .saturating_add(read_count("retry_waiting"));
            if read_count("failed") > 0 && warnings.len() < MAX_AGGREGATE_WARNINGS {
                warnings.push(json!({
                    "code": "embedding_queue_failed",
                    "workspace_index": workspace_index,
                    "queue": queue_name,
                }));
            }
        }
    }

    warnings.truncate(MAX_AGGREGATE_WARNINGS);
    let degraded = workspace_status_counts
        .get("error")
        .copied()
        .unwrap_or_default()
        > 0
        || workspace_status_counts
            .get("degraded")
            .copied()
            .unwrap_or_default()
            > 0
        || failed > 0;
    json!({
        "status": if degraded { "degraded" } else { "healthy" },
        "workspace_status_counts": workspace_status_counts,
        "pending": pending,
        "running": running,
        "failed": failed,
        "retry": retry,
        "warnings": warnings,
    })
}

pub(super) async fn run(
    worker_id: Option<String>,
    max_jobs: usize,
    interval_secs: u64,
    workspaces: Vec<PathBuf>,
    watch: bool,
) -> Result<()> {
    super::validate_worker_max_jobs(max_jobs)?;
    if watch && (interval_secs == 0 || interval_secs > 3600) {
        bail!("--interval-secs 必须在 1 到 3600 之间");
    }
    let mut fleet = Fleet::new(workspaces, worker_id)?;
    let mut signals = StopSignals::new().context("无法注册 knowledge worker 退出信号")?;
    let cancelled = Arc::new(AtomicBool::new(false));
    loop {
        let worker_cancelled = Arc::clone(&cancelled);
        let mut task = tokio::task::spawn_blocking(move || {
            let record = fleet.round(max_jobs, &worker_cancelled);
            (fleet, record)
        });
        let result = tokio::select! {
            biased;
            reason = signals.recv() => {
                cancelled.store(true, Ordering::Release);
                // Finish the in-flight lease before exiting; never detach a
                // writer thread or leave a knowingly half-finished job.
                let (mut fleet, record) = task.await.context("知识调度任务异常退出")?;
                print_record(&record)?;
                return print_record(&fleet.stopped(reason?));
            }
            result = &mut task => result.context("知识调度任务异常退出")?,
        };
        fleet = result.0;
        print_record(&result.1)?;
        if !watch {
            return Ok(());
        }
        tokio::select! {
            biased;
            reason = signals.recv() => return print_record(&fleet.stopped(reason?)),
            _ = tokio::time::sleep(Duration::from_secs(interval_secs)) => {},
        }
    }
}

/// Run the same explicit fleet scheduler under the long-lived host daemon.
///
/// The daemon owns signal handling, so this entry point receives a watch
/// channel instead of installing process-wide handlers. A shutdown waits for
/// the current blocking round to finish after setting its cancellation flag;
/// this keeps an in-flight SQLite lease from being abandoned knowingly.
#[cfg(unix)]
pub(super) async fn run_daemon_worker(
    worker_id: Option<String>,
    max_jobs: usize,
    interval_secs: u64,
    workspaces: Vec<PathBuf>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<()> {
    super::validate_worker_max_jobs(max_jobs)?;
    if interval_secs == 0 || interval_secs > 3600 {
        bail!("--knowledge-interval-secs 必须在 1 到 3600 之间");
    }
    let mut fleet = Fleet::new(workspaces, worker_id)?;
    let cancelled = Arc::new(AtomicBool::new(false));

    loop {
        if *shutdown.borrow() {
            let _ = fleet.stopped("daemon_shutdown");
            return Ok(());
        }
        let worker_cancelled = Arc::clone(&cancelled);
        let mut task = tokio::task::spawn_blocking(move || {
            let record = fleet.round(max_jobs, &worker_cancelled);
            (fleet, record)
        });
        let result = tokio::select! {
            biased;
            changed = shutdown.changed() => {
                cancelled.store(true, Ordering::Release);
                let (mut fleet, _record) = task.await.context("daemon 知识调度任务异常退出")?;
                let reason = if changed.is_ok() && *shutdown.borrow() {
                    "daemon_shutdown"
                } else {
                    "daemon_shutdown_channel_closed"
                };
                let _ = fleet.stopped(reason);
                return Ok(());
            }
            result = &mut task => result.context("daemon 知识调度任务异常退出")?,
        };
        fleet = result.0;
        eprintln!(
            "yunxi daemon knowledge worker status: {}",
            serde_json::to_string(&result.1).unwrap_or_else(|_| "{\"status\":\"error\"}".into())
        );

        if *shutdown.borrow() {
            let _ = fleet.stopped("daemon_shutdown");
            return Ok(());
        }
        let should_stop = tokio::select! {
            biased;
            changed = shutdown.changed() => changed.is_err() || *shutdown.borrow(),
            _ = tokio::time::sleep(Duration::from_secs(interval_secs)) => false,
        };
        if should_stop {
            let _ = fleet.stopped("daemon_shutdown");
            return Ok(());
        }
    }
}

/// Emit a read-only queue snapshot for a caller-selected fleet of workspaces.
///
/// This deliberately does not share the worker's scheduling state: it never
/// claims jobs, reclaims leases, advances retries, initializes databases, or
/// discovers directories. Each workspace is isolated so a corrupt database
/// produces a redacted item-level error without hiding healthy snapshots.
pub(super) fn run_status(workspaces: Vec<PathBuf>) -> Result<()> {
    if workspaces.is_empty() || workspaces.len() > MAX_STATUS_WORKSPACES {
        bail!("--workspace 必须显式指定 1 到 32 个工作区");
    }
    let mut selected: Vec<PathBuf> = Vec::new();
    for (index, path) in workspaces.iter().enumerate() {
        let path = super::canonicalize_worker_workspace(path)
            .map_err(|_| anyhow::anyhow!("工作区参数 #{index} 无法访问或不是目录"))?;
        if !selected.iter().any(|selected| selected == &path) {
            selected.push(path);
        }
    }

    let mut results = Vec::with_capacity(selected.len());
    for (workspace_index, path) in selected.iter().enumerate() {
        let store = yunxi_agent_storage::SqliteKnowledgeStore::for_workspace(path);
        match store.embedding_queue_status() {
            Ok(snapshot) => {
                let status = super::knowledge_queue_status_label(&snapshot);
                results.push(json!({
                    "workspace_index": workspace_index,
                    "database_present": snapshot.database_present,
                    "observed_at_millis": snapshot.observed_at_millis,
                    "active": snapshot.active,
                    "staging": snapshot.staging,
                    "status": status,
                }));
            }
            Err(_) => results.push(json!({
                "workspace_index": workspace_index,
                "database_present": true,
                "status": "error",
                "error_code": "knowledge_store_unavailable",
            })),
        }
    }

    let status = if results.iter().any(|item| item["status"] == "error") {
        "degraded"
    } else if results.iter().any(|item| item["status"] == "ready") {
        "ready"
    } else if results.iter().any(|item| item["status"] == "complete") {
        "complete"
    } else {
        "idle"
    };
    let aggregate = aggregate_status(&results);
    print_record(&json!({
        "schema_version": 1,
        "scheduler": "read_only_snapshot",
        "status": status,
        "workspace_count": results.len(),
        "embedding_model": yunxi_agent_persona::LOCAL_MEMORY_EMBEDDING_MODEL,
        "aggregate": aggregate,
        "workspaces": results,
    }))
}
