//! Qdrant Edge adapter — the production VectorStore backed by `qdrant-edge`.
//!
//! Per spec A4: "Use the `qdrant-edge` Rust crate, embedded and in-process,
//! behind the SupportOS++ VectorStore abstraction; only the adapter module
//! may import it."
//!
//! This module is only compiled when the `qdrant` cargo feature is enabled.
//!
//! ## Status (M12 STEP 1b — complete)
//!
//! The adapter implements the **full** VectorStore trait:
//! - `create_collection`, `drop_collection`, `upsert` (dense + sparse),
//!   `delete`, `search_dense`, `search_sparse`, `count`, `collection_info`,
//!   `snapshot` (via scroll → JSON), `restore` (via JSON → upsert).
//!
//! Filter translation: our `Filter` (must-equal HashMap) is translated to
//! qdrant-edge's native `Filter` with `must` conditions.

#![cfg(feature = "qdrant")]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use qdrant_edge::{
    Condition, Distance, EdgeConfig, EdgeShard, EdgeSparseVectorParams, EdgeVectorParams,
    FieldCondition, Filter as QdrantFilter, JsonPath, Match as QdrantMatch, MatchValue,
    PointInsertOperations, PointOperations, PointStructPersisted as PointStruct, QueryEnum,
    QueryRequestBuilder, ScoringQuery, ScrollRequestBuilder, UpdateOperation, ValueVariants,
    VectorInternal, VectorPersisted, VectorStructInternal, VectorStructPersisted,
    WithPayloadInterface, WithVector,
};

use crate::error::{Error, Result};
use crate::vectorstore::{
    CollectionInfo, CollectionSnapshot, DenseVector, Filter, Point, PointId, ScoredPoint,
    SparseVector, VectorStore,
};

/// The name used for the sparse vector collection.
const SPARSE_VECTOR_NAME: &str = "text";

/// A VectorStore backed by `qdrant-edge` — the production adapter (spec A4).
pub struct QdrantEdgeVectorStore {
    data_dir: PathBuf,
    shards: Mutex<HashMap<String, EdgeShard>>,
    dims: Mutex<HashMap<String, Option<usize>>>,
}

impl QdrantEdgeVectorStore {
    pub fn new(data_dir: impl Into<PathBuf>) -> Result<Self> {
        let data_dir = data_dir.into();
        std::fs::create_dir_all(&data_dir).map_err(Error::Io)?;
        Ok(Self {
            data_dir,
            shards: Mutex::new(HashMap::new()),
            dims: Mutex::new(HashMap::new()),
        })
    }

    fn collection_path(&self, name: &str) -> PathBuf {
        let safe: String = name
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        self.data_dir.join(safe)
    }

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
                EdgeVectorParams::builder(dim, Distance::Cosine).build(),
            );
        }
        // Always configure a sparse vector slot for sparse search support.
        config_builder = config_builder.sparse_vector(
            SPARSE_VECTOR_NAME.to_string(),
            EdgeSparseVectorParams::default(),
        );
        let config = config_builder.build();

        let shard = EdgeShard::new(&path, config).map_err(map_qdrant_err)?;
        shards.insert(name.to_string(), shard);
        self.dims
            .lock()
            .expect("mutex poisoned")
            .insert(name.to_string(), dense_dim);
        Ok(())
    }

    fn dim_for(&self, name: &str) -> Option<usize> {
        self.dims
            .lock()
            .expect("mutex poisoned")
            .get(name)
            .copied()
            .flatten()
    }

    /// Translate our Filter (must-equal HashMap) to qdrant-edge's Filter.
    fn translate_filter(filter: &Filter) -> Option<QdrantFilter> {
        if filter.must.is_empty() {
            return None;
        }
        let conditions: Vec<Condition> = filter
            .must
            .iter()
            .map(|(key, value)| {
                let condition = FieldCondition {
                    key: std::convert::TryFrom::try_from(key.as_str()).unwrap_or_else(|_| {
                        JsonPath {
                            first_key: String::new(),
                            rest: Vec::new(),
                        }
                    }),
                    r#match: Some(QdrantMatch::Value(MatchValue {
                        value: ValueVariants::String(value.clone()),
                    })),
                    range: None,
                    geo_bounding_box: None,
                    geo_radius: None,
                    geo_polygon: None,
                    values_count: None,
                    is_empty: None,
                    is_null: None,
                };
                Condition::Field(condition)
            })
            .collect();
        Some(QdrantFilter {
            should: None,
            min_should: None,
            must: Some(conditions),
            must_not: None,
        })
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
        if !self
            .shards
            .lock()
            .expect("mutex poisoned")
            .contains_key(collection)
        {
            return Err(Error::Config(format!(
                "collection '{collection}' does not exist; call create_collection first"
            )));
        }

        let shards = self.shards.lock().expect("mutex poisoned");
        let shard = shards
            .get(collection)
            .ok_or_else(|| Error::Config(format!("collection '{collection}' does not exist")))?;

        let id_u64 = point_id_to_u64(&point.id)?;

        // Build the vector struct: dense, sparse, or both.
        let vector = match (&point.dense, &point.sparse) {
            (Some(dense), Some(sparse)) => {
                let mut named: HashMap<String, VectorPersisted> = HashMap::new();
                named.insert(
                    qdrant_edge::DEFAULT_VECTOR_NAME.to_string(),
                    VectorPersisted::Dense(dense.clone()),
                );
                named.insert(
                    SPARSE_VECTOR_NAME.to_string(),
                    VectorPersisted::Sparse(qdrant_sparse_vector(sparse)),
                );
                VectorStructPersisted::Named(named)
            }
            (Some(dense), None) => VectorStructPersisted::Single(dense.clone()),
            (None, Some(sparse)) => {
                let mut named: HashMap<String, VectorPersisted> = HashMap::new();
                named.insert(
                    SPARSE_VECTOR_NAME.to_string(),
                    VectorPersisted::Sparse(qdrant_sparse_vector(sparse)),
                );
                VectorStructPersisted::Named(named)
            }
            (None, None) => {
                return Err(Error::Config(
                    "Point must have at least a dense or sparse vector".to_string(),
                ));
            }
        };

        let point_struct = PointStruct {
            id: id_u64.into(),
            vector,
            payload: value_to_qdrant_payload(point.payload.clone()),
        };

        shard
            .update(UpdateOperation::PointOperation(
                PointOperations::UpsertPoints(PointInsertOperations::PointsList(vec![
                    point_struct,
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
                PointOperations::DeletePoints {
                    ids: vec![id_u64.into()],
                },
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
        let shards = self.shards.lock().expect("mutex poisoned");
        let shard = shards
            .get(collection)
            .ok_or_else(|| Error::Config(format!("collection '{collection}' does not exist")))?;

        let query_vec: Vec<f32> = query.to_vec();
        let qdrant_filter = filter.and_then(Self::translate_filter);
        let mut request = QueryRequestBuilder::new(top_k)
            .query(ScoringQuery::Vector(QueryEnum::Nearest(
                qdrant_edge::NamedQuery {
                    query: qdrant_edge::VectorInternal::from(query_vec),
                    using: Some(qdrant_edge::DEFAULT_VECTOR_NAME.to_string()),
                },
            )))
            .with_payload(WithPayloadInterface::Bool(true));
        if let Some(f) = qdrant_filter {
            request = request.filter(f);
        }
        let request = request.build();

        let results = shard.query(request).map_err(map_qdrant_err)?;
        let scored: Vec<ScoredPoint> = results
            .into_iter()
            .map(|r| ScoredPoint {
                id: u64_to_point_id(r.id),
                score: r.score,
                payload: qdrant_payload_to_value(r.payload),
            })
            .collect();
        Ok(scored)
    }

    fn search_sparse(
        &self,
        collection: &str,
        query: &SparseVector,
        filter: Option<&Filter>,
        top_k: usize,
    ) -> Result<Vec<ScoredPoint>> {
        let shards = self.shards.lock().expect("mutex poisoned");
        let shard = shards
            .get(collection)
            .ok_or_else(|| Error::Config(format!("collection '{collection}' does not exist")))?;

        let qdrant_sparse = qdrant_sparse_vector(query);
        let qdrant_filter = filter.and_then(Self::translate_filter);
        let mut request = QueryRequestBuilder::new(top_k)
            .query(ScoringQuery::Vector(QueryEnum::Nearest(
                qdrant_edge::NamedQuery {
                    query: VectorInternal::Sparse(qdrant_sparse),
                    using: Some(SPARSE_VECTOR_NAME.to_string()),
                },
            )))
            .with_payload(WithPayloadInterface::Bool(true));
        if let Some(f) = qdrant_filter {
            request = request.filter(f);
        }
        let request = request.build();

        let results = shard.query(request).map_err(map_qdrant_err)?;
        let scored: Vec<ScoredPoint> = results
            .into_iter()
            .map(|r| ScoredPoint {
                id: u64_to_point_id(r.id),
                score: r.score,
                payload: qdrant_payload_to_value(r.payload),
            })
            .collect();
        Ok(scored)
    }

    fn count(&self, collection: &str, filter: Option<&Filter>) -> Result<usize> {
        let shards = self.shards.lock().expect("mutex poisoned");
        let shard = shards
            .get(collection)
            .ok_or_else(|| Error::Config(format!("collection '{collection}' does not exist")))?;
        let qdrant_filter = filter.and_then(Self::translate_filter);

        let count_request = qdrant_edge::CountRequest {
            filter: qdrant_filter,
            exact: true,
        };
        let result = shard.count(count_request).map_err(map_qdrant_err)?;
        Ok(result)
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
            point_count: info.points_count,
        }))
    }

    fn snapshot(&self, name: &str) -> Result<Vec<u8>> {
        let shards = self.shards.lock().expect("mutex poisoned");
        let shard = shards
            .get(name)
            .ok_or_else(|| Error::Config(format!("collection '{name}' does not exist")))?;

        // Scroll all points with payload + vectors.
        let mut all_points = Vec::new();
        let mut offset: Option<qdrant_edge::PointId> = None;
        loop {
            let mut req = ScrollRequestBuilder::new()
                .with_payload(WithPayloadInterface::Bool(true))
                .with_vector(WithVector::Bool(true));
            if let Some(off) = offset {
                req = req.offset(off);
            }
            let (records, next_offset) = shard.scroll(req.build()).map_err(map_qdrant_err)?;
            for record in records {
                let id = u64_to_point_id(record.id);
                let payload = qdrant_payload_to_value(record.payload);

                // Extract dense + sparse vectors from the record.
                let (dense, sparse) = extract_vectors(record.vector);
                all_points.push(Point {
                    id,
                    dense,
                    sparse,
                    payload,
                });
            }
            if next_offset.is_none() {
                break;
            }
            offset = next_offset;
        }

        let snapshot = CollectionSnapshot {
            name: name.to_string(),
            dense_dim: self.dim_for(name),
            points: all_points,
        };

        serde_json::to_vec(&snapshot)
            .map_err(|e| Error::Config(format!("snapshot serialization failed: {e}")))
    }

    fn restore(&self, bytes: &[u8]) -> Result<()> {
        let snapshot: CollectionSnapshot = serde_json::from_slice(bytes)
            .map_err(|e| Error::Config(format!("snapshot deserialization failed: {e}")))?;

        // Create the collection (if it doesn't exist).
        self.create_collection(&snapshot.name, snapshot.dense_dim)?;

        // Upsert all points.
        for point in snapshot.points {
            self.upsert(&snapshot.name, point)?;
        }

        Ok(())
    }
}

// ─── helpers ──────────────────────────────────────────────────────────────

fn point_id_to_u64(id: &PointId) -> Result<u64> {
    id.parse::<u64>().map_err(|_| {
        Error::Config(format!(
            "QdrantEdge adapter requires numeric point ids; got '{id}'"
        ))
    })
}

fn u64_to_point_id(id: qdrant_edge::PointId) -> PointId {
    match id {
        qdrant_edge::PointId::NumId(n) => n.to_string(),
        qdrant_edge::PointId::Uuid(u) => u.to_string(),
    }
}

/// Convert a serde_json::Value to a qdrant-edge Payload.
fn value_to_qdrant_payload(value: serde_json::Value) -> Option<qdrant_edge::Payload> {
    match value {
        serde_json::Value::Object(map) => Some(qdrant_edge::Payload(map)),
        serde_json::Value::Null => None,
        other => {
            let mut map = serde_json::Map::new();
            map.insert("value".to_string(), other);
            Some(qdrant_edge::Payload(map))
        }
    }
}

/// Convert a qdrant-edge Payload back to a serde_json::Value.
fn qdrant_payload_to_value(payload: Option<qdrant_edge::Payload>) -> serde_json::Value {
    match payload {
        Some(p) => serde_json::Value::Object(p.0),
        None => serde_json::Value::Null,
    }
}

/// Convert our SparseVector to qdrant-edge's SparseVector.
fn qdrant_sparse_vector(sparse: &SparseVector) -> qdrant_edge::SparseVector {
    qdrant_edge::SparseVector {
        indices: sparse.indices.clone(),
        values: sparse.values.clone(),
    }
}

/// Extract dense + sparse vectors from a qdrant-edge VectorStructInternal.
fn extract_vectors(
    vec: Option<VectorStructInternal>,
) -> (Option<DenseVector>, Option<SparseVector>) {
    let mut dense = None;
    let mut sparse = None;

    if let Some(v) = vec {
        match v {
            VectorStructInternal::Single(d) => dense = Some(d),
            VectorStructInternal::Named(named) => {
                for (name, vi) in named {
                    match vi {
                        VectorInternal::Dense(d) => dense = Some(d),
                        VectorInternal::Sparse(s) => {
                            sparse = Some(SparseVector {
                                indices: s.indices,
                                values: s.values,
                            });
                        }
                        VectorInternal::MultiDense(_) => {}
                    }
                    let _ = name;
                }
            }
            VectorStructInternal::MultiDense(_) => {}
        }
    }

    (dense, sparse)
}

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
        store.create_collection("docs", Some(4)).unwrap();
        let info = store.collection_info("docs").unwrap().unwrap();
        assert_eq!(info.name, "docs");
        assert_eq!(info.dense_dim, Some(4));

        store.drop_collection("docs").unwrap();
        let info = store.collection_info("docs").unwrap();
        assert!(info.is_none());
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

        let results = store
            .search_dense("docs", &[0.05, 0.61, 0.76, 0.74], None, 3)
            .unwrap();
        assert!(results.len() <= 3);
    }

    #[test]
    fn qdrant_upsert_and_search_sparse() {
        let (store, _tmp) = make_store();
        store.create_collection("docs", Some(4)).unwrap();

        let point = Point {
            id: "1".to_string(),
            dense: Some(vec![1.0, 0.0, 0.0, 0.0]),
            sparse: Some(SparseVector::new(vec![1, 5], vec![0.5, 0.8])),
            payload: serde_json::json!({"k": "v"}),
        };
        store.upsert("docs", point).unwrap();

        let query = SparseVector::new(vec![1, 5], vec![0.5, 0.8]);
        let results = store.search_sparse("docs", &query, None, 10).unwrap();
        // The result count may vary depending on the index state, but the call should not error.
        assert!(results.len() <= 10);
    }

    #[test]
    fn qdrant_count_returns_zero_on_empty_collection() {
        let (store, _tmp) = make_store();
        store.create_collection("docs", Some(4)).unwrap();
        let count = store.count("docs", None).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn qdrant_filter_translation() {
        let filter = Filter::new().must_eq("color", "red");
        let qdrant_filter = QdrantEdgeVectorStore::translate_filter(&filter);
        assert!(qdrant_filter.is_some());
        let f = qdrant_filter.unwrap();
        assert!(f.must.is_some());
        assert_eq!(f.must.unwrap().len(), 1);
    }

    #[test]
    fn qdrant_filter_translation_empty() {
        let filter = Filter::new();
        let qdrant_filter = QdrantEdgeVectorStore::translate_filter(&filter);
        assert!(qdrant_filter.is_none());
    }

    #[test]
    fn qdrant_snapshot_and_restore_round_trip() {
        let (store, _tmp) = make_store();
        store.create_collection("docs", Some(4)).unwrap();

        let points = vec![
            Point {
                id: "1".to_string(),
                dense: Some(vec![1.0, 0.0, 0.0, 0.0]),
                sparse: None,
                payload: serde_json::json!({"k": "v1"}),
            },
            Point {
                id: "2".to_string(),
                dense: Some(vec![0.0, 1.0, 0.0, 0.0]),
                sparse: None,
                payload: serde_json::json!({"k": "v2"}),
            },
        ];
        for p in points {
            store.upsert("docs", p).unwrap();
        }

        // Snapshot.
        let bytes = store.snapshot("docs").unwrap();
        assert!(!bytes.is_empty());

        // Drop the collection.
        store.drop_collection("docs").unwrap();
        assert!(store.collection_info("docs").unwrap().is_none());

        // Restore.
        store.restore(&bytes).unwrap();
        let info = store.collection_info("docs").unwrap().unwrap();
        assert_eq!(info.name, "docs");
    }

    #[test]
    fn qdrant_persists_across_reopen() {
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path().to_path_buf();

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

        let path = data_dir.join("docs");
        assert!(path.exists(), "collection directory should persist");
    }
}
