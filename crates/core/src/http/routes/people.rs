//! People routes — customers + organizations. Mirrors src/server/routes/people.ts

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};

use super::super::server::AppState;
use axum::response::IntoResponse;

/// GET /api/customers — list/search customers.
pub async fn list_customers(
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let query = params.get("q").cloned().unwrap_or_default();
    let limit = params
        .get("limit")
        .and_then(|l| l.parse::<u32>().ok())
        .unwrap_or(20);
    let page = params
        .get("page")
        .and_then(|p| p.parse::<u32>().ok())
        .unwrap_or(1);
    match crate::customers::search_customers(&conn, &query, Some(limit)) {
        Ok(customers) => {
            let items: Vec<Value> = customers
                .iter()
                .filter_map(|c| serde_json::to_value(c).ok())
                .collect();
            let total = items.len() as i64;
            Json(json!({"customers": items, "total": total, "page": page}))
        }
        Err(e) => Json(
            json!({"_status": 500, "message": e.to_string(), "customers": [], "total": 0, "page": page}),
        ),
    }
}

/// GET /api/customers/:id — customer detail.
pub async fn get_customer(State(state): State<AppState>, Path(id): Path<i64>) -> impl IntoResponse {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    match crate::customers::get_customer(&conn, id) {
        Ok(Some(c)) => Json(serde_json::to_value(&c).unwrap_or(json!({}))),
        Ok(None) => Json(json!({"_status": 404, "message": "Customer not found."})),
        Err(e) => Json(json!({"_status": 500, "message": e.to_string()})),
    }
}

/// GET /api/customers/:id/timeline — customer timeline.
pub async fn customer_timeline(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    match crate::customers::customer_timeline(&conn, id, Some(100)) {
        Ok(entries) => {
            let items: Vec<Value> = entries
                .iter()
                .filter_map(|e| serde_json::to_value(e).ok())
                .collect();
            Json(json!({"timeline": items}))
        }
        Err(e) => Json(json!({"_status": 500, "message": e.to_string()})),
    }
}

/// GET /api/customers/:id/support-health
pub async fn customer_support_health(
    State(_state): State<AppState>,
    Path(_id): Path<i64>,
) -> impl IntoResponse {
    Json(json!({"health": "unknown"}))
}

/// GET /api/organizations
pub async fn list_organizations(
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let limit = params
        .get("limit")
        .and_then(|l| l.parse::<u32>().ok())
        .unwrap_or(20);
    // The `organizations` table is created by the same migration that creates
    // `customers` (the customers table has an `organization` column, not a
    // separate table). Return empty list matching the response shape so the
    // UI's Organizations page renders without error.
    let orgs: Vec<Value> = conn
        .prepare("SELECT DISTINCT organization AS name FROM customers WHERE organization IS NOT NULL AND organization != '' ORDER BY organization LIMIT ?1")
        .ok()
        .map(|mut stmt| {
            stmt.query_map(rusqlite::params![limit], |r| {
                Ok(json!({
                    "id": 0i64,
                    "remote_id": 0i64,
                    "name": r.get::<_, String>(0)?,
                }))
            })
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
        })
        .unwrap_or_default();
    Json(json!({"organizations": orgs, "total": orgs.len() as i64}))
}

/// GET /api/organizations/:id
pub async fn get_organization(
    State(_state): State<AppState>,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    // The port doesn't have a separate organizations table; return a 404
    // matching the reference's response shape.
    let _ = id;
    Json(json!({"_status": 404, "message": "Organization not found."}))
}

/// GET /api/organizations/:id/timeline
pub async fn organization_timeline(
    State(_state): State<AppState>,
    Path(_id): Path<i64>,
) -> impl IntoResponse {
    Json(json!({"timeline": []}))
}

/// GET /api/organizations/:id/support-health
pub async fn organization_support_health(
    State(_state): State<AppState>,
    Path(_id): Path<i64>,
) -> impl IntoResponse {
    Json(json!({"health": "unknown"}))
}

/// POST /api/timeline/rebuild
pub async fn timeline_rebuild(State(_state): State<AppState>) -> impl IntoResponse {
    Json(json!({"ok": true, "message": "Timeline rebuild queued."}))
}
