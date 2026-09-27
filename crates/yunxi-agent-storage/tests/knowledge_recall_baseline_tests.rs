use tempfile::tempdir;
use yunxi_agent_persona::{LocalChargramEmbedding, MemoryEmbeddingProvider};
use yunxi_agent_storage::{
    KnowledgeAccessContext, KnowledgeDocument, KnowledgeSearchResult, KnowledgeSearchScope,
    KnowledgeSpaceKind, KnowledgeSpaceSpec, KnowledgeVectorMatch, KnowledgeVisibility,
    SqliteKnowledgeStore,
};

const DOCUMENT_SOURCE: &str = "offline-evaluation";
const DOCUMENT_VERSION: &str = "mixed";
const SOURCE_VERSION: &str = "ubuntu-24.04";

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
    ("curl", "network transfer"),
    ("ssh", "remote access"),
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

fn seed_baseline_store(store: &SqliteKnowledgeStore) {
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
                    source: DOCUMENT_SOURCE.to_string(),
                    version: DOCUMENT_VERSION.to_string(),
                    generation: 1,
                    owner: "system".to_string(),
                    visibility: KnowledgeVisibility::Public,
                    metadata_json: format!(
                        "{{\"collector\":\"linux.command_help\",\"source_type\":\"help\",\"source_version\":\"{SOURCE_VERSION}\",\"risk_class\":\"{}\"}}",
                        risk_class(command)
                    ),
                },
                &content,
                &Default::default(),
            )
            .expect("document");
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct RetrievalMetrics {
    total: usize,
    top1: usize,
    top5: usize,
    reciprocal_rank_sum: f64,
    source_ok: usize,
    version_ok: usize,
    risk_ok: usize,
}

impl RetrievalMetrics {
    fn record<T>(&mut self, expected: &str, expected_risk: &str, results: &[T])
    where
        T: EvaluationResult,
    {
        self.total += 1;
        if let Some(item) = results.first() {
            if item.source() == DOCUMENT_SOURCE {
                self.source_ok += 1;
            }
            if item.version() == DOCUMENT_VERSION {
                self.version_ok += 1;
            }
            if item.risk_class() == expected_risk {
                self.risk_ok += 1;
            }
        }
        let Some(rank) = results
            .iter()
            .position(|item| item.document_id() == expected)
        else {
            return;
        };
        let rank = rank + 1;
        if rank == 1 {
            self.top1 += 1;
        }
        if rank <= 5 {
            self.top5 += 1;
            self.reciprocal_rank_sum += 1.0 / rank as f64;
        }
    }

    fn recall_at_1(self) -> f64 {
        self.top1 as f64 / self.total as f64
    }

    fn recall_at_5(self) -> f64 {
        self.top5 as f64 / self.total as f64
    }

    fn mrr(self) -> f64 {
        self.reciprocal_rank_sum / self.total as f64
    }

    fn source_accuracy(self) -> f64 {
        self.source_ok as f64 / self.total as f64
    }

    fn version_accuracy(self) -> f64 {
        self.version_ok as f64 / self.total as f64
    }

    fn risk_accuracy(self) -> f64 {
        self.risk_ok as f64 / self.total as f64
    }
}

trait EvaluationResult {
    fn document_id(&self) -> &str;
    fn source(&self) -> &str;
    fn version(&self) -> &str;
    fn risk_class(&self) -> &str;
}

impl EvaluationResult for KnowledgeSearchResult {
    fn document_id(&self) -> &str {
        &self.document_id
    }

    fn source(&self) -> &str {
        &self.source
    }

    fn version(&self) -> &str {
        &self.version
    }

    fn risk_class(&self) -> &str {
        metadata_risk_class(&self.metadata_json)
    }
}

impl EvaluationResult for KnowledgeVectorMatch {
    fn document_id(&self) -> &str {
        &self.document_id
    }

    fn source(&self) -> &str {
        &self.source
    }

    fn version(&self) -> &str {
        &self.version
    }

    fn risk_class(&self) -> &str {
        metadata_risk_class(&self.metadata_json)
    }
}

#[derive(Clone, Debug)]
struct HybridResult {
    document_id: String,
    source: String,
    version: String,
    risk_class: String,
    score: f64,
    chunk_id: String,
}

impl EvaluationResult for HybridResult {
    fn document_id(&self) -> &str {
        &self.document_id
    }

    fn source(&self) -> &str {
        &self.source
    }

    fn version(&self) -> &str {
        &self.version
    }

    fn risk_class(&self) -> &str {
        &self.risk_class
    }
}

fn metadata_risk_class(metadata_json: &str) -> &str {
    metadata_json
        .split("\"risk_class\":\"")
        .nth(1)
        .and_then(|value| value.split('"').next())
        .unwrap_or("unknown")
}

fn risk_class(command: &str) -> &'static str {
    match command {
        "cat" | "find" | "grep" | "ip" | "ls" | "curl" | "ssh" | "journalctl" | "ps" | "top"
        | "free" | "df" | "du" => "read_only",
        "rm" => "destructive",
        "cp" | "pacman" | "sed" | "systemctl" | "tar" | "chmod" => "mutating",
        "awk" | "bash" | "fish" | "git" | "dnf" | "apt" | "systemd-analyze" => "mixed",
        _ => "unknown",
    }
}

fn normalize_bm25(rank: f64, matches: &[KnowledgeSearchResult]) -> f64 {
    let finite = matches
        .iter()
        .map(|item| item.rank)
        .filter(|value| value.is_finite())
        .collect::<Vec<_>>();
    let Some(best) = finite.iter().copied().min_by(f64::total_cmp) else {
        return 0.0;
    };
    let Some(worst) = finite.iter().copied().max_by(f64::total_cmp) else {
        return 0.0;
    };
    let span = worst - best;
    if !rank.is_finite() {
        0.0
    } else if span.abs() <= f64::EPSILON {
        1.0
    } else {
        ((worst - rank) / span).clamp(0.0, 1.0)
    }
}

fn normalize_cosine(score: f32) -> f64 {
    if score.is_finite() {
        (f64::from(score.clamp(-1.0, 1.0)) + 1.0) / 2.0
    } else {
        0.0
    }
}

fn hybrid_results(
    keyword_matches: &[KnowledgeSearchResult],
    vector_matches: &[KnowledgeVectorMatch],
) -> Vec<HybridResult> {
    let mut merged = std::collections::BTreeMap::<String, HybridResult>::new();
    for item in keyword_matches {
        let score = 0.5 * normalize_bm25(item.rank, keyword_matches);
        merged
            .entry(item.chunk_id.clone())
            .and_modify(|existing| existing.score += score)
            .or_insert_with(|| HybridResult {
                document_id: item.document_id.clone(),
                source: item.source.clone(),
                version: item.version.clone(),
                risk_class: metadata_risk_class(&item.metadata_json).to_string(),
                score,
                chunk_id: item.chunk_id.clone(),
            });
    }
    for item in vector_matches {
        let score = 0.5 * normalize_cosine(item.score);
        merged
            .entry(item.chunk_id.clone())
            .and_modify(|existing| existing.score += score)
            .or_insert_with(|| HybridResult {
                document_id: item.document_id.clone(),
                source: item.source.clone(),
                version: item.version.clone(),
                risk_class: metadata_risk_class(&item.metadata_json).to_string(),
                score,
                chunk_id: item.chunk_id.clone(),
            });
    }
    let mut results = merged.into_values().collect::<Vec<_>>();
    results.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });
    results
}

fn assert_metrics(
    label: &str,
    metrics: RetrievalMetrics,
    min_recall_at_5: f64,
    min_mrr: f64,
    min_risk_accuracy: f64,
) {
    eprintln!(
        "{label}: total={} recall@1={:.3} recall@5={:.3} mrr={:.3} source={:.3} version={:.3} risk={:.3}",
        metrics.total,
        metrics.recall_at_1(),
        metrics.recall_at_5(),
        metrics.mrr(),
        metrics.source_accuracy(),
        metrics.version_accuracy(),
        metrics.risk_accuracy()
    );
    assert_eq!(metrics.total, 220, "{label} task count changed");
    assert!(
        metrics.recall_at_5() >= min_recall_at_5,
        "{label} Recall@5 below threshold"
    );
    assert!(metrics.mrr() >= min_mrr, "{label} MRR below threshold");
    assert!(
        metrics.source_accuracy() >= 0.99,
        "{label} source accuracy below threshold"
    );
    assert!(
        metrics.version_accuracy() >= 0.99,
        "{label} version accuracy below threshold"
    );
    assert!(
        metrics.risk_accuracy() >= min_risk_accuracy,
        "{label} risk accuracy below threshold"
    );
}

#[test]
fn linux_knowledge_recall_baseline_reports_220_task_metrics() {
    let dir = tempdir().expect("tempdir");
    let store = SqliteKnowledgeStore::new(dir.path().join("knowledge.sqlite3"));
    seed_baseline_store(&store);

    let scope = scope();
    let mut metrics = RetrievalMetrics::default();
    for (command, _) in COMMANDS {
        let expected = format!("eval-linux-{command}");
        for intent in INTENTS {
            let results = store
                .search_versioned(&format!("{command} {intent}"), &scope, None, 5)
                .expect("search");
            assert!(!results.is_empty(), "no result for {command} / {intent}");
            metrics.record(&expected, risk_class(command), &results);
        }
    }
    assert_metrics("lexical", metrics, 1.0, 1.0, 0.99);
}

#[test]
fn linux_knowledge_vector_and_hybrid_recall_report_220_task_metrics() {
    let dir = tempdir().expect("tempdir");
    let store = SqliteKnowledgeStore::new(dir.path().join("knowledge.sqlite3"));
    seed_baseline_store(&store);
    let provider = LocalChargramEmbedding::default();

    for (command, _) in COMMANDS {
        store
            .index_document_with_embeddings(&format!("eval-linux-{command}"), &provider)
            .expect("vector index");
    }

    let scope = scope();
    let mut vector_metrics = RetrievalMetrics::default();
    let mut hybrid_metrics = RetrievalMetrics::default();
    for (command, _) in COMMANDS {
        let expected = format!("eval-linux-{command}");
        for intent in INTENTS {
            let query = provider
                .embed(&format!("{command} {intent}"))
                .expect("query embedding");
            let results = store
                .search_vectors_versioned(&query.values, provider.model_id(), &scope, None, 5)
                .expect("vector search");
            assert!(
                !results.is_empty(),
                "no vector result for {command} / {intent}"
            );
            vector_metrics.record(&expected, risk_class(command), &results);
            let keyword_results = store
                .search_versioned(&format!("{command} {intent}"), &scope, None, 5)
                .expect("keyword search");
            let hybrid = hybrid_results(&keyword_results, &results);
            hybrid_metrics.record(&expected, risk_class(command), &hybrid);
        }
    }

    assert_metrics("vector-only", vector_metrics, 0.85, 0.60, 0.60);
    assert_metrics("hybrid", hybrid_metrics, 0.95, 0.85, 0.90);
    assert!(
        hybrid_metrics.recall_at_1() >= vector_metrics.recall_at_1(),
        "hybrid Recall@1 regressed below vector-only"
    );
}

#[test]
fn linux_knowledge_integrity_metrics_cover_freshness_isolation_and_retraction() {
    let dir = tempdir().expect("tempdir");
    let store = SqliteKnowledgeStore::new(dir.path().join("knowledge.sqlite3"));
    seed_baseline_store(&store);

    let fresh_generation = 2;
    store
        .upsert_space(&KnowledgeSpaceSpec {
            space_id: "system-linux".to_string(),
            kind: KnowledgeSpaceKind::System,
            owner: "system".to_string(),
            visibility: KnowledgeVisibility::Public,
            source: "offline-evaluation".to_string(),
            version: "mixed".to_string(),
            generation: fresh_generation,
        })
        .expect("fresh space");
    store
        .ingest_text(
            &KnowledgeDocument {
                document_id: "eval-linux-systemctl".to_string(),
                space_id: "system-linux".to_string(),
                title: "systemctl fresh reference".to_string(),
                source: DOCUMENT_SOURCE.to_string(),
                version: DOCUMENT_VERSION.to_string(),
                generation: fresh_generation,
                owner: "system".to_string(),
                visibility: KnowledgeVisibility::Public,
                metadata_json: format!(
                    "{{\"collector\":\"linux.command_help\",\"source_version\":\"{SOURCE_VERSION}\",\"risk_class\":\"mutating\"}}"
                ),
            },
            "fresh-generation-marker: systemctl now documents the updated behavior",
            &Default::default(),
        )
        .expect("fresh document");
    let fresh_scope = KnowledgeSearchScope {
        generation: fresh_generation,
        ..scope()
    };
    let fresh = store
        .search_versioned("fresh-generation-marker", &fresh_scope, None, 5)
        .expect("fresh search");
    assert_eq!(fresh.len(), 1);
    assert_eq!(fresh[0].document_id, "eval-linux-systemctl");

    store
        .upsert_space(&KnowledgeSpaceSpec {
            space_id: "private-evaluation".to_string(),
            kind: KnowledgeSpaceKind::Private,
            owner: "owner-a".to_string(),
            visibility: KnowledgeVisibility::Private,
            source: "offline-evaluation".to_string(),
            version: "fixture".to_string(),
            generation: 1,
        })
        .expect("private space");
    let private_scope = KnowledgeSearchScope {
        space_id: "private-evaluation".to_string(),
        owner: "owner-a".to_string(),
        generation: 1,
        visibility: KnowledgeVisibility::Private,
    };
    store
        .ingest_text(
            &KnowledgeDocument {
                document_id: "private-evaluation-doc".to_string(),
                space_id: private_scope.space_id.clone(),
                title: "private evaluation".to_string(),
                source: "offline-evaluation".to_string(),
                version: "fixture".to_string(),
                generation: 1,
                owner: "owner-a".to_string(),
                visibility: KnowledgeVisibility::Private,
                metadata_json: "{\"collector\":\"private.import\",\"risk_class\":\"read_only\"}"
                    .to_string(),
            },
            "private isolation marker",
            &Default::default(),
        )
        .expect("private document");
    let unauthorized = KnowledgeAccessContext::new("owner-b").expect("access context");
    assert!(
        store
            .accessible_space_scope("private-evaluation", &unauthorized)
            .is_err()
    );

    let provider = LocalChargramEmbedding::default();
    store
        .index_document_with_embeddings("private-evaluation-doc", &provider)
        .expect("private vector");
    assert!(
        store
            .retract_document("private-evaluation-doc", &private_scope)
            .expect("retract")
    );
    let after_retract = store
        .search_versioned("private isolation marker", &private_scope, None, 5)
        .expect("retracted lexical search");
    let query = provider
        .embed("private isolation marker")
        .expect("retraction query embedding");
    let after_retract_vectors = store
        .search_vectors_versioned(&query.values, provider.model_id(), &private_scope, None, 5)
        .expect("retracted vector search");
    assert!(after_retract.is_empty());
    assert!(after_retract_vectors.is_empty());
    let reintroduced = store.ingest_text(
        &KnowledgeDocument {
            document_id: "private-evaluation-doc".to_string(),
            space_id: private_scope.space_id.clone(),
            title: "private evaluation".to_string(),
            source: "offline-evaluation".to_string(),
            version: "fixture".to_string(),
            generation: 1,
            owner: "owner-a".to_string(),
            visibility: KnowledgeVisibility::Private,
            metadata_json: "{\"collector\":\"private.import\",\"risk_class\":\"read_only\"}"
                .to_string(),
        },
        "reintroduced marker",
        &Default::default(),
    );
    assert!(reintroduced.is_err(), "retracted document was reintroduced");
    eprintln!("integrity: freshness=1/1 isolation=1/1 retraction=3/3");
}
