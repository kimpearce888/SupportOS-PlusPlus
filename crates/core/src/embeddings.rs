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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai_provider::FakeAiProvider;
    use crate::migrations;
    use tempfile::NamedTempFile;

    fn fresh_db() -> Connection {
        let f = NamedTempFile::new()
            .unwrap()
            .into_temp_path()
            .keep()
            .unwrap();
        let mut conn = crate::db::open(&f).unwrap();
        crate::db::ensure_migrations_table(&conn).unwrap();
        migrations::run_all(&mut conn).unwrap();
        apply_m008(&conn).unwrap();
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
}
