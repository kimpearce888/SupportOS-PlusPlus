//! Memory routes — mirrors src/server/routes/memory.ts
//!
//! Customer memory entries — AI-generated + manually added notes about a
//! customer (preferences, history, signal-overrides). Stored in the
//! `customer_memory` table.

use axum::extract::{Path, State};
use axum::Json;
use serde_json::{json, Value};

use super::super::server::AppState;

/// GET /api/memory/meta — overall memory store stats.
pub async fn meta(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let total: i64 = conn
        .query_row("SELECT COUNT(*) FROM customer_memory", [], |r| r.get(0))
        .unwrap_or(0);
    let by_source: Vec<Value> = conn
        .prepare("SELECT source, COUNT(*) FROM customer_memory GROUP BY source ORDER BY 2 DESC")
        .ok()
        .map(|mut stmt| {
            stmt.query_map([], |r| {
                Ok(json!({
                    "source": r.get::<_, String>(0)?,
                    "count": r.get::<_, i64>(1)?,
                }))
            })
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
        })
        .unwrap_or_default();
    Json(json!({
        "total": total,
        "by_source": by_source,
        "sections": by_source.iter().map(|s| s["source"].clone()).collect::<Vec<_>>(),
        "section_labels": by_source.iter().map(|s| {
            let src = s["source"].as_str().unwrap_or("manual");
            let label = match src {
                "ai" => "AI-generated",
                "manual" => "Manually added",
                "imported" => "Imported",
                _ => src,
            };
            json!({src: label})
        }).collect::<Vec<_>>(),
        "entry_kinds": ["preference", "history", "signal", "note"],
        "notes": "Memory stats aggregated from the local customer_memory table."
    }))
}

/// GET /api/memory/:customerId — list memory entries for a customer.
pub async fn get(State(state): State<AppState>, Path(customer_id): Path<i64>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let entries: Vec<Value> = conn
        .prepare("SELECT id, customer_id, key, value, source, created_at FROM customer_memory WHERE customer_id = ?1 ORDER BY created_at DESC")
        .ok()
        .map(|mut stmt| {
            stmt.query_map(rusqlite::params![customer_id], |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "customer_id": r.get::<_, i64>(1)?,
                    "key": r.get::<_, String>(2)?,
                    "value": r.get::<_, String>(3)?,
                    "source": r.get::<_, String>(4)?,
                    "created_at": r.get::<_, String>(5)?,
                }))
            })
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
        })
        .unwrap_or_default();
    Json(json!({"entries": entries, "customerId": customer_id}))
}

/// POST /api/memory/:customerId/entries — add a memory entry.
pub async fn add_entry(
    State(state): State<AppState>,
    Path(customer_id): Path<i64>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let key = body.get("key").and_then(|v| v.as_str()).unwrap_or("note");
    let value = body.get("value").and_then(|v| v.as_str()).unwrap_or("");
    let source = body
        .get("source")
        .and_then(|v| v.as_str())
        .unwrap_or("manual");
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let inserted = conn
        .execute(
            "INSERT INTO customer_memory (customer_id, key, value, source, created_at)
             VALUES (?1, ?2, ?3, ?4, strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
            rusqlite::params![customer_id, key, value, source],
        )
        .is_ok();
    drop(conn);
    if inserted {
        crate::http::event_bus::notify_sync(&state.bus, "memory", 1);
    }
    Json(json!({"ok": inserted, "customerId": customer_id}))
}
