//! Read-only Linux planning evidence.
//!
//! This module is deliberately not an executor or a command planner.  It
//! only resolves the active knowledge generation into bounded, provenance-
//! carrying reference evidence for the provider prompt.  Tool routing,
//! approval, sandboxing, and command execution remain outside this boundary.

use crate::linux_source;
use std::time::Instant;
use yunxi_agent_core::AgentConfig;
use yunxi_agent_persona::{LocalChargramEmbedding, MemoryEmbeddingProvider};
use yunxi_agent_storage::{KnowledgeSearchResult, KnowledgeVectorMatch, SqliteKnowledgeStore};

const MAX_KEYWORD_EVIDENCE: usize = 4;
const MAX_VECTOR_EVIDENCE: usize = 4;
const MAX_EVIDENCE_CHARS: usize = 12_000;
const MAX_DIAGNOSTIC_PROVENANCE: usize = 8;

pub(crate) struct LinuxPlanContext {
    generation: i64,
    source_version: Option<String>,
    keyword_matches: Vec<KnowledgeSearchResult>,
    vector_matches: Vec<KnowledgeVectorMatch>,
    retrieval_latency_millis: u64,
}

impl LinuxPlanContext {
    #[cfg(test)]
    pub(crate) fn for_test(
        generation: i64,
        source_version: Option<String>,
        keyword_matches: Vec<KnowledgeSearchResult>,
        vector_matches: Vec<KnowledgeVectorMatch>,
    ) -> Self {
        Self {
            generation,
            source_version,
            keyword_matches,
            vector_matches,
            retrieval_latency_millis: 0,
        }
    }

    pub(crate) fn diagnostic(&self) -> crate::KnowledgeRecallDiagnostic {
        let mut provenance = self
            .keyword_matches
            .iter()
            .map(|item| knowledge_evidence_diagnostic("keyword", item, None))
            .collect::<Vec<_>>();
        provenance.extend(
            self.vector_matches
                .iter()
                .take(MAX_DIAGNOSTIC_PROVENANCE.saturating_sub(provenance.len()))
                .map(|item| knowledge_vector_evidence_diagnostic(item)),
        );
        crate::KnowledgeRecallDiagnostic {
            generation: self.generation,
            source_version: self.source_version.clone(),
            keyword_evidence: self.keyword_matches.len(),
            vector_evidence: self.vector_matches.len(),
            retrieval_latency_millis: self.retrieval_latency_millis,
            provenance,
        }
    }

    pub(crate) fn render(self) -> String {
        let mut sections = Vec::new();
        if !self.keyword_matches.is_empty() {
            sections.push(crate::format_linux_knowledge_context(&self.keyword_matches));
        }
        if !self.vector_matches.is_empty() {
            sections.push(crate::format_linux_knowledge_vector_context(
                &self.vector_matches,
            ));
        }
        let body = sections.join("\n");
        let mut context = format!(
            "[Linux planning evidence | active_generation={} | keyword={} | vector={}]\n{}",
            self.generation,
            self.keyword_matches.len(),
            self.vector_matches.len(),
            body
        );
        if context.chars().count() > MAX_EVIDENCE_CHARS {
            context = context.chars().take(MAX_EVIDENCE_CHARS).collect();
            context.push_str("\n[Linux planning evidence truncated at a fixed character budget]");
        }
        context
    }
}

fn knowledge_evidence_diagnostic(
    retrieval: &str,
    item: &KnowledgeSearchResult,
    score: Option<f32>,
) -> crate::KnowledgeEvidenceDiagnostic {
    let metadata = serde_json::from_str::<serde_json::Value>(&item.metadata_json).ok();
    crate::KnowledgeEvidenceDiagnostic {
        retrieval: retrieval.to_string(),
        chunk_id: item.chunk_id.clone(),
        document_id: item.document_id.clone(),
        space_id: item.space_id.clone(),
        source: item.source.clone(),
        version: item.version.clone(),
        generation: item.generation,
        collector: metadata_label(metadata.as_ref(), "collector"),
        risk_level: metadata_label(metadata.as_ref(), "risk_level"),
        score,
    }
}

fn knowledge_vector_evidence_diagnostic(
    item: &KnowledgeVectorMatch,
) -> crate::KnowledgeEvidenceDiagnostic {
    let metadata = serde_json::from_str::<serde_json::Value>(&item.metadata_json).ok();
    crate::KnowledgeEvidenceDiagnostic {
        retrieval: "vector".to_string(),
        chunk_id: item.chunk_id.clone(),
        document_id: item.document_id.clone(),
        space_id: item.space_id.clone(),
        source: item.source.clone(),
        version: item.version.clone(),
        generation: item.generation,
        collector: metadata_label(metadata.as_ref(), "collector"),
        risk_level: metadata_label(metadata.as_ref(), "risk_level"),
        score: Some(item.score),
    }
}

fn metadata_label(metadata: Option<&serde_json::Value>, key: &str) -> String {
    metadata
        .and_then(|value| value.get(key))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown")
        .to_string()
}

pub(crate) fn build(config: &AgentConfig, prompt: &str) -> Option<LinuxPlanContext> {
    if prompt.trim().chars().count() < 3 {
        return None;
    }
    let started = Instant::now();
    let store = SqliteKnowledgeStore::for_workspace(&config.cwd);
    let scope = store
        .active_space_scope(
            "system-linux",
            "system",
            yunxi_agent_storage::KnowledgeVisibility::Public,
        )
        .ok()
        .flatten()?;
    let source_version = linux_source::detect_source_version();
    let keyword_matches = store
        .search_versioned(
            prompt,
            &scope,
            source_version.as_deref(),
            MAX_KEYWORD_EVIDENCE,
        )
        .unwrap_or_default();
    let vector_matches = LocalChargramEmbedding::default()
        .embed(prompt)
        .ok()
        .and_then(|embedding| {
            store
                .search_vectors_versioned(
                    &embedding.values,
                    &embedding.model,
                    &scope,
                    source_version.as_deref(),
                    MAX_VECTOR_EVIDENCE,
                )
                .ok()
        })
        .unwrap_or_default();
    let vector_matches =
        crate::filter_duplicate_linux_vector_evidence(&keyword_matches, vector_matches);
    if keyword_matches.is_empty() && vector_matches.is_empty() {
        return None;
    }
    Some(LinuxPlanContext {
        generation: scope.generation,
        source_version,
        keyword_matches,
        vector_matches,
        retrieval_latency_millis: started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
    })
}
