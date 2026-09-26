use std::fs;

use serde::Deserialize;
use tempfile::tempdir;
use yunxi_agent_storage::{
    KnowledgeChunk, KnowledgeDocument, KnowledgeSearchScope, KnowledgeSpaceKind,
    KnowledgeSpaceSpec, KnowledgeVector, KnowledgeVisibility, SqliteKnowledgeStore,
};

#[derive(Debug, Deserialize)]
struct FixtureDocument {
    document_id: String,
    version: String,
    title: String,
    content: String,
    metadata_json: String,
}

fn load_fixture() -> Vec<FixtureDocument> {
    let path = format!(
        "{}/tests/fixtures/knowledge-version-scope.jsonl",
        env!("CARGO_MANIFEST_DIR")
    );
    fs::read_to_string(path)
        .expect("fixture")
        .lines()
        .map(|line| serde_json::from_str(line).expect("fixture row"))
        .collect()
}

fn seed_fixture(store: &SqliteKnowledgeStore) -> KnowledgeSearchScope {
    store
        .upsert_space(&KnowledgeSpaceSpec {
            space_id: "system-linux".to_string(),
            kind: KnowledgeSpaceKind::System,
            owner: "system".to_string(),
            visibility: KnowledgeVisibility::Public,
            source: "fixture".to_string(),
            version: "mixed".to_string(),
            generation: 1,
        })
        .expect("space");
    for fixture in load_fixture() {
        store
            .upsert_document(&KnowledgeDocument {
                document_id: fixture.document_id.clone(),
                space_id: "system-linux".to_string(),
                title: fixture.title,
                source: "fixture".to_string(),
                version: fixture.version.clone(),
                generation: 1,
                owner: "system".to_string(),
                visibility: KnowledgeVisibility::Public,
                metadata_json: fixture.metadata_json.clone(),
            })
            .expect("document");
        store
            .upsert_chunk(&KnowledgeChunk {
                chunk_id: format!("{}#chunk-0", fixture.document_id),
                document_id: fixture.document_id,
                ordinal: 0,
                content: fixture.content,
                source: "fixture".to_string(),
                version: fixture.version,
                generation: 1,
                owner: "system".to_string(),
                visibility: KnowledgeVisibility::Public,
                metadata_json: fixture.metadata_json,
            })
            .expect("chunk");
    }
    KnowledgeSearchScope {
        space_id: "system-linux".to_string(),
        owner: "system".to_string(),
        generation: 1,
        visibility: KnowledgeVisibility::Public,
    }
}

#[test]
fn fixture_keeps_cross_release_fts_and_provenance_separate() {
    let dir = tempdir().expect("tempdir");
    let store = SqliteKnowledgeStore::new(dir.path().join("knowledge.sqlite3"));
    let scope = seed_fixture(&store);

    let all = store
        .search_versioned("systemctl status", &scope, None, 10)
        .expect("cross-release search");
    assert_eq!(all.len(), 2);
    assert!(
        all.iter()
            .all(|item| item.metadata_json.contains("collector"))
    );

    let ubuntu = store
        .search_versioned("systemctl status", &scope, Some("ubuntu-24.04"), 10)
        .expect("ubuntu search");
    assert_eq!(ubuntu.len(), 1);
    assert_eq!(ubuntu[0].version, "ubuntu-24.04");
    assert!(ubuntu[0].metadata_json.contains("linux.command_help"));

    let arch = store
        .search_versioned("systemctl status", &scope, Some("arch-rolling"), 10)
        .expect("arch search");
    assert_eq!(arch.len(), 1);
    assert_eq!(arch[0].version, "arch-rolling");
}

#[test]
fn fixture_vector_filter_update_and_retraction_are_deterministic() {
    let dir = tempdir().expect("tempdir");
    let store = SqliteKnowledgeStore::new(dir.path().join("knowledge.sqlite3"));
    let scope = seed_fixture(&store);
    store
        .upsert_vector(&KnowledgeVector {
            chunk_id: "eval-ubuntu-systemctl#chunk-0".to_string(),
            space_id: "system-linux".to_string(),
            embedding_model: "fixture-v1".to_string(),
            generation: 1,
            vector: vec![1.0, 0.0],
        })
        .expect("ubuntu vector");
    store
        .upsert_vector(&KnowledgeVector {
            chunk_id: "eval-arch-systemctl#chunk-0".to_string(),
            space_id: "system-linux".to_string(),
            embedding_model: "fixture-v1".to_string(),
            generation: 1,
            vector: vec![0.0, 1.0],
        })
        .expect("arch vector");

    let arch_vectors = store
        .search_vectors_versioned(&[0.0, 1.0], "fixture-v1", &scope, Some("arch-rolling"), 10)
        .expect("arch vectors");
    assert_eq!(arch_vectors.len(), 1);
    assert_eq!(arch_vectors[0].version, "arch-rolling");

    let ubuntu = KnowledgeDocument {
        document_id: "eval-ubuntu-systemctl".to_string(),
        space_id: "system-linux".to_string(),
        title: "systemctl updated on Ubuntu".to_string(),
        source: "fixture".to_string(),
        version: "ubuntu-24.04".to_string(),
        generation: 1,
        owner: "system".to_string(),
        visibility: KnowledgeVisibility::Public,
        metadata_json:
            "{\"collector\":\"linux.command_help\",\"risk_level\":\"read_only_reference\"}"
                .to_string(),
    };
    store
        .ingest_text(
            &ubuntu,
            "systemctl restart is a changed fixture phrase.",
            &Default::default(),
        )
        .expect("update");
    assert!(
        store
            .search_versioned("active service state", &scope, Some("ubuntu-24.04"), 10)
            .expect("old search")
            .is_empty()
    );
    assert_eq!(
        store
            .search_versioned("restart", &scope, Some("ubuntu-24.04"), 10)
            .expect("new search")
            .len(),
        1
    );

    assert!(
        store
            .retract_document("eval-arch-systemctl", &scope,)
            .expect("retract")
    );
    assert!(
        store
            .search_versioned("systemctl status", &scope, Some("arch-rolling"), 10)
            .expect("post-retract fts")
            .is_empty()
    );
    assert!(
        store
            .search_vectors_versioned(&[0.0, 1.0], "fixture-v1", &scope, Some("arch-rolling"), 10,)
            .expect("post-retract vectors")
            .is_empty()
    );
}
