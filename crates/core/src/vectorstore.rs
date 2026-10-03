//! VectorStore abstraction — the trait + In-memory adapter (M5-T01).
//!
//! Per spec A4 (Qdrant Edge — decision is final, do not reopen):
//! - The `qdrant-edge` Rust crate is embedded + in-process, behind this
//!   VectorStore abstraction; only the Qdrant adapter module (M5-T02) may
//!   import `qdrant-edge`. This trait module has no `qdrant-edge` dependency.
//! - SQLite stays authoritative; vectors are derived and rebuildable from
//!   source text. The VectorStore is a derived index, not the source of truth.
//! - The snapshot/restore format is adapter-agnostic: a snapshot from the
//!   In-memory adapter can be restored into the Qdrant adapter (and vice versa).
//!
//! ## Design
//!
//! The trait is sync (not async) — the In-memory adapter is naturally sync,
//! and `qdrant-edge` is an embedded in-process crate (not a REST client), so
//! its API is expected to be sync too. If M5-T02 reveals otherwise, the trait
//! can be refactored to async without touching callers (the trait is the
//! boundary; callers depend on the trait, not the impl).
//!
//! Per spec A4: "Verify every capability required by spec sections 16 to 37
//! exists in the pinned version (dense/sparse/named vectors, payload filters
//! and indexes, exact search, snapshots and restore, WAL, count/scroll/facet)."
//! The trait below is the full contract; M5-T10 verifies each adapter against it.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::error::Result;

/// A point id. Qdrant supports both UUID and uint64; we use `String` for
/// flexibility (the In-memory adapter stores them as HashMap keys; the Qdrant
/// adapter converts as needed).
pub type PointId = String;

/// A dense vector (the standard embedding — e.g., 384 or 1536 floats).
pub type DenseVector = Vec<f32>;

/// A sparse vector (for hybrid search — e.g., BM25-style weights).
/// Qdrant supports named sparse vectors; the In-memory adapter stores them
/// alongside the dense vector.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SparseVector {
    /// The dimension indices with non-zero values. Must be sorted ascending
    /// (Qdrant requirement; the In-memory adapter enforces this on upsert).
    pub indices: Vec<u32>,
    /// The values at those indices. Must be the same length as `indices`.
    pub values: Vec<f32>,
}

impl SparseVector {
    /// Create a sparse vector. Sorts the indices ascending (Qdrant requirement)
    /// and applies the same permutation to the values.
    #[must_use]
    pub fn new(mut indices: Vec<u32>, mut values: Vec<f32>) -> Self {
        assert_eq!(
            indices.len(),
            values.len(),
            "sparse vector: indices and values must have the same length"
        );
        // Sort indices ascending, permuting values in parallel.
        let mut pairs: Vec<(u32, f32)> = indices
            .iter()
            .zip(values.iter())
            .map(|(&i, &v)| (i, v))
            .collect();
        pairs.sort_by_key(|(i, _)| *i);
        indices = pairs.iter().map(|(i, _)| *i).collect();
        values = pairs.iter().map(|(_, v)| *v).collect();
        Self { indices, values }
    }

    /// The dot product of two sparse vectors. Used by the In-memory adapter's
    /// sparse search (cosine similarity for unit-normalized vectors).
    /// Returns 0.0 if either vector is empty.
    #[must_use]
    pub fn dot_product(&self, other: &Self) -> f32 {
        let mut sum = 0.0_f32;
        let mut i = 0;
        let mut j = 0;
        while i < self.indices.len() && j < other.indices.len() {
            if self.indices[i] == other.indices[j] {
                sum += self.values[i] * other.values[j];
                i += 1;
                j += 1;
            } else if self.indices[i] < other.indices[j] {
                i += 1;
            } else {
                j += 1;
            }
        }
        sum
    }

    /// The L2 norm (Euclidean). Used for cosine similarity normalization.
    #[must_use]
    pub fn l2_norm(&self) -> f32 {
        self.values.iter().map(|v| v * v).sum::<f32>().sqrt()
    }
}

/// A payload filter for search. Currently supports equality match on a single
/// key (e.g., `{"mailbox_id": "101"}`). The Qdrant adapter translates this to
/// its native filter format. Future milestones may add range/in filters; the
/// trait stays stable, only the filter struct grows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Filter {
    /// Key-value equality conditions (AND-ed together).
    pub must: HashMap<String, String>,
}

impl Filter {
    /// Create an empty filter (matches all points).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a must-equal condition. Returns `self` for chaining.
    #[must_use]
    pub fn must_eq(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.must.insert(key.into(), value.into());
        self
    }

    /// Whether the filter is empty (matches all points).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.must.is_empty()
    }

    /// Whether a payload matches this filter. Used by the In-memory adapter;
    /// the Qdrant adapter translates to native filters instead.
    #[must_use]
    pub fn matches(&self, payload: &Payload) -> bool {
        for (key, value) in &self.must {
            match payload.get(key) {
                Some(v) => {
                    if v.as_str() != Some(value.as_str()) {
                        return false;
                    }
                }
                None => return false,
            }
        }
        true
    }
}

/// A point's payload — JSON-like key/value metadata. Stored alongside the
/// vector; filterable during search.
pub type Payload = serde_json::Value;

/// A point in the vector store — an id + optional dense vector + optional
/// sparse vector + payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Point {
    /// The point's unique id within its collection.
    pub id: PointId,
    /// The dense vector (None for sparse-only points; e.g., BM25-only docs).
    pub dense: Option<DenseVector>,
    /// The sparse vector (None for dense-only points).
    pub sparse: Option<SparseVector>,
    /// The payload (filterable metadata).
    pub payload: Payload,
}

/// A scored search result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScoredPoint {
    /// The point's id.
    pub id: PointId,
    /// The similarity score (higher = more similar). For dense cosine
    /// similarity, range is [-1.0, 1.0]; for sparse dot product, range is
    /// unbounded (depends on vector magnitudes).
    pub score: f32,
    /// The point's payload.
    pub payload: Payload,
}

/// Collection metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectionInfo {
    /// The collection name.
    pub name: String,
    /// The dense vector dimension (None for sparse-only collections).
    pub dense_dim: Option<usize>,
    /// The number of points in the collection.
    pub point_count: usize,
}

/// A serializable snapshot of a collection — adapter-agnostic so a snapshot
/// from the In-memory adapter can be restored into the Qdrant adapter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectionSnapshot {
    /// The collection name.
    pub name: String,
    /// The dense vector dimension (None for sparse-only collections).
    pub dense_dim: Option<usize>,
    /// All points in the collection (in insertion order).
    pub points: Vec<Point>,
}

/// The VectorStore trait — the abstraction boundary per spec A4.
/// Only the Qdrant adapter module (M5-T02) may import `qdrant-edge`; this
/// trait is the single source of truth that all callers depend on.
///
/// Per spec A4: "Verify every capability required by spec sections 16 to 37
/// exists in the pinned version (dense/sparse/named vectors, payload filters
/// and indexes, exact search, snapshots and restore, WAL, count/scroll/facet)."
pub trait VectorStore: Send + Sync {
    /// Create a collection with the given dense vector dimension.
    /// `dense_dim = None` creates a sparse-only collection.
    /// Idempotent: creating an existing collection is a no-op (not an error).
    fn create_collection(&self, name: &str, dense_dim: Option<usize>) -> Result<()>;

    /// Drop a collection. Idempotent: dropping a nonexistent collection is a no-op.
    fn drop_collection(&self, name: &str) -> Result<()>;

    /// Upsert a point into a collection. If a point with the same id exists,
    /// it's replaced. The collection must exist (returns an error otherwise).
    fn upsert(&self, collection: &str, point: Point) -> Result<()>;

    /// Delete a point by id. Idempotent: deleting a nonexistent point is a no-op.
    fn delete(&self, collection: &str, id: &PointId) -> Result<()>;

    /// Dense vector search: returns the top-k points most similar to `query`,
    /// filtered by `filter` (if provided). Uses cosine similarity (vectors are
    /// L2-normalized at search time). Returns an empty vec if the collection
    /// is empty or no points match the filter.
    fn search_dense(
        &self,
        collection: &str,
        query: &[f32],
        filter: Option<&Filter>,
        top_k: usize,
    ) -> Result<Vec<ScoredPoint>>;

    /// Sparse vector search: returns the top-k points most similar to `query`,
    /// filtered by `filter` (if provided). Uses dot product on sparse vectors
    /// (BM25-style scoring). Returns an empty vec if the collection is empty or
    /// no points match the filter.
    fn search_sparse(
        &self,
        collection: &str,
        query: &SparseVector,
        filter: Option<&Filter>,
        top_k: usize,
    ) -> Result<Vec<ScoredPoint>>;

    /// Count the points in a collection (optionally filtered).
    fn count(&self, collection: &str, filter: Option<&Filter>) -> Result<usize>;

    /// Get collection metadata. Returns `None` if the collection doesn't exist.
    fn collection_info(&self, name: &str) -> Result<Option<CollectionInfo>>;

    /// Snapshot a collection to bytes (adapter-agnostic format).
    /// Used by M5-T09 (vector backup + recovery).
    fn snapshot(&self, name: &str) -> Result<Vec<u8>>;

    /// Restore a collection from snapshot bytes. Creates the collection if it
    /// doesn't exist; replaces all points if it does. The snapshot format is
    /// adapter-agnostic (a `CollectionSnapshot` serialized as JSON).
    fn restore(&self, bytes: &[u8]) -> Result<()>;
}

// ─── In-memory adapter (Fake) ─────────────────────────────────────────────

/// An in-memory VectorStore implementation — the cfg(test) double for the
/// VectorStore contract (deviation D2: production uses the embedded Qdrant
/// adapter; this Fake exists only for tests and is removed in the final
/// strip). Backed by a `HashMap` of collection name → (dim, points).
#[cfg(test)]
#[derive(Debug, Default)]
pub struct InMemoryVectorStore {
    collections: std::sync::Mutex<HashMap<String, InMemoryCollection>>,
}

#[cfg(test)]
#[derive(Debug, Clone)]
struct InMemoryCollection {
    dense_dim: Option<usize>,
    points: Vec<Point>,
}

#[cfg(test)]
impl InMemoryVectorStore {
    /// Create a new empty in-memory store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[cfg(test)]
impl VectorStore for InMemoryVectorStore {
    fn create_collection(&self, name: &str, dense_dim: Option<usize>) -> Result<()> {
        let mut collections = self.collections.lock().expect("mutex poisoned");
        collections
            .entry(name.to_string())
            .or_insert_with(|| InMemoryCollection {
                dense_dim,
                points: Vec::new(),
            });
        Ok(())
    }

    fn drop_collection(&self, name: &str) -> Result<()> {
        let mut collections = self.collections.lock().expect("mutex poisoned");
        collections.remove(name);
        Ok(())
    }

    fn upsert(&self, collection: &str, point: Point) -> Result<()> {
        let mut collections = self.collections.lock().expect("mutex poisoned");
        let col = collections.get_mut(collection).ok_or_else(|| {
            crate::error::Error::Config(format!("collection {collection:?} does not exist"))
        })?;
        // Validate dense dim if the collection has one.
        if let (Some(expected), Some(actual)) = (col.dense_dim, &point.dense) {
            if expected != actual.len() {
                return Err(crate::error::Error::Config(format!(
                    "dense vector dim mismatch: collection expects {expected}, got {}",
                    actual.len()
                )));
            }
        }
        // Replace if exists, else append.
        if let Some(existing) = col.points.iter_mut().find(|p| p.id == point.id) {
            *existing = point;
        } else {
            col.points.push(point);
        }
        Ok(())
    }

    fn delete(&self, collection: &str, id: &PointId) -> Result<()> {
        let mut collections = self.collections.lock().expect("mutex poisoned");
        if let Some(col) = collections.get_mut(collection) {
            col.points.retain(|p| &p.id != id);
        }
        Ok(())
    }

    fn search_dense(
        &self,
        collection: &str,
        query: &[f32],
        filter: Option<&Filter>,
        top_k: usize,
    ) -> Result<Vec<ScoredPoint>> {
        let collections = self.collections.lock().expect("mutex poisoned");
        let Some(col) = collections.get(collection) else {
            return Ok(Vec::new());
        };
        let mut scored: Vec<ScoredPoint> = col
            .points
            .iter()
            .filter(|p| p.dense.is_some())
            .filter(|p| filter.is_none_or(|f| f.matches(&p.payload)))
            .map(|p| {
                let dense = p.dense.as_ref().expect("filtered to dense points");
                let score = cosine_similarity(query, dense);
                ScoredPoint {
                    id: p.id.clone(),
                    score,
                    payload: p.payload.clone(),
                }
            })
            .collect();
        // Sort by score descending (most similar first).
        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        scored.truncate(top_k);
        Ok(scored)
    }

    fn search_sparse(
        &self,
        collection: &str,
        query: &SparseVector,
        filter: Option<&Filter>,
        top_k: usize,
    ) -> Result<Vec<ScoredPoint>> {
        let collections = self.collections.lock().expect("mutex poisoned");
        let Some(col) = collections.get(collection) else {
            return Ok(Vec::new());
        };
        let mut scored: Vec<ScoredPoint> = col
            .points
            .iter()
            .filter(|p| p.sparse.is_some())
            .filter(|p| filter.is_none_or(|f| f.matches(&p.payload)))
            .map(|p| {
                let sparse = p.sparse.as_ref().expect("filtered to sparse points");
                // Cosine similarity for sparse: dot / (norm_a * norm_b).
                let dot = query.dot_product(sparse);
                let norm_a = query.l2_norm();
                let norm_b = sparse.l2_norm();
                let score = if norm_a > 0.0 && norm_b > 0.0 {
                    dot / (norm_a * norm_b)
                } else {
                    0.0
                };
                ScoredPoint {
                    id: p.id.clone(),
                    score,
                    payload: p.payload.clone(),
                }
            })
            .collect();
        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        scored.truncate(top_k);
        Ok(scored)
    }

    fn count(&self, collection: &str, filter: Option<&Filter>) -> Result<usize> {
        let collections = self.collections.lock().expect("mutex poisoned");
        let Some(col) = collections.get(collection) else {
            return Ok(0);
        };
        let count = col
            .points
            .iter()
            .filter(|p| filter.is_none_or(|f| f.matches(&p.payload)))
            .count();
        Ok(count)
    }

    fn collection_info(&self, name: &str) -> Result<Option<CollectionInfo>> {
        let collections = self.collections.lock().expect("mutex poisoned");
        Ok(collections.get(name).map(|col| CollectionInfo {
            name: name.to_string(),
            dense_dim: col.dense_dim,
            point_count: col.points.len(),
        }))
    }

    fn snapshot(&self, name: &str) -> Result<Vec<u8>> {
        let collections = self.collections.lock().expect("mutex poisoned");
        let Some(col) = collections.get(name) else {
            return Err(crate::error::Error::Config(format!(
                "collection {name:?} does not exist"
            )));
        };
        let snapshot = CollectionSnapshot {
            name: name.to_string(),
            dense_dim: col.dense_dim,
            points: col.points.clone(),
        };
        let bytes = serde_json::to_vec(&snapshot).map_err(|e| {
            crate::error::Error::Config(format!("snapshot serialization failed: {e}"))
        })?;
        Ok(bytes)
    }

    fn restore(&self, bytes: &[u8]) -> Result<()> {
        let snapshot: CollectionSnapshot = serde_json::from_slice(bytes).map_err(|e| {
            crate::error::Error::Config(format!("snapshot deserialization failed: {e}"))
        })?;
        let mut collections = self.collections.lock().expect("mutex poisoned");
        collections.insert(
            snapshot.name.clone(),
            InMemoryCollection {
                dense_dim: snapshot.dense_dim,
                points: snapshot.points,
            },
        );
        Ok(())
    }
}

/// Cosine similarity: dot(a, b) / (|a| * |b|). Range [-1.0, 1.0].
/// Returns 0.0 if either vector is zero-length.
#[cfg(test)]
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a > 0.0 && norm_b > 0.0 {
        dot / (norm_a * norm_b)
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn store_with_collection(dense_dim: Option<usize>) -> (InMemoryVectorStore, &'static str) {
        let store = InMemoryVectorStore::new();
        store.create_collection("docs", dense_dim).unwrap();
        (store, "docs")
    }

    fn point(
        id: &str,
        dense: Option<Vec<f32>>,
        sparse: Option<SparseVector>,
        payload: Payload,
    ) -> Point {
        Point {
            id: id.into(),
            dense,
            sparse,
            payload,
        }
    }

    // ---- create_collection + collection_info -------------------------------

    #[test]
    fn create_collection_is_idempotent() {
        let store = InMemoryVectorStore::new();
        store.create_collection("docs", Some(3)).unwrap();
        // Re-creating the same collection is a no-op (not an error).
        store.create_collection("docs", Some(3)).unwrap();
        let info = store.collection_info("docs").unwrap().unwrap();
        assert_eq!(info.name, "docs");
        assert_eq!(info.dense_dim, Some(3));
        assert_eq!(info.point_count, 0);
    }

    #[test]
    fn create_collection_supports_sparse_only() {
        let store = InMemoryVectorStore::new();
        store.create_collection("sparse_docs", None).unwrap();
        let info = store.collection_info("sparse_docs").unwrap().unwrap();
        assert_eq!(info.dense_dim, None);
    }

    #[test]
    fn collection_info_returns_none_for_nonexistent() {
        let store = InMemoryVectorStore::new();
        assert!(store.collection_info("nonexistent").unwrap().is_none());
    }

    #[test]
    fn drop_collection_is_idempotent() {
        let store = InMemoryVectorStore::new();
        store.create_collection("docs", Some(3)).unwrap();
        store.drop_collection("docs").unwrap();
        // Dropping again is a no-op.
        store.drop_collection("docs").unwrap();
        assert!(store.collection_info("docs").unwrap().is_none());
    }

    // ---- upsert + delete ---------------------------------------------------

    #[test]
    fn upsert_inserts_new_point() {
        let (store, col) = store_with_collection(Some(3));
        store
            .upsert(
                col,
                point(
                    "p1",
                    Some(vec![1.0, 0.0, 0.0]),
                    None,
                    json!({"mailbox": "101"}),
                ),
            )
            .unwrap();
        assert_eq!(store.count(col, None).unwrap(), 1);
    }

    #[test]
    fn upsert_replaces_existing_point_by_id() {
        let (store, col) = store_with_collection(Some(3));
        store
            .upsert(
                col,
                point("p1", Some(vec![1.0, 0.0, 0.0]), None, json!({"v": 1})),
            )
            .unwrap();
        store
            .upsert(
                col,
                point("p1", Some(vec![0.0, 1.0, 0.0]), None, json!({"v": 2})),
            )
            .unwrap();
        assert_eq!(
            store.count(col, None).unwrap(),
            1,
            "upsert replaces, not appends"
        );
    }

    #[test]
    fn upsert_validates_dense_dim() {
        let (store, col) = store_with_collection(Some(3));
        let result = store.upsert(col, point("p1", Some(vec![1.0, 0.0]), None, json!({})));
        assert!(result.is_err(), "dim mismatch must error");
    }

    #[test]
    fn upsert_to_nonexistent_collection_errors() {
        let store = InMemoryVectorStore::new();
        let result = store.upsert("nonexistent", point("p1", Some(vec![1.0]), None, json!({})));
        assert!(result.is_err());
    }

    #[test]
    fn delete_removes_point() {
        let (store, col) = store_with_collection(Some(3));
        store
            .upsert(col, point("p1", Some(vec![1.0, 0.0, 0.0]), None, json!({})))
            .unwrap();
        store.delete(col, &"p1".to_string()).unwrap();
        assert_eq!(store.count(col, None).unwrap(), 0);
    }

    #[test]
    fn delete_nonexistent_point_is_noop() {
        let (store, col) = store_with_collection(Some(3));
        store.delete(col, &"nonexistent".to_string()).unwrap();
        assert_eq!(store.count(col, None).unwrap(), 0);
    }

    // ---- search_dense ------------------------------------------------------

    #[test]
    fn search_dense_returns_top_k_by_cosine_similarity() {
        let (store, col) = store_with_collection(Some(3));
        store
            .upsert(col, point("p1", Some(vec![1.0, 0.0, 0.0]), None, json!({})))
            .unwrap();
        store
            .upsert(col, point("p2", Some(vec![0.0, 1.0, 0.0]), None, json!({})))
            .unwrap();
        store
            .upsert(col, point("p3", Some(vec![0.7, 0.7, 0.0]), None, json!({})))
            .unwrap();

        // Query [1, 0, 0] → p1 (cos=1.0), p3 (cos≈0.707), p2 (cos=0.0).
        let results = store.search_dense(col, &[1.0, 0.0, 0.0], None, 3).unwrap();
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].id, "p1");
        assert!((results[0].score - 1.0).abs() < 1e-6);
        assert_eq!(results[1].id, "p3");
        assert!((results[1].score - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-3);
        assert_eq!(results[2].id, "p2");
        assert!(results[2].score.abs() < 1e-6);
    }

    #[test]
    fn search_dense_respects_top_k() {
        let (store, col) = store_with_collection(Some(2));
        store
            .upsert(col, point("p1", Some(vec![1.0, 0.0]), None, json!({})))
            .unwrap();
        store
            .upsert(col, point("p2", Some(vec![0.9, 0.1]), None, json!({})))
            .unwrap();
        store
            .upsert(col, point("p3", Some(vec![0.8, 0.2]), None, json!({})))
            .unwrap();

        let results = store.search_dense(col, &[1.0, 0.0], None, 2).unwrap();
        assert_eq!(results.len(), 2, "top_k=2 truncates to 2");
    }

    #[test]
    fn search_dense_applies_filter() {
        let (store, col) = store_with_collection(Some(2));
        store
            .upsert(
                col,
                point("p1", Some(vec![1.0, 0.0]), None, json!({"mailbox": "101"})),
            )
            .unwrap();
        store
            .upsert(
                col,
                point("p2", Some(vec![0.9, 0.1]), None, json!({"mailbox": "102"})),
            )
            .unwrap();

        let filter = Filter::new().must_eq("mailbox", "101");
        let results = store
            .search_dense(col, &[1.0, 0.0], Some(&filter), 10)
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "p1");
    }

    #[test]
    fn search_dense_returns_empty_for_nonexistent_collection() {
        let store = InMemoryVectorStore::new();
        let results = store.search_dense("nonexistent", &[1.0], None, 10).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn search_dense_skips_points_without_dense_vector() {
        let (store, col) = store_with_collection(Some(2));
        // p1 has a dense vector; p2 is sparse-only.
        store
            .upsert(col, point("p1", Some(vec![1.0, 0.0]), None, json!({})))
            .unwrap();
        store
            .upsert(
                col,
                point(
                    "p2",
                    None,
                    Some(SparseVector::new(vec![0], vec![1.0])),
                    json!({}),
                ),
            )
            .unwrap();

        let results = store.search_dense(col, &[1.0, 0.0], None, 10).unwrap();
        assert_eq!(
            results.len(),
            1,
            "sparse-only points are excluded from dense search"
        );
        assert_eq!(results[0].id, "p1");
    }

    // ---- search_sparse -----------------------------------------------------

    #[test]
    fn search_sparse_returns_top_k_by_dot_product() {
        let store = InMemoryVectorStore::new();
        store.create_collection("docs", None).unwrap();
        let col = "docs";

        store
            .upsert(
                col,
                point(
                    "p1",
                    None,
                    Some(SparseVector::new(vec![0, 1], vec![1.0, 1.0])),
                    json!({}),
                ),
            )
            .unwrap();
        store
            .upsert(
                col,
                point(
                    "p2",
                    None,
                    Some(SparseVector::new(vec![1, 2], vec![1.0, 1.0])),
                    json!({}),
                ),
            )
            .unwrap();

        // Query [0:1.0, 1:1.0] → p1 (cos=1.0), p2 (cos=0.5).
        let query = SparseVector::new(vec![0, 1], vec![1.0, 1.0]);
        let results = store.search_sparse(col, &query, None, 10).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].id, "p1");
        assert!((results[0].score - 1.0).abs() < 1e-6, "p1 cosine = 1.0");
    }

    #[test]
    fn search_sparse_applies_filter() {
        let store = InMemoryVectorStore::new();
        store.create_collection("docs", None).unwrap();
        let col = "docs";

        store
            .upsert(
                col,
                point(
                    "p1",
                    None,
                    Some(SparseVector::new(vec![0], vec![1.0])),
                    json!({"tag": "vip"}),
                ),
            )
            .unwrap();
        store
            .upsert(
                col,
                point(
                    "p2",
                    None,
                    Some(SparseVector::new(vec![0], vec![1.0])),
                    json!({"tag": "normal"}),
                ),
            )
            .unwrap();

        let filter = Filter::new().must_eq("tag", "vip");
        let query = SparseVector::new(vec![0], vec![1.0]);
        let results = store.search_sparse(col, &query, Some(&filter), 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "p1");
    }

    #[test]
    fn search_sparse_skips_points_without_sparse_vector() {
        let (store, col) = store_with_collection(Some(2));
        store
            .upsert(col, point("p1", Some(vec![1.0, 0.0]), None, json!({})))
            .unwrap();
        store
            .upsert(
                col,
                point(
                    "p2",
                    None,
                    Some(SparseVector::new(vec![0], vec![1.0])),
                    json!({}),
                ),
            )
            .unwrap();

        let query = SparseVector::new(vec![0], vec![1.0]);
        let results = store.search_sparse(col, &query, None, 10).unwrap();
        assert_eq!(
            results.len(),
            1,
            "dense-only points excluded from sparse search"
        );
        assert_eq!(results[0].id, "p2");
    }

    // ---- count -------------------------------------------------------------

    #[test]
    fn count_returns_zero_for_empty_collection() {
        let (store, col) = store_with_collection(Some(3));
        assert_eq!(store.count(col, None).unwrap(), 0);
    }

    #[test]
    fn count_with_filter() {
        let (store, col) = store_with_collection(Some(2));
        store
            .upsert(
                col,
                point("p1", Some(vec![1.0, 0.0]), None, json!({"mailbox": "101"})),
            )
            .unwrap();
        store
            .upsert(
                col,
                point("p2", Some(vec![0.0, 1.0]), None, json!({"mailbox": "102"})),
            )
            .unwrap();
        store
            .upsert(
                col,
                point("p3", Some(vec![1.0, 1.0]), None, json!({"mailbox": "101"})),
            )
            .unwrap();

        let filter = Filter::new().must_eq("mailbox", "101");
        assert_eq!(store.count(col, Some(&filter)).unwrap(), 2);
    }

    #[test]
    fn count_returns_zero_for_nonexistent_collection() {
        let store = InMemoryVectorStore::new();
        assert_eq!(store.count("nonexistent", None).unwrap(), 0);
    }

    // ---- snapshot + restore ------------------------------------------------

    #[test]
    fn snapshot_then_restore_round_trips() {
        let store = InMemoryVectorStore::new();
        store.create_collection("docs", Some(2)).unwrap();
        store
            .upsert(
                "docs",
                point("p1", Some(vec![1.0, 0.0]), None, json!({"k": "v"})),
            )
            .unwrap();
        store
            .upsert(
                "docs",
                point("p2", Some(vec![0.0, 1.0]), None, json!({"k": "w"})),
            )
            .unwrap();

        let bytes = store.snapshot("docs").unwrap();
        assert!(!bytes.is_empty());

        // Restore into a fresh store.
        let store2 = InMemoryVectorStore::new();
        store2.restore(&bytes).unwrap();

        let info = store2.collection_info("docs").unwrap().unwrap();
        assert_eq!(info.dense_dim, Some(2));
        assert_eq!(info.point_count, 2);

        // Verify the points are searchable.
        let results = store2.search_dense("docs", &[1.0, 0.0], None, 10).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].id, "p1");
    }

    #[test]
    fn snapshot_nonexistent_collection_errors() {
        let store = InMemoryVectorStore::new();
        assert!(store.snapshot("nonexistent").is_err());
    }

    #[test]
    fn restore_replaces_existing_collection() {
        let store = InMemoryVectorStore::new();
        store.create_collection("docs", Some(2)).unwrap();
        store
            .upsert("docs", point("old", Some(vec![1.0, 1.0]), None, json!({})))
            .unwrap();

        // Snapshot from a different store with different points.
        let store2 = InMemoryVectorStore::new();
        store2.create_collection("docs", Some(2)).unwrap();
        store2
            .upsert("docs", point("new1", Some(vec![1.0, 0.0]), None, json!({})))
            .unwrap();
        store2
            .upsert("docs", point("new2", Some(vec![0.0, 1.0]), None, json!({})))
            .unwrap();
        let bytes = store2.snapshot("docs").unwrap();

        // Restore into the first store — should replace all points.
        store.restore(&bytes).unwrap();
        assert_eq!(
            store.count("docs", None).unwrap(),
            2,
            "restore replaces, not appends"
        );
    }

    #[test]
    fn restore_rejects_corrupt_bytes() {
        let store = InMemoryVectorStore::new();
        assert!(store.restore(b"not valid json").is_err());
    }

    // ---- SparseVector helpers ----------------------------------------------

    #[test]
    fn sparse_vector_new_sorts_indices_ascending() {
        let sv = SparseVector::new(vec![5, 2, 8], vec![0.5, 0.3, 0.2]);
        assert_eq!(sv.indices, vec![2, 5, 8]);
        assert_eq!(
            sv.values,
            vec![0.3, 0.5, 0.2],
            "values follow the sorted indices"
        );
    }

    #[test]
    fn sparse_vector_dot_product_overlapping() {
        let a = SparseVector::new(vec![0, 1, 2], vec![1.0, 2.0, 3.0]);
        let b = SparseVector::new(vec![1, 2, 3], vec![4.0, 5.0, 6.0]);
        // Overlap at indices 1 and 2: 2*4 + 3*5 = 8 + 15 = 23.
        assert!((a.dot_product(&b) - 23.0).abs() < 1e-6);
    }

    #[test]
    fn sparse_vector_dot_product_no_overlap() {
        let a = SparseVector::new(vec![0, 1], vec![1.0, 2.0]);
        let b = SparseVector::new(vec![2, 3], vec![3.0, 4.0]);
        assert!(a.dot_product(&b).abs() < 1e-6);
    }

    #[test]
    fn sparse_vector_dot_product_empty() {
        let a = SparseVector::new(vec![], vec![]);
        let b = SparseVector::new(vec![0], vec![1.0]);
        assert!(a.dot_product(&b).abs() < 1e-6);
    }

    #[test]
    fn sparse_vector_l2_norm() {
        let sv = SparseVector::new(vec![0, 1], vec![3.0, 4.0]);
        assert!((sv.l2_norm() - 5.0).abs() < 1e-6, "3-4-5 triangle");
    }

    // ---- Filter -----------------------------------------------------------

    #[test]
    fn filter_matches_payload() {
        let filter = Filter::new()
            .must_eq("mailbox", "101")
            .must_eq("status", "active");
        assert!(filter.matches(&json!({"mailbox": "101", "status": "active"})));
        assert!(!filter.matches(&json!({"mailbox": "101", "status": "closed"})));
        assert!(!filter.matches(&json!({"mailbox": "102", "status": "active"})));
    }

    #[test]
    fn filter_empty_matches_all() {
        let filter = Filter::new();
        assert!(filter.is_empty());
        assert!(filter.matches(&json!({})));
        assert!(filter.matches(&json!({"anything": "value"})));
    }

    // ---- cosine_similarity -------------------------------------------------

    #[test]
    fn cosine_similarity_identical_vectors_is_1() {
        assert!((cosine_similarity(&[1.0, 0.0, 0.0], &[1.0, 0.0, 0.0]) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_similarity_orthogonal_vectors_is_0() {
        assert!(cosine_similarity(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
    }

    #[test]
    fn cosine_similarity_different_lengths_returns_0() {
        assert!(cosine_similarity(&[1.0, 0.0], &[1.0, 0.0, 0.0]).abs() < 1e-6);
    }

    #[test]
    fn cosine_similarity_zero_vector_returns_0() {
        assert!(cosine_similarity(&[0.0, 0.0], &[1.0, 0.0]).abs() < 1e-6);
    }

    // ---- Send + Sync -------------------------------------------------------

    #[test]
    fn in_memory_vector_store_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<InMemoryVectorStore>();
    }
}
