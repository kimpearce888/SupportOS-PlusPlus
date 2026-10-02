//! Attributes routes — mirrors src/server/routes/attributes.ts
//!
//! Conversation attributes — custom key/value pairs attached to a
//! conversation (e.g. "product_area", "ticket_category"). Used by the
//! Issue Radar and Operations pages to slice + filter.

use axum::extract::{Path, State};
use axum::Json;
use serde_json::{json, Value};

use super::super::server::AppState;

/// GET /api/attributes/catalog — list all distinct attribute keys in use.
pub async fn catalog(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let keys: Vec<Value> = conn
        .prepare("SELECT DISTINCT key FROM conversation_attributes ORDER BY key")
        .ok()
        .map(|mut stmt| {
            stmt.query_map([], |r| {
                Ok(json!({
                    "key": r.get::<_, String>(0)?,
                    "label": r.get::<_, String>(0)?,
                    "value_type": "string",
                    "description": "User-defined conversation attribute",
                }))
            })
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
        })
        .unwrap_or_default();
    Json(json!({
        "catalog": keys,
        "schema_version": "1.0",
        "note": "Attribute catalog generated from distinct keys in conversation_attributes."
    }))
}

/// GET /api/attributes/conversation/:id — list attributes for a conversation.
pub async fn conversation_attributes(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let attrs: Vec<Value> = conn
        .prepare("SELECT key, value FROM conversation_attributes WHERE conversation_id = ?1 ORDER BY key")
        .ok()
        .map(|mut stmt| {
            stmt.query_map(rusqlite::params![id], |r| {
                Ok(json!({
                    "key": r.get::<_, String>(0)?,
                    "value": r.get::<_, String>(1)?,
                }))
            })
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
        })
        .unwrap_or_default();
    Json(json!({"conversationId": id, "attributes": attrs}))
}

/// GET /api/attributes/report — aggregate counts per attribute key/value.
pub async fn report(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let rows: Vec<Value> = conn
        .prepare("SELECT key, value, COUNT(*) FROM conversation_attributes GROUP BY key, value ORDER BY 3 DESC LIMIT 200")
        .ok()
        .map(|mut stmt| {
            stmt.query_map([], |r| {
                Ok(json!({
                    "key": r.get::<_, String>(0)?,
                    "value": r.get::<_, String>(1)?,
                    "count": r.get::<_, i64>(2)?,
                }))
            })
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
        })
        .unwrap_or_default();
    Json(json!({"distributions": rows}))
}
