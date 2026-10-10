//! Hybrid search — dense + sparse + filters; merge + rank (M5-T08).
//!
//! Per spec M5: "hybrid search."
//! Per the reference notes: "v1.1.x: Local AI (LM Studio); embeddings;
//! hybrid search."
//!
//! ## Design
//!
//! Hybrid search combines:
//! - **Dense search** (semantic): the VectorStore's `search_dense` method
//!   (M5-T01), which uses cosine similarity on embedding vectors.
//! - **Sparse search** (lexical): the FTS5 index from M3-T06
//!   (`search::universal_search`), which matches text keywords.
//!
//! The results are merged + re-ranked using **Reciprocal Rank Fusion (RRF)**,
//! a standard technique that doesn't require score calibration:
//!   `score(doc) = sum(1 / (k + rank_i(doc)))` for each result list `i`
//! where `k` is a constant (default 60, per the original RRF paper).
//!
//! Per spec A12: "Keep pure logic separate from I/O." The RRF merge + rank
//! logic is a pure function — testable without a VectorStore or DB.

use rusqlite::Connection;

use crate::error::Result;
use crate::search;
use crate::vectorstore::{Filter, ScoredPoint, VectorStore};

/// The RRF constant `k`. A higher value gives more weight to lower-ranked
/// results. The standard value from the original paper is 60.
pub const RRF_K: u32 = 60;

/// A hybrid search result — combines the dense + sparse scores into a single
/// fused rank.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct HybridResult {
    /// The point/conversation id (shared between dense + sparse results).
    pub id: String,
    /// The fused RRF score (higher = more relevant).
    pub rrf_score: f64,
    /// The dense search score (if the point appeared in dense results).
    pub dense_score: Option<f32>,
    /// The sparse search score (if the point appeared in sparse results).
    pub sparse_score: Option<f32>,
    /// The payload (from the VectorStore point, if available).
    pub payload: serde_json::Value,
}

/// Fuse two ranked result lists using Reciprocal Rank Fusion (RRF).
///
/// `dense_results` and `sparse_results` are both sorted by score descending
/// (most relevant first). The RRF score for each unique id is:
///   `sum(1 / (k + rank))` for each list the id appears in,
/// where `rank` is 1-indexed (the first result has rank 1).
///
/// Returns the fused results sorted by RRF score descending, truncated to
/// `top_k`.
///
/// Pure function — testable without a VectorStore or DB.
#[must_use]
pub fn fuse_results(
    dense_results: &[ScoredPoint],
    sparse_results: &[ScoredPoint],
    top_k: usize,
) -> Vec<HybridResult> {
    fuse_results_with_k(dense_results, sparse_results, top_k, RRF_K)
}

/// Fuse two ranked result lists with a custom `k` constant. Used by tests
/// to verify the RRF formula with specific `k` values.
#[must_use]
pub fn fuse_results_with_k(
    dense_results: &[ScoredPoint],
    sparse_results: &[ScoredPoint],
    top_k: usize,
    k: u32,
) -> Vec<HybridResult> {
    let mut map: std::collections::HashMap<String, HybridResult> = std::collections::HashMap::new();

    // Dense results (rank starts at 1).
    for (rank, point) in dense_results.iter().enumerate() {
        let rrf = 1.0 / (f64::from(k) + (rank + 1) as f64);
        let entry = map.entry(point.id.clone()).or_insert_with(|| HybridResult {
            id: point.id.clone(),
            rrf_score: 0.0,
            dense_score: None,
            sparse_score: None,
            payload: point.payload.clone(),
        });
        entry.rrf_score += rrf;
        entry.dense_score = Some(point.score);
    }

    // Sparse results.
    for (rank, point) in sparse_results.iter().enumerate() {
        let rrf = 1.0 / (f64::from(k) + (rank + 1) as f64);
        let entry = map.entry(point.id.clone()).or_insert_with(|| HybridResult {
            id: point.id.clone(),
            rrf_score: 0.0,
            dense_score: None,
            sparse_score: None,
            payload: point.payload.clone(),
        });
        entry.rrf_score += rrf;
        entry.sparse_score = Some(point.score);
    }

    let mut results: Vec<HybridResult> = map.into_values().collect();
    // Sort by RRF score descending (most relevant first).
    results.sort_by(|a, b| {
        b.rrf_score
            .partial_cmp(&a.rrf_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    results.truncate(top_k);
    results
}

// ─── Reference docsSemantic.ts port (v1.4/v1.5 hybrid layer) ───────────────
//
// The reference `src/server/search/docsSemantic.ts` is the source of truth:
// - `cosineSimilarity`: cosine over stored Float32 embeddings, 0 on length
//   mismatch / empty / zero-norm inputs.
// - `mergeDocHits`: Reciprocal Rank Fusion of the FTS + semantic result
//   lists. Both retrievers contribute RANK-based scores only
//   (`1 / (RRF_K + rank + 1)` where `rank` is the 0-based ARRAY POSITION —
//   the reference iterates both lists with `forEach((h, i) => ...)`, so the
//   fused score deliberately ignores the raw cosine/FTS scores: they live on
//   incommensurable scales). Provenance is preserved per hit in `why`
//   ('fts' / 'semantic' — insertion-ordered like the reference's Set).
// - The fused score is rounded to 4 decimals (`Math.round(s*10000)/10000`),
//   ties break by ascending article id, and the list is truncated to
//   `limit`.

/// The RRF constant `k` for the docs-style `merge_doc_hits` fusion
/// (reference `docsSemantic.ts` `RRF_K = 60`).
pub const DOC_RRF_K: u64 = 60;

/// An FTS hit entering the RRF fusion (reference `FtsDocHit`).
///
/// `rank` is the 0-based FTS rank position. Note: like the reference, the
/// fusion uses the ARRAY POSITION of the hit as its rank — the field is kept
/// for shape parity with `FtsDocHit`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FtsDocHit {
    pub article_id: i64,
    /// FTS rank position, 0-based, ascending relevance.
    pub rank: usize,
}

/// A semantic hit entering the RRF fusion (reference `SemanticDocHit`).
///
/// `score` is the cosine similarity; the fusion uses the ARRAY POSITION of
/// the hit (the semantic list is expected pre-sorted by similarity — its
/// rank IS the signal), the field is kept for shape parity.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SemanticDocHit {
    pub article_id: i64,
    /// cosine similarity, typically -1..1 (higher = closer).
    pub score: f32,
}

/// A fused hit (reference `MergedDocHit`): RRF score + retriever provenance.
#[derive(Debug, Clone, PartialEq)]
pub struct MergedDocHit {
    pub article_id: i64,
    /// RRF fused score; comparable across retrievers, NOT a cosine value.
    pub score: f64,
    /// Which retrievers found the hit, insertion-ordered ('fts' first when
    /// present, then 'semantic') — mirrors the reference's Set order.
    pub why: Vec<&'static str>,
}

/// Reciprocal Rank Fusion of the FTS + semantic result lists
/// (reference `mergeDocHits`). Pure function — testable without a DB.
///
/// Both inputs are rank-ordered lists (best first); each list contributes
/// `1 / (DOC_RRF_K + position + 1)` per hit. Scores are rounded to 4
/// decimals, sorted descending (ties by ascending article id), truncated to
/// `limit`.
#[must_use]
pub fn merge_doc_hits(
    fts: &[FtsDocHit],
    semantic: &[SemanticDocHit],
    limit: usize,
) -> Vec<MergedDocHit> {
    // Insertion-ordered map: (article_id, fused score, why). The lists are
    // small (<= 40 FTS + <= 24 semantic in the route), so linear lookup is
    // fine and keeps the why-order reference-exact.
    fn entry<'a>(
        entries: &'a mut Vec<(i64, f64, Vec<&'static str>)>,
        id: i64,
    ) -> &'a mut (i64, f64, Vec<&'static str>) {
        if let Some(pos) = entries.iter().position(|(aid, _, _)| *aid == id) {
            &mut entries[pos]
        } else {
            entries.push((id, 0.0, Vec::new()));
            entries.last_mut().expect("just pushed")
        }
    }
    let mut entries: Vec<(i64, f64, Vec<&'static str>)> = Vec::new();
    for (i, h) in fts.iter().enumerate() {
        let e = entry(&mut entries, h.article_id);
        e.1 += 1.0 / (DOC_RRF_K as f64 + i as f64 + 1.0);
        if !e.2.contains(&"fts") {
            e.2.push("fts");
        }
    }
    for (i, h) in semantic.iter().enumerate() {
        let e = entry(&mut entries, h.article_id);
        e.1 += 1.0 / (DOC_RRF_K as f64 + i as f64 + 1.0);
        if !e.2.contains(&"semantic") {
            e.2.push("semantic");
        }
    }
    let mut merged: Vec<MergedDocHit> = entries
        .into_iter()
        .map(|(article_id, s, why)| MergedDocHit {
            article_id,
            // reference: Math.round(v.s * 10000) / 10000
            score: (s * 10_000.0).round() / 10_000.0,
            why,
        })
        .collect();
    merged.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.article_id.cmp(&b.article_id))
    });
    merged.truncate(limit);
    merged
}

/// Cosine similarity of two equal-length vectors
/// (reference `docsSemantic.ts` `cosineSimilarity`): 0 when lengths mismatch,
/// when empty, or when either vector is all-zero. Accumulation runs in f64
/// to match the reference's JS-number math, narrowing to f32 on return.
#[must_use]
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0_f64;
    let mut na = 0.0_f64;
    let mut nb = 0.0_f64;
    for i in 0..a.len() {
        dot += f64::from(a[i]) * f64::from(b[i]);
        na += f64::from(a[i]) * f64::from(a[i]);
        nb += f64::from(b[i]) * f64::from(b[i]);
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    (dot / (na.sqrt() * nb.sqrt())) as f32
}

/// Run a hybrid search combining dense (VectorStore) + sparse (FTS5) search.
///
/// Per spec: "hybrid search." The dense search uses the VectorStore's
/// `search_dense` (cosine similarity); the sparse search uses the FTS5 index
/// from M3-T06 (`search::universal_search`). Results are fused via RRF.
///
/// # Errors
///
/// Returns `Error::Sqlite` if the FTS5 query fails, or the VectorStore's
/// error if the dense search fails.
pub fn hybrid_search(
    conn: &Connection,
    vectorstore: &dyn VectorStore,
    collection: &str,
    query_vector: &[f32],
    query_text: &str,
    filter: Option<&Filter>,
    top_k: usize,
) -> Result<Vec<HybridResult>> {
    // 1. Dense search (VectorStore).
    let dense_results = vectorstore.search_dense(collection, query_vector, filter, top_k)?;

    // 2. Sparse search (FTS5 from M3-T06).
    // The FTS5 search returns SearchResult structs with remote_id (i64).
    // We convert them to ScoredPoint for the RRF fusion.
    let fts_results = search::universal_search(conn, query_text)?;
    let sparse_results: Vec<ScoredPoint> = fts_results
        .into_iter()
        .enumerate()
        .map(|(rank, r)| ScoredPoint {
            id: r.remote_id.to_string(),
            // FTS5 doesn't return a numeric score; use a descending rank-based
            // score so higher-ranked results have higher scores.
            score: 1.0 / (1.0 + rank as f32),
            payload: serde_json::json!({
                "type": r.resource_type,
                "title": r.title,
                "snippet": r.snippet,
            }),
        })
        .take(top_k)
        .collect();

    // 3. Fuse + rank via RRF.
    Ok(fuse_results(&dense_results, &sparse_results, top_k))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vectorstore::{InMemoryVectorStore, Point};
    use serde_json::json;

    fn scored(id: &str, score: f32) -> ScoredPoint {
        ScoredPoint {
            id: id.into(),
            score,
            payload: json!({}),
        }
    }

    // ---- fuse_results: RRF formula -----------------------------------------

    #[test]
    fn fuse_results_empty_inputs_returns_empty() {
        let results = fuse_results(&[], &[], 10);
        assert!(results.is_empty());
    }

    #[test]
    fn fuse_results_dense_only() {
        let dense = vec![scored("a", 1.0), scored("b", 0.8), scored("c", 0.6)];
        let results = fuse_results(&dense, &[], 10);
        assert_eq!(results.len(), 3);
        // The order follows the dense rank (since there's no sparse contribution).
        assert_eq!(results[0].id, "a");
        assert_eq!(results[1].id, "b");
        assert_eq!(results[2].id, "c");
        // Each has a dense_score but no sparse_score.
        assert!(results[0].dense_score.is_some());
        assert!(results[0].sparse_score.is_none());
    }

    #[test]
    fn fuse_results_sparse_only() {
        let sparse = vec![scored("x", 5.0), scored("y", 3.0)];
        let results = fuse_results(&[], &sparse, 10);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].id, "x");
        assert!(results[0].sparse_score.is_some());
        assert!(results[0].dense_score.is_none());
    }

    #[test]
    fn fuse_results_overlapping_ids_get_higher_rrf_score() {
        // "a" appears at rank 1 in dense + rank 1 in sparse → highest RRF score.
        let dense = vec![scored("a", 1.0), scored("b", 0.8)];
        let sparse = vec![scored("a", 5.0), scored("c", 3.0)];
        let results = fuse_results(&dense, &sparse, 10);
        assert_eq!(results[0].id, "a", "overlap at rank 1 → highest RRF");
        assert!(results[0].dense_score.is_some());
        assert!(results[0].sparse_score.is_some());
    }

    #[test]
    fn fuse_results_respects_top_k() {
        let dense: Vec<ScoredPoint> = (0..10).map(|i| scored(&format!("d{i}"), 1.0)).collect();
        let sparse: Vec<ScoredPoint> = (0..10).map(|i| scored(&format!("s{i}"), 1.0)).collect();
        let results = fuse_results(&dense, &sparse, 5);
        assert_eq!(results.len(), 5, "top_k=5 truncates");
    }

    // ---- fuse_results_with_k: explicit formula verification ---------------

    #[test]
    fn rrf_formula_with_k_60() {
        // Dense: "a" at rank 1 → rrf = 1/(60+1) = 1/61
        // Sparse: "a" at rank 1 → rrf = 1/(60+1) = 1/61
        // Total: 2/61 ≈ 0.03279
        let dense = vec![scored("a", 1.0)];
        let sparse = vec![scored("a", 5.0)];
        let results = fuse_results_with_k(&dense, &sparse, 10, 60);
        assert_eq!(results.len(), 1);
        let expected = 2.0 / 61.0;
        assert!(
            (results[0].rrf_score - expected).abs() < 1e-9,
            "rrf={}, expected={expected}",
            results[0].rrf_score
        );
    }

    #[test]
    fn rrf_formula_with_k_1() {
        // Dense: "a" at rank 1 → rrf = 1/(1+1) = 0.5
        // Sparse: "a" at rank 2 → rrf = 1/(1+2) = 1/3
        // Total: 0.5 + 1/3 ≈ 0.8333
        let dense = vec![scored("a", 1.0)];
        let sparse = vec![scored("other", 5.0), scored("a", 3.0)];
        let results = fuse_results_with_k(&dense, &sparse, 10, 1);
        let a_result = results.iter().find(|r| r.id == "a").unwrap();
        let expected = 0.5 + 1.0 / 3.0;
        assert!(
            (a_result.rrf_score - expected).abs() < 1e-9,
            "rrf={}, expected={expected}",
            a_result.rrf_score
        );
    }

    #[test]
    fn rrf_higher_rank_contributes_less() {
        // With k=1: rank 1 → 1/2 = 0.5, rank 2 → 1/3 ≈ 0.333
        let dense = vec![scored("rank1", 1.0), scored("rank2", 0.9)];
        let results = fuse_results_with_k(&dense, &[], 10, 1);
        assert!(
            results[0].rrf_score > results[1].rrf_score,
            "rank 1 > rank 2"
        );
        assert!((results[0].rrf_score - 0.5).abs() < 1e-9);
        assert!((results[1].rrf_score - 1.0 / 3.0).abs() < 1e-9);
    }

    // ---- hybrid_search integration (with InMemoryVectorStore) --------------

    fn store_with_points() -> InMemoryVectorStore {
        let store = InMemoryVectorStore::new();
        store.create_collection("docs", Some(3)).unwrap();
        // Point "p1" is semantically close to the query vector [1, 0, 0].
        store
            .upsert(
                "docs",
                Point {
                    id: "p1".into(),
                    dense: Some(vec![1.0, 0.0, 0.0]),
                    sparse: None,
                    payload: json!({"subject": "refund"}),
                },
            )
            .unwrap();
        // Point "p2" is semantically different but lexically matches "refund".
        store
            .upsert(
                "docs",
                Point {
                    id: "p2".into(),
                    dense: Some(vec![0.0, 1.0, 0.0]),
                    sparse: None,
                    payload: json!({"subject": "refund policy"}),
                },
            )
            .unwrap();
        store
    }

    #[test]
    fn hybrid_search_returns_fused_results() {
        let db_path = std::env::temp_dir().join(format!("test_hybrid_{}.db", std::process::id()));
        let mut conn = crate::db::open(&db_path).unwrap();
        // DB-03 (M047): the full boot — the hybrid pipeline's people-scope
        // SQL reads the reference conversations column names.
        crate::bootstrap::apply_all(&mut conn).unwrap();
        crate::search::apply_fts_migration(&conn).unwrap();

        // We don't have FTS5 data in this test DB, so the sparse search returns
        // empty. The hybrid search should still return dense-only results.
        let store = store_with_points();
        let results =
            hybrid_search(&conn, &store, "docs", &[1.0, 0.0, 0.0], "refund", None, 10).unwrap();
        assert!(!results.is_empty(), "should return dense-only results");
        // p1 should rank first (cosine similarity 1.0 with [1, 0, 0]).
        assert_eq!(results[0].id, "p1");
        let _ = std::fs::remove_file(db_path);
    }

    #[test]
    fn hybrid_search_with_filter() {
        let db_path = std::env::temp_dir().join(format!("test_hybrid2_{}.db", std::process::id()));
        let mut conn = crate::db::open(&db_path).unwrap();
        // DB-03 (M047): the full boot — the hybrid pipeline's people-scope
        // SQL reads the reference conversations column names.
        crate::bootstrap::apply_all(&mut conn).unwrap();
        crate::search::apply_fts_migration(&conn).unwrap();

        let store = InMemoryVectorStore::new();
        store.create_collection("docs", Some(2)).unwrap();
        store
            .upsert(
                "docs",
                Point {
                    id: "p1".into(),
                    dense: Some(vec![1.0, 0.0]),
                    sparse: None,
                    payload: json!({"mailbox": "101"}),
                },
            )
            .unwrap();
        store
            .upsert(
                "docs",
                Point {
                    id: "p2".into(),
                    dense: Some(vec![0.9, 0.1]),
                    sparse: None,
                    payload: json!({"mailbox": "102"}),
                },
            )
            .unwrap();

        let filter = Filter::new().must_eq("mailbox", "101");
        let results = hybrid_search(
            &conn,
            &store,
            "docs",
            &[1.0, 0.0],
            "test",
            Some(&filter),
            10,
        )
        .unwrap();
        assert_eq!(results.len(), 1, "filter excludes p2");
        assert_eq!(results[0].id, "p1");
        let _ = std::fs::remove_file(db_path);
    }

    #[test]
    fn hybrid_search_returns_empty_for_nonexistent_collection() {
        let db_path = std::env::temp_dir().join(format!("test_hybrid3_{}.db", std::process::id()));
        let mut conn = crate::db::open(&db_path).unwrap();
        // DB-03 (M047): the full boot — the hybrid pipeline's people-scope
        // SQL reads the reference conversations column names.
        crate::bootstrap::apply_all(&mut conn).unwrap();
        crate::search::apply_fts_migration(&conn).unwrap();

        let store = InMemoryVectorStore::new();
        let results =
            hybrid_search(&conn, &store, "nonexistent", &[1.0], "test", None, 10).unwrap();
        assert!(results.is_empty());
        let _ = std::fs::remove_file(db_path);
    }

    // ---- HybridResult serde -------------------------------------------------

    #[test]
    fn hybrid_result_serializes() {
        let r = HybridResult {
            id: "p1".into(),
            rrf_score: 0.0328,
            dense_score: Some(1.0),
            sparse_score: Some(3.5),
            payload: json!({"subject": "test"}),
        };
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains("\"id\":\"p1\""));
        assert!(s.contains("\"rrf_score\":0.0328"));
        assert!(s.contains("\"dense_score\":1.0"));
    }

    // ---- RRF_K constant ---------------------------------------------------

    #[test]
    fn rrf_k_is_60() {
        assert_eq!(RRF_K, 60);
    }

    // ---- cosine_similarity (reference docsSemantic.ts) ----------------------

    #[test]
    fn cosine_identical_vectors_is_one() {
        let v = [1.0_f32, 2.0, 3.0];
        assert!((cosine_similarity(&v, &v) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_orthogonal_vectors_is_zero() {
        let a = [1.0_f32, 0.0, 0.0];
        let b = [0.0_f32, 1.0, 0.0];
        assert_eq!(cosine_similarity(&a, &b), 0.0);
    }

    #[test]
    fn cosine_opposite_vectors_is_minus_one() {
        let a = [1.0_f32, 0.0];
        let b = [-1.0_f32, 0.0];
        assert!((cosine_similarity(&a, &b) + 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_known_value() {
        // a = [1,2,3], b = [4,5,6] -> 32 / (sqrt(14)*sqrt(77)) ≈ 0.974632
        let a = [1.0_f32, 2.0, 3.0];
        let b = [4.0_f32, 5.0, 6.0];
        let got = cosine_similarity(&a, &b);
        let expected = 32.0_f64 / (14.0_f64.sqrt() * 77.0_f64.sqrt());
        assert!((f64::from(got) - expected).abs() < 1e-6);
    }

    #[test]
    fn cosine_length_mismatch_is_zero() {
        let a = [1.0_f32, 2.0];
        let b = [1.0_f32, 2.0, 3.0];
        assert_eq!(cosine_similarity(&a, &b), 0.0);
    }

    #[test]
    fn cosine_empty_vectors_is_zero() {
        assert_eq!(cosine_similarity(&[], &[]), 0.0);
    }

    #[test]
    fn cosine_zero_vector_is_zero() {
        let a = [0.0_f32, 0.0];
        let b = [1.0_f32, 2.0];
        assert_eq!(cosine_similarity(&a, &b), 0.0);
        assert_eq!(cosine_similarity(&b, &a), 0.0);
    }

    // ---- merge_doc_hits (reference mergeDocHits) -----------------------------

    fn fts_hit(id: i64, rank: usize) -> FtsDocHit {
        FtsDocHit {
            article_id: id,
            rank,
        }
    }

    fn sem_hit(id: i64, score: f32) -> SemanticDocHit {
        SemanticDocHit {
            article_id: id,
            score,
        }
    }

    #[test]
    fn merge_doc_hits_empty_inputs() {
        assert!(merge_doc_hits(&[], &[], 10).is_empty());
    }

    #[test]
    fn merge_doc_hits_formula_uses_index_rank() {
        // Reference: score += 1/(RRF_K + i + 1) with i = array position.
        // Article 7 at FTS position 0 and semantic position 0:
        // 1/61 + 1/61 = 2/61 ≈ 0.0328 (rounded to 4 decimals).
        let merged = merge_doc_hits(&[fts_hit(7, 0)], &[sem_hit(7, 0.99)], 10);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].article_id, 7);
        let expected = (2.0_f64 / 61.0 * 10_000.0).round() / 10_000.0;
        assert!(
            (merged[0].score - expected).abs() < 1e-9,
            "score={}, expected={expected}",
            merged[0].score
        );
        assert_eq!(merged[0].why, vec!["fts", "semantic"]);
    }

    #[test]
    fn merge_doc_hits_overlapping_ranks_first() {
        // Article 1 is rank 0 in BOTH lists -> fused 2/61.
        // Article 2 is rank 0 in FTS only -> 1/61. Article 3 rank 0 semantic
        // only -> 1/61. Tie between 2 and 3 breaks by ascending article id.
        let fts = [fts_hit(1, 0), fts_hit(2, 1)];
        let sem = [sem_hit(1, 0.9), sem_hit(3, 0.5)];
        let merged = merge_doc_hits(&fts, &sem, 10);
        assert_eq!(merged[0].article_id, 1);
        assert_eq!(merged[0].why, vec!["fts", "semantic"]);
        assert_eq!(merged[1].article_id, 2);
        assert_eq!(merged[1].why, vec!["fts"]);
        assert_eq!(merged[2].article_id, 3);
        assert_eq!(merged[2].why, vec!["semantic"]);
        assert!(merged[0].score > merged[1].score);
        // Tie: articles 2 and 3 both have 1/61 -> equal scores.
        assert!((merged[1].score - merged[2].score).abs() < 1e-9);
    }

    #[test]
    fn merge_doc_hits_ignores_raw_scores_uses_positions() {
        // Raw cosine scores never enter the fused score: two runs with
        // wildly different raw scores produce IDENTICAL results (rank-based
        // fusion only — the reference's core RRF property).
        let m1 = merge_doc_hits(&[], &[sem_hit(1, 0.99), sem_hit(2, 0.01)], 10);
        let m2 = merge_doc_hits(&[], &[sem_hit(1, 0.50), sem_hit(2, 0.49)], 10);
        assert_eq!(m1.len(), 2);
        assert_eq!(m2.len(), 2);
        for (a, b) in m1.iter().zip(m2.iter()) {
            assert_eq!(a.article_id, b.article_id);
            assert_eq!(a.score, b.score);
            assert_eq!(a.why, b.why);
        }
        // Ranks differ -> scores differ (positions drive the fusion).
        assert!(m1[0].score > m1[1].score);
        assert!(m1[0].score > 0.0);
    }

    #[test]
    fn merge_doc_hits_respects_limit() {
        let fts: Vec<FtsDocHit> = (0..30).map(|i| fts_hit(i, i as usize)).collect();
        let sem: Vec<SemanticDocHit> = (100..130).map(|i| sem_hit(i, 0.5)).collect();
        assert_eq!(merge_doc_hits(&fts, &sem, 40).len(), 40);
        assert_eq!(merge_doc_hits(&fts, &sem, 5).len(), 5);
    }

    #[test]
    fn merge_doc_hits_rounds_to_four_decimals() {
        // Article 9 at FTS position 0 (1/61) + semantic position 1 (1/62)
        // = 123/3782 = 0.03252247... -> rounds to 0.0325 (reference
        // Math.round to 4-decimal precision).
        let fts = [fts_hit(9, 0)];
        let sem = [sem_hit(1, 0.9), sem_hit(9, 0.8)];
        let merged = merge_doc_hits(&fts, &sem, 10);
        let nine = merged
            .iter()
            .find(|m| m.article_id == 9)
            .expect("9 present");
        assert_eq!(nine.score, 0.0325);
    }

    #[test]
    fn merge_doc_hits_fts_list_processed_first_for_why_order() {
        // Even when the semantic list is given first at the same article, the
        // reference processes FTS first, so 'fts' precedes 'semantic'.
        let sem = [sem_hit(5, 0.9)];
        let fts = [fts_hit(5, 0)];
        let merged = merge_doc_hits(&fts, &sem, 10);
        assert_eq!(merged[0].why, vec!["fts", "semantic"]);
    }
}
