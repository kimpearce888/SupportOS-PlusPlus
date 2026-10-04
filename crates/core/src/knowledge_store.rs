//! Knowledge store — the port of the reference `KnowledgeRepository`
//! (`database/repositories/knowledgeRepo.ts`) and `KnowledgeIngestor`
//! (`knowledge/ingestor.ts`).
//!
//! The REAL knowledge store is `knowledge_sources` / `knowledge_documents` /
//! `knowledge_chunks` (M039, reference migration 013) with the
//! `fts_knowledge` FTS5 index maintained in the same pass as every write.
//! The v1.x knowledge routes operated on the legacy `knowledge_doc_freshness`
//! side table — they never touched the documents search and the AI pipeline
//! actually read — this module is the honest replacement.
//!
//! Safety invariants carried over:
//! - `upsert_document` is checksum-gated: identical content is a no-op
//!   (no version bump, no chunk churn).
//! - The chunk + FTS rebuild happens atomically (one transaction) so a
//!   crash mid-update can never leave a document with deleted chunks and
//!   no FTS rows (reference v1.6.0 fix).
//! - `delete_document` removes chunks + FTS rows + the document in one
//!   transaction — orphaned FTS rows kept matching searches for deleted
//!   documents in the reference's own history.
//! - File imports are confined to the `knowledge-import` folder under the
//!   data directory (the reference's v1.6.0 audit fix: `data/` itself —
//!   the live SQLite DB, WAL files, sync bundles — is never importable).

use std::path::{Path, PathBuf};

use rusqlite::{params, Connection};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

/// The folder (under the data directory) that file imports are confined to.
pub const IMPORT_DIR_NAME: &str = "knowledge-import";

// ─── chunking + checksums (reference shared/utils.ts chunkText) ──────────

/// `chunkText(text, 1200, 150)`: whitespace-normalize, cut at most `max_len`
/// chars per chunk preferring a sentence boundary, stepping back `overlap`.
#[must_use]
pub fn chunk_text(text: &str, max_len: usize, overlap: usize) -> Vec<String> {
    let clean: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if clean.len() <= max_len {
        return if clean.is_empty() {
            Vec::new()
        } else {
            vec![clean]
        };
    }
    let bytes = clean.as_bytes();
    let mut chunks: Vec<String> = Vec::new();
    let mut start = 0usize;
    while start < bytes.len() {
        let mut end = (start + max_len).min(bytes.len());
        if end < bytes.len() {
            // Prefer a sentence boundary in the back half of the window.
            let window = &clean[start..end];
            if let Some(dot) = window.rfind(". ") {
                if dot > max_len / 2 {
                    end = start + dot + 1;
                }
            }
        }
        // Keep cuts on char boundaries.
        while end < bytes.len() && !clean.is_char_boundary(end) {
            end += 1;
        }
        chunks.push(clean[start..end].trim().to_string());
        if end >= bytes.len() {
            break;
        }
        start = end.saturating_sub(overlap);
    }
    chunks
}

/// sha256 hex digest of the content (the change-detection checksum).
#[must_use]
pub fn checksum(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

// ─── sources ─────────────────────────────────────────────────────────────

/// `getSource` + `createSource` combined: the id of the named source,
/// creating it when missing (kind: manual / local_file / import).
pub fn get_or_create_source(
    conn: &Connection,
    name: &str,
    kind: &str,
    visibility: &str,
) -> Result<i64> {
    let existing: Option<i64> = conn
        .query_row(
            "SELECT id FROM knowledge_sources WHERE name = ?1",
            params![name],
            |r| r.get(0),
        )
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })?;
    if let Some(id) = existing {
        return Ok(id);
    }
    conn.execute(
        "INSERT INTO knowledge_sources (name, kind, visibility) VALUES (?1, ?2, ?3)",
        params![name, kind, visibility],
    )?;
    Ok(conn.last_insert_rowid())
}

/// `listSources`: every source with its document count.
#[must_use]
pub fn list_sources(conn: &Connection) -> Vec<Value> {
    conn.prepare(
        "SELECT s.id, s.name, s.kind, s.visibility, s.created_at,
           (SELECT COUNT(*) FROM knowledge_documents d WHERE d.source_id = s.id) AS document_count
         FROM knowledge_sources s ORDER BY s.name",
    )
    .and_then(|mut stmt| {
        let rows = stmt.query_map([], |r| {
            Ok(json!({
                "id": r.get::<_, i64>(0)?,
                "name": r.get::<_, String>(1)?,
                "kind": r.get::<_, String>(2)?,
                "visibility": r.get::<_, String>(3)?,
                "created_at": r.get::<_, String>(4)?,
                "document_count": r.get::<_, i64>(5)?,
            }))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
    })
    .unwrap_or_default()
}

// ─── documents ───────────────────────────────────────────────────────────

/// `listDocuments`: every document (optionally one source's) with the
/// content preview and chunk count, ordered by title.
#[must_use]
pub fn list_documents(conn: &Connection, source_id: Option<i64>) -> Vec<Value> {
    let sql = "SELECT d.id, d.source_id, d.title, d.visibility, d.version, d.content,
                d.format, d.created_at, d.updated_at,
                (SELECT COUNT(*) FROM knowledge_chunks c WHERE c.document_id = d.id) AS chunk_count
              FROM knowledge_documents d
              WHERE (?1 IS NULL OR d.source_id = ?1)
              ORDER BY d.title";
    conn.prepare(sql)
        .and_then(|mut stmt| {
            let rows = stmt.query_map(params![source_id], |r| {
                let content: Option<String> = r.get(5)?;
                let preview: String = content
                    .unwrap_or_default()
                    .chars()
                    .take(200)
                    .collect::<String>()
                    .trim()
                    .to_string();
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "source_id": r.get::<_, i64>(1)?,
                    "title": r.get::<_, String>(2)?,
                    "visibility": r.get::<_, String>(3)?,
                    "version": r.get::<_, Option<i64>>(4)?.unwrap_or(1),
                    "content_preview": preview,
                    "format": r.get::<_, Option<String>>(6)?,
                    "created_at": r.get::<_, String>(7)?,
                    "updated_at": r.get::<_, String>(8)?,
                    "chunk_count": r.get::<_, i64>(9)?,
                }))
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap_or_default()
}

/// `getDocument`: one document with the source name and chunk count.
#[must_use]
pub fn get_document(conn: &Connection, id: i64) -> Option<Value> {
    conn.query_row(
        "SELECT d.id, d.source_id, d.title, d.visibility, d.version, d.content, d.format,
                d.created_at, d.updated_at, d.last_reviewed_at, d.last_verified_at,
                s.name AS source_name,
                (SELECT COUNT(*) FROM knowledge_chunks c WHERE c.document_id = d.id) AS chunk_count
         FROM knowledge_documents d JOIN knowledge_sources s ON s.id = d.source_id
         WHERE d.id = ?1",
        params![id],
        |r| {
            let content: Option<String> = r.get(5)?;
            let preview: String = content
                .clone()
                .unwrap_or_default()
                .chars()
                .take(200)
                .collect::<String>()
                .trim()
                .to_string();
            Ok(json!({
                "id": r.get::<_, i64>(0)?,
                "source_id": r.get::<_, i64>(1)?,
                "title": r.get::<_, String>(2)?,
                "visibility": r.get::<_, String>(3)?,
                "version": r.get::<_, Option<i64>>(4)?.unwrap_or(1),
                "content": content.unwrap_or_default(),
                "content_preview": preview,
                "format": r.get::<_, Option<String>>(6)?,
                "created_at": r.get::<_, String>(7)?,
                "updated_at": r.get::<_, String>(8)?,
                "last_reviewed_at": r.get::<_, Option<String>>(9)?,
                "last_verified_at": r.get::<_, Option<String>>(10)?,
                "source_name": r.get::<_, String>(11)?,
                "chunk_count": r.get::<_, i64>(12)?,
            }))
        },
    )
    .ok()
}

/// `upsertDocument`: insert-or-update gated by the content checksum.
/// Returns `(document_id, changed)`; the chunk + FTS rebuild is one
/// transaction so a crash can never split them.
///
/// # Errors
///
/// Returns [`crate::error::Error::Sqlite`] when a statement fails.
pub fn upsert_document(
    conn: &Connection,
    source_id: i64,
    title: &str,
    content: &str,
    visibility: &str,
    format: &str,
) -> Result<(i64, bool)> {
    let checksum = checksum(content);
    let existing: Option<(i64, Option<String>)> = conn
        .query_row(
            "SELECT id, checksum FROM knowledge_documents WHERE source_id = ?1 AND title = ?2",
            params![source_id, title],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })?;
    if let Some((id, existing_checksum)) = &existing {
        if existing_checksum.as_deref() == Some(checksum.as_str()) {
            return Ok((*id, false));
        }
    }

    let chunks = chunk_text(content, 1200, 150);
    conn.execute_batch("BEGIN")?;
    let outcome = (|| -> Result<i64> {
        let doc_id = match existing {
            Some((id, _)) => {
                conn.execute(
                    "UPDATE knowledge_documents
                     SET version = version + 1, checksum = ?1, content = ?2, format = ?3,
                         visibility = ?4, updated_at = datetime('now')
                     WHERE id = ?5",
                    params![checksum, content, format, visibility, id],
                )?;
                id
            }
            None => {
                conn.execute(
                    "INSERT INTO knowledge_documents (source_id, title, visibility, version, checksum, content, format)
                     VALUES (?1, ?2, ?3, 1, ?4, ?5, ?6)",
                    params![source_id, title, visibility, checksum, content, format],
                )?;
                conn.last_insert_rowid()
            }
        };
        conn.execute(
            "DELETE FROM knowledge_chunks WHERE document_id = ?1",
            params![doc_id],
        )?;
        conn.execute(
            "DELETE FROM fts_knowledge WHERE document_id = ?1",
            params![doc_id],
        )?;
        for (index, chunk) in chunks.iter().enumerate() {
            conn.execute(
                "INSERT INTO knowledge_chunks (document_id, chunk_index, content, chunk_version)
                 VALUES (?1, ?2, ?3, 2)",
                params![doc_id, index as i64, chunk],
            )?;
            let chunk_id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO fts_knowledge (title, content, chunk_id, document_id, visibility)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![title, chunk, chunk_id, doc_id, visibility],
            )?;
        }
        conn.execute(
            "UPDATE knowledge_documents SET last_indexed_at = datetime('now') WHERE id = ?1",
            params![doc_id],
        )?;
        Ok(doc_id)
    })();
    match outcome {
        Ok(id) => {
            conn.execute_batch("COMMIT")?;
            Ok((id, true))
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

/// `deleteDocument`: chunks + FTS rows + the document, atomically.
/// Returns whether a document was removed.
///
/// # Errors
///
/// Returns [`crate::error::Error::Sqlite`] when a statement fails.
pub fn delete_document(conn: &Connection, id: i64) -> Result<bool> {
    conn.execute_batch("BEGIN")?;
    let outcome = (|| -> Result<bool> {
        conn.execute(
            "DELETE FROM knowledge_chunks WHERE document_id = ?1",
            params![id],
        )?;
        conn.execute(
            "DELETE FROM fts_knowledge WHERE document_id = ?1",
            params![id],
        )?;
        let removed = conn.execute("DELETE FROM knowledge_documents WHERE id = ?1", params![id])?;
        Ok(removed > 0)
    })();
    match outcome {
        Ok(removed) => {
            conn.execute_batch("COMMIT")?;
            Ok(removed)
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

/// `searchKnowledge`: FTS match with the optional customer-safe filter.
/// The query is sanitized to quoted prefix terms (max 8).
#[must_use]
pub fn search_knowledge(conn: &Connection, query: &str, visibility: Option<&str>) -> Vec<Value> {
    let fts_query: String = query
        .replace(['"', '*', '(', ')'], " ")
        .split_whitespace()
        .take(8)
        .map(|t| format!("\"{t}\"*"))
        .collect::<Vec<_>>()
        .join(" ");
    if fts_query.is_empty() {
        return Vec::new();
    }
    let sql = match visibility {
        Some(_) => {
            "SELECT f.chunk_id, f.document_id, f.title,
                    snippet(fts_knowledge, 1, '[', ']', '…', 12) AS snippet, f.visibility
             FROM fts_knowledge f WHERE fts_knowledge MATCH ?1 AND f.visibility = ?2
             ORDER BY rank LIMIT 25"
        }
        None => {
            "SELECT f.chunk_id, f.document_id, f.title,
                    snippet(fts_knowledge, 1, '[', ']', '…', 12) AS snippet, f.visibility
             FROM fts_knowledge f WHERE fts_knowledge MATCH ?1
             ORDER BY rank LIMIT 25"
        }
    };
    conn.prepare(sql)
        .and_then(|mut stmt| {
            let rows = if visibility.is_some() {
                stmt.query_map(params![fts_query, visibility], fts_row)?
            } else {
                stmt.query_map(params![fts_query], fts_row)?
            };
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap_or_default()
}

fn fts_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    Ok(json!({
        "chunk_id": r.get::<_, i64>(0)?,
        "document_id": r.get::<_, i64>(1)?,
        "title": r.get::<_, String>(2)?,
        "snippet": r.get::<_, String>(3)?,
        "visibility": r.get::<_, String>(4)?,
    }))
}

// ─── freshness stamps (v2.0.0 plan Phase 25 — the human-only actions) ────

/// `markReviewed`: stamp `last_reviewed_at`. Returns false when the
/// document does not exist (route 404s).
pub fn mark_reviewed(conn: &Connection, id: i64) -> Result<bool> {
    let rows = conn.execute(
        "UPDATE knowledge_documents SET last_reviewed_at = datetime('now') WHERE id = ?1",
        params![id],
    )?;
    Ok(rows > 0)
}

/// `markVerified`: stamp `last_verified_at`. Returns false when the
/// document does not exist (route 404s).
pub fn mark_verified(conn: &Connection, id: i64) -> Result<bool> {
    let rows = conn.execute(
        "UPDATE knowledge_documents SET last_verified_at = datetime('now') WHERE id = ?1",
        params![id],
    )?;
    Ok(rows > 0)
}

/// The freshness report rows over the REAL store (reduced v2.0.0 shape:
/// the deterministic date flags; the usage/conflict/ticket-correlation
/// flags are a documented follow-up). Keeps the legacy top-level
/// `{stale, fresh, documents}` envelope the port UI already parses, with
/// the per-document rows extended honestly.
#[must_use]
pub fn freshness_report(conn: &Connection) -> Value {
    let rows: Vec<Value> = conn
        .prepare(
            "SELECT d.id, d.title, d.visibility, d.version, d.updated_at,
                    d.last_reviewed_at, d.last_verified_at, s.name AS source_name,
                    (SELECT COUNT(*) FROM knowledge_chunks c WHERE c.document_id = d.id) AS chunk_count
             FROM knowledge_documents d JOIN knowledge_sources s ON s.id = d.source_id
             ORDER BY d.updated_at DESC LIMIT 200",
        )
        .and_then(|mut stmt| {
            let rows = stmt.query_map([], |r| {
                let updated_at: Option<String> = r.get(4)?;
                let reviewed_at: Option<String> = r.get(5)?;
                let days_since_update = updated_at
                    .as_deref()
                    .and_then(days_since);
                let days_since_review = reviewed_at
                    .as_deref()
                    .and_then(days_since);
                let stale = days_since_update.is_some_and(|d| d > 180);
                let unreviewed_long = match days_since_review {
                    None => days_since_update.is_some_and(|d| d > 90),
                    Some(d) => d > 90,
                };
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "document_id": r.get::<_, i64>(0)?,
                    "title": r.get::<_, String>(1)?,
                    "visibility": r.get::<_, String>(2)?,
                    "version": r.get::<_, Option<i64>>(3)?.unwrap_or(1),
                    "updated_at": updated_at,
                    "last_reviewed_at": reviewed_at,
                    "last_verified_at": r.get::<_, Option<String>>(6)?,
                    "source_name": r.get::<_, Option<String>>(7)?,
                    "chunk_count": r.get::<_, i64>(8)?,
                    "days_since_update": days_since_update,
                    "days_since_review": days_since_review,
                    "freshness_status": if stale { "stale" } else { "fresh" },
                    "flags": {
                        "stale": stale,
                        "unreviewed_long": unreviewed_long,
                    },
                }))
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap_or_default();
    let stale = rows
        .iter()
        .filter(|r| r.get("freshness_status").and_then(|v| v.as_str()) == Some("stale"))
        .count() as i64;
    let fresh = rows.len() as i64 - stale;
    json!({ "stale": stale, "fresh": fresh, "documents": rows })
}

/// Whole days between an SQLite `datetime('now')`-shaped stamp and now.
fn days_since(stamp: &str) -> Option<i64> {
    let naive =
        chrono::NaiveDateTime::parse_from_str(stamp.trim_end_matches('Z'), "%Y-%m-%d %H:%M:%S")
            .ok()
            .or_else(|| {
                chrono::NaiveDateTime::parse_from_str(
                    stamp.trim_end_matches('Z'),
                    "%Y-%m-%dT%H:%M:%S%.f",
                )
                .ok()
            })?;
    let delta = chrono::Utc::now().naive_utc() - naive;
    Some(delta.num_days())
}

// ─── related reads for the DocReader ─────────────────────────────────────

/// The honest "related tickets" estimate (reference v2.2.1 audit fix):
/// DISTINCT conversations whose AI analysis actually cited this document
/// — never a self-matching FTS document count.
#[must_use]
pub fn related_ticket_estimate(conn: &Connection, id: i64) -> i64 {
    conn.query_row(
        "SELECT COUNT(DISTINCT r.conversation_id) AS n
           FROM ai_sources s JOIN ai_runs r ON r.id = s.run_id
          WHERE s.source_type = 'knowledge_document' AND s.source_id = ?1
            AND r.conversation_id IS NOT NULL",
        params![id],
        |r| r.get(0),
    )
    .unwrap_or(0)
}

/// `searchKnownIssues`: FTS over the known-issues index, falling back to a
/// name LIKE match when the FTS table has no rows yet.
#[must_use]
pub fn search_known_issues_by_title(conn: &Connection, title: &str) -> Vec<Value> {
    let fts_query: String = title
        .replace(['"', '*', '(', ')'], " ")
        .split_whitespace()
        .take(8)
        .map(|t| format!("\"{t}\"*"))
        .collect::<Vec<_>>()
        .join(" ");
    if !fts_query.is_empty() {
        let rows: Vec<Value> = conn
            .prepare(
                "SELECT ki.id, COALESCE(ki.title, ki.name) AS title,
                        snippet(fts_known_issues, 0, '[', ']', '…', 12) AS snippet
                 FROM fts_known_issues f JOIN known_issues ki ON ki.id = f.known_issue_id
                 WHERE fts_known_issues MATCH ?1 ORDER BY rank LIMIT 5",
            )
            .and_then(|mut stmt| {
                let rows = stmt.query_map(params![fts_query], |r| {
                    Ok(json!({
                        "id": r.get::<_, i64>(0)?,
                        "title": r.get::<_, Option<String>>(1)?,
                        "snippet": r.get::<_, Option<String>>(2)?,
                    }))
                })?;
                rows.collect::<rusqlite::Result<Vec<_>>>()
            })
            .unwrap_or_default();
        if !rows.is_empty() {
            return rows;
        }
    }
    // LIKE fallback: any shared title term.
    let like = format!(
        "%{}%",
        title
            .split_whitespace()
            .next()
            .unwrap_or("**** no such issue ****")
    );
    conn.prepare(
        "SELECT id, COALESCE(title, name) AS title, NULL AS snippet
         FROM known_issues WHERE COALESCE(title, name) LIKE ?1 LIMIT 5",
    )
    .and_then(|mut stmt| {
        let rows = stmt.query_map(params![like], |r| {
            Ok(json!({
                "id": r.get::<_, i64>(0)?,
                "title": r.get::<_, Option<String>>(1)?,
                "snippet": r.get::<_, Option<String>>(2)?,
            }))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
    })
    .unwrap_or_default()
}

// ─── ingestion (reference KnowledgeIngestor) ─────────────────────────────

/// One imported document result (`{documentId, title, changed}`).
#[must_use]
pub fn import_result(document_id: i64, title: &str, changed: bool) -> Value {
    json!({ "documentId": document_id, "title": title, "changed": changed })
}

/// `importManual`: upsert every document under the (created) named source.
pub fn import_manual(
    conn: &Connection,
    source_name: &str,
    documents: &[Value],
    visibility: &str,
) -> Result<Vec<Value>> {
    let source_id = get_or_create_source(conn, source_name, "manual", visibility)?;
    let mut out = Vec::with_capacity(documents.len());
    for doc in documents {
        let title = doc
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let content = doc
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let format = doc
            .get("format")
            .and_then(|v| v.as_str())
            .unwrap_or("markdown");
        let (id, changed) = upsert_document(conn, source_id, title, content, visibility, format)?;
        out.push(import_result(id, title, changed));
    }
    Ok(out)
}

/// The importable-files listing: `{dir, files}` for the knowledge-import
/// folder under `data_dir` (created-empty is fine; stat races tolerated).
#[must_use]
pub fn importable_files(data_dir: &Path) -> Value {
    let dir = data_dir.join(IMPORT_DIR_NAME);
    let mut files: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !is_importable_filename(&name) {
                continue;
            }
            // Tolerate stat races (file vanishing mid-listing).
            if entry.metadata().map(|m| m.is_file()).unwrap_or(false) {
                files.push(name);
            }
        }
    }
    files.sort();
    json!({
        "dir": dir.to_string_lossy(),
        "files": files,
    })
}

/// The importable extension set (MD, TXT, CSV, JSON, HTML, PDF, DOCX).
fn is_importable_filename(name: &str) -> bool {
    let lower = name.to_lowercase();
    [
        ".md",
        ".markdown",
        ".txt",
        ".csv",
        ".json",
        ".html",
        ".htm",
        ".pdf",
        ".docx",
    ]
    .iter()
    .any(|ext| lower.ends_with(ext))
}

/// Lexically resolve `requested` against `base` (the reference
/// `path.resolve`: `.`/`..` normalization without touching the filesystem,
/// so non-existent escapes still classify as escapes).
fn lexically_resolve(base: &Path, requested: &Path) -> PathBuf {
    let joined = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        base.join(requested)
    };
    let mut out = PathBuf::new();
    for component in joined.components() {
        match component {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// `importFile`: read + parse + upsert. `path` must resolve INSIDE the
/// knowledge-import folder under `data_dir` (the safety allowlist — a
/// resolved sibling like `knowledge-import-x/` must not match). The
/// containment check is LEXICAL first (the reference `path.resolve` does
/// not require existence, so `../secret` gets the safety message even for
/// missing files), then canonical (symlink escapes).
pub fn import_file(
    conn: &Connection,
    data_dir: &Path,
    path: &str,
    source_name: Option<&str>,
    visibility: &str,
) -> Result<Vec<Value>> {
    let requested = PathBuf::from(path);
    let import_root = data_dir.join(IMPORT_DIR_NAME);
    let lexical = lexically_resolve(&import_root, &requested);
    let safety_message = "For safety, file imports must live inside the \"knowledge-import\" folder of the data directory. Create it and copy your documents there.";
    if !lexical.starts_with(&import_root) || lexical == import_root {
        return Err(Error::Validation(safety_message.to_string()));
    }
    let canonical = std::fs::canonicalize(&lexical)
        .map_err(|_| Error::Validation(format!("File not found: {path}")))?;
    let root_canonical =
        std::fs::canonicalize(&import_root).unwrap_or_else(|_| import_root.clone());
    if !canonical.starts_with(&root_canonical) || canonical == root_canonical {
        return Err(Error::Validation(safety_message.to_string()));
    }

    let metadata = std::fs::metadata(&canonical)
        .map_err(|_| Error::Validation(format!("File not found: {path}")))?;
    if metadata.is_dir() {
        // Import every importable file in the directory (cap 200).
        let mut files: Vec<PathBuf> = std::fs::read_dir(&canonical)
            .map_err(|e| Error::Validation(format!("Import failed: {e}")))?
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .map(|n| is_importable_filename(&n.to_string_lossy()))
                    .unwrap_or(false)
            })
            .collect();
        files.sort();
        files.truncate(200);
        let mut out = Vec::new();
        for file in files {
            let name = file
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "imported".to_string());
            out.extend(import_one_file(
                conn,
                &file,
                source_name.unwrap_or(&name),
                visibility,
            )?);
        }
        return Ok(out);
    }
    let fallback_name = canonical
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "imported".to_string());
    import_one_file(
        conn,
        &canonical,
        source_name.unwrap_or(&fallback_name),
        visibility,
    )
}

/// Parse + upsert one file (MD/TXT/CSV/JSON/HTML native; PDF/DOCX error
/// honestly — the reference degrades the same way without its parsers).
fn import_one_file(
    conn: &Connection,
    abs: &Path,
    source_name: &str,
    visibility: &str,
) -> Result<Vec<Value>> {
    let ext = abs
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let base = abs
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "imported".to_string());
    let raw = std::fs::read(abs).map_err(|e| Error::Validation(format!("Import failed: {e}")))?;
    let text = String::from_utf8_lossy(&raw).to_string();

    let docs: Vec<(String, String, String)> = match ext.as_str() {
        "md" | "markdown" => vec![(base, text, "markdown".to_string())],
        "txt" => vec![(base, text, "txt".to_string())],
        "csv" => split_csv_sections(&base, &text),
        "json" => parse_json_documents(&base, &text)?,
        "html" | "htm" => {
            let title = extract_html_title(&text).unwrap_or_else(|| base.clone());
            let body = html_to_text_keep_structure(&text);
            vec![(title, body, "html".to_string())]
        }
        "pdf" => {
            return Err(Error::Validation(
                "PDF parsing is unavailable or failed for this file. Supported best with text-based PDFs.".to_string(),
            ));
        }
        "docx" => {
            return Err(Error::Validation(
                "DOCX parsing is unavailable or failed for this file.".to_string(),
            ));
        }
        other => {
            return Err(Error::Validation(format!(
                "Unsupported file type: .{other}"
            )));
        }
    };

    let source_id = get_or_create_source(conn, source_name, "local_file", visibility)?;
    let mut out = Vec::with_capacity(docs.len());
    for (title, content, format) in docs {
        let (id, changed) =
            upsert_document(conn, source_id, &title, &content, visibility, &format)?;
        out.push(import_result(id, &title, changed));
    }
    Ok(out)
}

/// One document per CSV row: the title column (title/name/question/topic)
/// when present, else the first cell; content is the labelled row.
fn split_csv_sections(base: &str, text: &str) -> Vec<(String, String, String)> {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.is_empty() {
        return vec![(base.to_string(), String::new(), "txt".to_string())];
    }
    let header = parse_csv_line(lines[0]);
    let title_idx = header.iter().position(|h| {
        matches!(
            h.trim().to_lowercase().as_str(),
            "title" | "name" | "question" | "topic"
        )
    });
    let mut docs: Vec<(String, String, String)> = Vec::new();
    for line in lines.iter().take(501).skip(1) {
        let cells = parse_csv_line(line);
        if cells.iter().all(|c| c.trim().is_empty()) {
            continue;
        }
        let title = match title_idx {
            Some(idx) => cells.get(idx).map(String::as_str).unwrap_or(""),
            None => cells.first().map(String::as_str).unwrap_or(""),
        };
        let title: String = if title.trim().is_empty() {
            format!("{base} entry")
        } else {
            title.trim().chars().take(200).collect()
        };
        let content = header
            .iter()
            .enumerate()
            .map(|(i, h)| format!("{h}: {}", cells.get(i).map(String::as_str).unwrap_or("")))
            .collect::<Vec<_>>()
            .join("\n");
        docs.push((title, content, "txt".to_string()));
    }
    if docs.is_empty() {
        docs.push((base.to_string(), text.to_string(), "txt".to_string()));
    }
    docs
}

/// A quote-aware CSV line parser (handles "" escapes).
fn parse_csv_line(line: &str) -> Vec<String> {
    let mut cells: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        if in_quotes {
            if ch == '"' {
                if chars.get(i + 1) == Some(&'"') {
                    cur.push('"');
                    i += 1;
                } else {
                    in_quotes = false;
                }
            } else {
                cur.push(ch);
            }
        } else if ch == '"' {
            in_quotes = true;
        } else if ch == ',' {
            cells.push(cur.clone());
            cur.clear();
        } else {
            cur.push(ch);
        }
        i += 1;
    }
    cells.push(cur);
    cells
}

/// JSON import: an array (flattened records, cap 500), `{documents: [...]}`,
/// or a single object (flattened).
fn parse_json_documents(base: &str, text: &str) -> Result<Vec<(String, String, String)>> {
    let data: Value = serde_json::from_str(text)
        .map_err(|e| Error::Validation(format!("Import failed: invalid JSON: {e}")))?;
    let mut docs: Vec<(String, String, String)> = Vec::new();
    match &data {
        Value::Array(items) => {
            for (i, item) in items.iter().take(500).enumerate() {
                let title = item
                    .get("title")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .or_else(|| {
                        item.get("name")
                            .and_then(|v| v.as_str())
                            .filter(|s| !s.is_empty())
                    })
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("{base} [{}]", i + 1));
                docs.push((title, flatten_record(item), "txt".to_string()));
            }
        }
        Value::Object(obj) => {
            if let Some(Value::Array(items)) = obj.get("documents") {
                for item in items.iter().take(500) {
                    let title = item
                        .get("title")
                        .or_else(|| item.get("name"))
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                        .unwrap_or_else(|| base.to_string());
                    let content = item
                        .get("content")
                        .or_else(|| item.get("text"))
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                        .unwrap_or_else(|| flatten_record(item));
                    let format = item
                        .get("format")
                        .and_then(|v| v.as_str())
                        .unwrap_or("txt")
                        .to_string();
                    docs.push((title, content, format));
                }
            } else {
                let title = obj
                    .get("title")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
                    .unwrap_or_else(|| base.to_string());
                docs.push((title, flatten_record(&data), "txt".to_string()));
            }
        }
        _ => {
            return Err(Error::Validation(
                "Import failed: JSON must be an array or object.".to_string(),
            ));
        }
    }
    Ok(docs)
}

/// `k: v` lines for a record (nested values JSON-encoded).
fn flatten_record(obj: &Value) -> String {
    let mut lines: Vec<String> = Vec::new();
    if let Some(map) = obj.as_object() {
        for (k, v) in map {
            match v {
                Value::Null => {}
                Value::String(s) => lines.push(format!("{k}: {s}")),
                Value::Number(n) => lines.push(format!("{k}: {n}")),
                Value::Bool(b) => lines.push(format!("{k}: {b}")),
                other => lines.push(format!("{k}: {other}")),
            }
        }
    }
    lines.join("\n")
}

/// Strip tags but keep the block structure (reference htmlToTextKeepStructure).
#[must_use]
pub fn html_to_text_keep_structure(html: &str) -> String {
    let mut out = html.to_string();
    // Remove style/script blocks (case-insensitive, multiline).
    out = strip_tag_block(&out, "style");
    out = strip_tag_block(&out, "script");
    // Block ends become newlines.
    for tag in ["p", "div", "li", "h1", "h2", "h3", "h4", "h5", "h6", "tr"] {
        out = replace_ci(&out, &format!("</{tag}>"), "\n");
    }
    out = replace_ci(&out, "<br>", "\n");
    out = replace_ci(&out, "<br/>", "\n");
    out = replace_ci(&out, "<br />", "\n");
    // Remaining tags become spaces.
    let mut text = String::with_capacity(out.len());
    let mut inside = false;
    for ch in out.chars() {
        match ch {
            '<' => {
                inside = true;
                text.push(' ');
            }
            '>' => inside = false,
            _ if !inside => text.push(ch),
            _ => {}
        }
    }
    for (entity, decoded) in [
        ("&nbsp;", " "),
        ("&amp;", "&"),
        ("&lt;", "<"),
        ("&gt;", ">"),
        ("&quot;", "\""),
        ("&#39;", "'"),
    ] {
        text = text.replace(entity, decoded);
    }
    // Collapse whitespace runs.
    let mut collapsed = String::with_capacity(text.len());
    let mut last_space = false;
    for ch in text.chars() {
        let is_ws = ch == ' ' || ch == '\t';
        if is_ws {
            if !last_space {
                collapsed.push(' ');
            }
            last_space = true;
        } else {
            collapsed.push(ch);
            last_space = false;
        }
    }
    // Collapse 3+ newlines.
    while collapsed.contains("\n\n\n") {
        collapsed = collapsed.replace("\n\n\n", "\n\n");
    }
    collapsed.trim().to_string()
}

/// Remove `<tag ...>...</tag>` blocks case-insensitively.
fn strip_tag_block(input: &str, tag: &str) -> String {
    let lower = input.to_lowercase();
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut out = String::with_capacity(input.len());
    let mut pos = 0usize;
    while let Some(start) = lower[pos..].find(&open) {
        let abs_start = pos + start;
        out.push_str(&input[pos..abs_start]);
        match lower[abs_start..].find(&close) {
            Some(rel_end) => {
                pos = abs_start + rel_end + close.len();
            }
            None => {
                // Unterminated block: drop the rest.
                return out;
            }
        }
    }
    out.push_str(&input[pos..]);
    out
}

/// Case-insensitive replace (the `<br>` variants).
fn replace_ci(input: &str, from: &str, to: &str) -> String {
    let lower = input.to_lowercase();
    let from_lower = from.to_lowercase();
    let mut out = String::with_capacity(input.len());
    let mut pos = 0usize;
    while let Some(idx) = lower[pos..].find(&from_lower) {
        let abs = pos + idx;
        out.push_str(&input[pos..abs]);
        out.push_str(to);
        pos = abs + from.len();
    }
    out.push_str(&input[pos..]);
    out
}

/// `<title>...</title>` extraction (case-insensitive).
fn extract_html_title(html: &str) -> Option<String> {
    let lower = html.to_lowercase();
    let start = lower.find("<title>")? + "<title>".len();
    let end = lower[start..].find("</title>")? + start;
    let title = html[start..end].trim();
    if title.is_empty() {
        None
    } else {
        Some(title.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_db() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::bootstrap::apply_all(&mut conn).unwrap();
        conn
    }

    #[test]
    fn chunk_text_respects_max_len_and_boundaries() {
        // Short text: one chunk.
        assert_eq!(chunk_text("hello world", 1200, 150), vec!["hello world"]);
        // Long text: multiple chunks, each within max_len + boundary slack,
        // consecutive chunks overlap.
        let long = "Sentence one goes here. ".repeat(200);
        let chunks = chunk_text(&long, 1200, 150);
        assert!(chunks.len() > 1);
        for c in &chunks {
            assert!(c.len() <= 1200 + 8, "chunk too long: {}", c.len());
        }
        // Empty text: no chunks.
        assert!(chunk_text("   ", 1200, 150).is_empty());
    }

    #[test]
    fn checksum_is_stable_and_content_sensitive() {
        assert_eq!(checksum("abc"), checksum("abc"));
        assert_ne!(checksum("abc"), checksum("abd"));
        assert_eq!(checksum("abc").len(), 64);
    }

    #[test]
    fn upsert_is_checksum_gated_and_rebuilds_chunks_fts() {
        let conn = fresh_db();
        let source =
            get_or_create_source(&conn, "Manual import", "manual", "internal_only").unwrap();
        let (id, changed) = upsert_document(
            &conn,
            source,
            "Runbook",
            "Do the thing.",
            "internal_only",
            "markdown",
        )
        .unwrap();
        assert!(changed);
        // Identical content: no-op.
        let (id2, changed2) = upsert_document(
            &conn,
            source,
            "Runbook",
            "Do the thing.",
            "internal_only",
            "markdown",
        )
        .unwrap();
        assert_eq!(id, id2);
        assert!(!changed2);
        let version: i64 = conn
            .query_row(
                "SELECT version FROM knowledge_documents WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(version, 1);

        // Changed content: version bump + chunk/FTS rebuild.
        let (_id3, changed3) = upsert_document(
            &conn,
            source,
            "Runbook",
            "Do the thing. Then do more things. ",
            "internal_only",
            "markdown",
        )
        .unwrap();
        assert!(changed3);
        let version: i64 = conn
            .query_row(
                "SELECT version FROM knowledge_documents WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(version, 2);
        let chunks: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM knowledge_chunks WHERE document_id = ?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap();
        let fts: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM fts_knowledge WHERE document_id = ?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(chunks, 1);
        assert_eq!(fts, 1);
    }

    #[test]
    fn list_and_get_documents_carry_previews_and_counts() {
        let conn = fresh_db();
        let source = get_or_create_source(&conn, "Docs", "manual", "customer_safe").unwrap();
        upsert_document(
            &conn,
            source,
            "Timezones",
            "Schedules follow the workspace timezone. Re-save after changes.",
            "customer_safe",
            "markdown",
        )
        .unwrap();

        let docs = list_documents(&conn, None);
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0]["title"], "Timezones");
        assert_eq!(docs[0]["visibility"], "customer_safe");
        assert_eq!(docs[0]["chunk_count"], 1);
        assert!(docs[0]["content_preview"]
            .as_str()
            .unwrap()
            .starts_with("Schedules follow"));

        // Source filter.
        let docs = list_documents(&conn, Some(source));
        assert_eq!(docs.len(), 1);
        let docs = list_documents(&conn, Some(source + 999));
        assert!(docs.is_empty());

        let doc = get_document(&conn, docs_id(&conn)).unwrap();
        assert_eq!(doc["source_name"], "Docs");
        assert_eq!(doc["chunk_count"], 1);
        assert!(get_document(&conn, 9999).is_none());
    }

    fn docs_id(conn: &Connection) -> i64 {
        conn.query_row("SELECT id FROM knowledge_documents LIMIT 1", [], |r| {
            r.get(0)
        })
        .unwrap()
    }

    #[test]
    fn delete_removes_chunks_and_fts_atomically() {
        let conn = fresh_db();
        let source = get_or_create_source(&conn, "S", "manual", "internal_only").unwrap();
        let (id, _) = upsert_document(
            &conn,
            source,
            "Bye",
            "some content that will be deleted",
            "internal_only",
            "markdown",
        )
        .unwrap();
        assert!(delete_document(&conn, id).unwrap());
        for table in ["knowledge_documents", "knowledge_chunks", "fts_knowledge"] {
            let n: i64 = conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(n, 0, "{table} must be empty");
        }
        assert!(!delete_document(&conn, id).unwrap());
    }

    #[test]
    fn search_finds_documents_and_respects_visibility() {
        let conn = fresh_db();
        let source = get_or_create_source(&conn, "S", "manual", "internal_only").unwrap();
        upsert_document(
            &conn,
            source,
            "Widgets",
            "Widgets are configured in the settings panel.",
            "customer_safe",
            "markdown",
        )
        .unwrap();
        upsert_document(
            &conn,
            source,
            "Runbook",
            "Internal widgets escalation path.",
            "internal_only",
            "markdown",
        )
        .unwrap();

        let all = search_knowledge(&conn, "widgets", None);
        assert_eq!(all.len(), 2);
        let safe = search_knowledge(&conn, "widgets", Some("customer_safe"));
        assert_eq!(safe.len(), 1);
        assert_eq!(safe[0]["title"], "Widgets");
        assert_eq!(safe[0]["visibility"], "customer_safe");
        // Garbage queries return empty, never an error.
        assert!(search_knowledge(&conn, "\"(", None).is_empty());
    }

    #[test]
    fn review_verify_stamp_and_404() {
        let conn = fresh_db();
        let source = get_or_create_source(&conn, "S", "manual", "internal_only").unwrap();
        let (id, _) =
            upsert_document(&conn, source, "Doc", "content", "internal_only", "markdown").unwrap();
        assert!(!mark_reviewed(&conn, 999).unwrap());
        assert!(!mark_verified(&conn, 999).unwrap());
        assert!(mark_reviewed(&conn, id).unwrap());
        assert!(mark_verified(&conn, id).unwrap());
        let (reviewed, verified): (Option<String>, Option<String>) = conn
            .query_row(
                "SELECT last_reviewed_at, last_verified_at FROM knowledge_documents WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(reviewed.is_some());
        assert!(verified.is_some());
    }

    #[test]
    fn freshness_report_covers_the_real_store() {
        let conn = fresh_db();
        let source = get_or_create_source(&conn, "S", "manual", "internal_only").unwrap();
        upsert_document(
            &conn,
            source,
            "Fresh doc",
            "recent content",
            "internal_only",
            "markdown",
        )
        .unwrap();
        let report = freshness_report(&conn);
        assert_eq!(report["fresh"], 1);
        assert_eq!(report["stale"], 0);
        let row = &report["documents"][0];
        assert_eq!(row["title"], "Fresh doc");
        assert_eq!(row["source_name"], "S");
        assert_eq!(row["flags"]["stale"], false);
        assert_eq!(row["flags"]["unreviewed_long"], false);

        // A 200-day-old update is stale; 100-day-unreviewed needs review.
        conn.execute(
            "UPDATE knowledge_documents SET updated_at = datetime('now', '-200 days')",
            [],
        )
        .unwrap();
        let report = freshness_report(&conn);
        assert_eq!(report["stale"], 1);
        assert_eq!(report["documents"][0]["flags"]["stale"], true);
        assert_eq!(report["documents"][0]["flags"]["unreviewed_long"], true);
    }

    #[test]
    fn import_manual_upserts_under_the_named_source() {
        let conn = fresh_db();
        let docs = vec![json!({
            "title": "Pasted doc",
            "content": "pasted content",
            "format": "markdown"
        })];
        let out = import_manual(&conn, "Manual import", &docs, "internal_only").unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["title"], "Pasted doc");
        assert_eq!(out[0]["changed"], true);
        let sources = list_sources(&conn);
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0]["name"], "Manual import");
        assert_eq!(sources[0]["kind"], "manual");
        assert_eq!(sources[0]["document_count"], 1);
    }

    #[test]
    fn import_file_is_confined_to_the_import_folder() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::create_dir_all(root.join(IMPORT_DIR_NAME)).unwrap();
        std::fs::write(
            root.join(IMPORT_DIR_NAME).join("note.md"),
            "# Note\n\nBody here.",
        )
        .unwrap();

        // Relative name resolves inside the folder.
        let out = import_file(&conn_with_store(), &root, "note.md", None, "internal_only").unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["title"], "note");

        // Escape attempts are rejected (traversal + absolute outside).
        for escape in ["../secret.txt", "/etc/passwd"] {
            let err =
                import_file(&conn_with_store(), &root, escape, None, "internal_only").unwrap_err();
            assert!(
                err.to_string().contains("knowledge-import"),
                "escape {escape} must be rejected: {err}"
            );
        }
        // Windows-style separators are NOT separators on Unix: the name
        // stays inside the folder and fails as not-found (still a
        // rejection, never a read).
        assert!(import_file(
            &conn_with_store(),
            &root,
            "..\\secret.txt",
            None,
            "internal_only"
        )
        .is_err());
    }

    fn conn_with_store() -> Connection {
        fresh_db()
    }

    #[test]
    fn import_file_parses_each_format() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let import = root.join(IMPORT_DIR_NAME);
        std::fs::create_dir_all(&import).unwrap();
        std::fs::write(import.join("a.md"), "markdown body").unwrap();
        std::fs::write(import.join("b.txt"), "plain body").unwrap();
        std::fs::write(import.join("c.csv"), "title,answer\nWidgets,Yes\n").unwrap();
        std::fs::write(
            import.join("d.json"),
            r#"{"documents": [{"title": "J1", "content": "json body"}]}"#,
        )
        .unwrap();
        std::fs::write(
            import.join("e.html"),
            "<html><title>Page</title><body><p>Hello</p></body></html>",
        )
        .unwrap();

        let conn = fresh_db();
        for (file, expected_title, expected_format) in [
            ("a.md", "a", "markdown"),
            ("b.txt", "b", "txt"),
            ("d.json", "J1", "txt"),
            ("e.html", "Page", "html"),
        ] {
            let out = import_file(&conn, &root, file, None, "internal_only").unwrap();
            assert_eq!(out.len(), 1, "{file}");
            assert_eq!(out[0]["title"], expected_title, "{file}");
            let format: String = conn
                .query_row(
                    "SELECT format FROM knowledge_documents WHERE title = ?1",
                    params![expected_title],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(format, expected_format);
        }

        // CSV: one document per row, title from the header column.
        let out = import_file(&conn, &root, "c.csv", None, "internal_only").unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["title"], "Widgets");
        let content: String = conn
            .query_row(
                "SELECT content FROM knowledge_documents WHERE title = 'Widgets'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(content, "title: Widgets\nanswer: Yes");

        // PDF/DOCX degrade honestly.
        std::fs::write(import.join("f.pdf"), "%PDF-1.4 fake").unwrap();
        let err = import_file(&conn, &root, "f.pdf", None, "internal_only").unwrap_err();
        assert!(err.to_string().contains("PDF"));
    }

    #[test]
    fn importable_lists_only_importable_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let import = root.join(IMPORT_DIR_NAME);
        std::fs::create_dir_all(&import).unwrap();
        std::fs::write(import.join("ok.md"), "x").unwrap();
        std::fs::write(import.join("ok.PDF"), "x").unwrap();
        std::fs::write(import.join("skip.exe"), "x").unwrap();
        std::fs::create_dir_all(import.join("nested")).unwrap();

        let listing = importable_files(&root);
        let files = listing["files"].as_array().unwrap();
        assert_eq!(files.len(), 2);
        assert!(files.contains(&json!("ok.PDF")));
        // Missing folder: empty listing, not an error.
        let listing = importable_files(&root.join("elsewhere"));
        assert_eq!(listing["files"].as_array().map(Vec::len), Some(0));
    }

    #[test]
    fn related_ticket_estimate_counts_distinct_citing_conversations() {
        let conn = fresh_db();
        let source = get_or_create_source(&conn, "S", "manual", "customer_safe").unwrap();
        let (id, _) = upsert_document(
            &conn,
            source,
            "Cited",
            "cited content",
            "customer_safe",
            "markdown",
        )
        .unwrap();
        assert_eq!(related_ticket_estimate(&conn, id), 0);
        assert_eq!(related_ticket_estimate(&conn, 999), 0);
    }

    #[test]
    fn html_to_text_keeps_structure() {
        let html = r#"<html><head><style>body{color:red}</style><title>T</title></head>
<body><h1>Heading</h1><p>Para &amp; more</p><br/>after break<script>evil()</script></body></html>"#;
        let text = html_to_text_keep_structure(html);
        assert!(!text.contains("color:red"), "style removed: {text}");
        assert!(!text.contains("evil()"), "script removed: {text}");
        assert!(text.contains("Heading"));
        assert!(text.contains("Para & more"));
        assert!(text.contains("after break"));
        assert_eq!(extract_html_title(html), Some("T".to_string()));
    }

    #[test]
    fn csv_parser_handles_quotes_and_escapes() {
        let cells = parse_csv_line(r#"a,"b,c","say ""hi""",d"#);
        assert_eq!(cells, vec!["a", "b,c", "say \"hi\"", "d"]);
    }
}
