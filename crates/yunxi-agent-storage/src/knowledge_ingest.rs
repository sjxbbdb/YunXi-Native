//! Deterministic, side-effect-free preparation of knowledge source text.
//!
//! This module deliberately stops before persistence and embedding. It turns
//! authorized source text into bounded chunks that can later be written to the
//! isolated knowledge store without reading arbitrary paths or invoking tools.

use yunxi_agent_core::{AgentError, AgentResult};

pub const DEFAULT_MAX_INPUT_CHARS: usize = 1_000_000;
pub const DEFAULT_MAX_CHUNK_CHARS: usize = 1_800;
pub const DEFAULT_OVERLAP_CHARS: usize = 200;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KnowledgeChunkingOptions {
    pub max_input_chars: usize,
    pub max_chunk_chars: usize,
    pub overlap_chars: usize,
}

impl Default for KnowledgeChunkingOptions {
    fn default() -> Self {
        Self {
            max_input_chars: DEFAULT_MAX_INPUT_CHARS,
            max_chunk_chars: DEFAULT_MAX_CHUNK_CHARS,
            overlap_chars: DEFAULT_OVERLAP_CHARS,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KnowledgeChunkDraft {
    pub ordinal: i64,
    pub content: String,
    pub content_hash: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KnowledgeIngestSummary {
    pub document_id: String,
    pub content_hash: String,
    pub chunks_written: usize,
    pub chunks_removed: usize,
}

pub fn normalize_knowledge_text(input: &str, max_input_chars: usize) -> AgentResult<String> {
    if max_input_chars == 0 {
        return Err(ingest_error("knowledge input limit must be positive"));
    }
    let cleaned = strip_terminal_controls(input);
    let mut normalized_lines = Vec::new();
    let mut consecutive_blank_lines = 0usize;
    for line in cleaned.lines() {
        let line = line.trim_end();
        if line.trim().is_empty() {
            consecutive_blank_lines += 1;
            if consecutive_blank_lines > 1 {
                continue;
            }
        } else {
            consecutive_blank_lines = 0;
        }
        normalized_lines.push(line);
    }
    let normalized = normalized_lines.join("\n").trim().to_string();
    if normalized.chars().count() > max_input_chars {
        return Err(ingest_error(format!(
            "knowledge input exceeds the {} character limit",
            max_input_chars
        )));
    }
    Ok(normalized)
}

pub fn chunk_knowledge_text(
    input: &str,
    options: &KnowledgeChunkingOptions,
) -> AgentResult<Vec<KnowledgeChunkDraft>> {
    if options.max_chunk_chars == 0 {
        return Err(ingest_error("knowledge chunk limit must be positive"));
    }
    if options.overlap_chars >= options.max_chunk_chars {
        return Err(ingest_error(
            "knowledge chunk overlap must be smaller than the chunk limit",
        ));
    }
    let text = normalize_knowledge_text(input, options.max_input_chars)?;
    if text.is_empty() {
        return Ok(Vec::new());
    }

    let mut chunks = Vec::new();
    let mut paragraph = String::new();
    for part in text.split("\n\n") {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if paragraph.is_empty() {
            paragraph.push_str(part);
        } else if paragraph.chars().count() + 2 + part.chars().count() <= options.max_chunk_chars {
            paragraph.push_str("\n\n");
            paragraph.push_str(part);
        } else {
            append_paragraph_chunks(&paragraph, options, &mut chunks);
            paragraph.clear();
            paragraph.push_str(part);
        }
    }
    if !paragraph.is_empty() {
        append_paragraph_chunks(&paragraph, options, &mut chunks);
    }

    Ok(chunks
        .into_iter()
        .enumerate()
        .map(|(ordinal, content)| KnowledgeChunkDraft {
            ordinal: i64::try_from(ordinal).unwrap_or(i64::MAX),
            content_hash: stable_content_hash(&content),
            content,
        })
        .collect())
}

fn append_paragraph_chunks(
    paragraph: &str,
    options: &KnowledgeChunkingOptions,
    chunks: &mut Vec<String>,
) {
    let characters = paragraph.chars().collect::<Vec<_>>();
    if characters.len() <= options.max_chunk_chars {
        chunks.push(paragraph.to_string());
        return;
    }

    let mut start = 0usize;
    while start < characters.len() {
        let end = (start + options.max_chunk_chars).min(characters.len());
        chunks.push(characters[start..end].iter().collect());
        if end == characters.len() {
            break;
        }
        let next = end.saturating_sub(options.overlap_chars);
        start = if next > start { next } else { end };
    }
}

fn strip_terminal_controls(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '\0' {
            continue;
        }
        if character != '\x1b' {
            output.push(character);
            continue;
        }

        // Skip CSI/OSC and other ANSI escape sequences until their final byte.
        if let Some(next) = chars.next() {
            if next == ']' {
                loop {
                    match chars.next() {
                        Some('\x07') | None => break,
                        Some('\x1b') if chars.peek() == Some(&'\\') => {
                            chars.next();
                            break;
                        }
                        Some(_) => {}
                    }
                }
            } else if next == '[' || !(0x40..=0x7e).contains(&(next as u32)) {
                for sequence_char in chars.by_ref() {
                    if (0x40..=0x7e).contains(&(sequence_char as u32)) {
                        break;
                    }
                }
            }
        }
    }
    output.replace('\r', "\n")
}

fn stable_content_hash(content: &str) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in content.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

pub(crate) fn content_hash(content: &str) -> String {
    stable_content_hash(content)
}

fn ingest_error(message: impl Into<String>) -> AgentError {
    AgentError::Execution {
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization_removes_terminal_controls_and_normalizes_newlines() {
        let normalized = normalize_knowledge_text("\x1b[31m鱼\x1b[0m\r\n\0\n命令  \r下一行", 100)
            .expect("normalized text");
        assert_eq!(normalized, "鱼\n\n命令\n下一行");
    }

    #[test]
    fn chunking_keeps_paragraphs_and_reports_stable_hashes() {
        let options = KnowledgeChunkingOptions {
            max_input_chars: 100,
            max_chunk_chars: 20,
            overlap_chars: 4,
        };
        let input = "第一段内容。\n\n第二段内容。";
        let left = chunk_knowledge_text(input, &options).expect("chunks");
        let right = chunk_knowledge_text(input, &options).expect("chunks");
        assert_eq!(left, right);
        assert_eq!(left[0].ordinal, 0);
        assert_eq!(left[0].content_hash.len(), 16);
    }

    #[test]
    fn long_cjk_paragraphs_overlap_without_exceeding_the_limit() {
        let options = KnowledgeChunkingOptions {
            max_input_chars: 200,
            max_chunk_chars: 10,
            overlap_chars: 2,
        };
        let chunks =
            chunk_knowledge_text("一二三四五六七八九十一二三四五六", &options).expect("chunks");
        assert!(chunks.len() > 1);
        assert!(
            chunks
                .iter()
                .all(|chunk| chunk.content.chars().count() <= 10)
        );
        assert!(
            chunks[0]
                .content
                .ends_with(&chunks[1].content.chars().take(2).collect::<String>())
        );
    }

    #[test]
    fn invalid_limits_and_oversized_input_are_rejected() {
        assert!(normalize_knowledge_text("x", 0).is_err());
        assert!(normalize_knowledge_text("12345", 4).is_err());
        assert!(
            chunk_knowledge_text(
                "x",
                &KnowledgeChunkingOptions {
                    max_input_chars: 10,
                    max_chunk_chars: 4,
                    overlap_chars: 4,
                }
            )
            .is_err()
        );
    }
}
