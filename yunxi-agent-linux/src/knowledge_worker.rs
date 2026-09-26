//! Explicit multi-workspace scheduling over the existing knowledge job lease.
//!
//! No directory discovery, generation activation, memory indexing, or daemon
//! registration happens here. A cursor survives polling rounds; each visit
//! claims at most one job so a busy workspace cannot starve another one.

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

struct Workspace {
    path: PathBuf,
    failures: u32,
    retry_at: Option<Instant>,
}

struct Fleet {
    workspaces: Vec<Workspace>,
    worker_id: String,
    next: usize,
}

impl Fleet {
    fn new(workspaces: Vec<PathBuf>, worker_id: Option<String>) -> Result<Self> {
        if workspaces.is_empty() || workspaces.len() > 32 {
            bail!("--workspace 必须显式指定 1 到 32 个工作区");
        }
        let worker_id = worker_id
            .unwrap_or_else(|| format!("yunxi-linux-embedding-fleet-{}", std::process::id()));
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
                    path,
                    failures: 0,
                    retry_at: None,
                });
            }
        }
        Ok(Self {
            workspaces: selected,
            worker_id,
            next: 0,
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
            if workspace.retry_at.is_some_and(|when| when > Instant::now()) {
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
                    workspace.retry_at = None;
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
                    workspace.failures = workspace.failures.saturating_add(1);
                    let delay = (1u64 << workspace.failures.min(6)).min(60);
                    workspace.retry_at = Some(Instant::now() + Duration::from_secs(delay));
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
                let remaining = self.workspaces[index].retry_at
                    .map(|when| when.saturating_duration_since(Instant::now()).as_millis())
                    .unwrap_or(0);
                result["retry_after_secs"] = json!(remaining.div_ceil(1000));
            }
            result
        }).collect();
        json!({
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
        })
    }

    fn stopped(&self, reason: &str) -> Value {
        json!({
            "schema_version": 1, "status": "stopped", "reason": reason,
            "worker_id": self.worker_id, "scheduler": "explicit_round_robin",
            "workspace_count": self.workspaces.len(), "next_workspace_index": self.next,
        })
    }
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
                let (fleet, record) = task.await.context("知识调度任务异常退出")?;
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
