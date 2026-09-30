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
        crate::db::ensure_migrations_table(&conn).unwrap();
        crate::migrations::run_all(&mut conn).unwrap();
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
        crate::db::ensure_migrations_table(&conn).unwrap();
        crate::migrations::run_all(&mut conn).unwrap();
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
        crate::db::ensure_migrations_table(&conn).unwrap();
        crate::migrations::run_all(&mut conn).unwrap();
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
}
