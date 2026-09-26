//! Isolated local knowledge storage.
//!
//! Knowledge is intentionally kept separate from long-term memory.  This module
//! owns `knowledge.sqlite3` and only stores curated documents and their chunks;
//! it never imports, writes, or searches `memory_vectors`.

use crate::knowledge_ingest::{
    KnowledgeChunkingOptions, KnowledgeIngestSummary, chunk_knowledge_text, content_hash,
    normalize_knowledge_text,
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use yunxi_agent_core::{AgentError, AgentResult};
use yunxi_agent_persona::{cosine_similarity, yunxi_home_dir};

const KNOWLEDGE_DATABASE_FILE: &str = "knowledge.sqlite3";

/// The coarse-grained isolation boundary for a knowledge space.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KnowledgeSpaceKind {
    System,
    Project,
    Private,
}

impl KnowledgeSpaceKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Project => "project",
            Self::Private => "private",
        }
    }
}

/// Visibility is metadata and an access invariant, not a UI hint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KnowledgeVisibility {
    Public,
    Owner,
    Private,
}

impl KnowledgeVisibility {
    fn as_str(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Owner => "owner",
            Self::Private => "private",
        }
    }
}

/// Stable metadata describing the owner of a knowledge namespace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KnowledgeSpaceSpec {
    pub space_id: String,
    pub kind: KnowledgeSpaceKind,
    pub owner: String,
    pub visibility: KnowledgeVisibility,
    pub source: String,
    pub version: String,
    pub generation: i64,
}

/// A document is a source-level unit; its searchable content lives in chunks.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KnowledgeDocument {
    pub document_id: String,
    pub space_id: String,
    pub title: String,
    pub source: String,
    pub version: String,
    pub generation: i64,
    pub owner: String,
    pub visibility: KnowledgeVisibility,
    pub metadata_json: String,
}

/// A chunk is the smallest searchable unit in the knowledge store.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KnowledgeChunk {
    pub chunk_id: String,
    pub document_id: String,
    pub ordinal: i64,
    pub content: String,
    pub source: String,
    pub version: String,
    pub generation: i64,
    pub owner: String,
    pub visibility: KnowledgeVisibility,
    pub metadata_json: String,
}

/// An embedding stored in the knowledge domain. It is deliberately not a
/// `MemoryEmbedding` so the memory and knowledge vector stores cannot be
/// confused at the type boundary.
#[derive(Clone, Debug, PartialEq)]
pub struct KnowledgeVector {
    pub chunk_id: String,
    pub space_id: String,
    pub embedding_model: String,
    pub generation: i64,
    pub vector: Vec<f32>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KnowledgeSearchScope {
    pub space_id: String,
    pub owner: String,
    pub generation: i64,
    pub visibility: KnowledgeVisibility,
}

#[derive(Clone, Debug, PartialEq)]
pub struct KnowledgeSearchResult {
    pub chunk_id: String,
    pub document_id: String,
    pub space_id: String,
    pub title: String,
    pub content: String,
    pub source: String,
    pub version: String,
    pub generation: i64,
    pub owner: String,
    pub visibility: KnowledgeVisibility,
    pub rank: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct KnowledgeVectorMatch {
    pub chunk_id: String,
    pub document_id: String,
    pub space_id: String,
    pub title: String,
    pub content: String,
    pub source: String,
    pub embedding_model: String,
    pub generation: i64,
    pub score: f32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SqliteKnowledgeStore {
    database: PathBuf,
}

impl SqliteKnowledgeStore {
    pub fn for_workspace(cwd: impl AsRef<Path>) -> Self {
        Self::new(
            cwd.as_ref()
                .join(".yunxi")
                .join("knowledge")
                .join(KNOWLEDGE_DATABASE_FILE),
        )
    }

    pub fn global() -> Self {
        Self::new(
            yunxi_home_dir()
                .join("knowledge")
                .join(KNOWLEDGE_DATABASE_FILE),
        )
    }

    pub fn new(database: impl Into<PathBuf>) -> Self {
        Self {
            database: database.into(),
        }
    }

    pub fn database(&self) -> &Path {
        &self.database
    }

    pub fn initialize(&self) -> AgentResult<()> {
        let connection = self.open_connection()?;
        initialize_schema(&connection, &self.database)
    }

    pub fn upsert_space(&self, spec: &KnowledgeSpaceSpec) -> AgentResult<()> {
        validate_space(spec)?;
        let connection = self.open_connection()?;
        initialize_schema(&connection, &self.database)?;
        let now = now_millis();
        connection
            .execute(
                "INSERT INTO knowledge_spaces
                    (space_id, kind, owner, visibility, source, version, generation,
                     created_at_millis, updated_at_millis)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)
                 ON CONFLICT(space_id) DO UPDATE SET
                    kind = excluded.kind,
                    owner = excluded.owner,
                    visibility = excluded.visibility,
                    source = excluded.source,
                    version = excluded.version,
                    generation = excluded.generation,
                    updated_at_millis = excluded.updated_at_millis",
                params![
                    spec.space_id,
                    spec.kind.as_str(),
                    spec.owner,
                    spec.visibility.as_str(),
                    spec.source,
                    spec.version,
                    spec.generation,
                    now,
                ],
            )
            .map_err(|error| sqlite_error(&self.database, "upsert knowledge space", error))?;
        Ok(())
    }

    pub fn upsert_document(&self, document: &KnowledgeDocument) -> AgentResult<()> {
        validate_document(document)?;
        let mut connection = self.open_connection()?;
        initialize_schema(&connection, &self.database)?;
        let transaction = connection
            .transaction()
            .map_err(|error| sqlite_error(&self.database, "begin knowledge document", error))?;
        ensure_space_metadata(&transaction, document)?;
        let now = now_millis();
        transaction
            .execute(
                "INSERT INTO knowledge_documents
                    (document_id, space_id, title, source, version, generation, owner,
                     visibility, metadata_json, created_at_millis, updated_at_millis)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)
                 ON CONFLICT(document_id) DO UPDATE SET
                    space_id = excluded.space_id,
                    title = excluded.title,
                    source = excluded.source,
                    version = excluded.version,
                    generation = excluded.generation,
                    owner = excluded.owner,
                    visibility = excluded.visibility,
                    metadata_json = excluded.metadata_json,
                    updated_at_millis = excluded.updated_at_millis",
                params![
                    document.document_id,
                    document.space_id,
                    document.title,
                    document.source,
                    document.version,
                    document.generation,
                    document.owner,
                    document.visibility.as_str(),
                    document.metadata_json,
                    now,
                ],
            )
            .map_err(|error| sqlite_error(&self.database, "upsert knowledge document", error))?;
        transaction
            .commit()
            .map_err(|error| sqlite_error(&self.database, "commit knowledge document", error))
    }

    pub fn upsert_chunk(&self, chunk: &KnowledgeChunk) -> AgentResult<()> {
        validate_chunk(chunk)?;
        let mut connection = self.open_connection()?;
        initialize_schema(&connection, &self.database)?;
        let transaction = connection
            .transaction()
            .map_err(|error| sqlite_error(&self.database, "begin knowledge chunk", error))?;
        let document = transaction
            .query_row(
                "SELECT space_id, source, version, generation, owner, visibility
                 FROM knowledge_documents WHERE document_id = ?1",
                params![chunk.document_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| sqlite_error(&self.database, "read knowledge document", error))?
            .ok_or_else(|| storage_error("knowledge chunk references an unknown document"))?;
        if document.1 != chunk.source
            || document.2 != chunk.version
            || document.3 != chunk.generation
            || document.4 != chunk.owner
            || document.5 != chunk.visibility.as_str()
        {
            return Err(storage_error(
                "knowledge chunk metadata does not match its document",
            ));
        }
        let now = now_millis();
        transaction
            .execute(
                "INSERT INTO knowledge_chunks
                    (chunk_id, document_id, space_id, ordinal, content, source, version,
                     generation, owner, visibility, metadata_json, created_at_millis,
                     updated_at_millis)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?12)
                 ON CONFLICT(chunk_id) DO UPDATE SET
                    document_id = excluded.document_id,
                    space_id = excluded.space_id,
                    ordinal = excluded.ordinal,
                    content = excluded.content,
                    source = excluded.source,
                    version = excluded.version,
                    generation = excluded.generation,
                    owner = excluded.owner,
                    visibility = excluded.visibility,
                    metadata_json = excluded.metadata_json,
                    updated_at_millis = excluded.updated_at_millis",
                params![
                    chunk.chunk_id,
                    chunk.document_id,
                    document.0,
                    chunk.ordinal,
                    chunk.content,
                    chunk.source,
                    chunk.version,
                    chunk.generation,
                    chunk.owner,
                    chunk.visibility.as_str(),
                    chunk.metadata_json,
                    now,
                ],
            )
            .map_err(|error| sqlite_error(&self.database, "upsert knowledge chunk", error))?;
        transaction
            .commit()
            .map_err(|error| sqlite_error(&self.database, "commit knowledge chunk", error))
    }

    pub fn upsert_vector(&self, vector: &KnowledgeVector) -> AgentResult<()> {
        validate_vector(vector)?;
        let mut connection = self.open_connection()?;
        initialize_schema(&connection, &self.database)?;
        let transaction = connection
            .transaction()
            .map_err(|error| sqlite_error(&self.database, "begin knowledge vector", error))?;
        let chunk = transaction
            .query_row(
                "SELECT space_id, generation FROM knowledge_chunks WHERE chunk_id = ?1",
                params![vector.chunk_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()
            .map_err(|error| sqlite_error(&self.database, "read knowledge chunk", error))?
            .ok_or_else(|| storage_error("knowledge vector references an unknown chunk"))?;
        if chunk.0 != vector.space_id || chunk.1 != vector.generation {
            return Err(storage_error(
                "knowledge vector metadata does not match its chunk",
            ));
        }
        transaction
            .execute(
                "INSERT INTO knowledge_vectors
                    (chunk_id, space_id, embedding_model, dimensions, vector,
                     generation, indexed_at_millis)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(chunk_id, embedding_model, generation) DO UPDATE SET
                    space_id = excluded.space_id,
                    dimensions = excluded.dimensions,
                    vector = excluded.vector,
                    indexed_at_millis = excluded.indexed_at_millis",
                params![
                    vector.chunk_id,
                    vector.space_id,
                    vector.embedding_model,
                    i64::try_from(vector.vector.len()).unwrap_or(i64::MAX),
                    vector_to_blob(&vector.vector),
                    vector.generation,
                    now_millis(),
                ],
            )
            .map_err(|error| sqlite_error(&self.database, "upsert knowledge vector", error))?;
        transaction
            .commit()
            .map_err(|error| sqlite_error(&self.database, "commit knowledge vector", error))
    }

    /// Atomically replaces all vectors for one document, model, and generation.
    ///
    /// Embedding workers should use this boundary instead of writing vectors one
    /// by one. Every chunk reference is validated before the old model slice is
    /// removed, so an invalid batch cannot leave a partially indexed document.
    pub fn replace_document_vectors(
        &self,
        document_id: &str,
        embedding_model: &str,
        generation: i64,
        vectors: &[KnowledgeVector],
    ) -> AgentResult<usize> {
        if document_id.trim().is_empty() || embedding_model.trim().is_empty() || generation < 0 {
            return Err(storage_error("knowledge vector batch metadata is invalid"));
        }
        let mut connection = self.open_connection()?;
        initialize_schema(&connection, &self.database)?;
        let transaction = connection
            .transaction()
            .map_err(|error| sqlite_error(&self.database, "begin knowledge vector batch", error))?;
        let document = transaction
            .query_row(
                "SELECT space_id, generation FROM knowledge_documents WHERE document_id = ?1",
                params![document_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()
            .map_err(|error| sqlite_error(&self.database, "read knowledge vector document", error))?
            .ok_or_else(|| {
                storage_error("knowledge vector batch references an unknown document")
            })?;
        if document.1 != generation {
            return Err(storage_error(
                "knowledge vector batch generation does not match its document",
            ));
        }

        let mut seen_chunks = HashSet::with_capacity(vectors.len());
        let mut dimensions = None;
        for vector in vectors {
            validate_vector(vector)?;
            if vector.embedding_model != embedding_model || vector.generation != generation {
                return Err(storage_error(
                    "knowledge vector batch metadata does not match its request",
                ));
            }
            if dimensions.is_some_and(|expected| expected != vector.vector.len()) {
                return Err(storage_error(
                    "knowledge vector batch dimensions do not match",
                ));
            }
            dimensions = Some(vector.vector.len());
            if !seen_chunks.insert(vector.chunk_id.clone()) {
                return Err(storage_error(
                    "knowledge vector batch contains a duplicate chunk",
                ));
            }
            let chunk = transaction
                .query_row(
                    "SELECT document_id, space_id, generation
                     FROM knowledge_chunks WHERE chunk_id = ?1",
                    params![vector.chunk_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, i64>(2)?,
                        ))
                    },
                )
                .optional()
                .map_err(|error| {
                    sqlite_error(&self.database, "read knowledge vector batch chunk", error)
                })?
                .ok_or_else(|| {
                    storage_error("knowledge vector batch references an unknown chunk")
                })?;
            if vector.space_id != document.0
                || chunk.0 != document_id
                || chunk.1 != document.0
                || chunk.2 != generation
            {
                return Err(storage_error(
                    "knowledge vector batch chunk metadata does not match its document",
                ));
            }
        }

        transaction
            .execute(
                "DELETE FROM knowledge_vectors
                 WHERE embedding_model = ?1 AND generation = ?2
                   AND chunk_id IN (SELECT chunk_id FROM knowledge_chunks WHERE document_id = ?3)",
                params![embedding_model, generation, document_id],
            )
            .map_err(|error| {
                sqlite_error(&self.database, "remove stale knowledge vectors", error)
            })?;
        for vector in vectors {
            transaction
                .execute(
                    "INSERT INTO knowledge_vectors
                        (chunk_id, space_id, embedding_model, dimensions, vector,
                         generation, indexed_at_millis)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        vector.chunk_id,
                        vector.space_id,
                        vector.embedding_model,
                        i64::try_from(vector.vector.len()).unwrap_or(i64::MAX),
                        vector_to_blob(&vector.vector),
                        vector.generation,
                        now_millis(),
                    ],
                )
                .map_err(|error| {
                    sqlite_error(&self.database, "write knowledge vector batch", error)
                })?;
        }
        transaction.commit().map_err(|error| {
            sqlite_error(&self.database, "commit knowledge vector batch", error)
        })?;
        Ok(vectors.len())
    }

    pub fn ingest_text(
        &self,
        document: &KnowledgeDocument,
        input: &str,
        options: &KnowledgeChunkingOptions,
    ) -> AgentResult<KnowledgeIngestSummary> {
        validate_document(document)?;
        let normalized = normalize_knowledge_text(input, options.max_input_chars)?;
        let drafts = chunk_knowledge_text(&normalized, options)?;
        let document_hash = content_hash(&normalized);
        let mut connection = self.open_connection()?;
        initialize_schema(&connection, &self.database)?;
        let transaction = connection
            .transaction()
            .map_err(|error| sqlite_error(&self.database, "begin knowledge ingest", error))?;
        ensure_space_metadata(&transaction, document)?;
        let now = now_millis();
        transaction
            .execute(
                "INSERT INTO knowledge_documents
                    (document_id, space_id, title, source, version, generation, owner,
                     visibility, metadata_json, created_at_millis, updated_at_millis)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)
                 ON CONFLICT(document_id) DO UPDATE SET
                    space_id = excluded.space_id,
                    title = excluded.title,
                    source = excluded.source,
                    version = excluded.version,
                    generation = excluded.generation,
                    owner = excluded.owner,
                    visibility = excluded.visibility,
                    metadata_json = excluded.metadata_json,
                    updated_at_millis = excluded.updated_at_millis",
                params![
                    document.document_id,
                    document.space_id,
                    document.title,
                    document.source,
                    document.version,
                    document.generation,
                    document.owner,
                    document.visibility.as_str(),
                    document.metadata_json,
                    now,
                ],
            )
            .map_err(|error| sqlite_error(&self.database, "upsert ingested document", error))?;
        let existing_chunks = transaction
            .query_row(
                "SELECT COUNT(*) FROM knowledge_chunks WHERE document_id = ?1",
                params![document.document_id],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| sqlite_error(&self.database, "count stale knowledge chunks", error))?;
        let max_input_chars = i64::try_from(options.max_input_chars).unwrap_or(i64::MAX);
        let max_chunk_chars = i64::try_from(options.max_chunk_chars).unwrap_or(i64::MAX);
        let overlap_chars = i64::try_from(options.overlap_chars).unwrap_or(i64::MAX);
        let matching_chunks = transaction
            .query_row(
                "SELECT COUNT(*) FROM knowledge_chunks
                 WHERE document_id = ?1
                   AND json_extract(metadata_json, '$.document_hash') = ?2
                   AND CAST(json_extract(metadata_json, '$.chunking.max_input_chars') AS INTEGER) = ?3
                   AND CAST(json_extract(metadata_json, '$.chunking.max_chunk_chars') AS INTEGER) = ?4
                   AND CAST(json_extract(metadata_json, '$.chunking.overlap_chars') AS INTEGER) = ?5",
                params![
                    document.document_id,
                    document_hash,
                    max_input_chars,
                    max_chunk_chars,
                    overlap_chars,
                ],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| {
                sqlite_error(&self.database, "check unchanged knowledge chunks", error)
            })?;
        if existing_chunks == i64::try_from(drafts.len()).unwrap_or(i64::MAX)
            && matching_chunks == existing_chunks
        {
            transaction.commit().map_err(|error| {
                sqlite_error(&self.database, "commit unchanged knowledge ingest", error)
            })?;
            return Ok(KnowledgeIngestSummary {
                document_id: document.document_id.clone(),
                content_hash: document_hash,
                chunks_written: 0,
                chunks_removed: 0,
            });
        }
        let chunks_removed = existing_chunks;
        transaction
            .execute(
                "DELETE FROM knowledge_vectors
                 WHERE chunk_id IN (SELECT chunk_id FROM knowledge_chunks WHERE document_id = ?1)",
                params![document.document_id],
            )
            .map_err(|error| {
                sqlite_error(&self.database, "remove stale knowledge vectors", error)
            })?;
        transaction
            .execute(
                "DELETE FROM knowledge_chunks WHERE document_id = ?1",
                params![document.document_id],
            )
            .map_err(|error| {
                sqlite_error(&self.database, "remove stale knowledge chunks", error)
            })?;
        for draft in &drafts {
            let chunk_id = format!("{}#chunk-{}", document.document_id, draft.ordinal);
            let metadata_json = serde_json::json!({
                "content_hash": draft.content_hash,
                "document_hash": document_hash,
                "chunking": {
                    "max_input_chars": options.max_input_chars,
                    "max_chunk_chars": options.max_chunk_chars,
                    "overlap_chars": options.overlap_chars,
                },
            })
            .to_string();
            transaction
                .execute(
                    "INSERT INTO knowledge_chunks
                        (chunk_id, document_id, space_id, ordinal, content, source, version,
                         generation, owner, visibility, metadata_json, created_at_millis,
                         updated_at_millis)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?12)",
                    params![
                        chunk_id,
                        document.document_id,
                        document.space_id,
                        draft.ordinal,
                        draft.content,
                        document.source,
                        document.version,
                        document.generation,
                        document.owner,
                        document.visibility.as_str(),
                        metadata_json,
                        now,
                    ],
                )
                .map_err(|error| {
                    sqlite_error(&self.database, "write ingested knowledge chunk", error)
                })?;
        }
        transaction
            .commit()
            .map_err(|error| sqlite_error(&self.database, "commit knowledge ingest", error))?;
        Ok(KnowledgeIngestSummary {
            document_id: document.document_id.clone(),
            content_hash: document_hash,
            chunks_written: drafts.len(),
            chunks_removed: usize::try_from(chunks_removed).unwrap_or(usize::MAX),
        })
    }

    pub fn search(
        &self,
        query: &str,
        scope: &KnowledgeSearchScope,
        limit: usize,
    ) -> AgentResult<Vec<KnowledgeSearchResult>> {
        if query.trim().is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        validate_search_scope(scope)?;
        let connection = self.open_connection()?;
        initialize_schema(&connection, &self.database)?;
        let fts_query = make_fts_query(query);
        if fts_query.is_empty() {
            return Ok(Vec::new());
        }
        let mut statement = connection
            .prepare(
                "SELECT c.chunk_id, c.document_id, c.space_id, d.title, c.content,
                        c.source, c.version, c.generation, c.owner, c.visibility,
                        bm25(knowledge_chunks_fts) AS rank
                 FROM knowledge_chunks_fts f
                 JOIN knowledge_chunks c ON c.chunk_id = f.chunk_id
                 JOIN knowledge_documents d ON d.document_id = c.document_id
                 JOIN knowledge_spaces s ON s.space_id = c.space_id
                 WHERE knowledge_chunks_fts MATCH ?1
                   AND c.space_id = ?2
                   AND c.generation = ?3
                   AND c.owner = ?4
                   AND c.visibility = ?5
                   AND d.space_id = s.space_id
                   AND d.generation = c.generation
                   AND d.owner = c.owner
                   AND d.visibility = c.visibility
                   AND s.generation = c.generation
                   AND s.owner = c.owner
                   AND s.visibility = c.visibility
                 ORDER BY rank ASC, c.ordinal ASC
                 LIMIT ?6",
            )
            .map_err(|error| sqlite_error(&self.database, "prepare knowledge search", error))?;
        let rows = statement
            .query_map(
                params![
                    fts_query,
                    scope.space_id,
                    scope.generation,
                    scope.owner,
                    scope.visibility.as_str(),
                    i64::try_from(limit).unwrap_or(i64::MAX),
                ],
                |row| {
                    Ok(KnowledgeSearchResult {
                        chunk_id: row.get(0)?,
                        document_id: row.get(1)?,
                        space_id: row.get(2)?,
                        title: row.get(3)?,
                        content: row.get(4)?,
                        source: row.get(5)?,
                        version: row.get(6)?,
                        generation: row.get(7)?,
                        owner: row.get(8)?,
                        visibility: parse_visibility(&row.get::<_, String>(9)?).map_err(
                            |error| {
                                rusqlite::types::FromSqlError::Other(Box::new(
                                    std::io::Error::other(error),
                                ))
                            },
                        )?,
                        rank: row.get(10)?,
                    })
                },
            )
            .map_err(|error| sqlite_error(&self.database, "query knowledge chunks", error))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| sqlite_error(&self.database, "read knowledge chunks", error))
    }

    pub fn search_vectors(
        &self,
        query: &[f32],
        embedding_model: &str,
        scope: &KnowledgeSearchScope,
        top_k: usize,
    ) -> AgentResult<Vec<KnowledgeVectorMatch>> {
        if query.is_empty() || top_k == 0 {
            return Ok(Vec::new());
        }
        if query.iter().any(|value| !value.is_finite()) {
            return Err(storage_error(
                "knowledge query vector contains non-finite values",
            ));
        }
        if embedding_model.trim().is_empty() {
            return Err(storage_error("knowledge embedding model is required"));
        }
        validate_search_scope(scope)?;
        let connection = self.open_connection()?;
        initialize_schema(&connection, &self.database)?;
        let mut statement = connection
            .prepare(
                "SELECT v.chunk_id, c.document_id, c.space_id, d.title, c.content,
                        c.source, v.embedding_model, v.generation, v.dimensions, v.vector
                 FROM knowledge_vectors v
                 JOIN knowledge_chunks c ON c.chunk_id = v.chunk_id
                 JOIN knowledge_documents d ON d.document_id = c.document_id
                 JOIN knowledge_spaces s ON s.space_id = c.space_id
                 WHERE v.embedding_model = ?1
                   AND c.space_id = ?2
                   AND c.generation = ?3
                   AND c.owner = ?4
                   AND c.visibility = ?5
                   AND d.space_id = s.space_id
                   AND d.generation = c.generation
                   AND d.owner = c.owner
                   AND d.visibility = c.visibility
                   AND s.generation = c.generation
                   AND s.owner = c.owner
                   AND s.visibility = c.visibility",
            )
            .map_err(|error| {
                sqlite_error(&self.database, "prepare knowledge vector search", error)
            })?;
        let rows = statement
            .query_map(
                params![
                    embedding_model,
                    scope.space_id,
                    scope.generation,
                    scope.owner,
                    scope.visibility.as_str(),
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, i64>(7)?,
                        row.get::<_, i64>(8)?,
                        row.get::<_, Vec<u8>>(9)?,
                    ))
                },
            )
            .map_err(|error| sqlite_error(&self.database, "query knowledge vectors", error))?;
        let mut matches = Vec::new();
        for row in rows {
            let (
                chunk_id,
                document_id,
                space_id,
                title,
                content,
                source,
                model,
                generation,
                dimensions,
                blob,
            ) =
                row.map_err(|error| sqlite_error(&self.database, "read knowledge vector", error))?;
            let dimensions = usize::try_from(dimensions).unwrap_or_default();
            let values = match vector_from_blob(&blob, dimensions) {
                Ok(values) if values.len() == query.len() => values,
                _ => continue,
            };
            let Ok(score) = cosine_similarity(query, &values) else {
                continue;
            };
            if score.is_finite() {
                matches.push(KnowledgeVectorMatch {
                    chunk_id,
                    document_id,
                    space_id,
                    title,
                    content,
                    source,
                    embedding_model: model,
                    generation,
                    score,
                });
            }
        }
        matches.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| left.chunk_id.cmp(&right.chunk_id))
        });
        matches.truncate(top_k.min(100));
        Ok(matches)
    }

    fn open_connection(&self) -> AgentResult<Connection> {
        if let Some(parent) = self.database.parent() {
            std::fs::create_dir_all(parent).map_err(|error| AgentError::Execution {
                message: format!(
                    "failed to create knowledge directory {}: {error}",
                    parent.display()
                ),
            })?;
        }
        let connection = Connection::open(&self.database)
            .map_err(|error| sqlite_error(&self.database, "open knowledge database", error))?;
        connection
            .busy_timeout(std::time::Duration::from_secs(2))
            .map_err(|error| sqlite_error(&self.database, "configure knowledge database", error))?;
        Ok(connection)
    }
}

fn initialize_schema(connection: &Connection, path: &Path) -> AgentResult<()> {
    connection
        .execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             CREATE TABLE IF NOT EXISTS knowledge_schema (
                schema_version INTEGER NOT NULL
             );
             INSERT INTO knowledge_schema(schema_version)
                SELECT 1 WHERE NOT EXISTS (SELECT 1 FROM knowledge_schema);
             CREATE TABLE IF NOT EXISTS knowledge_spaces (
                space_id TEXT PRIMARY KEY,
                kind TEXT NOT NULL CHECK(kind IN ('system', 'project', 'private')),
                owner TEXT NOT NULL,
                visibility TEXT NOT NULL CHECK(visibility IN ('public', 'owner', 'private')),
                source TEXT NOT NULL,
                version TEXT NOT NULL,
                generation INTEGER NOT NULL CHECK(generation >= 0),
                created_at_millis INTEGER NOT NULL,
                updated_at_millis INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS knowledge_documents (
                document_id TEXT PRIMARY KEY,
                space_id TEXT NOT NULL REFERENCES knowledge_spaces(space_id),
                title TEXT NOT NULL,
                source TEXT NOT NULL,
                version TEXT NOT NULL,
                generation INTEGER NOT NULL CHECK(generation >= 0),
                owner TEXT NOT NULL,
                visibility TEXT NOT NULL CHECK(visibility IN ('public', 'owner', 'private')),
                metadata_json TEXT NOT NULL,
                created_at_millis INTEGER NOT NULL,
                updated_at_millis INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_knowledge_documents_scope
                ON knowledge_documents(space_id, generation, owner, visibility);
             CREATE TABLE IF NOT EXISTS knowledge_chunks (
                chunk_id TEXT PRIMARY KEY,
                document_id TEXT NOT NULL REFERENCES knowledge_documents(document_id),
                space_id TEXT NOT NULL REFERENCES knowledge_spaces(space_id),
                ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
                content TEXT NOT NULL,
                source TEXT NOT NULL,
                version TEXT NOT NULL,
                generation INTEGER NOT NULL CHECK(generation >= 0),
                owner TEXT NOT NULL,
                visibility TEXT NOT NULL CHECK(visibility IN ('public', 'owner', 'private')),
                metadata_json TEXT NOT NULL,
                created_at_millis INTEGER NOT NULL,
                updated_at_millis INTEGER NOT NULL,
                UNIQUE(document_id, ordinal)
             );
             CREATE INDEX IF NOT EXISTS idx_knowledge_chunks_scope
                ON knowledge_chunks(space_id, generation, owner, visibility);
             CREATE TABLE IF NOT EXISTS knowledge_vectors (
                chunk_id TEXT NOT NULL REFERENCES knowledge_chunks(chunk_id),
                space_id TEXT NOT NULL,
                embedding_model TEXT NOT NULL,
                dimensions INTEGER NOT NULL CHECK(dimensions > 0),
                vector BLOB NOT NULL,
                generation INTEGER NOT NULL CHECK(generation >= 0),
                indexed_at_millis INTEGER NOT NULL,
                PRIMARY KEY(chunk_id, embedding_model, generation)
             );
             CREATE INDEX IF NOT EXISTS idx_knowledge_vectors_scope
                ON knowledge_vectors(space_id, generation, embedding_model);
             CREATE VIRTUAL TABLE IF NOT EXISTS knowledge_chunks_fts USING fts5(
                chunk_id UNINDEXED,
                space_id UNINDEXED,
                content,
                title,
                tokenize = 'unicode61'
             );
             CREATE TRIGGER IF NOT EXISTS knowledge_chunks_ai AFTER INSERT ON knowledge_chunks BEGIN
                INSERT INTO knowledge_chunks_fts(rowid, chunk_id, space_id, content, title)
                SELECT new.rowid, new.chunk_id, new.space_id, new.content,
                       (SELECT title FROM knowledge_documents WHERE document_id = new.document_id);
             END;
             CREATE TRIGGER IF NOT EXISTS knowledge_chunks_au AFTER UPDATE ON knowledge_chunks BEGIN
                DELETE FROM knowledge_chunks_fts WHERE rowid = old.rowid;
                INSERT INTO knowledge_chunks_fts(rowid, chunk_id, space_id, content, title)
                SELECT new.rowid, new.chunk_id, new.space_id, new.content,
                       (SELECT title FROM knowledge_documents WHERE document_id = new.document_id);
             END;
             CREATE TRIGGER IF NOT EXISTS knowledge_chunks_ad AFTER DELETE ON knowledge_chunks BEGIN
                DELETE FROM knowledge_chunks_fts WHERE rowid = old.rowid;
             END;",
        )
        .map_err(|error| sqlite_error(path, "initialize knowledge schema", error))
}

fn ensure_space_metadata(
    transaction: &Transaction<'_>,
    document: &KnowledgeDocument,
) -> AgentResult<()> {
    let space = transaction
        .query_row(
            "SELECT kind, owner, visibility, source, version, generation
             FROM knowledge_spaces WHERE space_id = ?1",
            params![document.space_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            },
        )
        .optional()
        .map_err(|error| AgentError::Execution {
            message: format!("failed to read knowledge space: {error}"),
        })?
        .ok_or_else(|| storage_error("knowledge document references an unknown space"))?;
    if space.1 != document.owner
        || space.2 != document.visibility.as_str()
        || space.3 != document.source
        || space.4 != document.version
        || space.5 != document.generation
    {
        return Err(storage_error(
            "knowledge document metadata does not match its space",
        ));
    }
    Ok(())
}

fn validate_space(spec: &KnowledgeSpaceSpec) -> AgentResult<()> {
    if spec.space_id.trim().is_empty()
        || spec.owner.trim().is_empty()
        || spec.source.trim().is_empty()
        || spec.version.trim().is_empty()
        || spec.generation < 0
    {
        return Err(storage_error("knowledge space metadata is incomplete"));
    }
    if spec.kind == KnowledgeSpaceKind::System && spec.visibility != KnowledgeVisibility::Public {
        return Err(storage_error("system knowledge must be public"));
    }
    if spec.kind == KnowledgeSpaceKind::Private
        && spec.visibility != KnowledgeVisibility::Private
        && spec.visibility != KnowledgeVisibility::Owner
    {
        return Err(storage_error(
            "private knowledge must be owner or private visible",
        ));
    }
    if spec.visibility != KnowledgeVisibility::Public && spec.owner == "system" {
        return Err(storage_error(
            "non-public knowledge cannot be owned by system",
        ));
    }
    Ok(())
}

fn validate_document(document: &KnowledgeDocument) -> AgentResult<()> {
    if document.document_id.trim().is_empty()
        || document.space_id.trim().is_empty()
        || document.title.trim().is_empty()
        || document.source.trim().is_empty()
        || document.version.trim().is_empty()
        || document.owner.trim().is_empty()
        || document.generation < 0
        || serde_json::from_str::<serde_json::Value>(&document.metadata_json).is_err()
    {
        return Err(storage_error(
            "knowledge document metadata is incomplete or invalid",
        ));
    }
    Ok(())
}

fn validate_chunk(chunk: &KnowledgeChunk) -> AgentResult<()> {
    if chunk.chunk_id.trim().is_empty()
        || chunk.document_id.trim().is_empty()
        || chunk.content.trim().is_empty()
        || chunk.source.trim().is_empty()
        || chunk.version.trim().is_empty()
        || chunk.owner.trim().is_empty()
        || chunk.ordinal < 0
        || chunk.generation < 0
        || serde_json::from_str::<serde_json::Value>(&chunk.metadata_json).is_err()
    {
        return Err(storage_error(
            "knowledge chunk metadata is incomplete or invalid",
        ));
    }
    Ok(())
}

fn validate_vector(vector: &KnowledgeVector) -> AgentResult<()> {
    if vector.chunk_id.trim().is_empty()
        || vector.space_id.trim().is_empty()
        || vector.embedding_model.trim().is_empty()
        || vector.generation < 0
        || vector.vector.is_empty()
        || vector.vector.iter().any(|value| !value.is_finite())
    {
        return Err(storage_error(
            "knowledge vector metadata or values are invalid",
        ));
    }
    Ok(())
}

fn validate_search_scope(scope: &KnowledgeSearchScope) -> AgentResult<()> {
    if scope.space_id.trim().is_empty() || scope.owner.trim().is_empty() || scope.generation < 0 {
        return Err(storage_error("invalid knowledge search scope"));
    }
    Ok(())
}

fn vector_to_blob(values: &[f32]) -> Vec<u8> {
    let mut blob = Vec::with_capacity(values.len() * std::mem::size_of::<f32>());
    for value in values {
        blob.extend_from_slice(&value.to_le_bytes());
    }
    blob
}

fn vector_from_blob(blob: &[u8], dimensions: usize) -> Result<Vec<f32>, String> {
    let expected = dimensions.saturating_mul(std::mem::size_of::<f32>());
    if blob.len() != expected {
        return Err(format!(
            "knowledge vector blob has {} bytes; expected {expected}",
            blob.len()
        ));
    }
    Ok(blob
        .chunks_exact(std::mem::size_of::<f32>())
        .map(|bytes| f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
        .collect())
}

fn make_fts_query(query: &str) -> String {
    query
        .split_whitespace()
        .filter(|token| !token.is_empty())
        .map(|token| format!("\"{}\"", token.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" AND ")
}

fn parse_visibility(value: &str) -> Result<KnowledgeVisibility, String> {
    match value {
        "public" => Ok(KnowledgeVisibility::Public),
        "owner" => Ok(KnowledgeVisibility::Owner),
        "private" => Ok(KnowledgeVisibility::Private),
        other => Err(format!("unknown knowledge visibility {other}")),
    }
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or_default()
}

fn storage_error(message: impl Into<String>) -> AgentError {
    AgentError::Execution {
        message: message.into(),
    }
}

fn sqlite_error(path: &Path, action: &str, error: rusqlite::Error) -> AgentError {
    storage_error(format!("{action} {}: {error}", path.display()))
}
