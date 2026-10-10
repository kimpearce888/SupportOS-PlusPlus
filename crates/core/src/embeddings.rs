//! Embeddings pipeline — content-hash-gated re-embedding + retry cap (M5-T07).
//!
//! Per spec M5: "embeddings."
//! Per spec KNOWN PITFALLS: "Re-embed only when the content hash changes;
//! cap failed embedding retries."
//! Per the reference notes: "AI output cached by `hash(input_text) + prompt_version`
//! in the `ai_runs` table."
//!
//! ## Design
//!
//! The pipeline caches embedding results in the `ai_runs` SQLite table.
//! The cache key is `(input_hash, prompt_version, model)` — so:
//! - If the same text is embedded again with the same model + prompt version,
//!   the cached result is returned (no API call, no re-embedding).
//! - If the text changes (different content hash), a new embedding is fetched.
//! - If the model changes, a new embedding is fetched.
//! - If the prompt version changes (e.g., a new prompt template), a new
//!   embedding is fetched.
//!
//! Per KNOWN PITFALLS: "cap failed embedding retries." If the provider fails
//! to embed, the pipeline retries up to `MAX_EMBED_RETRIES` times (default 3)
//! with exponential backoff. After exhausting retries, it returns an error
//! (per spec: "Unknown" is a legitimate answer — but for embeddings, an empty
//! vector is the fail-safe, since a wrong-dimension vector would corrupt the
//! VectorStore).

use rusqlite::{params, Connection};

use crate::ai_provider::{EmbedResponse, LocalAiProvider};
use crate::error::{Error, Result};

/// The M008 migration: creates the `ai_runs` cache table.
pub const M008_SQL: &str = r#"
    CREATE TABLE IF NOT EXISTS ai_runs (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        input_hash      TEXT NOT NULL,
        prompt_version  TEXT NOT NULL,
        model           TEXT NOT NULL,
        response_json   TEXT NOT NULL,
        created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
    );
    CREATE INDEX IF NOT EXISTS idx_ai_runs_lookup
        ON ai_runs (input_hash, prompt_version, model);
    CREATE INDEX IF NOT EXISTS idx_ai_runs_created_at
        ON ai_runs (created_at);

    UPDATE app_state SET schema_version = 8 WHERE id = 1;
"#;

/// Apply M008 migration. Idempotent.
pub fn apply_m008(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS ai_runs (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            input_hash      TEXT NOT NULL,
            prompt_version  TEXT NOT NULL,
            model           TEXT NOT NULL,
            response_json   TEXT NOT NULL,
            created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
        );
        CREATE INDEX IF NOT EXISTS idx_ai_runs_lookup
            ON ai_runs (input_hash, prompt_version, model);
        CREATE INDEX IF NOT EXISTS idx_ai_runs_created_at
            ON ai_runs (created_at);",
    )?;
    let _ = conn.execute("UPDATE app_state SET schema_version = 8 WHERE id = 1", []);
    Ok(())
}

/// The maximum number of retries for a failed embedding call.
/// Per KNOWN PITFALLS: "cap failed embedding retries."
pub const MAX_EMBED_RETRIES: u32 = 3;

/// Compute the content hash for an embedding input. Uses SHA-256 (the same
/// hash used by the `sha2` crate — but we don't add a new dep; we use a
/// simple FNV-1a hash for the cache key since it's a cache, not a security
/// boundary). The hash is a hex string so it's safe to store as TEXT.
///
/// Per spec A12: "Less code first: prefer a crate, std, iterators." We use
/// the std `DefaultHasher` (which is SipHash-1-3) — it's fast and sufficient
/// for cache keying. If two different inputs produce the same hash (collision),
/// the worst case is a cache hit that returns the wrong embedding — which
/// would be caught by a downstream vector-search quality check. For a
/// production system, SHA-256 would be used; for SupportOS++'s local-first
/// single-user model, SipHash is adequate.
#[must_use]
pub fn content_hash(text: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Look up a cached embedding by `(input_hash, prompt_version, model)`.
/// Returns `None` if no cache entry exists.
fn lookup_cache(
    conn: &Connection,
    input_hash: &str,
    prompt_version: &str,
    model: &str,
) -> Result<Option<EmbedResponse>> {
    let json: Option<String> = conn
        .query_row(
            "SELECT response_json FROM ai_runs
             WHERE input_hash = ?1 AND prompt_version = ?2 AND model = ?3
             ORDER BY created_at DESC
             LIMIT 1",
            params![input_hash, prompt_version, model],
            |r| r.get(0),
        )
        .ok()
        .flatten();
    match json {
        None => Ok(None),
        Some(j) => {
            let resp: EmbedResponse = serde_json::from_str(&j)
                .map_err(|e| Error::Config(format!("ai_runs cache deserialization failed: {e}")))?;
            Ok(Some(resp))
        }
    }
}

/// Store an embedding result in the cache.
fn store_cache(
    conn: &Connection,
    input_hash: &str,
    prompt_version: &str,
    model: &str,
    response: &EmbedResponse,
) -> Result<()> {
    let json = serde_json::to_string(response)
        .map_err(|e| Error::Config(format!("ai_runs cache serialization failed: {e}")))?;
    conn.execute(
        "INSERT INTO ai_runs (input_hash, prompt_version, model, response_json)
         VALUES (?1, ?2, ?3, ?4)",
        params![input_hash, prompt_version, model, json],
    )?;
    Ok(())
}

/// Embed text with caching + retry. Per KNOWN PITFALLS:
/// - "Re-embed only when the content hash changes" → cache hit returns the
///   stored result without calling the provider.
/// - "Cap failed embedding retries" → up to `MAX_EMBED_RETRIES` retries on
///   provider failure; after that, returns an error.
///
/// # Errors
///
/// Returns `Error::Other` if all retries are exhausted, or `Error::Sqlite`/
/// `Error::Config` for cache I/O failures.
pub async fn embed_with_cache(
    conn: &Connection,
    provider: &dyn LocalAiProvider,
    model: &str,
    text: &str,
    prompt_version: &str,
) -> Result<EmbedResponse> {
    let hash = content_hash(text);

    // 1. Check the cache first (re-embed only when content hash changes).
    if let Some(cached) = lookup_cache(conn, &hash, prompt_version, model)? {
        return Ok(cached);
    }

    // 2. Cache miss — call the provider with retries.
    let mut last_error: Option<String> = None;
    for attempt in 0..MAX_EMBED_RETRIES {
        match provider.embed(model, text).await {
            Ok(response) => {
                // 3. Store the result in the cache.
                store_cache(conn, &hash, prompt_version, model, &response)?;
                return Ok(response);
            }
            Err(e) => {
                last_error = Some(e.to_string());
                // Exponential backoff: 100ms, 200ms, 400ms.
                if attempt + 1 < MAX_EMBED_RETRIES {
                    let delay_ms = 100_u64 * (1 << attempt);
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                }
            }
        }
    }

    // 4. All retries exhausted.
    Err(Error::Other(
        format!(
            "embedding failed after {MAX_EMBED_RETRIES} attempts (model={model}): {}",
            last_error.unwrap_or_else(|| "unknown error".into())
        )
        .into(),
    ))
}

/// Count the number of cached entries for a given model. Used by the Settings
/// UI to show cache size (e.g., "1,234 cached embeddings for model X").
///
/// # Errors
///
/// Returns `Error::Sqlite` if the query fails.
pub fn count_cache_entries(conn: &Connection, model: &str) -> Result<u32> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM ai_runs WHERE model = ?1",
        params![model],
        |r| r.get(0),
    )?;
    Ok(u32::try_from(count).unwrap_or(0))
}

/// Purge all cache entries older than `max_entries` per model (FIFO — oldest
/// entries are deleted first). Used by the Settings UI's "Clear cache" action.
///
/// # Errors
///
/// Returns `Error::Sqlite` if the delete fails.
pub fn trim_cache_per_model(conn: &Connection, max_entries: u32) -> Result<u32> {
    // Delete all but the newest `max_entries` rows per model.
    let rows = conn.execute(
        "DELETE FROM ai_runs
         WHERE id NOT IN (
             SELECT id FROM ai_runs a
             WHERE (SELECT COUNT(*) FROM ai_runs b
                    WHERE b.model = a.model AND b.created_at >= a.created_at)
                   <= ?1
         )",
        params![i64::from(max_entries as i32)],
    )?;
    Ok(u32::try_from(rows).unwrap_or(0))
}

/// Clear all cache entries. Used by the Settings UI's "Clear all cache" action.
///
/// # Errors
///
/// Returns `Error::Sqlite` if the delete fails.
pub fn clear_cache(conn: &Connection) -> Result<u32> {
    let rows = conn.execute("DELETE FROM ai_runs", [])?;
    Ok(u32::try_from(rows).unwrap_or(0))
}

// ─── Chunk embedding passes (worker queue handlers) ────────────────────────
//
// The reference behavior: the embedding jobs are a NO-OP until an embedding
// model is configured in Settings → LM Studio. These entry points are what
// the WorkerManager's `embed_*` job handlers call; the semantic layer
// (conversation/docs/knowledge chunk tables + local cosine fallback) wires
// the real passes.

// ─── Semantic chunk machinery (v1.5.0 hybrid ticket search) ────────────────
//
// The reference `conversationRepo.rechunkConversation` + the worker
// `embedPendingConversationChunks`/`embedPendingDocs`/`embedPendingKnowledge`
// passes are the source of truth. Chunk text = self-describing header
// (#number, subject, customer, tags) + thread bodies in chronological order,
// chunked by the shared `chunkText(text, 1200, 150)` utility; embeddings are
// ALWAYS stored locally (Float32 little-endian in the `embedding` BLOB) so
// semantic search works without Qdrant. Without an embedding model
// configured the embed passes are a documented no-op (FTS remains).

/// How many pending chunks one embed pass pulls (reference
/// `listConversationChunksNeedingEmbedding(60)` / docs / knowledge). The
/// whole list is embedded in ONE `/v1/embeddings` call, exactly like the
/// reference's `aiProvider.embed(texts)`.
pub const EMBED_PASS_LIMIT: i64 = 60;

/// Is `c` whitespace per the JS `\s` character class? (Unicode White_Space
/// plus U+FEFF, which JS counts but Rust's `char::is_whitespace` does not.)
fn is_js_whitespace(c: char) -> bool {
    c.is_whitespace() || c == '\u{feff}'
}

/// JS `String.slice(start, end)` semantics over UTF-16 code units: used for
/// every reference `.slice(...)` on chunk text (bodies are capped at 4000
/// units, snippets at 200). A slice that splits a surrogate pair decodes the
/// lone half as U+FFFD (a Rust `String` cannot hold a lone surrogate).
pub fn utf16_slice(s: &str, start: usize, end: usize) -> String {
    let units: Vec<u16> = s.encode_utf16().collect();
    if start >= units.len() || end <= start {
        return String::new();
    }
    let end = end.min(units.len());
    String::from_utf16_lossy(&units[start..end])
}

/// JS `String.length` (UTF-16 code units) — used for reference `.length`
/// caps (query max 500, snippet 200).
pub fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// Chunk `text` into pieces of at most `max_len` UTF-16 code units with
/// `overlap` units of overlap, preferring sentence boundaries — a reference-
/// exact port of `chunkText` in `src/shared/utils.ts`:
///
/// ```text
/// clean = text.replace(/\s+/g, ' ').trim()
/// if clean.length <= maxLen -> [clean] (or [] when empty)
/// loop: end = min(start + maxLen, len); if end < len and the last '. '
///       before `end` sits past the halfway mark, cut there instead;
///       push slice(start, end).trim(); start = end - overlap
/// ```
///
/// Deterministic: the same input always produces the same chunks.
#[must_use]
pub fn chunk_text(text: &str, max_len: usize, overlap: usize) -> Vec<String> {
    // Collapse whitespace runs to a single space (JS /\s+/g -> ' ').
    let mut collapsed = String::with_capacity(text.len());
    let mut in_ws = false;
    for c in text.chars() {
        if is_js_whitespace(c) {
            if !in_ws {
                collapsed.push(' ');
                in_ws = true;
            }
        } else {
            collapsed.push(c);
            in_ws = false;
        }
    }
    let clean = collapsed.trim();
    let units: Vec<u16> = clean.encode_utf16().collect();
    if units.len() <= max_len {
        return if clean.is_empty() {
            Vec::new()
        } else {
            vec![clean.to_string()]
        };
    }

    let mut chunks: Vec<String> = Vec::new();
    let mut start: usize = 0;
    while start < units.len() {
        let mut end = (start + max_len).min(units.len());
        if end < units.len() {
            // JS: const dot = clean.lastIndexOf('. ', end);
            //     if (dot > start + maxLen * 0.5) end = dot + 1;
            if let Some(dot) = last_index_of_dot_space(&units, end) {
                if dot as f64 > start as f64 + max_len as f64 * 0.5 {
                    end = dot + 1;
                }
            }
        }
        chunks.push(
            String::from_utf16_lossy(&units[start..end])
                .trim()
                .to_string(),
        );
        // JS: start = end - overlap; if (start < 0) start = 0;
        start = end.saturating_sub(overlap);
        if end >= units.len() {
            break;
        }
    }
    chunks
}

/// The greatest index `p <= from` where `units[p] == '.'` and
/// `units[p + 1] == ' '` (JS `String.lastIndexOf('. ', from)`).
fn last_index_of_dot_space(units: &[u16], from: usize) -> Option<usize> {
    if units.len() < 2 {
        return None;
    }
    let max_start = from.min(units.len() - 2);
    let mut p = max_start as isize;
    while p >= 0 {
        let idx = p as usize;
        if units[idx] == u16::from(b'.') && units[idx + 1] == u16::from(b' ') {
            return Some(idx);
        }
        p -= 1;
    }
    None
}

/// (Re)chunk a conversation for semantic ticket search — reference
/// `conversationRepo.rechunkConversation(localId)`:
///
/// - Header: `#number subject` (+ `Customer: first last`, `Tags: a,b` when
///   present).
/// - Thread bodies in chronological order (created_at, then id), each capped
///   at 4000 UTF-16 units, joined with `\n\n---\n\n`. The port's
///   `conversation_threads` has no `deleted_at` column, so the reference's
///   `deleted_at IS NULL` filter is vacuously true here.
/// - `chunkText(full, 1200, 150)` when there is thread text, else the bare
///   header as a single chunk; `chunk_version = 1` (reference inserts 1).
/// - Delete + insert in one transaction — thread changes invalidate previous
///   embeddings (state resets to `not_indexed`).
///
/// No-op (`Ok(())`) when the conversation does not exist.
///
/// # Errors
///
/// Returns `Error::Sqlite` on database failures.
pub fn chunk_conversation(conn: &Connection, conversation_id: i64) -> Result<()> {
    struct ConvRow {
        number: i64,
        subject: Option<String>,
        customer: Option<String>,
        tags: Option<String>,
    }
    let conv: Option<ConvRow> = conn
        .query_row(
            "SELECT c.number, c.subject,
                    TRIM(COALESCE(cu.first_name, '') || ' ' || COALESCE(cu.last_name, '')) AS customer,
                    (SELECT GROUP_CONCAT(t.name) FROM conversation_tags ct
                      JOIN tags t ON t.id = ct.tag_id WHERE ct.conversation_id = c.id) AS tags
             FROM conversations c LEFT JOIN customers cu ON cu.id = c.customer_id
             WHERE c.id = ?1",
            params![conversation_id],
            |r| {
                Ok(ConvRow {
                    number: r.get(0)?,
                    subject: r.get(1)?,
                    customer: r.get(2)?,
                    tags: r.get(3)?,
                })
            },
        )
        .ok();
    let Some(conv) = conv else {
        return Ok(()); // reference: if (!conv) return;
    };

    let mut stmt = conn.prepare(
        "SELECT body_text FROM conversation_threads
          WHERE conversation_id = ?1 AND body_text IS NOT NULL AND LENGTH(body_text) > 0
          ORDER BY created_at ASC, id ASC",
    )?;
    let bodies: Vec<String> = stmt
        .query_map(params![conversation_id], |r| r.get::<_, String>(0))?
        .filter_map(|r| r.ok())
        .collect();
    drop(stmt);

    let mut header_lines: Vec<String> = vec![format!(
        "#{} {}",
        conv.number,
        conv.subject.as_deref().unwrap_or("(no subject)")
    )];
    if let Some(customer) = conv.customer.as_deref().filter(|c| !c.is_empty()) {
        header_lines.push(format!("Customer: {customer}"));
    }
    if let Some(tags) = conv.tags.as_deref().filter(|t| !t.is_empty()) {
        header_lines.push(format!("Tags: {tags}"));
    }
    let header = header_lines.join("\n");

    let thread_text = bodies
        .iter()
        .map(|b| utf16_slice(b, 0, 4000))
        .collect::<Vec<_>>()
        .join("\n\n---\n\n");
    let full = format!("{header}\n\n{thread_text}");
    let chunks = if !thread_text.is_empty() {
        chunk_text(&full, 1200, 150)
    } else {
        vec![full]
    };

    conn.execute_batch("BEGIN")?;
    let tx = (|| -> Result<()> {
        conn.execute(
            "DELETE FROM conversation_chunks WHERE conversation_id = ?1",
            params![conversation_id],
        )?;
        let mut ins = conn.prepare(
            "INSERT INTO conversation_chunks (conversation_id, chunk_index, content, chunk_version)
             VALUES (?1, ?2, ?3, 1)",
        )?;
        for (i, c) in chunks.iter().enumerate() {
            ins.execute(params![conversation_id, i as i64, c])?;
        }
        Ok(())
    })();
    match tx {
        Ok(()) => conn.execute_batch("COMMIT")?,
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(e);
        }
    }
    Ok(())
}

/// Chunk-table stats (reference `conversationChunkStats` /
/// `docsEmbeddingStats`): total chunks, embedded (`indexed`), waiting
/// (`not_indexed`/`queued`) and `failed`. The hybrid search routes gate
/// their semantic layer on `indexed > 0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ConversationChunkStats {
    pub chunks: usize,
    pub indexed: usize,
    pub pending: usize,
    pub failed: usize,
}

/// Stats over `conversation_chunks` (reference `conversationChunkStats`):
/// total chunks, embedded (`indexed`), waiting (`not_indexed`/`queued`) and
/// `failed`. The hybrid search route gates its semantic layer on
/// `indexed > 0`.
///
/// # Errors
///
/// Returns `Error::Sqlite` if the query fails (e.g. the table is missing).
pub fn conversation_chunk_stats(conn: &Connection) -> Result<ConversationChunkStats> {
    let row = conn.query_row(
        "SELECT COUNT(*) AS chunks,
           SUM(CASE WHEN embedding_state = 'indexed' THEN 1 ELSE 0 END) AS indexed,
           SUM(CASE WHEN embedding_state IN ('not_indexed', 'queued') THEN 1 ELSE 0 END) AS pending,
           SUM(CASE WHEN embedding_state = 'failed' THEN 1 ELSE 0 END) AS failed
         FROM conversation_chunks",
        [],
        |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Option<i64>>(1)?,
                r.get::<_, Option<i64>>(2)?,
                r.get::<_, Option<i64>>(3)?,
            ))
        },
    );
    let (chunks, indexed, pending, failed) = match row {
        Ok(v) => v,
        Err(rusqlite::Error::QueryReturnedNoRows) => (0, None, None, None),
        Err(e) => return Err(e.into()),
    };
    Ok(ConversationChunkStats {
        chunks: usize::try_from(chunks).unwrap_or(0),
        indexed: indexed
            .map(|n| usize::try_from(n).unwrap_or(0))
            .unwrap_or(0),
        pending: pending
            .map(|n| usize::try_from(n).unwrap_or(0))
            .unwrap_or(0),
        failed: failed.map(|n| usize::try_from(n).unwrap_or(0)).unwrap_or(0),
    })
}

/// A ticket chunk with its stored local embedding — the no-Qdrant fallback
/// scan input (reference `listConversationChunksWithEmbedding` row).
#[derive(Debug, Clone)]
pub struct ConversationChunkWithEmbedding {
    pub conversation_id: i64,
    pub content: String,
    /// Decoded Float32 little-endian embedding.
    pub embedding: Vec<f32>,
}

/// A docs chunk with its stored local embedding — the no-Qdrant fallback
/// scan input (reference `listDocChunksWithEmbedding` row).
#[derive(Debug, Clone)]
pub struct DocChunkWithEmbedding {
    pub article_id: i64,
    pub content: String,
    /// Decoded Float32 little-endian embedding.
    pub embedding: Vec<f32>,
}

/// All docs chunks with a stored local embedding (the no-Qdrant fallback
/// scan, reference `listDocChunksWithEmbedding`), bounded to 2000 rows like
/// the reference.
///
/// # Errors
///
/// Returns `Error::Sqlite` if the query fails.
pub fn list_doc_chunks_with_embedding(conn: &Connection) -> Result<Vec<DocChunkWithEmbedding>> {
    let mut stmt = conn.prepare(
        "SELECT article_id, content, embedding FROM docs_chunks
          WHERE embedding IS NOT NULL AND embedding_state = 'indexed'
          LIMIT 2000",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<Vec<u8>>>(2)?.unwrap_or_default(),
            ))
        })?
        .filter_map(|r| r.ok())
        .map(|(article_id, content, blob)| DocChunkWithEmbedding {
            article_id,
            content,
            embedding: decode_f32_le(&blob),
        })
        .collect();
    Ok(rows)
}

/// Stats over `docs_chunks` (reference `docsEmbeddingStats`): total chunks,
/// embedded (`indexed`), waiting (`not_indexed`/`queued`) and `failed`. The
/// docs search route gates its semantic layer on `indexed > 0`.
///
/// # Errors
///
/// Returns `Error::Sqlite` if the query fails (e.g. the table is missing).
pub fn docs_chunk_stats(conn: &Connection) -> Result<ConversationChunkStats> {
    let row = conn.query_row(
        "SELECT COUNT(*) AS chunks,
           SUM(CASE WHEN embedding_state = 'indexed' THEN 1 ELSE 0 END) AS indexed,
           SUM(CASE WHEN embedding_state IN ('not_indexed', 'queued') THEN 1 ELSE 0 END) AS pending,
           SUM(CASE WHEN embedding_state = 'failed' THEN 1 ELSE 0 END) AS failed
         FROM docs_chunks",
        [],
        |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Option<i64>>(1)?,
                r.get::<_, Option<i64>>(2)?,
                r.get::<_, Option<i64>>(3)?,
            ))
        },
    );
    let (chunks, indexed, pending, failed) = match row {
        Ok(v) => v,
        Err(rusqlite::Error::QueryReturnedNoRows) => (0, None, None, None),
        Err(e) => return Err(e.into()),
    };
    Ok(ConversationChunkStats {
        chunks: usize::try_from(chunks).unwrap_or(0),
        indexed: indexed
            .map(|n| usize::try_from(n).unwrap_or(0))
            .unwrap_or(0),
        pending: pending
            .map(|n| usize::try_from(n).unwrap_or(0))
            .unwrap_or(0),
        failed: failed.map(|n| usize::try_from(n).unwrap_or(0)).unwrap_or(0),
    })
}

/// All ticket chunks with a stored local embedding (the no-Qdrant fallback
/// scan), bounded to 5000 rows like the reference.
///
/// # Errors
///
/// Returns `Error::Sqlite` if the query fails.
pub fn list_conversation_chunks_with_embedding(
    conn: &Connection,
) -> Result<Vec<ConversationChunkWithEmbedding>> {
    let mut stmt = conn.prepare(
        "SELECT conversation_id, content, embedding FROM conversation_chunks
          WHERE embedding IS NOT NULL AND embedding_state = 'indexed'
          LIMIT 5000",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<Vec<u8>>>(2)?.unwrap_or_default(),
            ))
        })?
        .filter_map(|r| r.ok())
        .map(
            |(conversation_id, content, blob)| ConversationChunkWithEmbedding {
                conversation_id,
                content,
                embedding: decode_f32_le(&blob),
            },
        )
        .collect();
    Ok(rows)
}

/// Decode a Float32 little-endian BLOB into a vector (the reference stores
/// `Buffer.from(Float32Array)` — native little-endian on all supported
/// platforms). Trailing partial words (length % 4 != 0) are dropped.
#[must_use]
pub fn decode_f32_le(bytes: &[u8]) -> Vec<f32> {
    let (words, _) = bytes.as_chunks::<4>();
    words.iter().map(|w| f32::from_le_bytes(*w)).collect()
}

/// Encode a vector as Float32 little-endian bytes for the `embedding` BLOB.
#[must_use]
pub fn encode_f32_le(vector: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(vector.len() * 4);
    for f in vector {
        out.extend_from_slice(&f.to_le_bytes());
    }
    out
}

/// Store a chunk embedding (Float32 LE) + model + `indexed` state.
/// `table` is one of the fixed chunk tables (call sites pass literals).
fn update_chunk_embedding(
    conn: &Connection,
    table: &str,
    chunk_id: i64,
    model: &str,
    embedding: &[f32],
) -> Result<()> {
    conn.execute(
        &format!("UPDATE {table} SET embedding = ?1, embedding_model = ?2, embedding_state = 'indexed' WHERE id = ?3"),
        params![encode_f32_le(embedding), model, chunk_id],
    )?;
    Ok(())
}

// ─── LM Studio embedding endpoint (reference lmStudioClient.embed) ────────

/// LM Studio settings that gate the embedding passes — reference
/// `settingsRepo.getLmStudio()` reads `lmstudio_base_url` (default
/// `http://127.0.0.1:1234`), `lmstudio_embedding_model` (default null) and
/// `lmstudio_timeout_ms` (default 120000) from `application_settings`. The
/// port additionally falls back to the AI Center (`ai_settings`) selection,
/// which is what the port's Settings → LM Studio UI writes.
#[derive(Debug, Clone, PartialEq)]
pub struct LmStudioEmbeddingSettings {
    /// Normalized base URL (no trailing '/', no trailing '/v1') — the
    /// reference client appends `/v1/embeddings`.
    pub base_url: String,
    pub embedding_model: Option<String>,
    pub timeout_ms: u64,
}

/// Read the LM Studio embedding settings (see [`LmStudioEmbeddingSettings`]).
///
/// # Errors
///
/// Returns `Error::Sqlite`/`Error::Config` when the settings store fails.
pub fn lm_studio_embedding_settings(conn: &Connection) -> Result<LmStudioEmbeddingSettings> {
    let model = crate::settings::get_string(conn, "lmstudio_embedding_model")
        .ok()
        .flatten()
        .filter(|m| !m.is_empty())
        .or_else(|| {
            crate::ai_center::get_ai_status(conn)
                .ok()
                .and_then(|s| s.embedding_model)
                .filter(|m| !m.is_empty())
        });
    let base = crate::settings::get_string(conn, "lmstudio_base_url")
        .ok()
        .flatten()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            crate::ai_center::get_ai_status(conn)
                .ok()
                .and_then(|s| s.base_url)
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| "http://127.0.0.1:1234".to_string());
    let timeout_ms = u64::try_from(
        crate::settings::get_i64(conn, "lmstudio_timeout_ms", 120_000).unwrap_or(120_000),
    )
    .unwrap_or(120_000);
    Ok(LmStudioEmbeddingSettings {
        base_url: normalize_lm_base_url(&base),
        embedding_model: model,
        timeout_ms,
    })
}

/// Reference `refreshFromSettings`: strip ONE trailing '/', then ONE
/// trailing '/v1' (so both `http://x:1234` and `http://x:1234/v1/` work).
#[must_use]
pub fn normalize_lm_base_url(url: &str) -> String {
    let s = url.strip_suffix('/').unwrap_or(url);
    let s = s.strip_suffix("/v1").unwrap_or(s);
    s.to_string()
}

/// The `/v1/embeddings` response body (OpenAI-compatible).
#[derive(Debug, Clone, serde::Deserialize)]
pub struct EmbeddingsResponseBody {
    pub data: Vec<EmbeddingsResponseItem>,
}

/// One embedding entry in the response.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct EmbeddingsResponseItem {
    pub embedding: Vec<f32>,
}

/// Embed a batch of texts via LM Studio's OpenAI-compatible
/// `POST {base_url}/v1/embeddings` — reference `lmStudioClient.embed`
/// (request body `{"input": [...], "model": ...}`, vectors mapped by index).
///
/// # Errors
///
/// `Error::Config` on connection/HTTP/parse failures, with reference-shaped
/// messages ("LM Studio embedding request failed (HTTP {status}): ...").
pub async fn embed_texts(
    base_url: &str,
    model: &str,
    texts: &[String],
    timeout_ms: u64,
) -> Result<Vec<Vec<f32>>> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(timeout_ms.max(1)))
        .build()
        .map_err(|e| Error::Config(format!("LM Studio embedding failed: {e}")))?;
    let res = client
        .post(format!("{}/v1/embeddings", base_url))
        .json(&serde_json::json!({ "input": texts, "model": model }))
        .send()
        .await
        .map_err(|e| {
            Error::Config(format!(
                "LM Studio embedding failed: {e}. Load an embedding model in LM Studio (e.g. nomic-embed) and retry."
            ))
        })?;
    if !res.status().is_success() {
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        let truncated: String = text.chars().take(300).collect();
        return Err(Error::Config(format!(
            "LM Studio embedding request failed (HTTP {status}): {truncated}"
        )));
    }
    let body: EmbeddingsResponseBody = res
        .json()
        .await
        .map_err(|e| Error::Config(format!("LM Studio embedding failed: {e}")))?;
    Ok(body.data.into_iter().map(|d| d.embedding).collect())
}

/// Run [`embed_texts`] to completion from synchronous callers (the worker
/// queue's `embed_*` handlers are sync). The future only touches owned data,
/// so it runs on a dedicated thread with its own current-thread runtime —
/// safe both inside and outside a Tokio context.
fn embed_texts_blocking(
    base_url: String,
    model: String,
    texts: Vec<String>,
    timeout_ms: u64,
) -> Result<Vec<Vec<f32>>> {
    std::thread::Builder::new()
        .name("lmstudio-embed".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| Error::Config(format!("LM Studio embedding failed: {e}")))?;
            rt.block_on(embed_texts(&base_url, &model, &texts, timeout_ms))
        })
        .map_err(Error::Io)?
        .join()
        .map_err(|_| Error::Config("LM Studio embedding worker thread panicked".into()))?
}

/// Per-table chunk metadata for the vector payload (reference `workers.ts`
/// embedPending{Docs,ConversationChunks,Knowledge}: each entity type carries
/// its parent's title/visibility in the Qdrant payload).
struct PendingChunk {
    id: i64,
    /// The parent entity id (document / article / conversation).
    entity_id: i64,
    content: String,
    title: String,
    visibility: String,
    /// Conversations only: the ticket number for the payload.
    number: Option<i64>,
}

/// The entity_type tag stored in the vector payload (reference constants).
fn entity_type_for(table: &str) -> &'static str {
    match table {
        "knowledge_chunks" => "knowledge_chunk",
        "docs_chunks" => "docs_chunk",
        _ => "conversation_chunk",
    }
}

/// List up to [`EMBED_PASS_LIMIT`] chunks needing embedding (reference
/// `list*NeedingEmbedding`): state `not_indexed` OR `failed`; docs +
/// conversation chunks cap retries at `embedding_attempts < 5`
/// (reference migration 010); knowledge retries without a cap.
fn list_pending_chunks(conn: &Connection, table: &str) -> Result<Vec<PendingChunk>> {
    let sql = match table {
        "knowledge_chunks" => format!(
            "SELECT c.id, c.document_id, c.content, d.title, d.visibility
             FROM knowledge_chunks c JOIN knowledge_documents d ON d.id = c.document_id
             WHERE c.embedding_state = 'not_indexed' OR c.embedding_state = 'failed'
             LIMIT {EMBED_PASS_LIMIT}"
        ),
        "docs_chunks" => format!(
            "SELECT c.id, c.article_id, c.content, a.name,
                    CASE WHEN a.status = 'published' THEN 'customer_safe' ELSE 'internal_only' END
             FROM docs_chunks c JOIN docs_articles a ON a.id = c.article_id
             WHERE (c.embedding_state = 'not_indexed' OR c.embedding_state = 'failed')
               AND c.embedding_attempts < 5
             LIMIT {EMBED_PASS_LIMIT}"
        ),
        _ => format!(
            "SELECT c.id, c.conversation_id, c.content,
                    COALESCE(substr(v.subject, 1, 300), '#' || v.number), v.number
             FROM conversation_chunks c JOIN conversations v ON v.id = c.conversation_id
             WHERE (c.embedding_state = 'not_indexed' OR c.embedding_state = 'failed')
               AND c.embedding_attempts < 5
             LIMIT {EMBED_PASS_LIMIT}"
        ),
    };
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map([], |r| {
            Ok(PendingChunk {
                id: r.get(0)?,
                entity_id: r.get(1)?,
                content: r.get(2)?,
                title: r.get(3)?,
                visibility: r
                    .get::<_, Option<String>>(4)
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| "internal_only".to_string()),
                number: r
                    .get::<_, Option<i64>>(4)
                    .ok()
                    .flatten()
                    .filter(|_| table == "conversation_chunks"),
            })
        })?
        .filter_map(|r| r.ok())
        .collect();
    drop(stmt);
    Ok(rows)
}

/// Mark a chunk failed (reference `mark*Failed`: state + attempt counter for
/// docs/conversation chunks; knowledge has no counter).
fn mark_chunk_failed(conn: &Connection, table: &str, chunk_id: i64) -> Result<()> {
    if table == "docs_chunks" || table == "conversation_chunks" {
        conn.execute(
            &format!(
                "UPDATE {table} SET embedding_state = 'failed',
                 embedding_attempts = embedding_attempts + 1 WHERE id = ?1"
            ),
            params![chunk_id],
        )?;
    } else {
        conn.execute(
            &format!("UPDATE {table} SET embedding_state = 'failed' WHERE id = ?1"),
            params![chunk_id],
        )?;
    }
    Ok(())
}

/// One embed pass shared by the conversation/docs/knowledge chunk tables —
/// the port of the reference's three `embedPending*` workers
/// (`workers.ts:780-908`):
///
/// - The whole pending list is embedded in ONE provider call; on provider
///   failure every chunk in the list is marked failed (the reference's
///   catch block over the whole chunk list).
/// - docs + conversation chunks: each non-empty vector is ALWAYS stored
///   locally (`*_chunks.embedding`, Float32 LE, state `indexed`) so semantic
///   search works without the vector store; empty vectors mark the chunk
///   failed (attempts +1). When Qdrant is connected the non-empty vectors
///   are additionally upserted in one `ensureCollection` + `upsert` pair
///   (results do not affect the local states).
/// - knowledge chunks: when Qdrant is connected (and the first vector is
///   non-empty) the vectors go to the store in one
///   `ensureCollection` + `upsert` pair and every chunk's state is set from
///   the single upsert result (`indexed`/`failed`); when it is not, they
///   are stored locally (keyword search remains primary).
/// - No embedding model configured: documented no-op (`Ok(0)`) — FTS
///   remains the search path.
fn embed_pending_chunks(
    conn: &Connection,
    table: &str,
    qdrant: Option<&crate::vectorstore_qdrant::EmbeddedQdrant>,
) -> Result<usize> {
    let pending = list_pending_chunks(conn, table)?;
    if pending.is_empty() {
        return Ok(0);
    }
    let settings = lm_studio_embedding_settings(conn)?;
    let Some(model) = settings.embedding_model else {
        return Ok(0); // no embedding model configured — FTS remains the search path
    };

    // Reference: the health check gates the Qdrant legs for the whole pass.
    let qdrant_connected = qdrant.is_some_and(|q| q.health().connected);

    // One provider call over the whole list (reference
    // `aiProvider.embed(chunks.map(c => c.content.slice(0, 4000))`).
    let texts: Vec<String> = pending
        .iter()
        .map(|c| utf16_slice(&c.content, 0, 4000))
        .collect();
    let vectors = match embed_texts_blocking(
        settings.base_url.clone(),
        model.clone(),
        texts,
        settings.timeout_ms,
    ) {
        Ok(v) => v,
        Err(_) => {
            // Reference catch: the whole chunk list is marked failed.
            for chunk in &pending {
                mark_chunk_failed(conn, table, chunk.id)?;
            }
            return Ok(0);
        }
    };

    if table == "knowledge_chunks" {
        // Reference embedPendingKnowledge: Qdrant upsert when connected,
        // local storage when not.
        if qdrant_connected && vectors.first().is_some_and(|v| !v.is_empty()) {
            let dim = vectors[0].len();
            let points: Vec<crate::vectorstore_qdrant::VectorPoint> = pending
                .iter()
                .enumerate()
                .map(|(i, chunk)| {
                    let empty: Vec<f32> = Vec::new();
                    let vector = vectors.get(i).unwrap_or(&empty);
                    vector_point(table, chunk, vector, &model)
                })
                .collect();
            let q = qdrant.expect("checked is_some above");
            // Reference order: ensureCollection(dimension) then upsert(points).
            let _ = q.ensure_collection(dim);
            let ok = q.upsert(&points);
            let state = if ok { "indexed" } else { "failed" };
            for chunk in &pending {
                set_chunk_state(conn, table, chunk.id, state, Some(&model))?;
            }
            return Ok(if ok { pending.len() } else { 0 });
        }
        // Qdrant unavailable: store embeddings locally (keyword search
        // remains primary).
        let mut embedded = 0_usize;
        for (i, chunk) in pending.iter().enumerate() {
            match vectors.get(i) {
                Some(v) if !v.is_empty() => {
                    update_chunk_embedding(conn, table, chunk.id, &model, v)?;
                    embedded += 1;
                }
                _ => set_chunk_state(conn, table, chunk.id, "failed", None)?,
            }
        }
        return Ok(embedded);
    }

    // Reference embedPending{Docs,ConversationChunks}: ALWAYS store locally
    // first (empty vectors mark failed), then one bulk upsert when
    // Qdrant is connected.
    let mut embedded = 0_usize;
    for (i, chunk) in pending.iter().enumerate() {
        match vectors.get(i) {
            Some(v) if !v.is_empty() => {
                update_chunk_embedding(conn, table, chunk.id, &model, v)?;
                embedded += 1;
            }
            // Empty/missing vector: reference marks failed.
            _ => mark_chunk_failed(conn, table, chunk.id)?,
        }
    }
    if qdrant_connected {
        let points: Vec<crate::vectorstore_qdrant::VectorPoint> = pending
            .iter()
            .enumerate()
            .filter_map(|(i, chunk)| {
                let v = vectors.get(i)?;
                if v.is_empty() {
                    return None;
                }
                Some(vector_point(table, chunk, v, &model))
            })
            .collect();
        if !points.is_empty() {
            // Reference order: ensureCollection(points[0].vector.length)
            // then upsert(points) — both results ignored for these tables.
            let q = qdrant.expect("checked is_some above");
            let _ = q.ensure_collection(points[0].vector.len());
            let _ = q.upsert(&points);
        }
    }
    Ok(embedded)
}

/// Build the reference `VectorPoint` payload for a chunk.
fn vector_point(
    table: &str,
    chunk: &PendingChunk,
    vector: &[f32],
    model: &str,
) -> crate::vectorstore_qdrant::VectorPoint {
    let mut payload = serde_json::json!({
        "entity_type": entity_type_for(table),
        "entity_id": chunk.entity_id,
        "chunk_id": chunk.id,
        "title": chunk.title,
        "text": utf16_slice(&chunk.content, 0, 2000),
        "visibility": chunk.visibility,
        "embedding_model": model,
        "index_version": 1
    });
    if table == "conversation_chunks" {
        payload
            .as_object_mut()
            .expect("payload is an object")
            .insert("number".to_string(), chunk.number.into());
    }
    crate::vectorstore_qdrant::VectorPoint {
        id: chunk.id,
        vector: vector.to_vec(),
        payload,
    }
}

/// Set the embedding state without touching the stored vector (reference
/// `setChunkEmbeddingState`).
fn set_chunk_state(
    conn: &Connection,
    table: &str,
    chunk_id: i64,
    state: &str,
    model: Option<&str>,
) -> Result<()> {
    conn.execute(
        &format!(
            "UPDATE {table} SET embedding_state = ?1,
             embedding_model = COALESCE(?2, embedding_model) WHERE id = ?3"
        ),
        params![state, model, chunk_id],
    )?;
    Ok(())
}

/// `embed_knowledge_chunks` — embed pending knowledge chunks. No-op (0)
/// until an embedding model is configured. Vectors go to the embedded
/// Qdrant store when connected (`indexed`/`failed` by the upsert result)
/// or locally to `knowledge_chunks.embedding` (Float32 LE) so the
/// Qdrant-less fallback scan works.
///
/// # Errors
///
/// Returns `Error::Sqlite` on database failures.
pub fn embed_pending_knowledge(
    conn: &Connection,
    qdrant: Option<&crate::vectorstore_qdrant::EmbeddedQdrant>,
) -> Result<usize> {
    embed_pending_chunks(conn, "knowledge_chunks", qdrant)
}

/// `embed_docs_chunks` — embed pending docs chunks (semantic docs search).
/// Same design as [`embed_pending_knowledge`] over `docs_chunks`.
///
/// # Errors
///
/// Returns `Error::Sqlite` on database failures.
pub fn embed_pending_docs(
    conn: &Connection,
    qdrant: Option<&crate::vectorstore_qdrant::EmbeddedQdrant>,
) -> Result<usize> {
    embed_pending_chunks(conn, "docs_chunks", qdrant)
}

/// `embed_conversation_chunks` — embed pending ticket chunks (semantic
/// ticket search). Same design as [`embed_pending_knowledge`] over
/// `conversation_chunks`.
///
/// # Errors
///
/// Returns `Error::Sqlite` on database failures.
pub fn embed_pending_conversation_chunks(
    conn: &Connection,
    qdrant: Option<&crate::vectorstore_qdrant::EmbeddedQdrant>,
) -> Result<usize> {
    embed_pending_chunks(conn, "conversation_chunks", qdrant)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai_provider::FakeAiProvider;
    use tempfile::NamedTempFile;

    fn fresh_db() -> Connection {
        let f = NamedTempFile::new()
            .unwrap()
            .into_temp_path()
            .keep()
            .unwrap();
        let mut conn = crate::db::open(&f).unwrap();
        // The canonical boot chain — tests must exercise the REAL schema
        // (chunk tables with embedding_attempts, mirror tables, guards),
        // never a partial one.
        crate::bootstrap::apply_all(&mut conn).unwrap();
        conn
    }

    // ---- M008 migration ----------------------------------------------------

    #[test]
    fn m008_creates_ai_runs_table() {
        let conn = fresh_db();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM ai_runs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn m008_creates_lookup_index() {
        let conn = fresh_db();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'index' AND name = 'idx_ai_runs_lookup'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn m008_is_idempotent() {
        let conn = fresh_db();
        apply_m008(&conn).unwrap();
    }

    // ---- content_hash ------------------------------------------------------

    #[test]
    fn content_hash_is_deterministic() {
        assert_eq!(content_hash("hello"), content_hash("hello"));
    }

    #[test]
    fn content_hash_differs_for_different_inputs() {
        assert_ne!(content_hash("hello"), content_hash("world"));
    }

    #[test]
    fn content_hash_returns_hex_string() {
        let hash = content_hash("test");
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()), "hash: {hash}");
    }

    // ---- embed_with_cache: cache hit ---------------------------------------

    #[tokio::test]
    async fn embed_with_cache_returns_cached_result_without_calling_provider() {
        let conn = fresh_db();
        let provider = FakeAiProvider::new().with_embedding_dim(8);

        // First call — cache miss, calls the provider, stores the result.
        let r1 = embed_with_cache(&conn, &provider, "fake-embedding-model", "hello", "v1")
            .await
            .unwrap();
        assert_eq!(r1.dim, 8);
        assert_eq!(
            count_cache_entries(&conn, "fake-embedding-model").unwrap(),
            1
        );

        // Second call — cache hit, returns the same result without calling the provider.
        let r2 = embed_with_cache(&conn, &provider, "fake-embedding-model", "hello", "v1")
            .await
            .unwrap();
        assert_eq!(r1.vector, r2.vector, "cache hit returns the same vector");
        assert_eq!(
            count_cache_entries(&conn, "fake-embedding-model").unwrap(),
            1,
            "no duplicate cache entry"
        );
    }

    #[tokio::test]
    async fn embed_with_cache_re_embeds_when_content_changes() {
        let conn = fresh_db();
        let provider = FakeAiProvider::new().with_embedding_dim(8);

        let r1 = embed_with_cache(&conn, &provider, "m", "text A", "v1")
            .await
            .unwrap();
        let r2 = embed_with_cache(&conn, &provider, "m", "text B", "v1")
            .await
            .unwrap();
        assert_ne!(
            r1.vector, r2.vector,
            "different content → different embedding"
        );
        assert_eq!(count_cache_entries(&conn, "m").unwrap(), 2);
    }

    #[tokio::test]
    async fn embed_with_cache_re_embeds_when_prompt_version_changes() {
        let conn = fresh_db();
        let provider = FakeAiProvider::new().with_embedding_dim(8);

        // Same text, same model, different prompt version → cache miss.
        embed_with_cache(&conn, &provider, "m", "same text", "v1")
            .await
            .unwrap();
        embed_with_cache(&conn, &provider, "m", "same text", "v2")
            .await
            .unwrap();
        // The Fake provider is deterministic by input hash, so the vectors are the same,
        // but the cache entries are separate (different prompt_version).
        assert_eq!(
            count_cache_entries(&conn, "m").unwrap(),
            2,
            "different prompt_version → separate cache entries"
        );
    }

    #[tokio::test]
    async fn embed_with_cache_re_embeds_when_model_changes() {
        let conn = fresh_db();
        let provider = FakeAiProvider::new().with_embedding_dim(8);

        // Same text, different model → cache miss.
        embed_with_cache(&conn, &provider, "model-a", "same text", "v1")
            .await
            .unwrap();
        embed_with_cache(&conn, &provider, "model-b", "same text", "v1")
            .await
            .unwrap();
        assert_eq!(count_cache_entries(&conn, "model-a").unwrap(), 1);
        assert_eq!(count_cache_entries(&conn, "model-b").unwrap(), 1);
    }

    // ---- embed_with_cache: retry cap ---------------------------------------

    #[tokio::test]
    async fn embed_with_cache_returns_error_after_max_retries() {
        let conn = fresh_db();
        // A provider that always fails. We use NoopAiProvider which returns
        // empty vectors — but embed_with_cache should still store the empty
        // result (it's a valid response, not an error). So for testing the
        // retry cap, we need a provider that actually returns Err.
        // We'll use a custom failing provider.
        struct FailingProvider;
        #[async_trait::async_trait]
        impl LocalAiProvider for FailingProvider {
            async fn list_models(&self) -> Result<Vec<crate::ai_provider::ModelInfo>> {
                Ok(Vec::new())
            }
            async fn chat(
                &self,
                _: &str,
                _: &[crate::ai_provider::ChatMessage],
            ) -> Result<crate::ai_provider::ChatResponse> {
                Err(Error::Config("always fails".into()))
            }
            async fn embed(&self, _: &str, _: &str) -> Result<crate::ai_provider::EmbedResponse> {
                Err(Error::Config("always fails".into()))
            }
            async fn is_available(&self) -> bool {
                false
            }
        }

        let provider = FailingProvider;
        let result = embed_with_cache(&conn, &provider, "failing-model", "test text", "v1").await;
        assert!(result.is_err(), "should fail after max retries");
        let err = result.unwrap_err().to_string();
        assert!(err.contains("failed after"), "got: {err}");
        assert!(err.contains("3"), "got: {err}");

        // No cache entry should be stored for a failed embedding.
        assert_eq!(count_cache_entries(&conn, "failing-model").unwrap(), 0);
    }

    // ---- cache management --------------------------------------------------

    #[tokio::test]
    async fn count_cache_entries_returns_correct_count() {
        let conn = fresh_db();
        let provider = FakeAiProvider::new().with_embedding_dim(4);
        assert_eq!(count_cache_entries(&conn, "m").unwrap(), 0);
        embed_with_cache(&conn, &provider, "m", "text A", "v1")
            .await
            .unwrap();
        embed_with_cache(&conn, &provider, "m", "text B", "v1")
            .await
            .unwrap();
        embed_with_cache(&conn, &provider, "m", "text C", "v1")
            .await
            .unwrap();
        assert_eq!(count_cache_entries(&conn, "m").unwrap(), 3);
    }

    #[tokio::test]
    async fn clear_cache_removes_all_entries() {
        let conn = fresh_db();
        let provider = FakeAiProvider::new().with_embedding_dim(4);
        embed_with_cache(&conn, &provider, "m", "text A", "v1")
            .await
            .unwrap();
        embed_with_cache(&conn, &provider, "m", "text B", "v1")
            .await
            .unwrap();
        assert_eq!(count_cache_entries(&conn, "m").unwrap(), 2);
        let deleted = clear_cache(&conn).unwrap();
        assert_eq!(deleted, 2);
        assert_eq!(count_cache_entries(&conn, "m").unwrap(), 0);
    }

    #[test]
    fn clear_cache_on_empty_returns_zero() {
        let conn = fresh_db();
        assert_eq!(clear_cache(&conn).unwrap(), 0);
    }

    #[tokio::test]
    async fn trim_cache_per_model_keeps_newest() {
        let conn = fresh_db();
        let provider = FakeAiProvider::new().with_embedding_dim(4);
        // Insert 5 entries with slight delays so created_at differs.
        for i in 0..5 {
            embed_with_cache(&conn, &provider, "m", &format!("text {i}"), "v1")
                .await
                .unwrap();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(count_cache_entries(&conn, "m").unwrap(), 5);
        // Trim to keep only the newest 2.
        let deleted = trim_cache_per_model(&conn, 2).unwrap();
        assert_eq!(deleted, 3, "3 oldest entries deleted");
        assert_eq!(count_cache_entries(&conn, "m").unwrap(), 2);
    }

    #[test]
    fn trim_cache_on_empty_returns_zero() {
        let conn = fresh_db();
        assert_eq!(trim_cache_per_model(&conn, 10).unwrap(), 0);
    }

    // ---- constants ---------------------------------------------------------

    #[test]
    fn max_embed_retries_is_3() {
        assert_eq!(MAX_EMBED_RETRIES, 3);
    }

    // ---- chunk_text (reference shared/utils.ts chunkText) -------------------

    #[test]
    fn chunk_text_short_input_is_single_chunk() {
        assert_eq!(chunk_text("hello world", 1200, 150), vec!["hello world"]);
        // Exactly maxLen -> still one chunk.
        let exact: String = "a".repeat(1200);
        assert_eq!(chunk_text(&exact, 1200, 150), vec![exact]);
    }

    #[test]
    fn chunk_text_empty_or_whitespace_is_empty() {
        assert!(chunk_text("", 1200, 150).is_empty());
        assert!(chunk_text("   \n\t  ", 1200, 150).is_empty());
    }

    #[test]
    fn chunk_text_collapses_whitespace_and_trims() {
        assert_eq!(
            chunk_text("  hello   world \n\n foo\tbar ", 1200, 150),
            vec!["hello world foo bar"]
        );
    }

    #[test]
    fn chunk_text_prefers_sentence_boundary_in_second_half() {
        // 700 A's + ". " + 598 B's = 1300 units. The '. ' at index 700 is
        // past the halfway mark (600), so the first chunk cuts at 701 (the
        // '.' included) and the next starts at 701-150 = 551.
        let text = format!("{}{}{}", "A".repeat(700), ". ", "B".repeat(598));
        let chunks = chunk_text(&text, 1200, 150);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0], format!("{}.", "A".repeat(700)));
        assert_eq!(
            chunks[1],
            format!("{}{}{}", "A".repeat(149), ". ", "B".repeat(598))
        );
    }

    #[test]
    fn chunk_text_without_sentence_boundary_uses_hard_cut_with_overlap() {
        let text = "A".repeat(1300);
        let chunks = chunk_text(&text, 1200, 150);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), 1200);
        assert_eq!(chunks[1].len(), 250); // 1300 - (1200 - 150)
                                          // Overlap: the tail of chunk 0 is the head of chunk 1.
        assert_eq!(&chunks[0][1050..1200], &chunks[1][0..150]);
    }

    #[test]
    fn chunk_text_ignores_sentence_boundary_in_first_half() {
        // '. ' at index 100 is NOT past halfway (600): hard cut at 1200.
        let text = format!("{}{}{}", "A".repeat(100), ". ", "B".repeat(1300));
        let chunks = chunk_text(&text, 1200, 150);
        assert_eq!(chunks[0].len(), 1200);
    }

    #[test]
    fn chunk_text_is_deterministic() {
        let text = format!("{}{}", "Sentence one. ".repeat(120), "tail words");
        let a = chunk_text(&text, 1200, 150);
        let b = chunk_text(&text, 1200, 150);
        assert_eq!(a, b);
        assert!(a.len() > 1);
    }

    #[test]
    fn chunk_text_uses_utf16_code_units() {
        // 500 emoji (2 UTF-16 units each = 1000) + ". " + 598 B's = 1600
        // units. The '. ' at unit 1000 is past halfway (600), so the first
        // chunk cuts at 1001 (surrogate-safe) — a chars()-based port would
        // cut at a different place.
        let text = format!("{}{}{}", "🙂".repeat(500), ". ", "B".repeat(598));
        assert_eq!(utf16_len(&text), 1600);
        let chunks = chunk_text(&text, 1200, 150);
        assert_eq!(chunks.len(), 2);
        assert!(chunks[0].ends_with('.'));
        assert_eq!(utf16_len(&chunks[0]), 1001);
        // Chunk 1 starts at unit 1001-150 = 851 — mid-surrogate-pair: JS
        // yields a lone surrogate there, Rust decodes U+FFFD (documented
        // deviation of the storage layer, same 749-unit span).
        assert!(chunks[1].starts_with('\u{fffd}'));
        assert!(chunks[1].contains(". "));
        assert!(chunks[1].ends_with(&"B".repeat(598)));
        assert_eq!(utf16_len(&chunks[1]), 749);
    }

    #[test]
    fn utf16_slice_matches_js_semantics() {
        let s = "héllo🙂wörld"; // 11 chars, 12 UTF-16 units
        assert_eq!(utf16_slice(s, 0, 5), "héllo");
        assert_eq!(utf16_slice(s, 0, 100), s);
        assert_eq!(utf16_slice(s, 6, 7), "\u{fffd}"); // splits the surrogate pair
        assert_eq!(utf16_slice(s, 5, 5), "");
        assert_eq!(utf16_slice(s, 100, 200), "");
    }

    // ---- chunk_conversation (reference rechunkConversation) ------------------

    fn insert_conv_with_threads(conn: &Connection, id: i64) {
        conn.execute(
            "INSERT INTO customers (id, remote_id, first_name, last_name, email)
             VALUES (?1, ?1, 'Ada', 'Lovelace', 'ada@example.com')",
            params![id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO conversations (id, remote_id, number, subject, mailbox_id, customer_id)
             VALUES (?1, ?1, ?2, 'Refund question', 1, ?1)",
            params![id, 100 + id],
        )
        .unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO tags (id, remote_id, name, slug) VALUES (1, 1, 'billing', 'billing')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO conversation_tags (conversation_id, tag_id) VALUES (?1, 1)",
            params![id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO conversation_threads (conversation_id, type, body_text, from_type, created_at)
             VALUES (?1, 'customer', 'My refund is late.', 'customer', '2026-01-01T10:00:00Z')",
            params![id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO conversation_threads (conversation_id, type, body_text, from_type, created_at)
             VALUES (?1, 'reply', 'Checking on it now.', 'user', '2026-01-01T11:00:00Z')",
            params![id],
        )
        .unwrap();
    }

    fn chunk_rows(conn: &Connection, conversation_id: i64) -> Vec<(i64, String, i64)> {
        let mut stmt = conn
            .prepare(
                "SELECT chunk_index, content, chunk_version FROM conversation_chunks
                  WHERE conversation_id = ?1 ORDER BY chunk_index",
            )
            .unwrap();
        stmt.query_map(params![conversation_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .unwrap()
        .filter_map(|r| r.ok())
        .collect()
    }

    #[test]
    fn chunk_conversation_creates_header_plus_threads() {
        let conn = fresh_db();
        insert_conv_with_threads(&conn, 5);
        chunk_conversation(&conn, 5).unwrap();
        let rows = chunk_rows(&conn, 5);
        assert_eq!(rows.len(), 1, "short conversation = one chunk");
        let (index, content, version) = &rows[0];
        assert_eq!(*index, 0);
        assert_eq!(*version, 1, "reference inserts chunk_version = 1");
        // chunkText collapses ALL whitespace runs (even for short text), so
        // the stored chunk is the single-spaced form of header + bodies.
        assert_eq!(
            content,
            "#105 Refund question Customer: Ada Lovelace Tags: billing My refund is late. --- Checking on it now."
        );
        // New chunks reset the embedding state machine.
        let stats = conversation_chunk_stats(&conn).unwrap();
        assert_eq!(stats.chunks, 1);
        assert_eq!(stats.indexed, 0);
        assert_eq!(stats.pending, 1);
        assert_eq!(stats.failed, 0);
    }

    #[test]
    fn chunk_conversation_no_threads_is_header_only_chunk() {
        let conn = fresh_db();
        conn.execute(
            "INSERT INTO conversations (id, remote_id, number, subject, mailbox_id, customer_id)
             VALUES (7, 7, 107, null, 1, 1)",
            [],
        )
        .unwrap();
        chunk_conversation(&conn, 7).unwrap();
        let rows = chunk_rows(&conn, 7);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].1, "#107 (no subject)\n\n");
    }

    #[test]
    fn chunk_conversation_skips_null_and_empty_thread_bodies() {
        // Reference filter: body_text IS NOT NULL AND LENGTH(body_text) > 0 —
        // a whitespace-only body still passes (SQLite LENGTH('   ') = 3).
        let conn = fresh_db();
        conn.execute(
            "INSERT INTO conversations (id, remote_id, number, subject, mailbox_id, customer_id)
             VALUES (9, 9, 109, 'S', 1, 1)",
            [],
        )
        .unwrap();
        for body in [Some(""), None] {
            conn.execute(
                "INSERT INTO conversation_threads (conversation_id, type, body_text, from_type)
                 VALUES (9, 'note', ?1, 'user')",
                params![body],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO conversation_threads (conversation_id, type, body_text, from_type)
             VALUES (9, 'customer', 'real body', 'customer')",
            [],
        )
        .unwrap();
        chunk_conversation(&conn, 9).unwrap();
        let rows = chunk_rows(&conn, 9);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].1, "#109 S real body");
    }

    #[test]
    fn chunk_conversation_rechunks_on_change() {
        let conn = fresh_db();
        insert_conv_with_threads(&conn, 5);
        chunk_conversation(&conn, 5).unwrap();
        // Mark the chunk as embedded, then add a thread and rechunk — the
        // old chunk (and its embedding) must be replaced.
        conn.execute(
            "UPDATE conversation_chunks SET embedding_state = 'indexed', embedding = x'00000000'",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO conversation_threads (conversation_id, type, body_text, from_type, created_at)
             VALUES (5, 'customer', 'A much longer follow-up message that adds text.', 'customer', '2026-01-02T10:00:00Z')",
            [],
        )
        .unwrap();
        chunk_conversation(&conn, 5).unwrap();
        let rows = chunk_rows(&conn, 5);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].1.contains("follow-up"));
        let stats = conversation_chunk_stats(&conn).unwrap();
        assert_eq!(stats.chunks, 1);
        assert_eq!(stats.indexed, 0, "rechunk resets embeddings");
        assert_eq!(stats.pending, 1);
    }

    #[test]
    fn chunk_conversation_unknown_id_is_noop() {
        let conn = fresh_db();
        chunk_conversation(&conn, 424242).unwrap();
        assert_eq!(conversation_chunk_stats(&conn).unwrap().chunks, 0);
    }

    #[test]
    fn chunk_conversation_long_threads_split_into_multiple_chunks() {
        let conn = fresh_db();
        conn.execute(
            "INSERT INTO conversations (id, remote_id, number, subject, mailbox_id, customer_id)
             VALUES (11, 11, 111, 'Long one', 1, 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO conversation_threads (conversation_id, type, body_text, from_type)
             VALUES (11, 'customer', ?, 'customer')",
            params![format!("{} ", "word ".repeat(700))],
        )
        .unwrap();
        chunk_conversation(&conn, 11).unwrap();
        let rows = chunk_rows(&conn, 11);
        assert!(
            rows.len() > 1,
            "expected multiple chunks, got {}",
            rows.len()
        );
        for (_, _, version) in &rows {
            assert_eq!(*version, 1);
        }
        // Deterministic: rechunking produces the same chunks.
        chunk_conversation(&conn, 11).unwrap();
        let again = chunk_rows(&conn, 11);
        assert_eq!(rows, again);
    }

    // ---- conversation_chunk_stats ---------------------------------------------

    #[test]
    fn conversation_chunk_stats_counts_states() {
        let conn = fresh_db();
        insert_conv_with_threads(&conn, 5);
        chunk_conversation(&conn, 5).unwrap();
        conn.execute(
            "INSERT INTO conversation_chunks (conversation_id, chunk_index, content, embedding_state)
             VALUES (5, 99, 'extra', 'indexed')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO conversation_chunks (conversation_id, chunk_index, content, embedding_state)
             VALUES (5, 98, 'failed one', 'failed')",
            [],
        )
        .unwrap();
        let stats = conversation_chunk_stats(&conn).unwrap();
        assert_eq!(stats.chunks, 3);
        assert_eq!(stats.indexed, 1);
        assert_eq!(stats.pending, 1);
        assert_eq!(stats.failed, 1);
    }

    #[test]
    fn conversation_chunk_stats_empty_table() {
        let conn = fresh_db();
        let stats = conversation_chunk_stats(&conn).unwrap();
        assert_eq!(
            stats,
            ConversationChunkStats {
                chunks: 0,
                indexed: 0,
                pending: 0,
                failed: 0
            }
        );
    }

    // ---- Float32 LE storage ---------------------------------------------------

    #[test]
    fn f32_le_round_trip() {
        let v = [0.1_f32, -2.5, 3.0e7, 0.0, -0.0];
        let bytes = encode_f32_le(&v);
        assert_eq!(bytes.len(), v.len() * 4);
        assert_eq!(decode_f32_le(&bytes), v);
    }

    #[test]
    fn f32_le_decode_drops_partial_words() {
        let bytes = encode_f32_le(&[1.0_f32, 2.0]);
        assert_eq!(decode_f32_le(&bytes[..bytes.len() - 1]).len(), 1);
        assert!(decode_f32_le(&[]).is_empty());
    }

    #[test]
    fn list_conversation_chunks_with_embedding_returns_indexed_only() {
        let conn = fresh_db();
        insert_conv_with_threads(&conn, 5);
        insert_conv_with_threads(&conn, 6);
        chunk_conversation(&conn, 5).unwrap();
        chunk_conversation(&conn, 6).unwrap();
        // Embed conv 5's chunk; leave conv 6 pending; add an indexed row with
        // a NULL embedding (must be excluded) and a not_indexed row with an
        // embedding (must be excluded too).
        let vector = [0.25_f32, 0.5, 0.75];
        conn.execute(
            "UPDATE conversation_chunks SET embedding = ?1, embedding_state = 'indexed'
             WHERE conversation_id = 5",
            params![encode_f32_le(&vector)],
        )
        .unwrap();
        let with_null: i64 = conn
            .query_row(
                "SELECT id FROM conversation_chunks WHERE conversation_id = 6",
                [],
                |r| r.get(0),
            )
            .unwrap();
        conn.execute(
            "UPDATE conversation_chunks SET embedding_state = 'indexed' WHERE id = ?1",
            params![with_null],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO conversation_chunks (conversation_id, chunk_index, content, embedding, embedding_state)
             VALUES (5, 90, 'null blob', NULL, 'indexed'),
                    (5, 91, 'wrong state', ?1, 'not_indexed')",
            params![encode_f32_le(&vector)],
        )
        .unwrap();

        let chunks = list_conversation_chunks_with_embedding(&conn).unwrap();
        assert_eq!(chunks.len(), 1, "only the indexed chunk with a blob");
        assert_eq!(chunks[0].conversation_id, 5);
        assert_eq!(chunks[0].embedding, vector);
        assert!(chunks[0].content.contains("Refund question"));
    }

    // ---- LM Studio settings ----------------------------------------------------

    #[test]
    fn lm_studio_settings_defaults() {
        let conn = fresh_db();
        let s = lm_studio_embedding_settings(&conn).unwrap();
        assert_eq!(s.base_url, "http://127.0.0.1:1234");
        assert_eq!(s.embedding_model, None);
        assert_eq!(s.timeout_ms, 120_000);
    }

    #[test]
    fn lm_studio_settings_reads_reference_keys() {
        let conn = fresh_db();
        crate::settings::set_string(&conn, "lmstudio_base_url", "http://localhost:9999/v1/")
            .unwrap();
        crate::settings::set_string(&conn, "lmstudio_embedding_model", "nomic-embed").unwrap();
        crate::settings::set_i64(&conn, "lmstudio_timeout_ms", 5_000).unwrap();
        let s = lm_studio_embedding_settings(&conn).unwrap();
        assert_eq!(s.base_url, "http://localhost:9999");
        assert_eq!(s.embedding_model.as_deref(), Some("nomic-embed"));
        assert_eq!(s.timeout_ms, 5_000);
    }

    #[test]
    fn lm_studio_settings_falls_back_to_ai_center() {
        let conn = fresh_db();
        crate::ai_center::set_embedding_model(&conn, "bge-small", 384).unwrap();
        crate::ai_center::set_base_url(&conn, "http://127.0.0.1:1234/v1").unwrap();
        let s = lm_studio_embedding_settings(&conn).unwrap();
        assert_eq!(s.embedding_model.as_deref(), Some("bge-small"));
        assert_eq!(s.base_url, "http://127.0.0.1:1234");
    }

    #[test]
    fn normalize_lm_base_url_strips_one_slash_and_v1() {
        assert_eq!(normalize_lm_base_url("http://x:1234"), "http://x:1234");
        assert_eq!(normalize_lm_base_url("http://x:1234/"), "http://x:1234");
        assert_eq!(normalize_lm_base_url("http://x:1234/v1"), "http://x:1234");
        assert_eq!(normalize_lm_base_url("http://x:1234/v1/"), "http://x:1234");
        assert_eq!(
            normalize_lm_base_url("http://x:1234/v11"),
            "http://x:1234/v11"
        );
    }

    // ---- embed_pending_* passes -------------------------------------------------

    #[test]
    fn embed_pending_is_noop_without_model() {
        let conn = fresh_db();
        insert_conv_with_threads(&conn, 5);
        chunk_conversation(&conn, 5).unwrap();
        assert_eq!(embed_pending_conversation_chunks(&conn, None).unwrap(), 0);
        assert_eq!(embed_pending_docs(&conn, None).unwrap(), 0);
        assert_eq!(embed_pending_knowledge(&conn, None).unwrap(), 0);
        let stats = conversation_chunk_stats(&conn).unwrap();
        assert_eq!(stats.indexed, 0, "state untouched without a model");
        assert_eq!(stats.pending, 1);
    }

    #[test]
    fn embed_pending_marks_failed_when_endpoint_unreachable() {
        let conn = fresh_db();
        insert_conv_with_threads(&conn, 5);
        chunk_conversation(&conn, 5).unwrap();
        // Port 9 (discard) is never listening locally: connection refused.
        crate::settings::set_string(&conn, "lmstudio_base_url", "http://127.0.0.1:9").unwrap();
        crate::settings::set_string(&conn, "lmstudio_embedding_model", "nomic-embed").unwrap();
        crate::settings::set_i64(&conn, "lmstudio_timeout_ms", 500).unwrap();
        // Reference catch block: the whole chunk list is marked failed
        // (attempts +1 for conversation chunks).
        assert_eq!(embed_pending_conversation_chunks(&conn, None).unwrap(), 0);
        let stats = conversation_chunk_stats(&conn).unwrap();
        assert_eq!(stats.indexed, 0);
        assert_eq!(stats.pending, 0, "chunk left the pending state");
        assert_eq!(stats.failed, 1);
        let attempts: i64 = conn
            .query_row(
                "SELECT embedding_attempts FROM conversation_chunks",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(attempts, 1);
    }

    #[tokio::test]
    async fn embed_pending_conversation_chunks_stores_vectors_locally() {
        let conn = fresh_db();
        insert_conv_with_threads(&conn, 5);
        chunk_conversation(&conn, 5).unwrap();

        // A fake LM Studio: every text embeds to [utf16_len, 0.5, 0.25].
        let app = axum::Router::new().route(
            "/v1/embeddings",
            axum::routing::post(
                |axum::Json(body): axum::Json<serde_json::Value>| async move {
                    let inputs = body["input"].as_array().map(Vec::len).unwrap_or(0);
                    let data: Vec<serde_json::Value> = (0..inputs)
                        .map(|_| serde_json::json!({ "embedding": [3.0, 0.5, 0.25] }))
                        .collect();
                    axum::Json(serde_json::json!({ "data": data }))
                },
            ),
        );
        // The embed pass is a SYNC call that blocks the test thread; a
        // server spawned on the test runtime would never be polled, and a
        // listener bound on the test runtime cannot be served elsewhere.
        // Bind inside the dedicated thread (std listener + from_std).
        let std_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        std_listener.set_nonblocking(true).unwrap();
        let addr = std_listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                let listener = tokio::net::TcpListener::from_std(std_listener).unwrap();
                let _ = axum::serve(listener, app).await;
            });
        });

        crate::settings::set_string(&conn, "lmstudio_base_url", &format!("http://{addr}")).unwrap();
        crate::settings::set_string(&conn, "lmstudio_embedding_model", "fake-embed").unwrap();
        crate::settings::set_i64(&conn, "lmstudio_timeout_ms", 2_000).unwrap();

        let embedded = embed_pending_conversation_chunks(&conn, None).unwrap();
        assert_eq!(embedded, 1);
        let stats = conversation_chunk_stats(&conn).unwrap();
        assert_eq!(stats.indexed, 1);
        assert_eq!(stats.pending, 0);

        let chunks = list_conversation_chunks_with_embedding(&conn).unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].embedding, vec![3.0, 0.5, 0.25]);
        let model: String = conn
            .query_row(
                "SELECT embedding_model FROM conversation_chunks WHERE conversation_id = 5",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(model, "fake-embed");

        // Second pass: nothing pending anymore.
        assert_eq!(embed_pending_conversation_chunks(&conn, None).unwrap(), 0);
    }

    #[tokio::test]
    async fn embed_pending_docs_and_knowledge_share_the_pass() {
        let conn = fresh_db();
        conn.execute(
            "INSERT INTO docs_collections (remote_id, name) VALUES (1, 'Docs')",
            [],
        )
        .unwrap();
        let docs_collection = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO docs_articles (collection_local_id, remote_id, name, text_plain) VALUES (?1, 10, 'Article', 'docs article body')",
            [docs_collection],
        )
        .unwrap();
        let docs_article = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO docs_chunks (article_id, chunk_index, content) VALUES (?1, 0, 'docs chunk')",
            [docs_article],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO knowledge_sources (name, kind, visibility) VALUES ('s', 'import', 'internal_only')",
            [],
        )
        .unwrap();
        let source = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO knowledge_documents (source_id, title, visibility) VALUES (?1, 'Doc', 'internal_only')",
            [source],
        )
        .unwrap();
        let document = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO knowledge_chunks (document_id, chunk_index, content) VALUES (?1, 0, 'knowledge chunk')",
            [document],
        )
        .unwrap();
        crate::settings::set_string(&conn, "lmstudio_embedding_model", "fake-embed").unwrap();
        // No base_url override -> default http://127.0.0.1:1234 which is not
        // running in CI: both passes mark their chunks failed (reference
        // catch block).
        assert_eq!(embed_pending_docs(&conn, None).unwrap(), 0);
        assert_eq!(embed_pending_knowledge(&conn, None).unwrap(), 0);
        let docs_state: String = conn
            .query_row("SELECT embedding_state FROM docs_chunks", [], |r| r.get(0))
            .unwrap();
        let knowledge_state: String = conn
            .query_row("SELECT embedding_state FROM knowledge_chunks", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(docs_state, "failed");
        assert_eq!(knowledge_state, "failed");
    }

    #[tokio::test]
    async fn embed_texts_maps_vectors_by_input_index() {
        let app = axum::Router::new().route(
            "/v1/embeddings",
            axum::routing::post(
                |axum::Json(body): axum::Json<serde_json::Value>| async move {
                    // Echo: embedding = [input_index, 1.0].
                    let inputs = body["input"].as_array().map(Vec::len).unwrap_or(0);
                    let data: Vec<serde_json::Value> = (0..inputs)
                        .map(|i| serde_json::json!({ "embedding": [i as f32, 1.0] }))
                        .collect();
                    axum::Json(serde_json::json!({ "data": data }))
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let texts: Vec<String> = vec!["a".into(), "b".into(), "c".into()];
        let vectors = embed_texts(&format!("http://{addr}"), "m", &texts, 2_000)
            .await
            .unwrap();
        assert_eq!(vectors.len(), 3);
        assert_eq!(vectors[0], vec![0.0, 1.0]);
        assert_eq!(vectors[1], vec![1.0, 1.0]);
        assert_eq!(vectors[2], vec![2.0, 1.0]);
    }

    #[tokio::test]
    async fn embed_texts_http_error_carries_status() {
        let app = axum::Router::new().route(
            "/v1/embeddings",
            axum::routing::post(|| async {
                (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "boom")
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let err = embed_texts(&format!("http://{addr}"), "m", &["x".to_string()], 2_000)
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("HTTP 500"), "message: {msg}");
        assert!(msg.contains("boom"), "message: {msg}");
    }

    #[test]
    fn embed_pass_limit_matches_reference() {
        assert_eq!(EMBED_PASS_LIMIT, 60);
    }
}
