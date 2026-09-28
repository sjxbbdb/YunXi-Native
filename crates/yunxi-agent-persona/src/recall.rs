use crate::dedup::dedup_key_for_record;
use crate::memory::{
    MemoryKind, MemoryRecallRequest, MemoryRecallResult, MemoryRecord, MemoryScope, now_millis,
};
use std::collections::BTreeMap;

pub(crate) const RECALL_RELEVANCE_THRESHOLD: f32 = 2.0;
const ALWAYS_ON_LIMIT: usize = 3;

#[derive(Clone, Debug, Default)]
pub struct MemoryRecallEngine;

impl MemoryRecallEngine {
    pub fn recall(
        &self,
        records: &[MemoryRecord],
        request: &MemoryRecallRequest,
    ) -> MemoryRecallResult {
        self.recall_internal(records, request, true, None, 0)
    }

    pub(crate) fn recall_prompt_relevant_with_semantic(
        &self,
        records: &[MemoryRecord],
        request: &MemoryRecallRequest,
        semantic_scores_per_mille: &BTreeMap<String, u16>,
        semantic_min_score_per_mille: u16,
    ) -> MemoryRecallResult {
        self.recall_internal(
            records,
            request,
            false,
            Some(semantic_scores_per_mille),
            semantic_min_score_per_mille,
        )
    }

    fn recall_internal(
        &self,
        records: &[MemoryRecord],
        request: &MemoryRecallRequest,
        include_always_on: bool,
        semantic_scores_per_mille: Option<&BTreeMap<String, u16>>,
        semantic_min_score_per_mille: u16,
    ) -> MemoryRecallResult {
        let (records, dropped_duplicates) = collapse_duplicate_records(records, request);
        let mut dropped_unrelated = 0;
        let mut always_on_count = 0;
        let mut scored = records
            .iter()
            .filter_map(|record| {
                if include_always_on && is_always_on_profile_record(record) {
                    if always_on_count < ALWAYS_ON_LIMIT {
                        always_on_count += 1;
                        return Some((
                            RECALL_RELEVANCE_THRESHOLD + record.importance,
                            record.clone(),
                        ));
                    }
                    dropped_unrelated += 1;
                    return None;
                }
                let score = score_record_with_semantic(
                    record,
                    request,
                    semantic_scores_per_mille,
                    semantic_min_score_per_mille,
                );
                if !request.query.trim().is_empty() && score >= RECALL_RELEVANCE_THRESHOLD {
                    Some((score, record.clone()))
                } else {
                    dropped_unrelated += 1;
                    None
                }
            })
            .collect::<Vec<_>>();
        scored.sort_by(|left, right| {
            right
                .0
                .partial_cmp(&left.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| right.1.updated_at_millis.cmp(&left.1.updated_at_millis))
        });

        let mut records = Vec::new();
        let mut budget_used_chars = 0;
        let mut truncated = false;
        let mut dropped_by_budget = 0;
        for (_, record) in scored {
            if records.len() >= request.max_records {
                truncated = true;
                dropped_by_budget += 1;
                continue;
            }
            let len = record.content.chars().count();
            if budget_used_chars + len > request.budget_chars {
                truncated = true;
                dropped_by_budget += 1;
                continue;
            }
            budget_used_chars += len;
            records.push(record);
        }

        MemoryRecallResult {
            records,
            budget_used_chars,
            truncated,
            always_on_count,
            dropped_unrelated,
            dropped_by_budget,
            dropped_duplicates,
        }
    }
}

fn collapse_duplicate_records(
    records: &[MemoryRecord],
    request: &MemoryRecallRequest,
) -> (Vec<MemoryRecord>, usize) {
    let mut by_key: BTreeMap<String, MemoryRecord> = BTreeMap::new();
    let mut dropped_duplicates = 0;
    let now = now_millis();
    for record in records
        .iter()
        .filter(|record| record.is_recallable_at(now))
        .filter(|record| scope_matches(&record.scope, request.workspace_fingerprint.as_deref()))
    {
        let key = if record.dedup_key.trim().is_empty() {
            dedup_key_for_record(record).as_storage_key()
        } else {
            record.dedup_key.clone()
        };
        match by_key.get_mut(&key) {
            Some(existing) => {
                dropped_duplicates += 1;
                if should_replace_duplicate(existing, record) {
                    *existing = record.clone();
                }
            }
            None => {
                by_key.insert(key, record.clone());
            }
        }
    }
    (by_key.into_values().collect(), dropped_duplicates)
}

fn should_replace_duplicate(existing: &MemoryRecord, candidate: &MemoryRecord) -> bool {
    candidate.updated_at_millis > existing.updated_at_millis
        || (candidate.updated_at_millis == existing.updated_at_millis
            && candidate.revision > existing.revision)
        || (candidate.updated_at_millis == existing.updated_at_millis
            && candidate.revision == existing.revision
            && candidate.importance > existing.importance)
}

fn scope_matches(scope: &MemoryScope, workspace_fingerprint: Option<&str>) -> bool {
    match scope {
        MemoryScope::GlobalUser | MemoryScope::AgentIdentity | MemoryScope::Relationship => true,
        MemoryScope::Workspace { root_fingerprint } => {
            workspace_fingerprint == Some(root_fingerprint.as_str())
        }
    }
}

pub(crate) fn score_record(record: &MemoryRecord, request: &MemoryRecallRequest) -> f32 {
    let mut score = 0.0;
    let query = request.query.to_ascii_lowercase();
    let content = record.content.to_ascii_lowercase();
    let query_terms = recall_query_terms(&query);
    for term in query_terms.iter().filter(|term| term.len() >= 2) {
        if content.contains(term) {
            // A matched CJK bigram is a stronger signal than one broad
            // character but slightly weaker than an exact ASCII token. This
            // keeps Chinese paraphrases useful without making every common
            // one-character word a recall trigger.
            score += if term.chars().all(is_cjk) { 1.5 } else { 2.0 };
        }
    }
    let compact_query = query.split_whitespace().collect::<String>();
    if compact_query.chars().count() >= 2 && content.contains(&compact_query) {
        score += 1.5;
    }
    if matches!(record.scope, MemoryScope::Workspace { .. }) {
        score += 0.5;
    }
    if kind_trigger_matches(record.kind, &query, &content) {
        score += 2.0;
    }
    if score > 0.0 {
        score += record.importance + record.confidence * 0.5;
    }
    score
}

/// Build cheap, deterministic lexical terms for recall.
///
/// The previous implementation split only on ASCII whitespace. That works
/// for English, but a natural Chinese question such as "我的回答风格偏好" has
/// no spaces and therefore scored zero unless the vector path happened to
/// rescue it. We keep ASCII tokens intact and add adjacent CJK bigrams. The
/// latter gives us useful phrase-level matching without introducing a large
/// tokenizer dependency into the persona crate.
fn recall_query_terms(query: &str) -> Vec<String> {
    let mut terms = std::collections::BTreeSet::new();
    let mut ascii = String::new();
    let mut cjk = Vec::new();

    let flush_ascii = |terms: &mut std::collections::BTreeSet<String>, ascii: &mut String| {
        if ascii.chars().count() >= 2 {
            terms.insert(std::mem::take(ascii));
        } else {
            ascii.clear();
        }
    };
    let flush_cjk = |terms: &mut std::collections::BTreeSet<String>, cjk: &mut Vec<char>| {
        if cjk.len() >= 2 {
            terms.insert(cjk.iter().collect());
            for pair in cjk.windows(2) {
                terms.insert(pair.iter().collect());
            }
        }
        cjk.clear();
    };

    for character in query.chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.' | '+' | '#') {
            flush_cjk(&mut terms, &mut cjk);
            ascii.push(character);
        } else if is_cjk(character) {
            flush_ascii(&mut terms, &mut ascii);
            cjk.push(character);
        } else {
            flush_ascii(&mut terms, &mut ascii);
            flush_cjk(&mut terms, &mut cjk);
        }
    }
    flush_ascii(&mut terms, &mut ascii);
    flush_cjk(&mut terms, &mut cjk);
    terms.into_iter().collect()
}

fn is_cjk(character: char) -> bool {
    matches!(
        character as u32,
        0x3400..=0x4DBF
            | 0x4E00..=0x9FFF
            | 0xF900..=0xFAFF
            | 0x20000..=0x2FA1F
    )
}

pub(crate) fn score_record_with_semantic(
    record: &MemoryRecord,
    request: &MemoryRecallRequest,
    semantic_scores_per_mille: Option<&BTreeMap<String, u16>>,
    semantic_min_score_per_mille: u16,
) -> f32 {
    let mut score = score_record(record, request);
    if let Some(score_per_mille) = semantic_scores_per_mille
        .and_then(|scores| scores.get(&record.id))
        .copied()
        .filter(|score| *score >= semantic_min_score_per_mille)
    {
        let semantic_score = f32::from(score_per_mille) / 1_000.0;
        score = score.max(
            RECALL_RELEVANCE_THRESHOLD
                + semantic_score * 2.0
                + record.importance * 0.25
                + record.confidence * 0.1,
        );
    }
    score
}

fn is_always_on_profile_record(record: &MemoryRecord) -> bool {
    if !matches!(record.kind, MemoryKind::Preference) {
        return false;
    }
    if !matches!(
        record.scope,
        MemoryScope::GlobalUser | MemoryScope::Relationship
    ) {
        return false;
    }
    contains_any(
        &record.content,
        &[
            "中文", "英文", "语言", "回答", "交流", "称呼", "语气", "表达", "language", "reply",
            "answer", "tone",
        ],
    )
}

fn kind_trigger_matches(kind: MemoryKind, query: &str, content: &str) -> bool {
    match kind {
        MemoryKind::ProjectContext | MemoryKind::ToolTraceSummary => {
            contains_any(
                query,
                &[
                    "项目",
                    "仓库",
                    "代码",
                    "源码",
                    "workspace",
                    "repo",
                    "project",
                ],
            ) && contains_any(
                content,
                &[
                    "项目",
                    "仓库",
                    "代码",
                    "源码",
                    "workspace",
                    "repo",
                    "project",
                ],
            )
        }
        MemoryKind::PersonalFact => contains_any(
            query,
            &["我", "我的", "个人", "偏好", "profile", "me", "my"],
        ),
        MemoryKind::Goal => contains_any(query, &["目标", "计划", "下一步", "goal", "plan"]),
        MemoryKind::Correction => contains_any(
            query,
            &["要求", "约束", "不要", "纠正", "rule", "constraint"],
        ),
        MemoryKind::RelationshipNote | MemoryKind::EmotionalState => {
            contains_any(query, &["关系", "情绪", "感受", "relationship", "emotion"])
        }
        MemoryKind::Preference | MemoryKind::Event => false,
    }
}

fn contains_any(value: &str, needles: &[&str]) -> bool {
    let lower = value.to_ascii_lowercase();
    needles
        .iter()
        .any(|needle| value.contains(needle) || lower.contains(&needle.to_ascii_lowercase()))
}
