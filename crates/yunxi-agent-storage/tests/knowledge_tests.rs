use rusqlite::Connection;
use tempfile::tempdir;
use yunxi_agent_storage::{
    KnowledgeChunk, KnowledgeDocument, KnowledgeSearchScope, KnowledgeSpaceKind,
    KnowledgeSpaceSpec, KnowledgeVisibility, SqliteKnowledgeStore,
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
