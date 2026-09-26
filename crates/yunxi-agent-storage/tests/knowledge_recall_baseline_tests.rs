use tempfile::tempdir;
use yunxi_agent_storage::{
    KnowledgeDocument, KnowledgeSearchScope, KnowledgeSpaceKind, KnowledgeSpaceSpec,
    KnowledgeVisibility, SqliteKnowledgeStore,
};

const COMMANDS: &[(&str, &str)] = &[
    ("systemctl", "service lifecycle"),
    ("journalctl", "system logs"),
    ("systemd-analyze", "boot diagnostics"),
    ("fish", "interactive shell"),
    ("git", "repository history"),
    ("pacman", "package management"),
    ("apt", "package management"),
    ("dnf", "package management"),
    ("ps", "process inspection"),
    ("top", "process inspection"),
    ("free", "memory inspection"),
    ("df", "filesystem capacity"),
    ("du", "directory usage"),
    ("find", "file discovery"),
    ("grep", "text search"),
    ("sed", "text transformation"),
    ("awk", "structured text"),
    ("tar", "archive management"),
    ("chmod", "file permissions"),
    ("ip", "network inspection"),
];

const INTENTS: &[&str] = &[
    "explain command",
    "check status",
    "read logs",
    "inspect network",
    "review permissions",
    "check disk",
    "list processes",
    "manage packages",
    "use safely",
    "troubleshoot failure",
];

fn scope() -> KnowledgeSearchScope {
    KnowledgeSearchScope {
        space_id: "system-linux".to_string(),
        owner: "system".to_string(),
        generation: 1,
        visibility: KnowledgeVisibility::Public,
    }
}

#[test]
fn linux_knowledge_recall_baseline_covers_two_hundred_tasks() {
    let dir = tempdir().expect("tempdir");
    let store = SqliteKnowledgeStore::new(dir.path().join("knowledge.sqlite3"));
    store
        .upsert_space(&KnowledgeSpaceSpec {
            space_id: "system-linux".to_string(),
            kind: KnowledgeSpaceKind::System,
            owner: "system".to_string(),
            visibility: KnowledgeVisibility::Public,
            source: "offline-evaluation".to_string(),
            version: "mixed".to_string(),
            generation: 1,
        })
        .expect("space");

    for (command, domain) in COMMANDS {
        let document_id = format!("eval-linux-{command}");
        let content = format!(
            "Linux command {command}. Domain: {domain}. This reference can explain command {command} for {domain}: explain command, check status, read logs, inspect network, review permissions, check disk, list processes, manage packages, use safely, troubleshoot failure."
        );
        store
            .ingest_text(
                &KnowledgeDocument {
                    document_id,
                    space_id: "system-linux".to_string(),
                    title: format!("{command} reference"),
                    source: "offline-evaluation".to_string(),
                    version: "fixture".to_string(),
                    generation: 1,
                    owner: "system".to_string(),
                    visibility: KnowledgeVisibility::Public,
                    metadata_json: format!(
                        "{{\"collector\":\"offline-evaluation\",\"domain\":\"{domain}\"}}"
                    ),
                },
                &content,
                &Default::default(),
            )
            .expect("document");
    }

    let scope = scope();
    let mut top1 = 0usize;
    let mut top5 = 0usize;
    let total = COMMANDS.len() * INTENTS.len();
    for (command, _) in COMMANDS {
        let expected = format!("eval-linux-{command}");
        for intent in INTENTS {
            let results = store
                .search_versioned(&format!("{command} {intent}"), &scope, None, 5)
                .expect("search");
            assert!(!results.is_empty(), "no result for {command} / {intent}");
            if results[0].document_id == expected {
                top1 += 1;
            }
            if results.iter().any(|item| item.document_id == expected) {
                top5 += 1;
            }
        }
    }

    assert_eq!(total, 200);
    assert_eq!(top5, total, "every evaluation task must recall its source");
    assert_eq!(
        top1, total,
        "the deterministic baseline must rank source first"
    );
}
