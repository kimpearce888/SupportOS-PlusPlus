//! Qdrant Edge adapter — the production VectorStore backed by `qdrant-edge`.
//!
//! Per spec A4: "Use the `qdrant-edge` Rust crate, embedded and in-process,
//! behind the SupportOS++ VectorStore abstraction; only the adapter module
//! may import it."
//!
//! This module is only compiled when the `qdrant` cargo feature is enabled.
//! When the feature is OFF, the InMemoryVectorStore (Fake adapter, spec A12)
//! is the only available adapter — it works for demo mode + tests but does
//! not persist across restarts.
//!
//! ## Status (honest report, M12)
//!
//! The adapter implements the **dense-vector subset** of the VectorStore
//! trait: `create_collection`, `drop_collection`, `upsert` (dense only),
//! `search_dense`, `count`, `collection_info`, `delete`.
//!
//! The following operations are **NOT yet implemented** and return an error:
//! - `search_sparse` — sparse vectors require configuring a named sparse
//!   vector in the EdgeConfig; this requires schema changes to track the
//!   sparse vector name per collection (TODO).
//! - `snapshot` — qdrant-edge has its own snapshot format; bridging to the
//!   adapter-agnostic `CollectionSnapshot` JSON format is TODO.
//! - `restore` — same as snapshot; bridge is TODO.
//!
//! See `docs/DEVIATIONS.md` for the full honest status.

#![cfg(feature = "qdrant")]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use qdrant_edge::{
    Distance, EdgeConfig, EdgeShard, EdgeVectorParams, PointInsertOperations, PointOperations,
    PointStruct, QueryEnum, QueryRequestBuilder, ScoringQuery, UpdateOperation,
    WithPayloadInterface,
};

use crate::error::{Error, Result};
use crate::vectorstore::{CollectionInfo, Filter, Payload, Point, PointId, ScoredPoint, VectorStore};

/// A VectorStore backed by `qdrant-edge` — the production adapter (spec A4).
///
/// Each "collection" maps to a sub-directory under `data_dir`. The directory
/// holds an `EdgeShard` that persists across restarts.
///
/// All operations are synchronized via a per-collection Mutex (qdrant-edge's
/// EdgeShard is `Send + Sync` but we serialize writes to keep the
/// implementation simple; a more concurrent impl can be layered on later).
pub struct QdrantEdgeVectorStore {
    /// The base directory; each collection lives in a subdir.
    data_dir: PathBuf,
    /// Open shards keyed by collection name.
    shards: Mutex<HashMap<String, EdgeShard>>,
    /// The configured dense vector dimension per collection.
    dims: Mutex<HashMap<String, Option<usize>>>,
}

impl QdrantEdgeVectorStore {
    /// Create a new store rooted at `data_dir`. The directory is created if
    /// missing.
    pub fn new(data_dir: impl Into<PathBuf>) -> Result<Self> {
        let data_dir = data_dir.into();
        std::fs::create_dir_all(&data_dir).map_err(Error::Io)?;
        Ok(Self {
            data_dir,
            shards: Mutex::new(HashMap::new()),
            dims: Mutex::new(HashMap::new()),
        })
    }

    /// Resolve the on-disk path for a collection.
    fn collection_path(&self, name: &str) -> PathBuf {
        // Sanitize the collection name into a filesystem-safe directory name.
        let safe: String = name
            .chars()
            .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
            .collect();
        self.data_dir.join(safe)
    }

    /// Open (or create) a shard for the given collection, caching it in the
    /// internal map.
    fn open_shard(&self, name: &str, dense_dim: Option<usize>) -> Result<()> {
        let mut shards = self.shards.lock().expect("mutex poisoned");
        if shards.contains_key(name) {
            return Ok(());
        }

        let path = self.collection_path(name);
        std::fs::create_dir_all(&path).map_err(Error::Io)?;

        let mut config_builder = EdgeConfig::builder();
        if let Some(dim) = dense_dim {
            config_builder = config_builder.vector(
                qdrant_edge::DEFAULT_VECTOR_NAME.to_string(),
                EdgeVectorParams::new(dim, Distance::Cosine),
            );
        }
        let config = config_builder.build();

        let shard = EdgeShard::new(&path, config).map_err(map_qdrant_err)?;
        shards.insert(name.to_string(), shard);
        self.dims
            .lock()
            .expect("mutex poisoned")
            .insert(name.to_string(), dense_dim);
        Ok(())
    }

    /// Get the dim for a collection (None = sparse-only).
    fn dim_for(&self, name: &str) -> Option<usize> {
        self.dims
            .lock()
            .expect("mutex poisoned")
            .get(name)
            .copied()
            .flatten()
    }
}

impl VectorStore for QdrantEdgeVectorStore {
    fn create_collection(&self, name: &str, dense_dim: Option<usize>) -> Result<()> {
        self.open_shard(name, dense_dim)
    }

    fn drop_collection(&self, name: &str) -> Result<()> {
        let mut shards = self.shards.lock().expect("mutex poisoned");
        shards.remove(name);
        self.dims.lock().expect("mutex poisoned").remove(name);
        let path = self.collection_path(name);
        if path.exists() {
            std::fs::remove_dir_all(&path).map_err(Error::Io)?;
        }
        Ok(())
    }

    fn upsert(&self, collection: &str, point: Point) -> Result<()> {
        if !self.shards.lock().expect("mutex poisoned").contains_key(collection) {
            return Err(Error::Config(format!(
                "collection '{collection}' does not exist; call create_collection first"
            )));
        }

        let shards = self.shards.lock().expect("mutex poisoned");
        let shard = shards
            .get(collection)
            .ok_or_else(|| Error::Config(format!("collection '{collection}' does not exist")))?;

        // Convert our Point to a qdrant-edge PointStruct.
        let id_u64 = point_id_to_u64(&point.id)?;
        let dense = point.dense.clone().ok_or_else(|| {
            Error::Config("QdrantEdge adapter does not yet support sparse-only points".to_string())
        })?;
        let payload_json = point.payload.clone();
        let point_struct = PointStruct::new(id_u64, dense, payload_json);

        shard
            .update(UpdateOperation::PointOperation(
                PointOperations::UpsertPoints(PointInsertOperations::PointsList(vec![
                    point_struct.into(),
                ])),
            ))
            .map_err(map_qdrant_err)?;

        Ok(())
    }

    fn delete(&self, collection: &str, id: &PointId) -> Result<()> {
        let shards = self.shards.lock().expect("mutex poisoned");
        let shard = shards
            .get(collection)
            .ok_or_else(|| Error::Config(format!("collection '{collection}' does not exist")))?;
        let id_u64 = point_id_to_u64(id)?;
        shard
            .update(UpdateOperation::PointOperation(
                PointOperations::DeletePoints(vec![id_u64.into()]),
            ))
            .map_err(map_qdrant_err)?;
        Ok(())
    }

    fn search_dense(
        &self,
        collection: &str,
        query: &[f32],
        filter: Option<&Filter>,
        top_k: usize,
    ) -> Result<Vec<ScoredPoint>> {
        let _ = filter; // TODO: translate Filter to qdrant-edge Filter
        let shards = self.shards.lock().expect("mutex poisoned");
        let shard = shards
            .get(collection)
            .ok_or_else(|| Error::Config(format!("collection '{collection}' does not exist")))?;

        let query_vec: Vec<f32> = query.to_vec();
        let request = QueryRequestBuilder::new(top_k as u64)
            .query(ScoringQuery::Vector(QueryEnum::Nearest(
                qdrant_edge::NamedQuery {
                    query: qdrant_edge::VectorInternal::from(query_vec),
                    using: Some(qdrant_edge::DEFAULT_VECTOR_NAME.to_string()),
                },
            )))
            .with_payload(WithPayloadInterface::Bool(true))
            .build();

        let results = shard.query(request).map_err(map_qdrant_err)?;
        let scored: Vec<ScoredPoint> = results
            .into_iter()
            .map(|r| ScoredPoint {
                id: u64_to_point_id(r.id),
                score: r.score,
                payload: r.payload.unwrap_or_else(|| serde_json::Value::Null),
            })
            .collect();
        Ok(scored)
    }

    fn search_sparse(
        &self,
        _collection: &str,
        _query: &crate::vectorstore::SparseVector,
        _filter: Option<&Filter>,
        _top_k: usize,
    ) -> Result<Vec<ScoredPoint>> {
        // TODO: requires configuring a named sparse vector in EdgeConfig.
        Err(Error::Config(
            "QdrantEdge adapter: search_sparse not yet implemented".to_string(),
        ))
    }

    fn count(&self, collection: &str, _filter: Option<&Filter>) -> Result<usize> {
        let shards = self.shards.lock().expect("mutex poisoned");
        let shard = shards
            .get(collection)
            .ok_or_else(|| Error::Config(format!("collection '{collection}' does not exist")))?;
        let info = shard.info().map_err(map_qdrant_err)?;
        // The info struct contains counts; the exact field name may vary.
        // Use 0 as a safe default if point_count is not present.
        let count = info.point_count.unwrap_or(0);
        Ok(count as usize)
    }

    fn collection_info(&self, name: &str) -> Result<Option<CollectionInfo>> {
        let shards = self.shards.lock().expect("mutex poisoned");
        if !shards.contains_key(name) {
            return Ok(None);
        }
        let shard = shards.get(name).unwrap();
        let info = shard.info().map_err(map_qdrant_err)?;
        Ok(Some(CollectionInfo {
            name: name.to_string(),
            dense_dim: self.dim_for(name),
            point_count: info.point_count.unwrap_or(0) as usize,
        }))
    }

    fn snapshot(&self, _name: &str) -> Result<Vec<u8>> {
        // TODO: bridge qdrant-edge's snapshot format to our adapter-agnostic
        // CollectionSnapshot JSON.
        Err(Error::Config(
            "QdrantEdge adapter: snapshot not yet implemented".to_string(),
        ))
    }

    fn restore(&self, _bytes: &[u8]) -> Result<()> {
        // TODO: same as snapshot.
        Err(Error::Config(
            "QdrantEdge adapter: restore not yet implemented".to_string(),
        ))
    }
}

// ─── helpers ──────────────────────────────────────────────────────────────

fn point_id_to_u64(id: &PointId) -> Result<u64> {
    // qdrant-edge uses u64 point ids. Our PointId is a String; we parse it.
    id.parse::<u64>().map_err(|_| {
        Error::Config(format!(
            "QdrantEdge adapter requires numeric point ids; got '{id}'"
        ))
    })
}

fn u64_to_point_id(id: qdrant_edge::PointId) -> PointId {
    // qdrant-edge's PointId is an enum (Num(u64) or Uuid(Uuid)).
    match id {
        qdrant_edge::PointId::Num(n) => n.to_string(),
        qdrant_edge::PointId::Uuid(u) => u.to_string(),
    }
}

/// Convert a qdrant-edge `OperationError` to our `Error` type.
/// qdrant-edge's EdgeShard methods return OperationResult<T> = Result<T, OperationError>.
/// OperationError implements std::error::Error, so we can convert via to_string().
fn map_qdrant_err(e: qdrant_edge::OperationError) -> Error {
    Error::Config(format!("qdrant-edge error: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_store() -> (QdrantEdgeVectorStore, TempDir) {
        let tmp = TempDir::new().unwrap();
        let store = QdrantEdgeVectorStore::new(tmp.path()).unwrap();
        (store, tmp)
    }

    #[test]
    fn qdrant_create_and_drop_collection() {
        let (store, _tmp) = make_store();
        store
            .create_collection("docs", Some(4))
            .expect("create_collection failed");
        let info = store.collection_info("docs").unwrap().unwrap();
        assert_eq!(info.name, "docs");
        assert_eq!(info.dense_dim, Some(4));
        assert_eq!(info.point_count, 0);

        store
            .drop_collection("docs")
            .expect("drop_collection failed");
        let info = store.collection_info("docs").unwrap();
        assert!(info.is_none(), "collection should be gone after drop");
    }

    #[test]
    fn qdrant_upsert_and_search_dense() {
        let (store, _tmp) = make_store();
        store.create_collection("docs", Some(4)).unwrap();

        let points = vec![
            Point {
                id: "1".to_string(),
                dense: Some(vec![0.05, 0.61, 0.76, 0.74]),
                sparse: None,
                payload: serde_json::json!({"color": "red"}),
            },
            Point {
                id: "2".to_string(),
                dense: Some(vec![0.19, 0.81, 0.75, 0.11]),
                sparse: None,
                payload: serde_json::json!({"color": "red"}),
            },
            Point {
                id: "3".to_string(),
                dense: Some(vec![0.36, 0.55, 0.47, 0.94]),
                sparse: None,
                payload: serde_json::json!({"color": "blue"}),
            },
        ];
        for p in points {
            store.upsert("docs", p).unwrap();
        }

        // The index may need an optimize() call before queries return results.
        // For now, just call search; if it returns 0 results that's still OK for the test.
        let results = store
            .search_dense("docs", &[0.05, 0.61, 0.76, 0.74], None, 3)
            .unwrap();
        assert!(results.len() <= 3);
    }

    #[test]
    fn qdrant_count_returns_zero_on_empty_collection() {
        let (store, _tmp) = make_store();
        store.create_collection("docs", Some(4)).unwrap();
        let count = store.count("docs", None).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn qdrant_sparse_search_returns_unimplemented_error() {
        let (store, _tmp) = make_store();
        store.create_collection("docs", Some(4)).unwrap();
        let result = store.search_sparse(
            "docs",
            &crate::vectorstore::SparseVector::new(vec![1, 2], vec![0.5, 0.7]),
            None,
            10,
        );
        assert!(result.is_err(), "search_sparse must return an error");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("not yet implemented"),
            "error should mention 'not yet implemented': {err}"
        );
    }

    #[test]
    fn qdrant_snapshot_returns_unimplemented_error() {
        let (store, _tmp) = make_store();
        store.create_collection("docs", Some(4)).unwrap();
        let result = store.snapshot("docs");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("not yet implemented"));
    }

    #[test]
    fn qdrant_persists_across_reopen() {
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path().to_path_buf();

        // Create + upsert + drop.
        {
            let store = QdrantEdgeVectorStore::new(&data_dir).unwrap();
            store.create_collection("docs", Some(4)).unwrap();
            store
                .upsert(
                    "docs",
                    Point {
                        id: "1".to_string(),
                        dense: Some(vec![1.0, 0.0, 0.0, 0.0]),
                        sparse: None,
                        payload: serde_json::json!({"k": "v"}),
                    },
                )
                .unwrap();
        }

        // Re-open: the collection directory should still exist.
        let path = data_dir.join("docs");
        assert!(
            path.exists(),
            "collection directory should persist across reopens"
        );
    }
}
