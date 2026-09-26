use rusqlite::Connection;
use std::sync::{Arc, Barrier};
use std::thread;
use tempfile::tempdir;
use yunxi_agent_persona::{
    LocalChargramEmbedding, MemoryEmbedding, MemoryEmbeddingError, MemoryEmbeddingProvider,
};
use yunxi_agent_storage::{
    KnowledgeChunk, KnowledgeChunkingOptions, KnowledgeDocument, KnowledgeEmbeddingJobStatus,
    KnowledgeSearchScope, KnowledgeSpaceKind, KnowledgeSpaceSpec, KnowledgeVector,
    KnowledgeVisibility, SqliteKnowledgeStore,
};

fn space(
    id: &str,
    kind: KnowledgeSpaceKind,
    owner: &str,
    visibility: KnowledgeVisibility,
    generation: i64,
) -> KnowledgeSpaceSpec {
    KnowledgeSpaceSpec {
        space_id: id.to_string(),
        kind,
        owner: owner.to_string(),
        visibility,
        source: "fixture".to_string(),
        version: "2026.09".to_string(),
        generation,
    }
}

fn document(space_id: &str, owner: &str, visibility: KnowledgeVisibility) -> KnowledgeDocument {
    KnowledgeDocument {
        document_id: format!("{space_id}-doc"),
        space_id: space_id.to_string(),
        title: "systemctl reference".to_string(),
        source: "fixture".to_string(),
        version: "2026.09".to_string(),
        generation: 1,
        owner: owner.to_string(),
        visibility,
        metadata_json: "{}".to_string(),
    }
}

fn chunk(
    document_id: &str,
    owner: &str,
    visibility: KnowledgeVisibility,
    content: &str,
) -> KnowledgeChunk {
    KnowledgeChunk {
        chunk_id: format!("{document_id}-chunk"),
        document_id: document_id.to_string(),
        ordinal: 0,
        content: content.to_string(),
        source: "fixture".to_string(),
        version: "2026.09".to_string(),
        generation: 1,
        owner: owner.to_string(),
        visibility,
        metadata_json: "{}".to_string(),
    }
}

fn queue_fixture() -> (tempfile::TempDir, SqliteKnowledgeStore, KnowledgeDocument) {
    let dir = tempdir().expect("tempdir");
    let store = SqliteKnowledgeStore::new(dir.path().join("knowledge.sqlite3"));
    store
        .upsert_space(&space(
            "system-linux",
            KnowledgeSpaceKind::System,
            "system",
            KnowledgeVisibility::Public,
            1,
        ))
        .expect("space");
    let document = document("system-linux", "system", KnowledgeVisibility::Public);
    store.upsert_document(&document).expect("document");
    (dir, store, document)
}

#[test]
fn embedding_jobs_are_idempotent_and_have_bounded_transitions() {
    let (_dir, store, document) = queue_fixture();

    let queued = store
        .enqueue_embedding_job(&document.document_id, "fixture-v1", 1)
        .expect("enqueue");
    assert_eq!(queued.status, KnowledgeEmbeddingJobStatus::Pending);
    let duplicate = store
        .enqueue_embedding_job(&document.document_id, "fixture-v1", 1)
        .expect("duplicate enqueue");
    assert_eq!(duplicate, queued);

    let claimed = store
        .claim_embedding_job("worker-a")
        .expect("claim")
        .expect("pending job");
    assert_eq!(claimed.status, KnowledgeEmbeddingJobStatus::Running);
    assert_eq!(claimed.worker_id.as_deref(), Some("worker-a"));
    assert!(
        store
            .claim_embedding_job("worker-b")
            .expect("second claim")
            .is_none()
    );
    assert!(
        store
            .complete_embedding_job(claimed.job_id, "worker-b")
            .is_err()
    );

    let completed = store
        .complete_embedding_job(claimed.job_id, "worker-a")
        .expect("complete");
    assert_eq!(completed.status, KnowledgeEmbeddingJobStatus::Completed);
    assert!(
        store
            .fail_embedding_job(claimed.job_id, "worker-a", "too late")
            .is_err()
    );
    assert!(
        store
            .claim_embedding_job("worker-c")
            .expect("claim after completion")
            .is_none()
    );
}

#[test]
fn embedding_job_enqueue_requires_existing_document_generation() {
    let (_dir, store, document) = queue_fixture();
    assert!(
        store
            .enqueue_embedding_job("missing", "fixture-v1", 1)
            .is_err()
    );
    assert!(
        store
            .enqueue_embedding_job(&document.document_id, "fixture-v1", 2)
            .is_err()
    );
    assert!(
        store
            .enqueue_embedding_job(&document.document_id, "", 1)
            .is_err()
    );
}

#[test]
fn embedding_worker_indexes_and_completes_one_job() {
    let (_dir, store, document) = queue_fixture();
    let knowledge_chunk = chunk(
        &document.document_id,
        "system",
        KnowledgeVisibility::Public,
        "systemctl status shows the current service state",
    );
    store.upsert_chunk(&knowledge_chunk).expect("chunk");
    let provider = LocalChargramEmbedding::default();
    let queued = store
        .enqueue_embedding_job(&document.document_id, provider.model_id(), 1)
        .expect("enqueue");

    let result = store
        .process_next_embedding_job("worker-a", &provider)
        .expect("worker step")
        .expect("claimed job");
    assert_eq!(result.job.job_id, queued.job_id);
    assert_eq!(result.job.status, KnowledgeEmbeddingJobStatus::Completed);
    assert_eq!(result.chunks_indexed, 1);

    let query = provider.embed("service state").expect("query embedding");
    let matches = store
        .search_vectors(
            &query.values,
            provider.model_id(),
            &KnowledgeSearchScope {
                space_id: "system-linux".to_string(),
                owner: "system".to_string(),
                generation: 1,
                visibility: KnowledgeVisibility::Public,
            },
            5,
        )
        .expect("knowledge vector search");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].document_id, document.document_id);
    assert!(
        store
            .process_next_embedding_job("worker-b", &provider)
            .expect("idle worker")
            .is_none()
    );
}

#[test]
fn embedding_worker_marks_provider_model_mismatch_failed() {
    let (_dir, store, document) = queue_fixture();
    let provider = LocalChargramEmbedding::default();
    let queued = store
        .enqueue_embedding_job(&document.document_id, "different-model", 1)
        .expect("enqueue");

    let result = store
        .process_next_embedding_job("worker-a", &provider)
        .expect("worker step")
        .expect("claimed job");
    assert_eq!(result.job.job_id, queued.job_id);
    assert_eq!(result.job.status, KnowledgeEmbeddingJobStatus::Failed);
    assert_eq!(result.chunks_indexed, 0);
    assert!(
        result
            .job
            .last_error
            .as_deref()
            .is_some_and(|message| message.contains("does not match queued model"))
    );
}

#[test]
fn embedding_worker_rejects_a_stale_document_generation() {
    let (_dir, store, document) = queue_fixture();
    let provider = LocalChargramEmbedding::default();
    let queued = store
        .enqueue_current_document_embedding_job(&document.document_id, provider.model_id())
        .expect("enqueue current generation");
    store
        .upsert_space(&space(
            "system-linux",
            KnowledgeSpaceKind::System,
            "system",
            KnowledgeVisibility::Public,
            2,
        ))
        .expect("new space generation");
    let mut updated = document.clone();
    updated.generation = 2;
    updated.version = "2026.10".to_string();
    store
        .upsert_document(&updated)
        .expect("new document generation");

    let result = store
        .process_next_embedding_job("worker-a", &provider)
        .expect("worker step")
        .expect("claimed job");
    assert_eq!(result.job.job_id, queued.job_id);
    assert_eq!(result.job.status, KnowledgeEmbeddingJobStatus::Failed);
    assert!(
        result
            .job
            .last_error
            .as_deref()
            .is_some_and(|message| message.contains("generation 1 is stale"))
    );
}

#[test]
fn concurrent_embedding_claims_assign_a_job_to_only_one_worker() {
    let (_dir, store, document) = queue_fixture();
    store
        .enqueue_embedding_job(&document.document_id, "fixture-v1", 1)
        .expect("enqueue");

    let store = Arc::new(store);
    let barrier = Arc::new(Barrier::new(3));
    let workers = ["worker-a", "worker-b"]
        .into_iter()
        .map(|worker| {
            let store = Arc::clone(&store);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                store.claim_embedding_job(worker).expect("claim")
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    let claims = workers
        .into_iter()
        .map(|worker| worker.join().expect("worker"))
        .collect::<Vec<_>>();
    assert_eq!(claims.iter().filter(|claim| claim.is_some()).count(), 1);
}

#[test]
fn expired_embedding_worker_lease_is_reclaimed_and_old_worker_cannot_complete() {
    let (dir, store, document) = queue_fixture();
    store
        .enqueue_embedding_job(&document.document_id, "fixture-v1", 1)
        .expect("enqueue");
    let claimed = store
        .claim_embedding_job("worker-a")
        .expect("claim")
        .expect("job");

    let connection = Connection::open(dir.path().join("knowledge.sqlite3")).expect("database");
    connection
        .execute(
            "UPDATE knowledge_embedding_jobs SET updated_at_millis = 0 WHERE job_id = ?1",
            [claimed.job_id],
        )
        .expect("expire lease");

    let reclaimed = store
        .claim_embedding_job("worker-b")
        .expect("reclaim")
        .expect("reclaimed job");
    assert_eq!(reclaimed.job_id, claimed.job_id);
    assert_eq!(reclaimed.status, KnowledgeEmbeddingJobStatus::Running);
    assert_eq!(reclaimed.attempts, 2);
    assert_eq!(
        reclaimed.last_error.as_deref(),
        Some("embedding worker lease expired")
    );
    assert!(
        store
            .complete_embedding_job(claimed.job_id, "worker-a")
            .is_err()
    );
    store
        .complete_embedding_job(reclaimed.job_id, "worker-b")
        .expect("new worker completes");
}

#[test]
fn failed_embedding_job_keeps_previous_vector_available() {
    let (_dir, store, document) = queue_fixture();
    let knowledge_chunk = chunk(
        &document.document_id,
        "system",
        KnowledgeVisibility::Public,
        "systemctl status keeps the service state visible",
    );
    store.upsert_chunk(&knowledge_chunk).expect("chunk");
    store
        .upsert_vector(&KnowledgeVector {
            chunk_id: knowledge_chunk.chunk_id.clone(),
            space_id: document.space_id.clone(),
            embedding_model: "fixture-v1".to_string(),
            generation: 1,
            vector: vec![1.0, 0.0],
        })
        .expect("old vector");
    let job = store
        .enqueue_embedding_job(&document.document_id, "fixture-v1", 1)
        .expect("enqueue");
    let claimed = store
        .claim_embedding_job("worker-a")
        .expect("claim")
        .expect("job");
    assert_eq!(claimed.job_id, job.job_id);
    store
        .fail_embedding_job(claimed.job_id, "worker-a", "provider unavailable")
        .expect("fail");

    let vectors = store
        .search_vectors(
            &[1.0, 0.0],
            "fixture-v1",
            &KnowledgeSearchScope {
                space_id: document.space_id,
                owner: "system".to_string(),
                generation: 1,
                visibility: KnowledgeVisibility::Public,
            },
            5,
        )
        .expect("search old vector");
    assert_eq!(vectors.len(), 1);
    assert_eq!(vectors[0].chunk_id, knowledge_chunk.chunk_id);
}

#[test]
fn knowledge_store_uses_separate_database_and_searches_active_scope() {
    let dir = tempdir().expect("tempdir");
    let database = dir.path().join("knowledge").join("knowledge.sqlite3");
    let store = SqliteKnowledgeStore::new(&database);
    store
        .upsert_space(&space(
            "system-linux",
            KnowledgeSpaceKind::System,
            "system",
            KnowledgeVisibility::Public,
            1,
        ))
        .expect("space");
    let document = document("system-linux", "system", KnowledgeVisibility::Public);
    store.upsert_document(&document).expect("document");
    store
        .upsert_chunk(&chunk(
            &document.document_id,
            "system",
            KnowledgeVisibility::Public,
            "systemctl status reads the active service state without changing it",
        ))
        .expect("chunk");

    let results = store
        .search(
            "systemctl status",
            &KnowledgeSearchScope {
                space_id: "system-linux".to_string(),
                owner: "system".to_string(),
                generation: 1,
                visibility: KnowledgeVisibility::Public,
            },
            5,
        )
        .expect("search");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].chunk_id, "system-linux-doc-chunk");
    assert_eq!(results[0].metadata_json, document.metadata_json);
    assert_eq!(store.database(), database.as_path());
    assert_eq!(
        database.file_name().and_then(|name| name.to_str()),
        Some("knowledge.sqlite3")
    );

    let connection = Connection::open(&database).expect("sqlite database");
    let tables = connection
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
        .expect("table query")
        .query_map([], |row| row.get::<_, String>(0))
        .expect("table rows")
        .collect::<Result<Vec<_>, _>>()
        .expect("table names");
    assert!(tables.iter().any(|name| name == "knowledge_spaces"));
    assert!(tables.iter().any(|name| name == "knowledge_vectors"));
    assert!(tables.iter().any(|name| name == "knowledge_chunks_fts"));
    assert!(!tables.iter().any(|name| name == "memory_vectors"));
}

#[test]
fn knowledge_search_cannot_cross_owner_or_generation_boundaries() {
    let dir = tempdir().expect("tempdir");
    let store = SqliteKnowledgeStore::new(dir.path().join("knowledge.sqlite3"));
    store
        .upsert_space(&space(
            "private-alice",
            KnowledgeSpaceKind::Private,
            "alice",
            KnowledgeVisibility::Private,
            1,
        ))
        .expect("private space");
    let document = document("private-alice", "alice", KnowledgeVisibility::Private);
    store.upsert_document(&document).expect("private document");
    store
        .upsert_chunk(&chunk(
            &document.document_id,
            "alice",
            KnowledgeVisibility::Private,
            "private deployment note for systemctl restart",
        ))
        .expect("private chunk");

    let wrong_owner = KnowledgeSearchScope {
        space_id: "private-alice".to_string(),
        owner: "bob".to_string(),
        generation: 1,
        visibility: KnowledgeVisibility::Private,
    };
    assert!(
        store
            .search("deployment", &wrong_owner, 5)
            .unwrap()
            .is_empty()
    );

    let wrong_generation = KnowledgeSearchScope {
        owner: "alice".to_string(),
        generation: 2,
        ..wrong_owner
    };
    assert!(
        store
            .search("deployment", &wrong_generation, 5)
            .unwrap()
            .is_empty()
    );

    let authorized = KnowledgeSearchScope {
        space_id: "private-alice".to_string(),
        owner: "alice".to_string(),
        generation: 1,
        visibility: KnowledgeVisibility::Private,
    };
    assert_eq!(store.search("deployment", &authorized, 5).unwrap().len(), 1);
}

#[test]
fn knowledge_metadata_boundaries_are_rejected() {
    let dir = tempdir().expect("tempdir");
    let store = SqliteKnowledgeStore::new(dir.path().join("knowledge.sqlite3"));
    assert!(
        store
            .upsert_space(&space(
                "invalid-system",
                KnowledgeSpaceKind::System,
                "system",
                KnowledgeVisibility::Private,
                1,
            ))
            .is_err()
    );

    store
        .upsert_space(&space(
            "project",
            KnowledgeSpaceKind::Project,
            "alice",
            KnowledgeVisibility::Owner,
            1,
        ))
        .expect("project space");
    let mut document = document("project", "alice", KnowledgeVisibility::Owner);
    document.metadata_json = "not-json".to_string();
    assert!(store.upsert_document(&document).is_err());
}

#[test]
fn system_space_allows_document_specific_versions_but_project_space_is_strict() {
    let dir = tempdir().expect("tempdir");
    let store = SqliteKnowledgeStore::new(dir.path().join("knowledge.sqlite3"));

    let mut system = space(
        "system-linux",
        KnowledgeSpaceKind::System,
        "system",
        KnowledgeVisibility::Public,
        1,
    );
    system.version = "mixed".to_string();
    store.upsert_space(&system).expect("system space");

    let mut ubuntu = document("system-linux", "system", KnowledgeVisibility::Public);
    ubuntu.document_id = "system-linux-ubuntu-doc".to_string();
    ubuntu.version = "ubuntu-24.04".to_string();
    store.upsert_document(&ubuntu).expect("ubuntu document");

    let mut arch = ubuntu.clone();
    arch.document_id = "system-linux-arch-doc".to_string();
    arch.version = "arch-rolling".to_string();
    store.upsert_document(&arch).expect("arch document");

    store
        .upsert_space(&space(
            "project",
            KnowledgeSpaceKind::Project,
            "alice",
            KnowledgeVisibility::Owner,
            1,
        ))
        .expect("project space");
    let mut mismatched_project = document("project", "alice", KnowledgeVisibility::Owner);
    mismatched_project.version = "project-specific".to_string();
    assert!(store.upsert_document(&mismatched_project).is_err());
}

#[test]
fn mixed_system_search_can_filter_by_source_version() {
    let dir = tempdir().expect("tempdir");
    let store = SqliteKnowledgeStore::new(dir.path().join("knowledge.sqlite3"));
    let mut system = space(
        "system-linux",
        KnowledgeSpaceKind::System,
        "system",
        KnowledgeVisibility::Public,
        1,
    );
    system.version = "mixed".to_string();
    store.upsert_space(&system).expect("system space");

    let mut ubuntu = document("system-linux", "system", KnowledgeVisibility::Public);
    ubuntu.document_id = "system-linux-ubuntu-doc".to_string();
    ubuntu.version = "ubuntu-24.04".to_string();
    store.upsert_document(&ubuntu).expect("ubuntu document");
    let mut ubuntu_chunk = chunk(
        &ubuntu.document_id,
        "system",
        KnowledgeVisibility::Public,
        "ubuntu systemctl status reference",
    );
    ubuntu_chunk.version = ubuntu.version.clone();
    store.upsert_chunk(&ubuntu_chunk).expect("ubuntu chunk");

    let mut arch = ubuntu.clone();
    arch.document_id = "system-linux-arch-doc".to_string();
    arch.version = "arch-rolling".to_string();
    store.upsert_document(&arch).expect("arch document");
    let mut arch_chunk = chunk(
        &arch.document_id,
        "system",
        KnowledgeVisibility::Public,
        "arch systemctl status reference",
    );
    arch_chunk.version = arch.version.clone();
    store.upsert_chunk(&arch_chunk).expect("arch chunk");

    store
        .upsert_vector(&KnowledgeVector {
            chunk_id: ubuntu_chunk.chunk_id.clone(),
            space_id: "system-linux".to_string(),
            embedding_model: "fixture-v1".to_string(),
            generation: 1,
            vector: vec![1.0, 0.0],
        })
        .expect("ubuntu vector");
    store
        .upsert_vector(&KnowledgeVector {
            chunk_id: arch_chunk.chunk_id.clone(),
            space_id: "system-linux".to_string(),
            embedding_model: "fixture-v1".to_string(),
            generation: 1,
            vector: vec![0.0, 1.0],
        })
        .expect("arch vector");

    let scope = KnowledgeSearchScope {
        space_id: "system-linux".to_string(),
        owner: "system".to_string(),
        generation: 1,
        visibility: KnowledgeVisibility::Public,
    };
    let fts = store
        .search_versioned("systemctl", &scope, Some("ubuntu-24.04"), 10)
        .expect("versioned fts search");
    assert_eq!(fts.len(), 1);
    assert_eq!(fts[0].version, "ubuntu-24.04");
    assert!(
        store
            .search_versioned("systemctl", &scope, Some("fedora-41"), 10)
            .expect("missing version search")
            .is_empty()
    );
    assert!(
        store
            .search_versioned("systemctl", &scope, Some("   "), 10)
            .is_err()
    );

    let vectors = store
        .search_vectors_versioned(&[1.0, 0.0], "fixture-v1", &scope, Some("arch-rolling"), 10)
        .expect("versioned vector search");
    assert_eq!(vectors.len(), 1);
    assert_eq!(vectors[0].version, "arch-rolling");
    assert_eq!(vectors[0].chunk_id, arch_chunk.chunk_id);
}

#[test]
fn knowledge_retraction_is_scoped_atomic_and_removes_derived_rows() {
    let dir = tempdir().expect("tempdir");
    let store = SqliteKnowledgeStore::new(dir.path().join("knowledge.sqlite3"));
    store
        .upsert_space(&space(
            "system-linux",
            KnowledgeSpaceKind::System,
            "system",
            KnowledgeVisibility::Public,
            1,
        ))
        .expect("system space");
    let document = document("system-linux", "system", KnowledgeVisibility::Public);
    store.upsert_document(&document).expect("document");
    let chunk = chunk(
        &document.document_id,
        "system",
        KnowledgeVisibility::Public,
        "systemctl status reference",
    );
    store.upsert_chunk(&chunk).expect("chunk");
    store
        .upsert_vector(&KnowledgeVector {
            chunk_id: chunk.chunk_id.clone(),
            space_id: "system-linux".to_string(),
            embedding_model: "fixture-v1".to_string(),
            generation: 1,
            vector: vec![1.0, 0.0],
        })
        .expect("vector");
    let scope = KnowledgeSearchScope {
        space_id: "system-linux".to_string(),
        owner: "system".to_string(),
        generation: 1,
        visibility: KnowledgeVisibility::Public,
    };
    assert!(
        store
            .retract_document(
                &document.document_id,
                &KnowledgeSearchScope {
                    owner: "alice".to_string(),
                    ..scope.clone()
                }
            )
            .is_err()
    );
    assert_eq!(store.search("systemctl", &scope, 5).unwrap().len(), 1);
    assert!(
        store
            .retract_document(&document.document_id, &scope)
            .expect("retraction")
    );
    assert!(store.search("systemctl", &scope, 5).unwrap().is_empty());
    assert!(
        store
            .search_vectors(&[1.0, 0.0], "fixture-v1", &scope, 5)
            .unwrap()
            .is_empty()
    );
    assert!(
        !store
            .retract_document(&document.document_id, &scope)
            .expect("missing retraction")
    );
}

#[test]
fn knowledge_vectors_rank_within_model_and_scope() {
    let dir = tempdir().expect("tempdir");
    let store = SqliteKnowledgeStore::new(dir.path().join("knowledge.sqlite3"));
    store
        .upsert_space(&space(
            "system-linux",
            KnowledgeSpaceKind::System,
            "system",
            KnowledgeVisibility::Public,
            1,
        ))
        .expect("space");
    let first = document("system-linux", "system", KnowledgeVisibility::Public);
    store.upsert_document(&first).expect("document");
    let first_chunk = chunk(
        &first.document_id,
        "system",
        KnowledgeVisibility::Public,
        "systemctl status shows the current unit state",
    );
    store.upsert_chunk(&first_chunk).expect("chunk");

    let second = KnowledgeDocument {
        document_id: "system-linux-doc-2".to_string(),
        title: "network reference".to_string(),
        ..first.clone()
    };
    store.upsert_document(&second).expect("second document");
    let second_chunk = KnowledgeChunk {
        chunk_id: "system-linux-doc-2-chunk".to_string(),
        document_id: second.document_id.clone(),
        content: "ip route prints the routing table".to_string(),
        ..first_chunk.clone()
    };
    store.upsert_chunk(&second_chunk).expect("second chunk");

    store
        .upsert_vector(&KnowledgeVector {
            chunk_id: first_chunk.chunk_id.clone(),
            space_id: "system-linux".to_string(),
            embedding_model: "fixture-v1".to_string(),
            generation: 1,
            vector: vec![1.0, 0.0],
        })
        .expect("first vector");
    store
        .upsert_vector(&KnowledgeVector {
            chunk_id: second_chunk.chunk_id.clone(),
            space_id: "system-linux".to_string(),
            embedding_model: "fixture-v1".to_string(),
            generation: 1,
            vector: vec![0.0, 1.0],
        })
        .expect("second vector");
    store
        .upsert_vector(&KnowledgeVector {
            chunk_id: second_chunk.chunk_id.clone(),
            space_id: "system-linux".to_string(),
            embedding_model: "other-model".to_string(),
            generation: 1,
            vector: vec![1.0, 0.0],
        })
        .expect("other model vector");

    let scope = KnowledgeSearchScope {
        space_id: "system-linux".to_string(),
        owner: "system".to_string(),
        generation: 1,
        visibility: KnowledgeVisibility::Public,
    };
    let matches = store
        .search_vectors(&[1.0, 0.0], "fixture-v1", &scope, 10)
        .expect("vector search");
    assert_eq!(matches.len(), 2);
    assert_eq!(matches[0].chunk_id, first_chunk.chunk_id);
    assert_eq!(matches[0].metadata_json, first.metadata_json);
    assert!(matches[0].score > matches[1].score);
    assert!(
        matches
            .iter()
            .all(|item| item.embedding_model == "fixture-v1")
    );
}

#[test]
fn knowledge_vectors_reject_invalid_values_and_mismatched_chunks() {
    let dir = tempdir().expect("tempdir");
    let store = SqliteKnowledgeStore::new(dir.path().join("knowledge.sqlite3"));
    store
        .upsert_space(&space(
            "system-linux",
            KnowledgeSpaceKind::System,
            "system",
            KnowledgeVisibility::Public,
            1,
        ))
        .expect("space");
    let document = document("system-linux", "system", KnowledgeVisibility::Public);
    store.upsert_document(&document).expect("document");
    let chunk = chunk(
        &document.document_id,
        "system",
        KnowledgeVisibility::Public,
        "systemctl status",
    );
    store.upsert_chunk(&chunk).expect("chunk");

    let base = KnowledgeVector {
        chunk_id: chunk.chunk_id.clone(),
        space_id: "system-linux".to_string(),
        embedding_model: "fixture-v1".to_string(),
        generation: 1,
        vector: vec![1.0, 0.0],
    };
    let mut non_finite = base.clone();
    non_finite.vector[0] = f32::NAN;
    assert!(store.upsert_vector(&non_finite).is_err());

    let mut wrong_space = base.clone();
    wrong_space.space_id = "other-space".to_string();
    assert!(store.upsert_vector(&wrong_space).is_err());

    let scope = KnowledgeSearchScope {
        space_id: "system-linux".to_string(),
        owner: "system".to_string(),
        generation: 1,
        visibility: KnowledgeVisibility::Public,
    };
    assert!(
        store
            .search_vectors(&[f32::INFINITY, 0.0], "fixture-v1", &scope, 1)
            .is_err()
    );
}

#[test]
fn ingest_text_replaces_stale_chunks_and_vectors_atomically() {
    let dir = tempdir().expect("tempdir");
    let store = SqliteKnowledgeStore::new(dir.path().join("knowledge.sqlite3"));
    store
        .upsert_space(&space(
            "system-linux",
            KnowledgeSpaceKind::System,
            "system",
            KnowledgeVisibility::Public,
            1,
        ))
        .expect("space");
    let document = KnowledgeDocument {
        document_id: "ingested-doc".to_string(),
        space_id: "system-linux".to_string(),
        title: "ingested command notes".to_string(),
        source: "fixture".to_string(),
        version: "2026.09".to_string(),
        generation: 1,
        owner: "system".to_string(),
        visibility: KnowledgeVisibility::Public,
        metadata_json: "{}".to_string(),
    };
    let options = KnowledgeChunkingOptions {
        max_input_chars: 100,
        max_chunk_chars: 20,
        overlap_chars: 2,
    };
    let first = store
        .ingest_text(&document, "old systemctl note", &options)
        .expect("first ingest");
    assert_eq!(first.chunks_written, 1);
    assert_eq!(first.chunks_removed, 0);
    store
        .upsert_vector(&KnowledgeVector {
            chunk_id: "ingested-doc#chunk-0".to_string(),
            space_id: "system-linux".to_string(),
            embedding_model: "fixture-v1".to_string(),
            generation: 1,
            vector: vec![1.0, 0.0],
        })
        .expect("old vector");

    let second = store
        .ingest_text(&document, "new ip route note", &options)
        .expect("second ingest");
    assert_eq!(second.chunks_written, 1);
    assert_eq!(second.chunks_removed, 1);
    let scope = KnowledgeSearchScope {
        space_id: "system-linux".to_string(),
        owner: "system".to_string(),
        generation: 1,
        visibility: KnowledgeVisibility::Public,
    };
    assert!(store.search("systemctl", &scope, 5).unwrap().is_empty());
    assert_eq!(store.search("ip route", &scope, 5).unwrap().len(), 1);
    assert!(
        store
            .search_vectors(&[1.0, 0.0], "fixture-v1", &scope, 5)
            .unwrap()
            .is_empty()
    );

    store
        .upsert_vector(&KnowledgeVector {
            chunk_id: "ingested-doc#chunk-0".to_string(),
            space_id: "system-linux".to_string(),
            embedding_model: "fixture-v1".to_string(),
            generation: 1,
            vector: vec![0.0, 1.0],
        })
        .expect("new vector");
    let unchanged = store
        .ingest_text(&document, "new ip route note", &options)
        .expect("unchanged ingest");
    assert_eq!(unchanged.chunks_written, 0);
    assert_eq!(unchanged.chunks_removed, 0);
    assert_eq!(
        store
            .search_vectors(&[0.0, 1.0], "fixture-v1", &scope, 5)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn replace_document_vectors_is_atomic_and_removes_stale_model_rows() {
    let dir = tempdir().expect("tempdir");
    let store = SqliteKnowledgeStore::new(dir.path().join("knowledge.sqlite3"));
    store
        .upsert_space(&space(
            "system-linux",
            KnowledgeSpaceKind::System,
            "system",
            KnowledgeVisibility::Public,
            1,
        ))
        .expect("space");
    let document = document("system-linux", "system", KnowledgeVisibility::Public);
    store.upsert_document(&document).expect("document");
    let first_chunk = chunk(
        &document.document_id,
        "system",
        KnowledgeVisibility::Public,
        "systemctl status",
    );
    let second_chunk = KnowledgeChunk {
        chunk_id: format!("{}-chunk-2", document.document_id),
        ordinal: 1,
        content: "ip route show".to_string(),
        ..first_chunk.clone()
    };
    store.upsert_chunk(&first_chunk).expect("first chunk");
    store.upsert_chunk(&second_chunk).expect("second chunk");
    let original = vec![
        KnowledgeVector {
            chunk_id: first_chunk.chunk_id.clone(),
            space_id: "system-linux".to_string(),
            embedding_model: "fixture-v1".to_string(),
            generation: 1,
            vector: vec![1.0, 0.0],
        },
        KnowledgeVector {
            chunk_id: second_chunk.chunk_id.clone(),
            space_id: "system-linux".to_string(),
            embedding_model: "fixture-v1".to_string(),
            generation: 1,
            vector: vec![0.0, 1.0],
        },
    ];
    assert_eq!(
        store
            .replace_document_vectors(&document.document_id, "fixture-v1", 1, &original)
            .expect("initial vector batch"),
        2
    );

    let replacement = vec![KnowledgeVector {
        chunk_id: second_chunk.chunk_id.clone(),
        space_id: "system-linux".to_string(),
        embedding_model: "fixture-v1".to_string(),
        generation: 1,
        vector: vec![0.5, 0.5],
    }];
    assert_eq!(
        store
            .replace_document_vectors(&document.document_id, "fixture-v1", 1, &replacement)
            .expect("replacement vector batch"),
        1
    );
    let scope = KnowledgeSearchScope {
        space_id: "system-linux".to_string(),
        owner: "system".to_string(),
        generation: 1,
        visibility: KnowledgeVisibility::Public,
    };
    let matches = store
        .search_vectors(&[0.5, 0.5], "fixture-v1", &scope, 10)
        .expect("vector search");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].chunk_id, second_chunk.chunk_id);

    let invalid = KnowledgeVector {
        chunk_id: first_chunk.chunk_id.clone(),
        space_id: "wrong-space".to_string(),
        embedding_model: "fixture-v1".to_string(),
        generation: 1,
        vector: vec![1.0, 0.0],
    };
    assert!(
        store
            .replace_document_vectors(&document.document_id, "fixture-v1", 1, &[invalid])
            .is_err()
    );
    let matches_after_error = store
        .search_vectors(&[0.5, 0.5], "fixture-v1", &scope, 10)
        .expect("vector search after rejected batch");
    assert_eq!(matches_after_error.len(), 1);
    assert_eq!(matches_after_error[0].chunk_id, second_chunk.chunk_id);
}

#[test]
fn local_embedding_indexes_knowledge_chunks_without_touching_memory_vectors() {
    let dir = tempdir().expect("tempdir");
    let store = SqliteKnowledgeStore::new(dir.path().join("knowledge.sqlite3"));
    store
        .upsert_space(&space(
            "system-linux",
            KnowledgeSpaceKind::System,
            "system",
            KnowledgeVisibility::Public,
            1,
        ))
        .expect("space");
    let document = document("system-linux", "system", KnowledgeVisibility::Public);
    store.upsert_document(&document).expect("document");
    let mut knowledge_chunk = chunk(
        &document.document_id,
        "system",
        KnowledgeVisibility::Public,
        "systemctl status shows a service state",
    );
    store.upsert_chunk(&knowledge_chunk).expect("chunk");

    let provider = LocalChargramEmbedding::default();
    let summary = store
        .index_document_with_embeddings(&document.document_id, &provider)
        .expect("knowledge embeddings");
    assert_eq!(summary.chunks_indexed, 1);
    assert_eq!(summary.embedding_model, provider.model_id());
    assert_eq!(summary.dimensions, provider.dimensions());
    let unchanged = store
        .index_document_with_embeddings(&document.document_id, &provider)
        .expect("unchanged knowledge embeddings");
    assert_eq!(unchanged.chunks_indexed, 0);
    knowledge_chunk.content = "systemctl restart changes the service state".to_string();
    store.upsert_chunk(&knowledge_chunk).expect("changed chunk");
    let rebuilt = store
        .index_document_with_embeddings(&document.document_id, &provider)
        .expect("rebuilt knowledge embeddings");
    assert_eq!(rebuilt.chunks_indexed, 1);
    let query = provider.embed("systemctl status").expect("query embedding");
    let matches = store
        .search_vectors(
            &query.values,
            provider.model_id(),
            &KnowledgeSearchScope {
                space_id: "system-linux".to_string(),
                owner: "system".to_string(),
                generation: 1,
                visibility: KnowledgeVisibility::Public,
            },
            5,
        )
        .expect("knowledge vector search");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].document_id, document.document_id);
}

#[derive(Clone)]
struct MutatingEmbeddingProvider {
    store: SqliteKnowledgeStore,
    replacement: KnowledgeChunk,
}

impl MemoryEmbeddingProvider for MutatingEmbeddingProvider {
    fn model_id(&self) -> &str {
        "fixture-mutating-v1"
    }

    fn dimensions(&self) -> usize {
        2
    }

    fn embed(&self, _text: &str) -> Result<MemoryEmbedding, MemoryEmbeddingError> {
        self.store
            .upsert_chunk(&self.replacement)
            .map_err(|error| MemoryEmbeddingError::Provider(error.to_string()))?;
        MemoryEmbedding::new(self.model_id(), vec![1.0, 0.0])
    }
}

#[test]
fn embedding_index_rejects_chunks_changed_during_embedding() {
    let dir = tempdir().expect("tempdir");
    let store = SqliteKnowledgeStore::new(dir.path().join("knowledge.sqlite3"));
    store
        .upsert_space(&space(
            "system-linux",
            KnowledgeSpaceKind::System,
            "system",
            KnowledgeVisibility::Public,
            1,
        ))
        .expect("space");
    let document = document("system-linux", "system", KnowledgeVisibility::Public);
    store.upsert_document(&document).expect("document");
    let original = chunk(
        &document.document_id,
        "system",
        KnowledgeVisibility::Public,
        "systemctl original content",
    );
    store.upsert_chunk(&original).expect("chunk");
    let mut replacement = original.clone();
    replacement.content = "systemctl changed during embedding".to_string();
    let provider = MutatingEmbeddingProvider {
        store: store.clone(),
        replacement,
    };

    let result = store.index_document_with_embeddings(&document.document_id, &provider);
    assert!(result.is_err());
    let matches = store
        .search_vectors(
            &[1.0, 0.0],
            provider.model_id(),
            &KnowledgeSearchScope {
                space_id: "system-linux".to_string(),
                owner: "system".to_string(),
                generation: 1,
                visibility: KnowledgeVisibility::Public,
            },
            5,
        )
        .expect("vector search");
    assert!(matches.is_empty());
}
