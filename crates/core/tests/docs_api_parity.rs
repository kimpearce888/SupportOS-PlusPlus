//! DC-01: Docs channel API — collections / stats / hybrid search / article
//! read against the synced docs mirror.
//!
//! The audit found the docs routes stubbed onto the `knowledge_doc_freshness`
//! table: LIKE-only title search, no article read, fake counters. This test
//! boots the REAL HTTP server in demo mode (like tests/e2e_demo_boot.rs),
//! lets the demo sync populate the docs mirror through the REAL sync upserts
//! (two collections, four categories, nine articles — chunks + FTS rows
//! included), then live-probes the reference wire contract
//! (src/server/routes/docs.ts v1.4.0):
//!
//!   - GET /api/docs/collections — mirror shape + docs_sync_available;
//!   - GET /api/docs/stats — collections/articles/published/drafts/internal/
//!     total_views/last_synced_at + chat/email conversation counts +
//!     docs_chunks readiness counters;
//!   - GET /api/docs/articles — paging, collectionId/status filters, FTS `q`,
//!     reference 422 envelopes, reference ordering;
//!   - GET /api/docs/articles/:id — summary + text, the reference 404
//!     envelope, 422 for non-positive ids;
//!   - GET /api/docs/search — FTS hits with snippets + `why` provenance, the
//!     honest no-model mode note, 422 validation, and the semantic toggle.

use serde_json::Value;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const PORT: u16 = 3989;

// Multi-thread runtime: the server's async task must keep running while the
// test drives it from the test thread (same rationale as e2e_demo_boot.rs).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn docs_api_matches_the_reference_contract() {
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

    // ── Wait for boot + the demo docs mirror (9 articles) ───────────────────
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Ok(r) = client.get(format!("{base}/health")).send().await {
            if r.status().as_u16() == 200 {
                break;
            }
        }
        assert!(
            Instant::now() < deadline,
            "server did not come up on port {PORT}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let seeded = {
            let conn = http_conn.lock().unwrap_or_else(|p| p.into_inner());
            let docs: i64 = conn
                .query_row("SELECT COUNT(*) FROM docs", [], |r| r.get(0))
                .unwrap_or(0);
            docs >= 9
        };
        if seeded {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "demo docs mirror did not populate (docs < 9)"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    // ── 1. Collections: mirror shape + sync flag ────────────────────────────
    let r = client
        .get(format!("{base}/api/docs/collections"))
        .send()
        .await
        .expect("collections");
    assert_eq!(r.status().as_u16(), 200);
    let body: Value = r.json().await.expect("collections body");
    let cols = body["collections"].as_array().expect("collections array");
    assert_eq!(cols.len(), 2, "two demo collections: {cols:#?}");
    // ORDER BY name -> Billing & Account first.
    assert_eq!(cols[0]["name"].as_str(), Some("Billing & Account"));
    assert_eq!(cols[0]["remote_id"].as_i64(), Some(802));
    assert_eq!(cols[0]["slug"].as_str(), Some("billing-account"));
    assert_eq!(cols[0]["visibility"].as_str(), Some("public"));
    assert_eq!(cols[0]["article_count"].as_i64(), Some(4));
    assert!(cols[0]["last_synced_at"].as_str().is_some());
    assert_eq!(cols[1]["name"].as_str(), Some("Getting Started"));
    assert_eq!(cols[1]["article_count"].as_i64(), Some(5));
    // Demo mode (fake provider) -> the reference's honest capability flag.
    assert_eq!(body["docs_sync_available"].as_bool(), Some(true));

    // ── 2. Stats: reference counters over the mirror ────────────────────────
    let r = client
        .get(format!("{base}/api/docs/stats"))
        .send()
        .await
        .expect("stats");
    assert_eq!(r.status().as_u16(), 200);
    let s: Value = r.json().await.expect("stats body");
    assert_eq!(s["collections"].as_i64(), Some(2));
    assert_eq!(s["articles"].as_i64(), Some(9));
    assert_eq!(s["published"].as_i64(), Some(7));
    assert_eq!(s["drafts"].as_i64(), Some(1));
    assert_eq!(s["internal"].as_i64(), Some(1));
    assert_eq!(s["total_views"].as_i64(), Some(3260));
    assert!(s["last_synced_at"].as_str().is_some());
    // Conversation-channel mix (the demo world's 6 beacon chats + email).
    assert_eq!(s["chat_sessions"].as_i64(), Some(6));
    assert!(s["email_conversations"].as_i64().unwrap_or(0) > 0);
    // The sync chunks every article (1200/150) — readiness counters are live.
    assert!(s["docs_chunks"].as_i64().unwrap_or(0) >= 9);
    assert_eq!(s["docs_chunks_indexed"].as_i64(), Some(0));
    assert_eq!(s["docs_chunks_failed"].as_i64(), Some(0));

    // ── 3. Articles: paging, filters, 422s, reference ordering ─────────────
    let r = client
        .get(format!("{base}/api/docs/articles?pageSize=5"))
        .send()
        .await
        .expect("articles page 1");
    assert_eq!(r.status().as_u16(), 200);
    let a: Value = r.json().await.expect("articles body");
    assert_eq!(a["total"].as_i64(), Some(9));
    assert_eq!(a["page"].as_i64(), Some(1));
    assert_eq!(a["page_size"].as_i64(), Some(5));
    assert_eq!(a["articles"].as_array().unwrap().len(), 5);
    let first = &a["articles"][0];
    // Summary shape (reference DocsArticleSummary) — joined names included.
    for key in [
        "id",
        "remote_id",
        "collection_id",
        "collection_name",
        "category_id",
        "category_name",
        "number",
        "slug",
        "name",
        "status",
        "preview",
        "words",
        "views",
        "remote_created_at",
        "remote_updated_at",
    ] {
        assert!(first.get(key).is_some(), "summary key missing: {key}");
    }
    assert!(first["collection_name"].as_str().is_some());
    assert!(first["words"].as_i64().is_some());
    assert!(first["preview"].as_str().is_some());
    // Page 2.
    let r = client
        .get(format!("{base}/api/docs/articles?page=2&pageSize=5"))
        .send()
        .await
        .expect("articles page 2");
    assert_eq!(r.status().as_u16(), 200);
    let a2: Value = r.json().await.expect("articles page 2 body");
    assert_eq!(a2["total"].as_i64(), Some(9));
    assert_eq!(a2["articles"].as_array().unwrap().len(), 4);

    // collectionId filter — Getting Started (id 1) has 5 articles.
    let r = client
        .get(format!("{base}/api/docs/articles?collectionId=1"))
        .send()
        .await
        .expect("articles by collection");
    assert_eq!(r.status().as_u16(), 200);
    let ac: Value = r.json().await.expect("articles by collection body");
    assert_eq!(ac["total"].as_i64(), Some(5));
    assert!(ac["articles"]
        .as_array()
        .unwrap()
        .iter()
        .all(|row| row["collection_name"].as_str() == Some("Getting Started")));

    // status filter.
    let r = client
        .get(format!("{base}/api/docs/articles?status=internal"))
        .send()
        .await
        .expect("articles by status");
    assert_eq!(r.status().as_u16(), 200);
    let st: Value = r.json().await.expect("articles by status body");
    assert_eq!(st["total"].as_i64(), Some(1));
    assert_eq!(
        st["articles"][0]["name"].as_str(),
        Some("SSO troubleshooting checklist (internal)")
    );

    // q filter (FTS): "invoice" matches the two billing articles.
    let r = client
        .get(format!("{base}/api/docs/articles?q=invoice"))
        .send()
        .await
        .expect("articles by q");
    assert_eq!(r.status().as_u16(), 200);
    let q: Value = r.json().await.expect("articles by q body");
    assert_eq!(q["total"].as_i64(), Some(2), "q=invoice: {q:#?}");

    // Validation envelopes (reference messages, exact).
    for (url, message) in [
        (
            "/api/docs/articles?page=0",
            "page must be >= 1 and pageSize must be 1-100.",
        ),
        (
            "/api/docs/articles?pageSize=101",
            "page must be >= 1 and pageSize must be 1-100.",
        ),
        (
            "/api/docs/articles?collectionId=0",
            "collectionId must be a positive integer.",
        ),
        (
            "/api/docs/articles?status=archived",
            "status must be one of 'published', 'draft', 'internal'.",
        ),
    ] {
        let r = client
            .get(format!("{base}{url}"))
            .send()
            .await
            .expect("422 probe");
        assert_eq!(r.status().as_u16(), 422, "url: {url}");
        let e: Value = r.json().await.expect("422 body");
        assert_eq!(e["statusCode"], 422);
        assert_eq!(e["error"], "ValidationError");
        assert_eq!(e["message"], message);
    }

    // ── 4. Article read: summary + text, 404, 422 ──────────────────────────
    // The demo sync inserts collection 801's articles first -> local id 1 is
    // "Creating your first report".
    let r = client
        .get(format!("{base}/api/docs/articles/1"))
        .send()
        .await
        .expect("article detail");
    assert_eq!(r.status().as_u16(), 200);
    let d: Value = r.json().await.expect("article detail body");
    let article = &d["article"];
    assert_eq!(article["name"].as_str(), Some("Creating your first report"));
    assert_eq!(article["number"].as_i64(), Some(101));
    assert_eq!(article["category_name"].as_str(), Some("Setup"));
    let text = article["text"].as_str().expect("detail text");
    assert!(text.contains("open the Reports section"));
    assert!(article["preview"].as_str().is_some());

    let r = client
        .get(format!("{base}/api/docs/articles/99999"))
        .send()
        .await
        .expect("article 404");
    assert_eq!(r.status().as_u16(), 404);
    let e: Value = r.json().await.expect("404 body");
    assert_eq!(e["statusCode"], 404);
    assert_eq!(e["error"], "NotFound");
    assert_eq!(
        e["message"],
        "Article not found in the local docs mirror. Run a sync with a Docs API key configured."
    );

    let r = client
        .get(format!("{base}/api/docs/articles/0"))
        .send()
        .await
        .expect("article 422");
    assert_eq!(r.status().as_u16(), 422);
    let e: Value = r.json().await.expect("422 body");
    assert_eq!(e["message"], "Article id must be a positive integer.");

    // ── 5. Search: FTS hits + honest notes + validation ─────────────────────
    let r = client
        .get(format!("{base}/api/docs/search?q=invoice"))
        .send()
        .await
        .expect("search");
    assert_eq!(r.status().as_u16(), 200);
    let sr: Value = r.json().await.expect("search body");
    assert_eq!(sr["query"].as_str(), Some("invoice"));
    assert_eq!(sr["total"].as_i64(), Some(2), "search: {sr:#?}");
    assert_eq!(sr["used_semantic"].as_bool(), Some(false));
    assert_eq!(sr["semantic_available"].as_bool(), Some(false));
    assert_eq!(
        sr["mode_note"].as_str(),
        Some("Keyword search (FTS5). Semantic search needs an embedding model: Settings → LM Studio → embedding model, then the docs embedding job runs on the next sync.")
    );
    let hit = &sr["hits"][0];
    assert!(hit["article"]["id"].as_i64().is_some());
    assert!(hit["score"].as_f64().is_some());
    assert_eq!(hit["why"][0].as_str(), Some("fts"));
    // FTS snippet (bracketed highlight) wins over the preview.
    let snippet = hit["snippet"].as_str().expect("snippet");
    assert!(
        snippet.contains('[') && snippet.contains(']'),
        "snippet: {snippet}"
    );
    assert_eq!(hit["matched_chunk"].as_null(), Some(()));

    // Multi-word FTS narrows further (both tokens must match).
    let r = client
        .get(format!("{base}/api/docs/search?q=receipts+invoices"))
        .send()
        .await
        .expect("search multiword");
    let m: Value = r.json().await.expect("search multiword body");
    assert_eq!(m["total"].as_i64(), Some(1));
    assert_eq!(
        m["hits"][0]["article"]["name"].as_str(),
        Some("Downloading receipts and invoices")
    );

    // limit=1 caps the fused list.
    let r = client
        .get(format!("{base}/api/docs/search?q=plan&limit=1"))
        .send()
        .await
        .expect("search limit");
    let l: Value = r.json().await.expect("search limit body");
    assert_eq!(l["total"].as_i64(), Some(1));

    // Validation.
    for (url, message) in [
        ("/api/docs/search", "q is required."),
        ("/api/docs/search?q=", "q is required."),
        ("/api/docs/search?q=%20%20", "q is required."),
        ("/api/docs/search?q=x&limit=0", "limit must be 1-100."),
        ("/api/docs/search?q=x&limit=101", "limit must be 1-100."),
    ] {
        let r = client
            .get(format!("{base}{url}"))
            .send()
            .await
            .expect("422 probe");
        assert_eq!(r.status().as_u16(), 422, "url: {url}");
        let e: Value = r.json().await.expect("422 body");
        assert_eq!(e["statusCode"], 422);
        assert_eq!(e["error"], "ValidationError");
        assert_eq!(e["message"], message);
    }

    // The semantic toggle reports its disabled note (no model configured,
    // so the toggle check runs first — reference gate order).
    let r = client
        .get(format!("{base}/api/docs/search?q=invoice&semantic=0"))
        .send()
        .await
        .expect("search semantic off");
    let so: Value = r.json().await.expect("search semantic off body");
    assert_eq!(so["used_semantic"].as_bool(), Some(false));
    assert_eq!(
        so["mode_note"].as_str(),
        Some("Keyword search (FTS) - semantic retrieval disabled for this query.")
    );

    // The chunks the sync created are visible to the embedding job's
    // pending list (docs_chunks rows in not_indexed state).
    let pending: i64 = {
        let conn = http_conn.lock().unwrap_or_else(|p| p.into_inner());
        conn.query_row(
            "SELECT COUNT(*) FROM docs_chunks WHERE embedding_state = 'not_indexed'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0)
    };
    assert!(pending >= 9, "sync must chunk every article: {pending}");
}
