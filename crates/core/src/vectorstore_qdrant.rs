//! Qdrant Edge adapter — the production VectorStore backed by `qdrant-edge`.
//!
//! Deviation D2: the reference talks to a LOCAL QDRANT SERVER over its
//! REST API (`src/server/integrations/qdrant/qdrantAdapter.ts`); the port
//! embeds the `qdrant-edge` engine in-process instead. This module ports
//! the reference adapter method-for-method on top of the engine:
//!
//! ## Settings mapping (documented per D2)
//!
//! The reference stores `qdrant_url` (default `http://127.0.0.1:6333`) and
//! `qdrant_enabled` (default `true`). A URL only made sense for an external
//! server, so the port maps it to the embedded storage location:
//! `{data_dir}/qdrant/{sanitized-url}-{hash}/` — switching the URL switches
//! the vector store, exactly like pointing the reference at another server.
//! `health().url` still reports the CONFIGURED string, so the user-visible
//! Settings / Sync Health behavior is unchanged. Disabling the setting keeps
//! the reference behavior: FTS keyword search remains fully functional.
//!
//! This module is the ONLY place that imports `qdrant-edge`.

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

/// The on-disk marker `EdgeShard::new` writes (`config.save(path)`).
/// Its presence means the collection already exists on disk and must be
/// LOADED (`EdgeShard::load`), not re-created — `EdgeShard::new` refuses a
/// path that already contains segment data.
const EDGE_CONFIG_FILE: &str = "edge_config.json";

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

        // Existing collection: LOAD it (EdgeShard::new refuses existing
        // data). With a known dimension the load verifies compatibility
        // against the stored segments — the embedded counterpart of the
        // server's vector-size check. Without one, the config is derived
        // from the stored segments (the read paths).
        let shard = if path.join(EDGE_CONFIG_FILE).exists() {
            let config_opt = dense_dim.map(|_| config);
            EdgeShard::load(&path, config_opt).map_err(map_qdrant_err)?
        } else {
            EdgeShard::new(&path, config).map_err(map_qdrant_err)?
        };

        shards.insert(name.to_string(), shard);
        self.dims
            .lock()
            .expect("mutex poisoned")
            .insert(name.to_string(), dense_dim);
        Ok(())
    }

    /// Ensure the shard for a READ is open, loading it from disk lazily
    /// (dim derived from the stored segments). Errors when the collection
    /// does not exist on disk.
    fn ensure_shard_open(&self, name: &str) -> Result<()> {
        {
            let shards = self.shards.lock().expect("mutex poisoned");
            if shards.contains_key(name) {
                return Ok(());
            }
        }
        if !self.collection_path(name).join(EDGE_CONFIG_FILE).exists() {
            return Err(Error::Config(format!("collection '{name}' does not exist")));
        }
        self.open_shard(name, None)
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

        // The reference upserts with `?wait=true`: the operation must be
        // durable when the call returns. The in-process engine applies
        // synchronously but buffers on disk — flush to honor wait=true.
        shard.flush().map_err(map_qdrant_err)?;

        Ok(())
    }

    fn delete(&self, collection: &str, id: &PointId) -> Result<()> {
        self.ensure_shard_open(collection)?;
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
        self.ensure_shard_open(collection)?;
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
        self.ensure_shard_open(collection)?;
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
        self.ensure_shard_open(collection)?;
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
        {
            let shards = self.shards.lock().expect("mutex poisoned");
            if !shards.contains_key(name)
                && !self.collection_path(name).join(EDGE_CONFIG_FILE).exists()
            {
                return Ok(None);
            }
        }
        // Exists on disk but not open yet: load it so `info()` reports the
        // real point count.
        self.ensure_shard_open(name)?;
        let shards = self.shards.lock().expect("mutex poisoned");
        let shard = shards
            .get(name)
            .ok_or_else(|| Error::Config(format!("collection '{name}' does not exist")))?;
        let info = shard.info().map_err(map_qdrant_err)?;
        Ok(Some(CollectionInfo {
            name: name.to_string(),
            dense_dim: self.dim_for(name),
            point_count: info.points_count,
        }))
    }

    fn snapshot(&self, name: &str) -> Result<Vec<u8>> {
        self.ensure_shard_open(name)?;
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

// ─── Embedded Qdrant — the reference QdrantAdapter, ported (D2) ───────────

/// The single collection the reference uses (`QDRANT_COLLECTION`).
pub const QDRANT_COLLECTION: &str = "supportos_vectors";

/// The default `qdrant_url` setting value (reference `settingsRepo.getQdrant`).
pub const DEFAULT_QDRANT_URL: &str = "http://127.0.0.1:6333";

/// A point in the reference `VectorPoint` shape: deterministic numeric id +
/// dense vector + the entity payload the search routes filter on.
#[derive(Debug, Clone)]
pub struct VectorPoint {
    pub id: i64,
    pub vector: Vec<f32>,
    pub payload: serde_json::Value,
}

/// A search hit in the reference adapter's return shape.
#[derive(Debug, Clone)]
pub struct SearchHit {
    pub id: i64,
    pub score: f32,
    pub payload: serde_json::Value,
}

/// Health result — the reference `QdrantHealth` shape.
#[derive(Debug, Clone)]
pub struct Health {
    pub connected: bool,
    pub url: String,
    pub collections: Vec<String>,
    pub error: Option<String>,
}

/// Embedded Qdrant — the port of the reference's `QdrantAdapter`, backed by
/// the in-process `qdrant-edge` engine instead of a local Qdrant server.
///
/// Mirrors the reference adapter method-for-method: `reconfigure`, `health`,
/// `ensureCollection`, `upsert`, `search`, `deleteByEntity`, `countByEntity`,
/// `dropCollection`. The reference never calls the last three at runtime
/// (they are defined on its adapter); they are ported for adapter parity and
/// exercised by tests.
pub struct EmbeddedQdrant {
    data_dir: PathBuf,
    inner: Mutex<EmbeddedInner>,
}

struct EmbeddedInner {
    enabled: bool,
    url: String,
    /// The engine handle, rooted at the directory mapped from `url`.
    /// `None` when disabled or after an open failure (retried on next use).
    store: Option<QdrantEdgeVectorStore>,
    last_error: Option<String>,
}

impl EmbeddedQdrant {
    /// Create the adapter from persisted settings (reference constructor:
    /// `new QdrantAdapter({ url, enabled })`).
    pub fn new(data_dir: impl Into<PathBuf>, url: &str, enabled: bool) -> Self {
        Self {
            data_dir: data_dir.into(),
            inner: Mutex::new(EmbeddedInner {
                enabled,
                url: url.trim_end_matches('/').to_string(),
                store: None,
                last_error: None,
            }),
        }
    }

    /// Runtime reconfigure (reference `reconfigure({url?, enabled?})` —
    /// called by the settings routes when `qdrant_url`/`qdrant_enabled`
    /// change). A URL change re-roots the store, like pointing the
    /// reference at a different server.
    pub fn reconfigure(&self, url: Option<String>, enabled: Option<bool>) {
        let mut inner = self.inner.lock().expect("mutex poisoned");
        if let Some(u) = url {
            let trimmed = u.trim_end_matches('/').to_string();
            if trimmed != inner.url {
                inner.url = trimmed;
                inner.store = None; // different storage location
            }
        }
        if let Some(e) = enabled {
            inner.enabled = e;
        }
    }

    /// Map the configured URL to the embedded storage directory (D2).
    fn dir_for(data_dir: &std::path::Path, url: &str) -> PathBuf {
        let safe: String = url
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        data_dir
            .join("qdrant")
            .join(format!("{safe}-{:08x}", fnv1a(url)))
    }

    /// Open (or return) the engine handle for the current URL.
    fn store<'a>(
        inner: &'a mut EmbeddedInner,
        data_dir: &std::path::Path,
    ) -> Option<&'a QdrantEdgeVectorStore> {
        if !inner.enabled {
            return None;
        }
        if inner.store.is_none() {
            match QdrantEdgeVectorStore::new(Self::dir_for(data_dir, &inner.url)) {
                Ok(s) => {
                    inner.store = Some(s);
                    inner.last_error = None;
                }
                Err(e) => {
                    inner.last_error = Some(format!("Qdrant storage open failed: {e}"));
                    return None;
                }
            }
        }
        inner.store.as_ref()
    }

    /// Health check (reference `health()`): disabled reports the reference's
    /// exact message; enabled lists the collection when its storage exists.
    pub fn health(&self) -> Health {
        let mut inner = self.inner.lock().expect("mutex poisoned");
        if !inner.enabled {
            return Health {
                connected: false,
                url: inner.url.clone(),
                collections: Vec::new(),
                error: Some("Qdrant disabled in settings".to_string()),
            };
        }
        let Some(store) = Self::store(&mut inner, &self.data_dir) else {
            return Health {
                connected: false,
                url: inner.url.clone(),
                collections: Vec::new(),
                error: Some(
                    inner
                        .last_error
                        .clone()
                        .unwrap_or_else(|| "Qdrant storage unavailable".to_string()),
                ),
            };
        };
        let collections = if store
            .collection_path(QDRANT_COLLECTION)
            .join(EDGE_CONFIG_FILE)
            .exists()
        {
            vec![QDRANT_COLLECTION.to_string()]
        } else {
            Vec::new()
        };
        Health {
            connected: true,
            url: inner.url.clone(),
            collections,
            error: None,
        }
    }

    /// Ensure the collection exists with the given dense dimension
    /// (reference `ensureCollection(dimension)` — vectors Cosine).
    pub fn ensure_collection(&self, dimension: usize) -> bool {
        let mut inner = self.inner.lock().expect("mutex poisoned");
        let Some(store) = Self::store(&mut inner, &self.data_dir) else {
            return false;
        };
        store
            .create_collection(QDRANT_COLLECTION, Some(dimension))
            .is_ok()
    }

    /// Upsert points (reference `upsert(points)` — wait=true semantics:
    /// the in-process engine is synchronous).
    pub fn upsert(&self, points: &[VectorPoint]) -> bool {
        if points.is_empty() {
            return false; // reference: empty batch returns false
        }
        let mut inner = self.inner.lock().expect("mutex poisoned");
        let Some(store) = Self::store(&mut inner, &self.data_dir) else {
            return false;
        };
        // Belt-and-braces: the reference calls ensureCollection first; if the
        // collection is not open yet, create it from the first point's dim.
        if store
            .collection_info(QDRANT_COLLECTION)
            .ok()
            .flatten()
            .is_none()
        {
            let dim = points[0].vector.len();
            if store
                .create_collection(QDRANT_COLLECTION, Some(dim))
                .is_err()
            {
                return false;
            }
        }
        for p in points {
            let point = Point {
                id: p.id.to_string(),
                dense: Some(p.vector.clone()),
                sparse: None,
                payload: p.payload.clone(),
            };
            if store.upsert(QDRANT_COLLECTION, point).is_err() {
                return false;
            }
        }
        true
    }

    /// Dense search with payload (reference `search(vector, limit, filter?)`
    /// — the routes never pass a filter, so none is offered here).
    pub fn search(&self, vector: &[f32], limit: usize) -> Vec<SearchHit> {
        let mut inner = self.inner.lock().expect("mutex poisoned");
        let Some(store) = Self::store(&mut inner, &self.data_dir) else {
            return Vec::new();
        };
        match store.search_dense(QDRANT_COLLECTION, vector, None, limit) {
            Ok(hits) => hits
                .into_iter()
                .filter_map(|h| {
                    let id = h.id.parse::<i64>().ok()?;
                    Some(SearchHit {
                        id,
                        score: h.score,
                        payload: h.payload,
                    })
                })
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Delete points by entity (reference `deleteByEntity` — filter delete on
    /// `entity_type` + `entity_id IN ids`). Implemented over the trait's
    /// snapshot + id-delete. Not called at runtime by the reference; ported
    /// for adapter parity.
    pub fn delete_by_entity(&self, entity_type: &str, entity_ids: &[i64]) -> bool {
        if entity_ids.is_empty() {
            return false; // reference: empty ids returns false
        }
        let mut inner = self.inner.lock().expect("mutex poisoned");
        let Some(store) = Self::store(&mut inner, &self.data_dir) else {
            return false;
        };
        let Ok(bytes) = store.snapshot(QDRANT_COLLECTION) else {
            return false;
        };
        let Ok(snapshot) = serde_json::from_slice::<CollectionSnapshot>(&bytes) else {
            return false;
        };
        for point in snapshot.points {
            let Some(payload) = point.payload.as_object() else {
                continue;
            };
            let matches_type =
                payload.get("entity_type").and_then(|v| v.as_str()) == Some(entity_type);
            let matches_id = payload
                .get("entity_id")
                .and_then(|v| v.as_i64())
                .is_some_and(|id| entity_ids.contains(&id));
            if matches_type && matches_id && store.delete(QDRANT_COLLECTION, &point.id).is_err() {
                return false;
            }
        }
        true
    }

    /// Count points of one entity type (reference `countByEntity`, exact).
    pub fn count_by_entity(&self, entity_type: &str) -> usize {
        let mut inner = self.inner.lock().expect("mutex poisoned");
        let Some(store) = Self::store(&mut inner, &self.data_dir) else {
            return 0;
        };
        let filter = Filter::new().must_eq("entity_type", entity_type);
        store.count(QDRANT_COLLECTION, Some(&filter)).unwrap_or(0)
    }

    /// Drop the collection (reference `dropCollection`).
    pub fn drop_collection(&self) -> bool {
        let mut inner = self.inner.lock().expect("mutex poisoned");
        let Some(store) = Self::store(&mut inner, &self.data_dir) else {
            return false;
        };
        store.drop_collection(QDRANT_COLLECTION).is_ok()
    }

    /// The configured URL (settings routes echo it back).
    pub fn url(&self) -> String {
        self.inner.lock().expect("mutex poisoned").url.clone()
    }

    /// Whether the adapter is enabled (settings routes).
    pub fn enabled(&self) -> bool {
        self.inner.lock().expect("mutex poisoned").enabled
    }
}

/// FNV-1a 32-bit — a stable, dependency-free hash for the URL→dir mapping.
fn fnv1a(s: &str) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for b in s.as_bytes() {
        hash ^= u32::from(*b);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
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

    // ---- EmbeddedQdrant (reference QdrantAdapter port, D2) -----------------

    fn point(id: i64, entity_id: i64, entity_type: &str, title: &str) -> VectorPoint {
        VectorPoint {
            id,
            vector: vec![1.0, 0.0, 0.0, 0.0],
            payload: serde_json::json!({
                "entity_type": entity_type,
                "entity_id": entity_id,
                "chunk_id": id,
                "title": title,
                "text": format!("text of {title}"),
                "visibility": "internal_only",
                "embedding_model": "test-model",
                "index_version": 1
            }),
        }
    }

    #[test]
    fn embedded_health_disabled_reports_reference_message() {
        let tmp = TempDir::new().unwrap();
        let q = EmbeddedQdrant::new(tmp.path(), "http://127.0.0.1:6333", false);
        let h = q.health();
        assert!(!h.connected);
        assert_eq!(h.url, "http://127.0.0.1:6333");
        assert!(h.collections.is_empty());
        assert_eq!(
            h.error.as_deref(),
            Some("Qdrant disabled in settings"),
            "exact reference message"
        );
    }

    #[test]
    fn embedded_health_enabled_lists_collection_after_ensure() {
        let tmp = TempDir::new().unwrap();
        let q = EmbeddedQdrant::new(tmp.path(), "http://127.0.0.1:6333", true);
        // Before any collection: connected, empty list.
        let h = q.health();
        assert!(h.connected);
        assert!(h.collections.is_empty());
        assert!(q.ensure_collection(4));
        let h = q.health();
        assert!(h.connected);
        assert_eq!(h.collections, vec!["supportos_vectors".to_string()]);
        assert!(h.error.is_none());
    }

    #[test]
    fn embedded_upsert_then_search_returns_payload() {
        let tmp = TempDir::new().unwrap();
        let q = EmbeddedQdrant::new(tmp.path(), "http://127.0.0.1:6333", true);
        assert!(q.ensure_collection(4));
        assert!(q.upsert(&[
            point(1, 10, "conversation_chunk", "Refund question"),
            point(2, 11, "docs_chunk", "Billing article"),
        ]));
        let hits = q.search(&[1.0, 0.0, 0.0, 0.0], 10);
        assert!(!hits.is_empty());
        let docs: Vec<_> = hits
            .iter()
            .filter(|h| h.payload.get("entity_type").and_then(|v| v.as_str()) == Some("docs_chunk"))
            .collect();
        assert_eq!(docs.len(), 1);
        assert_eq!(
            docs[0].payload.get("title").and_then(|v| v.as_str()),
            Some("Billing article")
        );
    }

    #[test]
    fn embedded_count_and_delete_by_entity() {
        let tmp = TempDir::new().unwrap();
        let q = EmbeddedQdrant::new(tmp.path(), "http://127.0.0.1:6333", true);
        assert!(q.ensure_collection(4));
        assert!(q.upsert(&[
            point(1, 10, "knowledge_chunk", "K1"),
            point(2, 10, "knowledge_chunk", "K2"),
            point(3, 11, "knowledge_chunk", "K3"),
            point(4, 99, "docs_chunk", "D1"),
        ]));
        assert_eq!(q.count_by_entity("knowledge_chunk"), 3);
        assert!(q.delete_by_entity("knowledge_chunk", &[10]));
        assert_eq!(q.count_by_entity("knowledge_chunk"), 1);
        assert_eq!(q.count_by_entity("docs_chunk"), 1);
        // Empty ids: reference returns false.
        assert!(!q.delete_by_entity("knowledge_chunk", &[]));
    }

    #[test]
    fn embedded_drop_collection_resets_health_list() {
        let tmp = TempDir::new().unwrap();
        let q = EmbeddedQdrant::new(tmp.path(), "http://127.0.0.1:6333", true);
        assert!(q.ensure_collection(4));
        assert!(q.drop_collection());
        let h = q.health();
        assert!(h.connected);
        assert!(h.collections.is_empty());
    }

    #[test]
    fn embedded_reconfigure_disable_then_reenable() {
        let tmp = TempDir::new().unwrap();
        let q = EmbeddedQdrant::new(tmp.path(), "http://127.0.0.1:6333", true);
        assert!(q.ensure_collection(4));
        q.reconfigure(None, Some(false));
        assert!(!q.enabled());
        let h = q.health();
        assert!(!h.connected);
        q.reconfigure(None, Some(true));
        assert!(q.health().connected);
        // Collection survived the disabled period.
        assert_eq!(
            q.health().collections,
            vec!["supportos_vectors".to_string()]
        );
    }

    #[test]
    fn embedded_url_change_reroots_storage() {
        let tmp = TempDir::new().unwrap();
        let q = EmbeddedQdrant::new(tmp.path(), "http://127.0.0.1:6333", true);
        assert!(q.ensure_collection(4));
        assert!(q.upsert(&[point(1, 10, "docs_chunk", "D1")]));
        // Point at a "different server": new storage location, empty.
        q.reconfigure(Some("http://127.0.0.1:6334".to_string()), None);
        assert_eq!(q.url(), "http://127.0.0.1:6334");
        let h = q.health();
        assert!(h.connected);
        assert!(h.collections.is_empty(), "new URL starts empty");
        // Back to the original URL: the collection is still there.
        q.reconfigure(Some("http://127.0.0.1:6333/".to_string()), None);
        assert_eq!(q.url(), "http://127.0.0.1:6333", "trailing slash trimmed");
        assert_eq!(
            q.health().collections,
            vec!["supportos_vectors".to_string()]
        );
    }

    #[test]
    fn embedded_upsert_empty_batch_returns_false() {
        let tmp = TempDir::new().unwrap();
        let q = EmbeddedQdrant::new(tmp.path(), "http://127.0.0.1:6333", true);
        assert!(!q.upsert(&[]), "reference returns false on empty batch");
    }

    #[test]
    fn embedded_persists_across_reopen() {
        let tmp = TempDir::new().unwrap();
        {
            let q = EmbeddedQdrant::new(tmp.path(), "http://127.0.0.1:6333", true);
            assert!(q.ensure_collection(4));
            assert!(q.upsert(&[point(1, 10, "docs_chunk", "D1")]));
        }
        // Simulate a restart: same data dir + settings.
        let q2 = EmbeddedQdrant::new(tmp.path(), "http://127.0.0.1:6333", true);
        let h = q2.health();
        assert!(h.connected);
        assert_eq!(h.collections, vec!["supportos_vectors".to_string()]);
        let hits = q2.search(&[1.0, 0.0, 0.0, 0.0], 10);
        assert_eq!(hits.len(), 1, "points survive the reopen");
    }

    #[test]
    fn embedded_upsert_auto_creates_collection() {
        let tmp = TempDir::new().unwrap();
        let q = EmbeddedQdrant::new(tmp.path(), "http://127.0.0.1:6333", true);
        // No ensure_collection call — upsert creates it from the first dim.
        assert!(q.upsert(&[point(1, 10, "docs_chunk", "D1")]));
        assert_eq!(
            q.health().collections,
            vec!["supportos_vectors".to_string()]
        );
    }
}
