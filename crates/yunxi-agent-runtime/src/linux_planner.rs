//! Read-only Linux planning evidence.
//!
//! This module is deliberately not an executor or a command planner.  It
//! only resolves the active knowledge generation into bounded, provenance-
//! carrying reference evidence for the provider prompt.  Tool routing,
//! approval, sandboxing, and command execution remain outside this boundary.

use crate::linux_source;
use yunxi_agent_core::AgentConfig;
use yunxi_agent_persona::{LocalChargramEmbedding, MemoryEmbeddingProvider};
use yunxi_agent_storage::{KnowledgeSearchResult, KnowledgeVectorMatch, SqliteKnowledgeStore};

const MAX_KEYWORD_EVIDENCE: usize = 4;
const MAX_VECTOR_EVIDENCE: usize = 4;
const MAX_EVIDENCE_CHARS: usize = 12_000;

pub(crate) struct LinuxPlanContext {
    generation: i64,
    keyword_matches: Vec<KnowledgeSearchResult>,
    vector_matches: Vec<KnowledgeVectorMatch>,
}

impl LinuxPlanContext {
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

pub(crate) fn build(config: &AgentConfig, prompt: &str) -> Option<LinuxPlanContext> {
    if prompt.trim().chars().count() < 3 {
        return None;
    }
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
        keyword_matches,
        vector_matches,
    })
}
