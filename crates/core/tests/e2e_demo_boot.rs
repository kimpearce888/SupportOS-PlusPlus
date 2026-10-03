//! Runtime verification of the demo-mode boot (task-doc Phase 3 "run it for
//! real"): boots the REAL HTTP server against a throwaway data dir exactly
//! the way the Tauri shell does (crates/app/src-tauri/src/lib.rs), waits for
//! the demo seed, then exercises the surfaces the task doc names — health,
//! conversations, SSE hello, simulated webhook + rating, Operations Center,
//! dashboard, search, Qdrant settings — and drives the embedded vector
//! engine through its full lifecycle (index, search + filters, delete,
//! re-index, snapshot, restore).
//!
//! This is the cargo counterpart of the reference's e2e boot class
//! (tests/e2e/*.e2e.test.ts boot the real Fastify server against a temp DB
//! and issue real HTTP requests).

use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use spp_core::vectorstore::{Filter, Point, SparseVector, VectorStore};

const PORT: u16 = 3999;

/// One verification step's outcome.
struct Step {
    name: &'static str,
    ok: bool,
    detail: String,
}

fn step(out: &mut Vec<Step>, name: &'static str, ok: bool, detail: String) {
    out.push(Step { name, ok, detail });
}

// Multi-thread runtime: the server's async tasks must keep running while the
// SSE step performs a BLOCKING raw-TCP read on the test thread (a
// current-thread runtime would starve the server and deadlock the read).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn demo_boot_runtime_verification() {
    // Visible diagnostics: the demo boot chain logs warnings (e.g. "Demo
    // initial sync failed") that the seed-step detail below correlates with.
    spp_core::logging::init();

    // ── Boot, mirroring the Tauri shell (app/src-tauri/src/lib.rs) ──────────
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let data_dir = tmp.path().to_path_buf();
    let db_path = data_dir.join("supportos-plusplus.db");
    let mut conn = spp_core::db::open(&db_path).expect("open DB");
    spp_core::bootstrap::apply_all(&mut conn).expect("apply all migrations");

    let http_conn = Arc::new(Mutex::new(conn));
    let bus = spp_core::http::EventBus::default();
    let provider = Arc::new(spp_core::helpscout::FakeHelpScoutProvider::new_demo())
        as Arc<dyn spp_core::helpscout::HelpScoutProvider>;
    let sync = Arc::new(
        spp_core::sync_engine::SyncEngine::new(http_conn.clone(), provider).with_bus(bus.clone()),
    );
    let qdrant = spp_core::http::server::AppState::qdrant_from_settings(
        &http_conn.lock().unwrap_or_else(|p| p.into_inner()),
        &data_dir,
    );
    let state = spp_core::http::server::AppState {
        conn: http_conn.clone(),
        data_dir: data_dir.clone(),
        port: PORT,
        host: "127.0.0.1".to_string(),
        demo_mode: true,
        bus,
        limiter: spp_core::http::RateLimiter::new(),
        sync: Some(sync),
        real: None,
        provider_kind: "fake".to_string(),
        workers: None,
        qdrant,
    };
    let server = spp_core::http::HttpServer::new(state);
    tokio::spawn(async move {
        if let Err(e) = server.serve().await {
            eprintln!("HTTP server failed: {e}");
        }
    });

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("reqwest client");
    let base = format!("http://127.0.0.1:{PORT}");
    let mut out: Vec<Step> = Vec::new();

    // ── 1. Server comes up and answers /health ─────────────────────────────
    let mut up = false;
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if let Ok(r) = client.get(format!("{base}/health")).send().await {
            if r.status().as_u16() == 200 {
                up = true;
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    step(&mut out, "boot /health 200", up, "GET /health".into());

    // ── 2. Demo seed lands (initial sync + demoSeed) ────────────────────────
    // The boot chain sets `demo_data_loaded` only after the FULL seed pass
    // (mirror + demoSeed), so waiting on it removes the mid-seed race for
    // the count assertions below.
    let mut seeded = false;
    let mut conv_count = 0usize;
    let mut knowledge_count = 0usize;
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        // The guard is confined inside `seed_counts` — no lock is ever held
        // across the sleep await below (clippy::await_holding_lock).
        let (convs, docs, loaded) = seed_counts(&http_conn);
        conv_count = convs;
        knowledge_count = docs;
        if loaded {
            seeded = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    let cust_count = {
        let c = http_conn.lock().unwrap_or_else(|p| p.into_inner());
        c.query_row("SELECT COUNT(*) FROM customers", [], |r| r.get(0))
            .unwrap_or(0)
    };
    let (tag_count, known_issue_count, ai_runs, thread_count, ratings_count) = {
        let c = http_conn.lock().unwrap_or_else(|p| p.into_inner());
        (
            c.query_row("SELECT COUNT(*) FROM tags", [], |r| r.get(0))
                .unwrap_or(0),
            c.query_row("SELECT COUNT(*) FROM known_issues", [], |r| r.get(0))
                .unwrap_or(0),
            c.query_row("SELECT COUNT(*) FROM ai_runs", [], |r| r.get(0))
                .unwrap_or(0),
            c.query_row("SELECT COUNT(*) FROM conversation_threads", [], |r| {
                r.get(0)
            })
            .unwrap_or(0),
            c.query_row("SELECT COUNT(*) FROM ratings", [], |r| r.get(0))
                .unwrap_or(0),
        )
    };
    // Diagnostics when the seed did not land: which resources finished?
    let sync_diag = {
        let c = http_conn.lock().unwrap_or_else(|p| p.into_inner());
        let mut st: Vec<String> = Vec::new();
        if let Ok(mut stmt) =
            c.prepare("SELECT resource, status, last_error FROM sync_checkpoints ORDER BY resource")
        {
            if let Ok(rows) = stmt.query_map([], |r| {
                Ok(format!(
                    "{}:{}{}",
                    r.get::<_, String>(0).unwrap_or_default(),
                    r.get::<_, String>(1).unwrap_or_default(),
                    r.get::<_, String>(2)
                        .ok()
                        .map(|e| format!("({e})"))
                        .unwrap_or_default()
                ))
            }) {
                for row in rows.flatten() {
                    st.push(row);
                }
            }
        }
        st.join(",")
    };
    // Reference-derived demo counts (fakeData.ts + demoSeed.ts): 21 world
    // conversations with c12 merged away from listings → 20 mirror rows,
    // 48 threads, 8 customers (3001-3008), 14 tags, 7 ratings; the seed
    // writes 5 knowledge documents, 2 known issues and 6 AI runs (4 sample
    // analyses + the repeated-question pair).
    let counts_ok = seeded
        && conv_count == 20
        && thread_count == 48
        && cust_count == 8
        && tag_count == 14
        && ratings_count == 7
        && knowledge_count == 5
        && known_issue_count == 2
        && ai_runs == 6;
    step(
        &mut out,
        "demo seed counts (reference: 21 world conversations/20 mirrored, 8 customers)",
        counts_ok,
        format!(
            "conversations={conv_count} threads={thread_count} customers={cust_count} \
             tags={tag_count} ratings={ratings_count} knowledge_documents={knowledge_count} \
             known_issues={known_issue_count} ai_runs={ai_runs} sync_state={sync_diag}"
        ),
    );

    // ── 3. Conversations API answers with the seeded rows ──────────────────
    match client.get(format!("{base}/api/conversations")).send().await {
        Ok(r) => {
            let status = r.status().as_u16();
            let body: Value = r.json().await.unwrap_or(Value::Null);
            let n = body["conversations"]
                .as_array()
                .map(|a| a.len())
                .unwrap_or(0);
            step(
                &mut out,
                "GET /api/conversations",
                status == 200 && n > 0,
                format!("status={status} conversations_in_page={n}"),
            );
        }
        Err(e) => step(&mut out, "GET /api/conversations", false, e.to_string()),
    }

    // ── 4. SSE: hello event with the channel list ──────────────────────────
    let sse = sse_hello(&base);
    step(
        &mut out,
        "SSE hello event",
        sse.is_some(),
        sse.unwrap_or_else(|| "no hello event within 5s".into()),
    );

    // ── 5. Simulated webhook through the real HMAC pipeline ────────────────
    match client
        .post(format!("{base}/api/demo/simulate-webhook"))
        .json(&json!({"event": "convo.created", "conversationRemoteId": 105000}))
        .send()
        .await
    {
        Ok(r) => {
            let status = r.status().as_u16();
            let body: Value = r.json().await.unwrap_or(Value::Null);
            step(
                &mut out,
                "POST /api/demo/simulate-webhook",
                status == 200 && body["ok"] == json!(true),
                format!("status={status} body={body}"),
            );
        }
        Err(e) => step(
            &mut out,
            "POST /api/demo/simulate-webhook",
            false,
            e.to_string(),
        ),
    }

    // ── 6. Simulated rating event ───────────────────────────────────────────
    match client
        .post(format!("{base}/api/demo/simulate-rating"))
        .json(&json!({"conversationRemoteId": 105000, "rating": "great", "comments": ""}))
        .send()
        .await
    {
        Ok(r) => {
            let status = r.status().as_u16();
            let body: Value = r.json().await.unwrap_or(Value::Null);
            step(
                &mut out,
                "POST /api/demo/simulate-rating",
                status == 200 && body["ok"] == json!(true),
                format!("status={status} body={body}"),
            );
        }
        Err(e) => step(
            &mut out,
            "POST /api/demo/simulate-rating",
            false,
            e.to_string(),
        ),
    }

    // ── 7. Operations Center: 16 tiles; record real vs unavailable ─────────
    match client
        .get(format!("{base}/api/operations/center"))
        .send()
        .await
    {
        Ok(r) => {
            let status = r.status().as_u16();
            let body: Value = r.json().await.unwrap_or(Value::Null);
            let tiles = body["tiles"].as_array().cloned().unwrap_or_default();
            let unavailable: Vec<String> = tiles
                .iter()
                .filter(|t| t["available"] == json!(false) || t["count"].is_null())
                .filter_map(|t| t["key"].as_str().map(String::from))
                .collect();
            step(
                &mut out,
                "GET /api/operations/center (16 tiles)",
                status == 200 && tiles.len() == 16,
                format!(
                    "status={status} tiles={} unavailable={:?}",
                    tiles.len(),
                    unavailable
                ),
            );
        }
        Err(e) => step(&mut out, "GET /api/operations/center", false, e.to_string()),
    }

    // ── 8. Dashboard analytics answers ──────────────────────────────────────
    match client
        .get(format!("{base}/api/analytics/dashboard"))
        .send()
        .await
    {
        Ok(r) => {
            let status = r.status().as_u16();
            let body: Value = r.json().await.unwrap_or(Value::Null);
            step(
                &mut out,
                "GET /api/analytics/dashboard",
                status == 200,
                format!("status={status} ratings={}", body["ratings"]),
            );
        }
        Err(e) => step(
            &mut out,
            "GET /api/analytics/dashboard",
            false,
            e.to_string(),
        ),
    }

    // ── 9. Universal search answers (FTS path; embeddings absent in demo) ──
    match client
        .post(format!("{base}/api/search"))
        .json(&json!({"query": "invoice"}))
        .send()
        .await
    {
        Ok(r) => {
            let status = r.status().as_u16();
            step(
                &mut out,
                "POST /api/search",
                status == 200,
                format!("status={status}"),
            );
        }
        Err(e) => step(&mut out, "POST /api/search", false, e.to_string()),
    }

    // ── 10. Qdrant settings surface (embedded engine) ──────────────────────
    match client
        .get(format!("{base}/api/settings/qdrant"))
        .send()
        .await
    {
        Ok(r) => {
            let status = r.status().as_u16();
            let body: Value = r.json().await.unwrap_or(Value::Null);
            step(
                &mut out,
                "GET /api/settings/qdrant",
                status == 200,
                format!("status={status} body={body}"),
            );
        }
        Err(e) => step(&mut out, "GET /api/settings/qdrant", false, e.to_string()),
    }
    match client
        .post(format!("{base}/api/settings/qdrant/test"))
        .send()
        .await
    {
        Ok(r) => {
            let status = r.status().as_u16();
            let body: Value = r.json().await.unwrap_or(Value::Null);
            step(
                &mut out,
                "POST /api/settings/qdrant/test",
                status == 200,
                format!("status={status} body={body}"),
            );
        }
        Err(e) => step(
            &mut out,
            "POST /api/settings/qdrant/test",
            false,
            e.to_string(),
        ),
    }

    // ── 11. Embedded vector engine lifecycle (D2, default build) ────────────
    vector_lifecycle(&mut out, &data_dir);

    // ── Report ──────────────────────────────────────────────────────────────
    println!("\n==== RUNTIME VERIFICATION REPORT ====");
    for s in &out {
        println!(
            "[{}] {} — {}",
            if s.ok { "PASS" } else { "FAIL" },
            s.name,
            s.detail
        );
    }
    let failed: Vec<&str> = out.iter().filter(|s| !s.ok).map(|s| s.name).collect();
    println!(
        "==== {} steps, {} failed{} ====",
        out.len(),
        failed.len(),
        if failed.is_empty() {
            String::new()
        } else {
            format!(": {failed:?}")
        }
    );
    assert!(failed.is_empty(), "runtime verification failed: {failed:?}");
}

/// (conversations, knowledge_documents, demo_data_loaded) counts — guard
/// stays inside.
fn seed_counts(conn: &Arc<Mutex<rusqlite::Connection>>) -> (usize, usize, bool) {
    let c = conn.lock().unwrap_or_else(|p| p.into_inner());
    (
        c.query_row("SELECT COUNT(*) FROM conversations", [], |r| r.get(0))
            .unwrap_or(0),
        c.query_row("SELECT COUNT(*) FROM knowledge_documents", [], |r| r.get(0))
            .unwrap_or(0),
        c.query_row(
            "SELECT value FROM application_settings WHERE key = 'demo_data_loaded'",
            [],
            |r| r.get::<_, String>(0),
        )
        .ok()
        .and_then(|v| serde_json::from_str::<bool>(&v).ok().or(Some(v == "true")))
        .unwrap_or(false),
    )
}

/// Open a raw TCP connection to the SSE endpoint and read until the `hello`
/// event arrives (or 5s). Returns the hello data JSON as a string.
fn sse_hello(base: &str) -> Option<String> {
    let addr = base.trim_start_matches("http://").to_string();
    let mut stream = TcpStream::connect(&addr).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    let req = format!(
        "GET /api/events HTTP/1.1\r\nHost: {addr}\r\nAccept: text/event-stream\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(req.as_bytes()).ok()?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&buf);
                if let Some(pos) = text.find("event: hello") {
                    let data_start = text[pos..]
                        .find("data: ")
                        .map(|d| pos + d + 6)
                        .unwrap_or(pos);
                    let line_end = text[data_start..]
                        .find('\n')
                        .map(|e| data_start + e)
                        .unwrap_or(text.len());
                    return Some(text[data_start..line_end].trim().to_string());
                }
            }
            Err(_) => break,
        }
    }
    None
}

/// Drive the embedded vector engine through the full lifecycle the task doc
/// requires: index, search, payload filters, delete, re-index, snapshot,
/// restore. Two layers are exercised, both in the DEFAULT build:
///   1. `EmbeddedQdrant` — the reference qdrantAdapter.ts surface the server
///      wires from settings (ensureCollection/upsert/search/deleteByEntity/
///      countByEntity/dropCollection/health).
///   2. `QdrantEdgeVectorStore` — the VectorStore trait implementation
///      (named collections, filtered + sparse search, snapshot/restore).
fn vector_lifecycle(out: &mut Vec<Step>, data_dir: &std::path::Path) {
    use spp_core::vectorstore_qdrant::{EmbeddedQdrant, VectorPoint};

    // ── Layer 1: the runtime adapter (reference method surface) ────────────
    let dir = data_dir.join("vec-lifecycle");
    let adapter = EmbeddedQdrant::new(&dir, "http://127.0.0.1:6333", true);

    let health = adapter.health();
    step(
        out,
        "vector: adapter health (embedded engine connected)",
        health.connected,
        format!(
            "connected={} url={} collections={:?}",
            health.connected, health.url, health.collections
        ),
    );

    let mk = |id: i64, vector: Vec<f32>, entity: &str, text: &str| VectorPoint {
        id,
        vector,
        payload: json!({"entity_type": entity, "entity_id": id, "text": text}),
    };

    // index: ensureCollection + upsert
    let ensured = adapter.ensure_collection(3);
    let upserted = adapter.upsert(&[
        mk(1, vec![1.0, 0.0, 0.0], "ticket", "invoice question"),
        mk(2, vec![0.0, 1.0, 0.0], "doc", "billing guide"),
        mk(3, vec![0.9, 0.1, 0.0], "doc", "invoice faq"),
    ]);
    let tickets = adapter.count_by_entity("ticket");
    let docs = adapter.count_by_entity("doc");
    step(
        out,
        "vector: index (ensureCollection + upsert x3)",
        ensured && upserted && tickets == 1 && docs == 2,
        format!("ensured={ensured} upserted={upserted} tickets={tickets} docs={docs}"),
    );

    // search: nearest to [1,0,0] is id 1 or 3
    let hits = adapter.search(&[1.0, 0.0, 0.0], 3);
    let top = hits.first().map(|h| h.id).unwrap_or(0);
    step(
        out,
        "vector: dense search ranking",
        hits.len() == 3 && (top == 1 || top == 3),
        format!("hits={} top={top}", hits.len()),
    );

    // delete by entity + re-index
    let deleted = adapter.delete_by_entity("ticket", &[1]);
    let docs_after = adapter.count_by_entity("doc");
    let re_upserted = adapter.upsert(&[mk(4, vec![0.1, 0.9, 0.2], "ticket", "refund invoice")]);
    let tickets_after = adapter.count_by_entity("ticket");
    step(
        out,
        "vector: deleteByEntity",
        deleted && docs_after == 2,
        format!("deleted={deleted} docs_after={docs_after}"),
    );
    step(
        out,
        "vector: re-index (upsert after delete)",
        re_upserted && tickets_after == 1,
        format!("tickets_after={tickets_after}"),
    );

    // drop
    let dropped = adapter.drop_collection();
    step(
        out,
        "vector: dropCollection",
        dropped,
        format!("dropped={dropped}"),
    );

    // ── Layer 2: the trait store (filters, sparse, snapshot/restore) ────────
    let dir2 = data_dir.join("vec-trait");
    let store =
        spp_core::vectorstore_qdrant::QdrantEdgeVectorStore::new(&dir2).expect("open trait store");

    let tp = |id: u64, dense: Vec<f32>, entity: &str| Point {
        // The qdrant-edge engine requires NUMERIC point ids; the PointId
        // string must parse as u64 (see point_id_to_u64).
        id: id.to_string(),
        dense: Some(dense),
        sparse: Some(SparseVector::new(vec![1, 5], vec![0.9, 0.4])),
        payload: json!({"entity_type": entity}),
    };

    let indexed = store
        .create_collection("lifecycle", Some(3))
        .and_then(|_| store.upsert("lifecycle", tp(1, vec![1.0, 0.0, 0.0], "ticket")))
        .and_then(|_| store.upsert("lifecycle", tp(2, vec![0.0, 1.0, 0.0], "doc")))
        .and_then(|_| store.upsert("lifecycle", tp(3, vec![0.9, 0.1, 0.0], "doc")))
        .is_ok();
    let n0 = store.count("lifecycle", None).unwrap_or(0);
    step(
        out,
        "vector: trait index (named collection)",
        indexed && n0 == 3,
        format!("indexed={indexed} count={n0}"),
    );

    let f = Filter::new().must_eq("entity_type", "doc");
    let filtered = store
        .search_dense("lifecycle", &[1.0, 0.0, 0.0], Some(&f), 10)
        .unwrap_or_default();
    let all_docs = filtered
        .iter()
        .all(|s| s.payload["entity_type"] == json!("doc"));
    step(
        out,
        "vector: filtered search (entity_type=doc)",
        filtered.len() == 2 && all_docs,
        format!("hits={} all_docs={all_docs}", filtered.len()),
    );

    let sparse = store
        .search_sparse("lifecycle", &SparseVector::new(vec![1], vec![1.0]), None, 3)
        .unwrap_or_default();
    step(
        out,
        "vector: sparse search",
        !sparse.is_empty(),
        format!("hits={}", sparse.len()),
    );

    let snap = store.snapshot("lifecycle").ok();
    let snap_len = snap.as_ref().map(|s| s.len()).unwrap_or(0);
    let restored = snap.as_ref().map(|bytes| {
        let dir3 = data_dir.join("vec-restore");
        let store2 = spp_core::vectorstore_qdrant::QdrantEdgeVectorStore::new(&dir3)
            .expect("open restore store");
        store2
            .restore(bytes)
            .and_then(|_| store2.count("lifecycle", None))
            .map(|n| {
                println!("restore count = {n}");
                n == 3
            })
            .unwrap_or(false)
    });
    step(
        out,
        "vector: snapshot/restore round-trip",
        restored.unwrap_or(false),
        format!("snapshot_bytes={snap_len} restored_count==3"),
    );
}
