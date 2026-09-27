use rusqlite::{Connection, params};
use std::time::{SystemTime, UNIX_EPOCH};
use tempfile::tempdir;
use yunxi_agent_storage::{
    KnowledgeChunkingOptions, KnowledgeDocument, KnowledgeEmbeddingQueueStatus,
    KnowledgeEmbeddingQueueSummary, KnowledgeSpaceKind, KnowledgeSpaceSpec, KnowledgeVisibility,
    MAX_EMBEDDING_JOB_ATTEMPTS, SqliteKnowledgeStore,
};

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock")
        .as_millis()
        .try_into()
        .expect("millis fit")
}

fn system_space() -> KnowledgeSpaceSpec {
    KnowledgeSpaceSpec {
        space_id: "system-linux".to_string(),
        kind: KnowledgeSpaceKind::System,
        owner: "system".to_string(),
        visibility: KnowledgeVisibility::Public,
        source: "fixture".to_string(),
        version: "mixed".to_string(),
        generation: 1,
    }
}

fn document(id: &str, generation: i64) -> KnowledgeDocument {
    KnowledgeDocument {
        document_id: id.to_string(),
        space_id: "system-linux".to_string(),
        title: id.to_string(),
        source: "fixture".to_string(),
        version: "fixture-v1".to_string(),
        generation,
        owner: "system".to_string(),
        visibility: KnowledgeVisibility::Public,
        metadata_json: "{}".to_string(),
    }
}

fn active_store() -> (tempfile::TempDir, SqliteKnowledgeStore) {
    let directory = tempdir().expect("tempdir");
    let store = SqliteKnowledgeStore::new(directory.path().join("knowledge.sqlite3"));
    store.upsert_space(&system_space()).expect("space");
    (directory, store)
}

fn add_active_job(store: &SqliteKnowledgeStore, id: &str) -> i64 {
    let document = document(id, 1);
    store.upsert_document(&document).expect("document");
    store
        .enqueue_current_document_embedding_job(id, "fixture-v1")
        .expect("job")
        .job_id
}

fn update_active_job(
    store: &SqliteKnowledgeStore,
    job_id: i64,
    status: &str,
    attempts: i64,
    next_attempt_at_millis: i64,
    created_at_millis: i64,
    updated_at_millis: i64,
) {
    let connection = Connection::open(store.database()).expect("database");
    connection
        .execute(
            "UPDATE knowledge_embedding_jobs
             SET status = ?1, attempts = ?2, worker_id = ?3, last_error = ?4,
                 next_attempt_at_millis = ?5, created_at_millis = ?6,
                 updated_at_millis = ?7
             WHERE job_id = ?8",
            params![
                status,
                attempts,
                (status == "running").then_some("fixture-worker"),
                (status == "failed").then_some("fixture failure"),
                next_attempt_at_millis,
                created_at_millis,
                updated_at_millis,
                job_id,
            ],
        )
        .expect("update job fixture");
}

fn assert_zero_summary(summary: &KnowledgeEmbeddingQueueSummary) {
    assert_eq!(summary.total, 0);
    assert_eq!(summary.pending, 0);
    assert_eq!(summary.running, 0);
    assert_eq!(summary.completed, 0);
    assert_eq!(summary.failed, 0);
    assert_eq!(summary.pending_ready, 0);
    assert_eq!(summary.retry_due, 0);
    assert_eq!(summary.retry_waiting, 0);
    assert_eq!(summary.exhausted, 0);
    assert_eq!(summary.terminal_failed, 0);
    assert_eq!(summary.expired_leases, 0);
    assert_eq!(summary.oldest_pending_at_millis, None);
    assert_eq!(summary.next_attempt_at_millis, None);
}

#[test]
fn queue_status_reports_mixed_states_and_retry_boundaries() {
    let (_directory, store) = active_store();
    let jobs = [
        add_active_job(&store, "pending-ready"),
        add_active_job(&store, "pending-waiting"),
        add_active_job(&store, "running-fresh"),
        add_active_job(&store, "running-expired"),
        add_active_job(&store, "completed"),
        add_active_job(&store, "retry-due"),
        add_active_job(&store, "retry-waiting"),
        add_active_job(&store, "terminal-failed"),
        add_active_job(&store, "exhausted"),
    ];
    let max = MAX_EMBEDDING_JOB_ATTEMPTS;
    update_active_job(&store, jobs[0], "pending", 0, 0, 10, 10);
    update_active_job(&store, jobs[1], "pending", 0, i64::MAX - 2, 20, 20);
    update_active_job(&store, jobs[2], "running", 1, 0, 30, now_millis());
    update_active_job(&store, jobs[3], "running", 1, 0, 40, 0);
    update_active_job(&store, jobs[4], "completed", 1, 0, 50, 50);
    update_active_job(&store, jobs[5], "failed", 1, 0, 60, 60);
    update_active_job(&store, jobs[6], "failed", 1, i64::MAX - 1, 70, 70);
    update_active_job(&store, jobs[7], "failed", 1, i64::MAX, 80, 80);
    update_active_job(&store, jobs[8], "failed", max, i64::MAX, 90, 90);

    let status = store.embedding_queue_status().expect("queue status");
    assert!(status.database_present);
    assert!(status.observed_at_millis > 0);
    let summary = status.active;
    assert_eq!(summary.total, 9);
    assert_eq!(summary.pending, 2);
    assert_eq!(summary.running, 2);
    assert_eq!(summary.completed, 1);
    assert_eq!(summary.failed, 4);
    assert_eq!(summary.pending_ready, 1);
    assert_eq!(summary.retry_due, 1);
    assert_eq!(summary.retry_waiting, 1);
    assert_eq!(summary.exhausted, 1);
    assert_eq!(summary.terminal_failed, 1);
    assert_eq!(summary.expired_leases, 1);
    assert_eq!(summary.oldest_pending_at_millis, Some(10));
    assert_eq!(summary.next_attempt_at_millis, Some(0));
    assert_zero_summary(&status.staging);
}

#[test]
fn queue_status_keeps_active_and_staging_queues_separate() {
    let (_directory, store) = active_store();
    let active_job = add_active_job(&store, "active-document");
    let generation = store
        .begin_generation_build(
            "system-linux",
            "system",
            KnowledgeVisibility::Public,
            Some("fixture-v1"),
            Some(2),
        )
        .expect("generation")
        .generation;
    let staged = document("staging-document", generation);
    store
        .stage_text_document(
            &staged,
            "staging knowledge",
            &KnowledgeChunkingOptions::default(),
        )
        .expect("staged document");
    let staging_job = store
        .enqueue_staging_embedding_job(
            "system-linux",
            &staged.document_id,
            "fixture-v1",
            generation,
        )
        .expect("staging job")
        .job_id;
    update_active_job(&store, active_job, "completed", 1, 0, 1, 1);
    let connection = Connection::open(store.database()).expect("database");
    connection
        .execute(
            "UPDATE knowledge_staging_embedding_jobs
             SET status = 'failed', attempts = 1, last_error = 'staging failure',
                 next_attempt_at_millis = 0, updated_at_millis = 1
             WHERE job_id = ?1",
            [staging_job],
        )
        .expect("staging update");

    let status = store.embedding_queue_status().expect("queue status");
    assert_eq!(status.active.total, 1);
    assert_eq!(status.active.completed, 1);
    assert_eq!(status.active.failed, 0);
    assert_eq!(status.staging.total, 1);
    assert_eq!(status.staging.failed, 1);
    assert_eq!(status.staging.retry_due, 1);
}

#[test]
fn queue_status_observation_does_not_mutate_lease_attempts_or_timestamps() {
    let (_directory, store) = active_store();
    let job_id = add_active_job(&store, "running-job");
    let before = {
        let connection = Connection::open(store.database()).expect("database");
        connection
            .query_row(
                "UPDATE knowledge_embedding_jobs
                 SET status = 'running', attempts = 2, worker_id = 'worker-a',
                     next_attempt_at_millis = 0, updated_at_millis = 123
                 WHERE job_id = ?1
                 RETURNING status, attempts, worker_id, next_attempt_at_millis,
                           created_at_millis, updated_at_millis",
                [job_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                    ))
                },
            )
            .expect("fixture row")
    };
    let _status = store.embedding_queue_status().expect("queue status");
    let after = {
        let connection = Connection::open(store.database()).expect("database");
        connection
            .query_row(
                "SELECT status, attempts, worker_id, next_attempt_at_millis,
                        created_at_millis, updated_at_millis
                 FROM knowledge_embedding_jobs WHERE job_id = ?1",
                [job_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                    ))
                },
            )
            .expect("fixture row")
    };
    assert_eq!(after, before);
    assert_eq!(
        store
            .embedding_queue_status()
            .expect("repeat status")
            .active
            .expired_leases,
        1
    );
}

#[test]
fn queue_status_missing_database_is_empty_without_creating_anything() {
    let directory = tempdir().expect("tempdir");
    let database = directory.path().join("nested").join("knowledge.sqlite3");
    let store = SqliteKnowledgeStore::new(&database);

    let status = store
        .embedding_queue_status()
        .expect("missing database status");
    assert!(!status.database_present);
    assert_zero_summary(&status.active);
    assert_zero_summary(&status.staging);
    assert!(!database.exists());
    assert!(!database.parent().expect("parent").exists());
}

#[test]
fn queue_status_rejects_legacy_schema_without_migrating_it() {
    let directory = tempdir().expect("tempdir");
    let database = directory.path().join("knowledge.sqlite3");
    let connection = Connection::open(&database).expect("database");
    connection
        .execute_batch(
            "CREATE TABLE knowledge_embedding_jobs (
                job_id INTEGER PRIMARY KEY,
                document_id TEXT NOT NULL,
                embedding_model TEXT NOT NULL,
                generation INTEGER NOT NULL,
                status TEXT NOT NULL,
                attempts INTEGER NOT NULL,
                worker_id TEXT,
                last_error TEXT,
                created_at_millis INTEGER NOT NULL,
                updated_at_millis INTEGER NOT NULL
             );",
        )
        .expect("legacy table");
    drop(connection);

    let store = SqliteKnowledgeStore::new(&database);
    let error = store
        .embedding_queue_status()
        .expect_err("legacy schema must fail explicitly");
    assert!(
        error
            .to_string()
            .contains("schema is missing or unsupported")
    );

    let connection = Connection::open(database).expect("database");
    let schema_rows = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'knowledge_schema'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .expect("schema query");
    assert_eq!(schema_rows, 0);
}

#[test]
fn queue_status_serializes_stable_observation_fields() {
    let status = KnowledgeEmbeddingQueueStatus {
        database_present: false,
        observed_at_millis: 123,
        active: KnowledgeEmbeddingQueueSummary::default(),
        staging: KnowledgeEmbeddingQueueSummary::default(),
    };
    let value = serde_json::to_value(status).expect("status json");
    assert_eq!(value["database_present"], false);
    assert_eq!(value["observed_at_millis"], 123);
    assert_eq!(value["active"]["total"], 0);
    assert_eq!(value["staging"]["expired_leases"], 0);
}
