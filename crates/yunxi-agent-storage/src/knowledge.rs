//! Isolated local knowledge storage.
//!
//! Knowledge is intentionally kept separate from long-term memory.  This module
//! owns `knowledge.sqlite3` and only stores curated documents and their chunks;
//! it never imports, writes, or searches `memory_vectors`.

use crate::knowledge_ingest::{
    KnowledgeChunkingOptions, KnowledgeIngestSummary, chunk_knowledge_text, content_hash,
    normalize_knowledge_text,
};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use yunxi_agent_core::{AgentError, AgentResult};
use yunxi_agent_persona::{MemoryEmbeddingProvider, cosine_similarity, yunxi_home_dir};

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
    pub metadata_json: String,
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
    pub metadata_json: String,
    pub source: String,
    pub version: String,
    pub embedding_model: String,
    pub generation: i64,
    pub score: f32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KnowledgeEmbeddingSummary {
    pub document_id: String,
    pub embedding_model: String,
    pub dimensions: usize,
    pub chunks_indexed: usize,
}

/// Durable state for one document/model/generation embedding request.
///
/// The queue is intentionally only a storage contract. It does not invoke an
/// embedding provider or run a background worker.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KnowledgeEmbeddingJobStatus {
    Pending,
    Running,
    Completed,
    Failed,
}

impl KnowledgeEmbeddingJobStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }

    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "pending" => Ok(Self::Pending),
            "running" => Ok(Self::Running),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            other => Err(format!("unknown knowledge embedding job status {other}")),
        }
    }
}

/// A persisted embedding job and its lease/status metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KnowledgeEmbeddingJob {
    pub job_id: i64,
    pub document_id: String,
    pub embedding_model: String,
    pub generation: i64,
    pub status: KnowledgeEmbeddingJobStatus,
    pub attempts: i64,
    pub worker_id: Option<String>,
    pub last_error: Option<String>,
    pub created_at_millis: i64,
    pub updated_at_millis: i64,
}

/// The outcome of one storage-owned embedding worker step.
///
/// A provider failure is represented as a `Failed` job instead of bubbling out
/// as an uncommitted error. This lets a long-lived daemon keep processing later
/// jobs while preserving the failure reason for diagnostics.
#[derive(Clone, Debug, PartialEq)]
pub struct KnowledgeEmbeddingWorkerResult {
    pub job: KnowledgeEmbeddingJob,
    pub chunks_indexed: usize,
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

    /// Enqueue one idempotent document/model/generation embedding request.
    pub fn enqueue_embedding_job(
        &self,
        document_id: &str,
        embedding_model: &str,
        generation: i64,
    ) -> AgentResult<KnowledgeEmbeddingJob> {
        validate_embedding_job_input(document_id, embedding_model, generation)?;
        let mut connection = self.open_connection()?;
        initialize_schema(&connection, &self.database)?;
        let transaction = connection
            .transaction()
            .map_err(|error| sqlite_error(&self.database, "begin embedding job enqueue", error))?;
        let document_generation = transaction
            .query_row(
                "SELECT generation FROM knowledge_documents WHERE document_id = ?1",
                params![document_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(|error| sqlite_error(&self.database, "read embedding job document", error))?
            .ok_or_else(|| storage_error("embedding job references an unknown document"))?;
        if document_generation != generation {
            return Err(storage_error(
                "embedding job generation does not match its document",
            ));
        }
        let now = now_millis();
        transaction
            .execute(
                "INSERT INTO knowledge_embedding_jobs
                    (document_id, embedding_model, generation, status, attempts,
                     worker_id, last_error, created_at_millis, updated_at_millis)
                 VALUES (?1, ?2, ?3, 'pending', 0, NULL, NULL, ?4, ?4)
                 ON CONFLICT(document_id, embedding_model, generation) DO NOTHING",
                params![document_id, embedding_model, generation, now],
            )
            .map_err(|error| sqlite_error(&self.database, "enqueue embedding job", error))?;
        let job_id = transaction
            .query_row(
                "SELECT job_id FROM knowledge_embedding_jobs
                 WHERE document_id = ?1 AND embedding_model = ?2 AND generation = ?3",
                params![document_id, embedding_model, generation],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| sqlite_error(&self.database, "read enqueued embedding job", error))?;
        let job = read_embedding_job(&transaction, &self.database, job_id)?;
        transaction
            .commit()
            .map_err(|error| sqlite_error(&self.database, "commit embedding job enqueue", error))?;
        Ok(job)
    }

    /// Enqueue an embedding job for the document's current generation.
    ///
    /// This is the safe convenience boundary for callers that do not already
    /// hold a document snapshot. The generation is read from the same store
    /// immediately before applying the idempotent enqueue contract.
    pub fn enqueue_current_document_embedding_job(
        &self,
        document_id: &str,
        embedding_model: &str,
    ) -> AgentResult<KnowledgeEmbeddingJob> {
        if document_id.trim().is_empty() {
            return Err(storage_error("embedding job document id is invalid"));
        }
        let connection = self.open_connection()?;
        initialize_schema(&connection, &self.database)?;
        let generation = connection
            .query_row(
                "SELECT generation FROM knowledge_documents WHERE document_id = ?1",
                params![document_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(|error| {
                sqlite_error(&self.database, "read current embedding document", error)
            })?
            .ok_or_else(|| storage_error("embedding job references an unknown document"))?;
        self.enqueue_embedding_job(document_id, embedding_model, generation)
    }

    /// Atomically claim the oldest pending embedding job for one worker.
    pub fn claim_embedding_job(
        &self,
        worker_id: &str,
    ) -> AgentResult<Option<KnowledgeEmbeddingJob>> {
        validate_embedding_worker_id(worker_id)?;
        let mut connection = self.open_connection()?;
        initialize_schema(&connection, &self.database)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| sqlite_error(&self.database, "begin embedding job claim", error))?;
        let job_id = transaction
            .query_row(
                "SELECT job_id FROM knowledge_embedding_jobs
                 WHERE status = 'pending'
                 ORDER BY created_at_millis ASC, job_id ASC LIMIT 1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(|error| sqlite_error(&self.database, "find pending embedding job", error))?;
        let Some(job_id) = job_id else {
            transaction
                .commit()
                .map_err(|error| sqlite_error(&self.database, "commit empty job claim", error))?;
            return Ok(None);
        };
        let updated = transaction
            .execute(
                "UPDATE knowledge_embedding_jobs
                 SET status = 'running', attempts = attempts + 1,
                     worker_id = ?1, updated_at_millis = ?2
                 WHERE job_id = ?3 AND status = 'pending'",
                params![worker_id, now_millis(), job_id],
            )
            .map_err(|error| sqlite_error(&self.database, "claim embedding job", error))?;
        if updated != 1 {
            return Err(storage_error("embedding job claim lost its pending state"));
        }
        let job = read_embedding_job(&transaction, &self.database, job_id)?;
        transaction
            .commit()
            .map_err(|error| sqlite_error(&self.database, "commit embedding job claim", error))?;
        Ok(Some(job))
    }

    /// Mark a running job complete, requiring the worker lease to match.
    pub fn complete_embedding_job(
        &self,
        job_id: i64,
        worker_id: &str,
    ) -> AgentResult<KnowledgeEmbeddingJob> {
        self.finish_embedding_job(
            job_id,
            worker_id,
            KnowledgeEmbeddingJobStatus::Completed,
            None,
        )
    }

    /// Mark a running job failed while leaving any previous vector generation intact.
    pub fn fail_embedding_job(
        &self,
        job_id: i64,
        worker_id: &str,
        error_message: &str,
    ) -> AgentResult<KnowledgeEmbeddingJob> {
        if error_message.trim().is_empty() || error_message.chars().count() > 4096 {
            return Err(storage_error("embedding job failure message is invalid"));
        }
        self.finish_embedding_job(
            job_id,
            worker_id,
            KnowledgeEmbeddingJobStatus::Failed,
            Some(error_message),
        )
    }

    /// Claim and process one pending embedding job with the supplied provider.
    ///
    /// The provider is checked against the job's requested model before any
    /// vector work begins. Successful indexing is committed before the job is
    /// marked completed; provider/indexing failures mark the job failed and
    /// leave any previous vector generation untouched.
    pub fn process_next_embedding_job<P: MemoryEmbeddingProvider>(
        &self,
        worker_id: &str,
        provider: &P,
    ) -> AgentResult<Option<KnowledgeEmbeddingWorkerResult>> {
        let Some(job) = self.claim_embedding_job(worker_id)? else {
            return Ok(None);
        };
        let outcome = match self.read_document_generation(&job.document_id) {
            Err(error) => Err(storage_error(error.to_string())),
            Ok(None) => Err(storage_error("embedding job document no longer exists")),
            Ok(Some(generation)) if generation != job.generation => Err(storage_error(format!(
                "embedding job generation {} is stale; current document generation differs",
                job.generation
            ))),
            Ok(Some(_)) if job.embedding_model != provider.model_id() => {
                Err(storage_error(format!(
                    "embedding provider model {} does not match queued model {}",
                    provider.model_id(),
                    job.embedding_model
                )))
            }
            Ok(Some(_)) => self
                .index_document_with_embeddings(&job.document_id, provider)
                .map(|summary| summary.chunks_indexed),
        };
        match outcome {
            Ok(chunks_indexed) => {
                let completed = self.complete_embedding_job(job.job_id, worker_id)?;
                Ok(Some(KnowledgeEmbeddingWorkerResult {
                    job: completed,
                    chunks_indexed,
                }))
            }
            Err(error) => {
                let message = error.to_string();
                let message = if message.chars().count() > 4096 {
                    message.chars().take(4096).collect::<String>()
                } else {
                    message
                };
                let failed = self.fail_embedding_job(job.job_id, worker_id, &message)?;
                Ok(Some(KnowledgeEmbeddingWorkerResult {
                    job: failed,
                    chunks_indexed: 0,
                }))
            }
        }
    }

    fn read_document_generation(&self, document_id: &str) -> AgentResult<Option<i64>> {
        let connection = self.open_connection()?;
        initialize_schema(&connection, &self.database)?;
        connection
            .query_row(
                "SELECT generation FROM knowledge_documents WHERE document_id = ?1",
                params![document_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(|error| sqlite_error(&self.database, "read document generation", error))
    }

    fn finish_embedding_job(
        &self,
        job_id: i64,
        worker_id: &str,
        status: KnowledgeEmbeddingJobStatus,
        error_message: Option<&str>,
    ) -> AgentResult<KnowledgeEmbeddingJob> {
        if job_id <= 0 {
            return Err(storage_error("embedding job id is invalid"));
        }
        validate_embedding_worker_id(worker_id)?;
        let mut connection = self.open_connection()?;
        initialize_schema(&connection, &self.database)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| {
                sqlite_error(&self.database, "begin embedding job transition", error)
            })?;
        let updated = transaction
            .execute(
                "UPDATE knowledge_embedding_jobs
                 SET status = ?1, last_error = ?2, updated_at_millis = ?3
                 WHERE job_id = ?4 AND status = 'running' AND worker_id = ?5",
                params![
                    status.as_str(),
                    error_message,
                    now_millis(),
                    job_id,
                    worker_id
                ],
            )
            .map_err(|error| sqlite_error(&self.database, "transition embedding job", error))?;
        if updated != 1 {
            return Err(storage_error(
                "embedding job is not running under the requested worker",
            ));
        }
        let job = read_embedding_job(&transaction, &self.database, job_id)?;
        transaction.commit().map_err(|error| {
            sqlite_error(&self.database, "commit embedding job transition", error)
        })?;
        Ok(job)
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

    /// Atomically retract one document and all of its chunks and vectors.
    ///
    /// Retraction is scoped like search: a caller must prove the document
    /// belongs to the requested space, owner, generation, and visibility.
    pub fn retract_document(
        &self,
        document_id: &str,
        scope: &KnowledgeSearchScope,
    ) -> AgentResult<bool> {
        if document_id.trim().is_empty() {
            return Err(storage_error("knowledge document id is required"));
        }
        validate_search_scope(scope)?;
        let mut connection = self.open_connection()?;
        initialize_schema(&connection, &self.database)?;
        let transaction = connection
            .transaction()
            .map_err(|error| sqlite_error(&self.database, "begin knowledge retraction", error))?;
        let document = transaction
            .query_row(
                "SELECT space_id, generation, owner, visibility
                 FROM knowledge_documents WHERE document_id = ?1",
                params![document_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| sqlite_error(&self.database, "read knowledge document", error))?;
        let Some(document) = document else {
            transaction.commit().map_err(|error| {
                sqlite_error(&self.database, "commit missing knowledge retraction", error)
            })?;
            return Ok(false);
        };
        if document.0 != scope.space_id
            || document.1 != scope.generation
            || document.2 != scope.owner
            || document.3 != scope.visibility.as_str()
        {
            return Err(storage_error(
                "knowledge document is outside the requested retraction scope",
            ));
        }
        transaction
            .execute(
                "DELETE FROM knowledge_embedding_jobs WHERE document_id = ?1",
                params![document_id],
            )
            .map_err(|error| sqlite_error(&self.database, "delete embedding jobs", error))?;
        transaction
            .execute(
                "DELETE FROM knowledge_vectors
                 WHERE chunk_id IN (
                    SELECT chunk_id FROM knowledge_chunks WHERE document_id = ?1
                 )",
                params![document_id],
            )
            .map_err(|error| sqlite_error(&self.database, "delete knowledge vectors", error))?;
        transaction
            .execute(
                "DELETE FROM knowledge_chunks WHERE document_id = ?1",
                params![document_id],
            )
            .map_err(|error| sqlite_error(&self.database, "delete knowledge chunks", error))?;
        transaction
            .execute(
                "DELETE FROM knowledge_documents WHERE document_id = ?1",
                params![document_id],
            )
            .map_err(|error| sqlite_error(&self.database, "delete knowledge document", error))?;
        transaction
            .commit()
            .map_err(|error| sqlite_error(&self.database, "commit knowledge retraction", error))?;
        Ok(true)
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
        // A low-level chunk upsert can change content outside `ingest_text`.
        // Invalidate derived vectors before the write so an incremental index
        // check can never mistake an old embedding for a current one.
        transaction
            .execute(
                "DELETE FROM knowledge_vectors WHERE chunk_id = ?1",
                params![chunk.chunk_id],
            )
            .map_err(|error| sqlite_error(&self.database, "invalidate knowledge vectors", error))?;
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
        self.replace_document_vectors_checked(
            document_id,
            embedding_model,
            generation,
            vectors,
            None,
        )
    }

    /// Atomically replaces vectors after checking that the chunks embedded by
    /// the caller are still the chunks currently stored for the document.
    ///
    /// The optional snapshot is used by the embedding path, where reading and
    /// embedding chunks necessarily happens outside the replacement
    /// transaction. A concurrent chunk update must invalidate that work before
    /// any old vectors are removed or new vectors are written.
    fn replace_document_vectors_checked(
        &self,
        document_id: &str,
        embedding_model: &str,
        generation: i64,
        vectors: &[KnowledgeVector],
        expected_chunks: Option<&[(String, String)]>,
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

        if let Some(expected_chunks) = expected_chunks {
            let mut statement = transaction
                .prepare(
                    "SELECT chunk_id, content
                     FROM knowledge_chunks
                     WHERE document_id = ?1 ORDER BY ordinal ASC, chunk_id ASC",
                )
                .map_err(|error| {
                    sqlite_error(&self.database, "prepare knowledge chunk snapshot", error)
                })?;
            let current_chunks = statement
                .query_map(params![document_id], |row| {
                    let chunk_id = row.get::<_, String>(0)?;
                    let content = row.get::<_, String>(1)?;
                    Ok((chunk_id, content_hash(&content)))
                })
                .map_err(|error| {
                    sqlite_error(&self.database, "query knowledge chunk snapshot", error)
                })?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| {
                    sqlite_error(&self.database, "read knowledge chunk snapshot", error)
                })?;
            drop(statement);
            if current_chunks != expected_chunks {
                return Err(storage_error(
                    "knowledge chunks changed during embedding; retry indexing",
                ));
            }
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

    /// Embeds every current chunk and atomically replaces this document's
    /// vectors for the provider model. The knowledge vector type and storage
    /// remain distinct from the long-term memory vector domain.
    pub fn index_document_with_embeddings<P: MemoryEmbeddingProvider>(
        &self,
        document_id: &str,
        provider: &P,
    ) -> AgentResult<KnowledgeEmbeddingSummary> {
        if document_id.trim().is_empty() {
            return Err(storage_error("knowledge embedding document id is empty"));
        }
        let connection = self.open_connection()?;
        initialize_schema(&connection, &self.database)?;
        let document = connection
            .query_row(
                "SELECT space_id, generation FROM knowledge_documents WHERE document_id = ?1",
                params![document_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()
            .map_err(|error| {
                sqlite_error(&self.database, "read knowledge embedding document", error)
            })?
            .ok_or_else(|| storage_error("knowledge embedding references an unknown document"))?;
        let chunk_count = connection
            .query_row(
                "SELECT COUNT(*) FROM knowledge_chunks WHERE document_id = ?1",
                params![document_id],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| {
                sqlite_error(&self.database, "count knowledge embedding chunks", error)
            })?;
        let indexed_count = connection
            .query_row(
                "SELECT COUNT(*)
                 FROM knowledge_vectors v
                 JOIN knowledge_chunks c ON c.chunk_id = v.chunk_id
                 WHERE c.document_id = ?1
                   AND v.embedding_model = ?2
                   AND v.generation = ?3
                   AND v.dimensions = ?4
                   AND length(v.vector) = v.dimensions * 4",
                params![
                    document_id,
                    provider.model_id(),
                    document.1,
                    i64::try_from(provider.dimensions()).unwrap_or(i64::MAX),
                ],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| {
                sqlite_error(&self.database, "count indexed knowledge chunks", error)
            })?;
        if chunk_count == indexed_count {
            return Ok(KnowledgeEmbeddingSummary {
                document_id: document_id.to_string(),
                embedding_model: provider.model_id().to_string(),
                dimensions: provider.dimensions(),
                chunks_indexed: 0,
            });
        }
        let mut statement = connection
            .prepare(
                "SELECT chunk_id, space_id, generation, content
                 FROM knowledge_chunks
                 WHERE document_id = ?1 ORDER BY ordinal ASC, chunk_id ASC",
            )
            .map_err(|error| {
                sqlite_error(&self.database, "prepare knowledge embedding chunks", error)
            })?;
        let rows = statement
            .query_map(params![document_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .map_err(|error| {
                sqlite_error(&self.database, "query knowledge embedding chunks", error)
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| {
                sqlite_error(&self.database, "read knowledge embedding chunks", error)
            })?;
        drop(statement);
        let mut expected_chunks = Vec::new();
        let mut vectors = Vec::new();
        for row in rows {
            let (chunk_id, space_id, generation, content) = row;
            expected_chunks.push((chunk_id.clone(), content_hash(&content)));
            let embedding = provider.embed(&content).map_err(|error| {
                storage_error(format!("knowledge embedding provider failed: {error}"))
            })?;
            if embedding.model != provider.model_id()
                || embedding.dimensions() != provider.dimensions()
            {
                return Err(storage_error(
                    "knowledge embedding provider returned unexpected model or dimensions",
                ));
            }
            vectors.push(KnowledgeVector {
                chunk_id,
                space_id,
                embedding_model: embedding.model,
                generation,
                vector: embedding.values,
            });
        }
        let chunks_indexed = self.replace_document_vectors_checked(
            document_id,
            provider.model_id(),
            document.1,
            &vectors,
            Some(&expected_chunks),
        )?;
        Ok(KnowledgeEmbeddingSummary {
            document_id: document_id.to_string(),
            embedding_model: provider.model_id().to_string(),
            dimensions: provider.dimensions(),
            chunks_indexed,
        })
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
        self.search_versioned(query, scope, None, limit)
    }

    pub fn search_versioned(
        &self,
        query: &str,
        scope: &KnowledgeSearchScope,
        source_version: Option<&str>,
        limit: usize,
    ) -> AgentResult<Vec<KnowledgeSearchResult>> {
        if query.trim().is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        validate_source_version(source_version)?;
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
                        d.metadata_json, c.source, c.version, c.generation, c.owner, c.visibility,
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
                   AND (?6 IS NULL OR c.version = ?6)
                 ORDER BY rank ASC, c.ordinal ASC
                 LIMIT ?7",
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
                    source_version,
                    i64::try_from(limit).unwrap_or(i64::MAX),
                ],
                |row| {
                    Ok(KnowledgeSearchResult {
                        chunk_id: row.get(0)?,
                        document_id: row.get(1)?,
                        space_id: row.get(2)?,
                        title: row.get(3)?,
                        content: row.get(4)?,
                        metadata_json: row.get(5)?,
                        source: row.get(6)?,
                        version: row.get(7)?,
                        generation: row.get(8)?,
                        owner: row.get(9)?,
                        visibility: parse_visibility(&row.get::<_, String>(10)?).map_err(
                            |error| {
                                rusqlite::types::FromSqlError::Other(Box::new(
                                    std::io::Error::other(error),
                                ))
                            },
                        )?,
                        rank: row.get(11)?,
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
        self.search_vectors_versioned(query, embedding_model, scope, None, top_k)
    }

    pub fn search_vectors_versioned(
        &self,
        query: &[f32],
        embedding_model: &str,
        scope: &KnowledgeSearchScope,
        source_version: Option<&str>,
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
        validate_source_version(source_version)?;
        validate_search_scope(scope)?;
        let connection = self.open_connection()?;
        initialize_schema(&connection, &self.database)?;
        let mut statement = connection
            .prepare(
                "SELECT v.chunk_id, c.document_id, c.space_id, d.title, c.content,
                        d.metadata_json, c.source, c.version, v.embedding_model, v.generation,
                        v.dimensions, v.vector
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
                   AND s.visibility = c.visibility
                   AND (?6 IS NULL OR c.version = ?6)",
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
                    source_version,
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
                        row.get::<_, String>(7)?,
                        row.get::<_, String>(8)?,
                        row.get::<_, i64>(9)?,
                        row.get::<_, i64>(10)?,
                        row.get::<_, Vec<u8>>(11)?,
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
                metadata_json,
                source,
                version,
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
                    metadata_json,
                    source,
                    version,
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

fn read_embedding_job(
    transaction: &Transaction<'_>,
    path: &Path,
    job_id: i64,
) -> AgentResult<KnowledgeEmbeddingJob> {
    let row = transaction
        .query_row(
            "SELECT job_id, document_id, embedding_model, generation, status,
                    attempts, worker_id, last_error, created_at_millis, updated_at_millis
             FROM knowledge_embedding_jobs WHERE job_id = ?1",
            params![job_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, i64>(9)?,
                ))
            },
        )
        .map_err(|error| sqlite_error(path, "read embedding job", error))?;
    Ok(KnowledgeEmbeddingJob {
        job_id: row.0,
        document_id: row.1,
        embedding_model: row.2,
        generation: row.3,
        status: KnowledgeEmbeddingJobStatus::parse(&row.4).map_err(storage_error)?,
        attempts: row.5,
        worker_id: row.6,
        last_error: row.7,
        created_at_millis: row.8,
        updated_at_millis: row.9,
    })
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
             UPDATE knowledge_schema SET schema_version = 2 WHERE schema_version < 2;
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
             CREATE TABLE IF NOT EXISTS knowledge_embedding_jobs (
                job_id INTEGER PRIMARY KEY AUTOINCREMENT,
                document_id TEXT NOT NULL REFERENCES knowledge_documents(document_id),
                embedding_model TEXT NOT NULL,
                generation INTEGER NOT NULL CHECK(generation >= 0),
                status TEXT NOT NULL CHECK(status IN ('pending', 'running', 'completed', 'failed')),
                attempts INTEGER NOT NULL CHECK(attempts >= 0),
                worker_id TEXT,
                last_error TEXT,
                created_at_millis INTEGER NOT NULL,
                updated_at_millis INTEGER NOT NULL,
                UNIQUE(document_id, embedding_model, generation)
             );
             CREATE INDEX IF NOT EXISTS idx_knowledge_embedding_jobs_pending
                ON knowledge_embedding_jobs(status, created_at_millis, job_id);
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
        || (space.0 != "system" && space.4 != document.version)
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

fn validate_source_version(source_version: Option<&str>) -> AgentResult<()> {
    if source_version.is_some_and(|version| version.trim().is_empty()) {
        return Err(storage_error("knowledge source version filter is empty"));
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

fn validate_embedding_job_input(
    document_id: &str,
    embedding_model: &str,
    generation: i64,
) -> AgentResult<()> {
    if document_id.trim().is_empty() || embedding_model.trim().is_empty() || generation < 0 {
        return Err(storage_error("embedding job metadata is invalid"));
    }
    Ok(())
}

fn validate_embedding_worker_id(worker_id: &str) -> AgentResult<()> {
    if worker_id.trim().is_empty() || worker_id.chars().count() > 128 {
        return Err(storage_error("embedding worker id is invalid"));
    }
    Ok(())
}

fn sqlite_error(path: &Path, action: &str, error: rusqlite::Error) -> AgentError {
    storage_error(format!("{action} {}: {error}", path.display()))
}
