//! People routes — customers + organizations. Mirrors src/server/routes/people.ts
//!
//! P5 part 4: the read side now serves the REAL organizations table (the
//! v1.x routes projected `SELECT DISTINCT organization FROM customers` with
//! fake `id: 0` rows, answered a hardcoded 404 on detail, an empty timeline
//! and `"unknown"` health) and the domain gains its write surface —
//! `POST /api/customers`, `PATCH /api/customers/:id`,
//! `POST /api/organizations`, `PATCH /api/organizations/:id` — plus a real
//! `POST /api/timeline/rebuild` over the customer-events sweep. Validation
//! is zod-parity with the incidents convention: violations are 400 with the
//! joined issue messages capped at 300 chars; unknown ids 404.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use super::super::server::AppState;
use crate::people_store::{self, CustomerPatch, NewCustomer};

/// The reference error envelope (incidents convention).
fn bad_request(message: String) -> Response {
    let capped: String = message.chars().take(300).collect();
    (
        StatusCode::BAD_REQUEST,
        Json(json!({
            "statusCode": 400,
            "error": "BadRequest",
            "message": capped
        })),
    )
        .into_response()
}

fn not_found(message: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({
            "statusCode": 404,
            "error": "NotFound",
            "message": message
        })),
    )
        .into_response()
}

fn internal(e: crate::error::Error) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({
            "statusCode": 500,
            "error": "Internal",
            "message": e.to_string()
        })),
    )
        .into_response()
}

/// GET /api/customers — the paginated customer summary list (UI-04,
/// reference people.ts:6-12 + peopleRepo.listCustomers): `q` searches
/// name/email/organization (+ customer_emails) with escaped LIKE
/// wildcards; page clamps mirror the reference `clampListParam`
/// (page 1..100000 default 1, pageSize 1..200 default 50); `total` counts
/// the SAME predicate the rows were drawn from.
pub async fn list_customers(
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let page = clamp_list_param(params.get("page"), 1, 1, 100_000);
    let page_size = clamp_list_param(params.get("pageSize"), 50, 1, 200);
    let query = params
        .get("q")
        .map(|s| s.as_str())
        .filter(|s| !s.trim().is_empty());
    match people_store::list_customers_summary(&conn, query, page, page_size) {
        Ok((customers, total)) => (
            StatusCode::OK,
            Json(json!({"customers": customers, "total": total, "page": page})),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"message": e.to_string(), "customers": [], "total": 0, "page": page})),
        ),
    }
}

/// GET /api/customers/:id — the customer detail (UI-04, reference
/// people.ts:14-56): the full `CustomerDetailData` envelope — customer
/// summary + conversations (50 newest, with assignee names), ratings,
/// AI memories, properties, websites, social profiles, address, recent
/// topics and previous resolutions.
pub async fn get_customer(State(state): State<AppState>, Path(id): Path<i64>) -> impl IntoResponse {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    match people_store::get_customer_detail(&conn, id) {
        Ok(Some(detail)) => (StatusCode::OK, Json(detail)).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({"message": "Customer not found."})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"message": e.to_string()})),
        )
            .into_response(),
    }
}

/// `clampListParam` (reference routes/helpers.ts:10-14): the fallback
/// for absent/empty values, `Number()` semantics for the rest — garbage
/// (NaN/Infinity) falls back, finite numbers truncate toward zero and
/// clamp into `[min, max]`.
fn clamp_list_param(raw: Option<&String>, fallback: i64, min: i64, max: i64) -> i64 {
    match raw {
        Some(s) if !s.is_empty() => s
            .parse::<f64>()
            .ok()
            .filter(|n| n.is_finite())
            .map(|n| (n.trunc() as i64).clamp(min, max))
            .unwrap_or(fallback),
        _ => fallback,
    }
}

/// GET /api/customers/:id/timeline — reference people.ts:86-97: the
/// event-kind timeline over `customer_events` with the kind filter
/// (truncated to 40 chars, empty = unfiltered) and the page clamps
/// (pageSize 1..200 default 100, page 1..100000 default 1, offset =
/// (page-1) * pageSize), serving `{events, total, kind_counts}`; unknown
/// customers answer the reference 404 envelope (existence is a plain
/// `WHERE id = ?` like `getCustomerByLocalId`).
pub async fn customer_timeline(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Response {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let exists: bool = conn
        .query_row(
            "SELECT 1 FROM customers WHERE id = ?1",
            rusqlite::params![id],
            |_| Ok(()),
        )
        .is_ok();
    if !exists {
        return not_found("Customer not found.");
    }
    // `(q.kind ?? '').slice(0, 40) || null` — truncate first, then the
    // empty string means "no filter".
    let kind: Option<String> = params
        .get("kind")
        .map(|k| k.chars().take(40).collect::<String>())
        .filter(|k| !k.is_empty());
    let page_size = clamp_list_param(params.get("pageSize"), 100, 1, 200);
    let offset = (clamp_list_param(params.get("page"), 1, 1, 100000) - 1) * page_size;
    let (events, total) = match crate::customer_events::list_for_customer(
        &conn,
        id,
        kind.as_deref(),
        page_size,
        offset,
    ) {
        Ok(v) => v,
        Err(e) => return internal(e),
    };
    let kind_counts = match crate::customer_events::kind_counts(&conn, id) {
        Ok(v) => v,
        Err(e) => return internal(e),
    };
    (
        StatusCode::OK,
        Json(json!({"events": events, "total": total, "kind_counts": kind_counts})),
    )
        .into_response()
}

/// GET /api/customers/:id/support-health — the no-score support-health
/// report (audit M22 / AN-15; reference people.ts:98-106 +
/// `SupportHealthService.forCustomer`): operational metrics + attention
/// flags + incident exposure, each traceable to conversations, wrapped in
/// the reference `{ report }` envelope.
pub async fn customer_support_health(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Response {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    match people_store::customer_support_health(&conn, id) {
        Ok(Some(report)) => (StatusCode::OK, Json(json!({ "report": report }))).into_response(),
        Ok(None) => not_found("Customer not found."),
        Err(e) => internal(e),
    }
}

/// GET /api/organizations — the real store: org rows with parsed domains,
/// member and conversation counts (was the customers.organization text
/// projection with fake ids).
pub async fn list_organizations(
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let limit = params
        .get("limit")
        .and_then(|l| l.parse::<u32>().ok())
        .unwrap_or(20);
    let query = params
        .get("q")
        .map(|s| s.as_str())
        .filter(|s| !s.is_empty());
    match people_store::list_organizations(&conn, query, limit) {
        Ok(orgs) => (
            StatusCode::OK,
            Json(json!({"organizations": orgs, "total": orgs.len() as i64})),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"organizations": [], "total": 0, "message": e.to_string()})),
        ),
    }
}

/// GET /api/organizations/:id — the real detail payload: org row, members,
/// stats (was a hardcoded 404).
pub async fn get_organization(State(state): State<AppState>, Path(id): Path<i64>) -> Response {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    match people_store::get_organization_detail(&conn, id) {
        Ok(Some(org)) => (StatusCode::OK, Json(org)).into_response(),
        Ok(None) => not_found("Organization not found."),
        Err(e) => internal(e),
    }
}

/// GET /api/organizations/:id/timeline — reference people.ts:108-118:
/// the union of member customers' events with the same kind/page clamps
/// as the customer timeline, serving `{events, total}` where each event
/// carries `customer_name`; unknown organizations (deleted included, like
/// `getOrganizationDetail`'s `deleted_at IS NULL`) answer the reference
/// 404 envelope.
pub async fn organization_timeline(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Response {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let exists: bool = conn
        .query_row(
            "SELECT 1 FROM organizations WHERE id = ?1 AND deleted_at IS NULL",
            rusqlite::params![id],
            |_| Ok(()),
        )
        .is_ok();
    if !exists {
        return not_found("Organization not found.");
    }
    // Same kind truncation/clamps as the customer timeline.
    let kind: Option<String> = params
        .get("kind")
        .map(|k| k.chars().take(40).collect::<String>())
        .filter(|k| !k.is_empty());
    let page_size = clamp_list_param(params.get("pageSize"), 100, 1, 200);
    let offset = (clamp_list_param(params.get("page"), 1, 1, 100000) - 1) * page_size;
    match crate::customer_events::list_for_organization(
        &conn,
        id,
        kind.as_deref(),
        page_size,
        offset,
    ) {
        Ok((events, total)) => (
            StatusCode::OK,
            Json(json!({"events": events, "total": total})),
        )
            .into_response(),
        Err(e) => internal(e),
    }
}

/// GET /api/organizations/:id/support-health — the no-score support-health
/// report over the union of member conversations (reference people.ts:120-128
/// + `SupportHealthService.forOrganization`), in the `{ report }` envelope.
pub async fn organization_support_health(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Response {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    match people_store::organization_support_health(&conn, id) {
        Ok(Some(report)) => (StatusCode::OK, Json(json!({ "report": report }))).into_response(),
        Ok(None) => not_found("Organization not found."),
        Err(e) => internal(e),
    }
}

// ─── Validation helpers (zod-parity) ────────────────────────────────────────

/// A required-or-optional bounded text field. `None` = absent/null,
/// `Some(Err(msg))` = violation. Non-string JSON values are violations.
fn bounded_text(
    body: &Value,
    key: &str,
    max: usize,
    required: bool,
) -> Option<Result<String, String>> {
    match body.get(key) {
        None | Some(Value::Null) => {
            if required {
                Some(Err(format!("{key} is required.")))
            } else {
                None
            }
        }
        Some(Value::String(s)) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                if required {
                    Some(Err(format!("{key} is required.")))
                } else {
                    // An empty optional string is treated as absent.
                    None
                }
            } else if trimmed.chars().count() > max {
                Some(Err(format!("{key} must be at most {max} characters.")))
            } else {
                Some(Ok(trimmed.to_string()))
            }
        }
        Some(_) => Some(Err(format!(
            "Expected string, received non-string for {key}."
        ))),
    }
}

/// An optional bounded text field that distinguishes absent from explicit
/// null (PATCH semantics): `None` = absent, `Some(None)` = clear,
/// `Some(Some(v))` = set.
fn patch_bounded_text(
    body: &Value,
    key: &str,
    max: usize,
) -> Result<Option<Option<String>>, String> {
    match body.get(key) {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(None)),
        Some(Value::String(s)) => {
            let trimmed = s.trim();
            if trimmed.chars().count() > max {
                Err(format!("{key} must be at most {max} characters."))
            } else if trimmed.is_empty() {
                // An empty string clears, like null.
                Ok(Some(None))
            } else {
                Ok(Some(Some(trimmed.to_string())))
            }
        }
        Some(_) => Err(format!("Expected string, received non-string for {key}.")),
    }
}

/// Email shape check (honest-lite: the reference runs a full RFC-style
/// zod.email; the port requires one '@', a non-empty local part, a domain
/// with a dot, and no whitespace — documented in the module doc).
fn check_email(email: &str) -> Result<(), String> {
    let mut parts = email.split('@');
    let local = parts.next().unwrap_or_default();
    let domain = parts.next().unwrap_or_default();
    if parts.next().is_some()
        || local.is_empty()
        || domain.is_empty()
        || !domain.contains('.')
        || email.chars().any(|c| c.is_whitespace())
    {
        Err("Email must be a valid email address.".to_string())
    } else {
        Ok(())
    }
}

/// The domains array: each entry a trimmed non-empty string ≤200 chars, at
/// most 20 entries. Absent → None; null → treated as absent (domains are
/// only ever set, never cleared — an org without domains is `[]`).
fn parse_domains(body: &Value) -> Result<Option<Vec<String>>, String> {
    match body.get("domains") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(items)) => {
            if items.len() > 20 {
                return Err("domains must contain at most 20 entries.".to_string());
            }
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    Value::String(s) => {
                        let trimmed = s.trim();
                        if trimmed.is_empty() {
                            return Err("domains entries must be non-empty strings.".to_string());
                        }
                        if trimmed.chars().count() > 200 {
                            return Err(
                                "domains entries must be at most 200 characters.".to_string()
                            );
                        }
                        out.push(trimmed.to_lowercase());
                    }
                    _ => return Err("Expected string, received non-string in domains.".to_string()),
                }
            }
            Ok(Some(out))
        }
        Some(_) => Err("Expected array, received non-array for domains.".to_string()),
    }
}

// ─── Write routes ───────────────────────────────────────────────────────────

/// POST /api/customers — the reference customerCreateSchema surface:
/// firstName/lastName optional (1..80), at least one of firstName,
/// lastName or email required (a customer with no identity is a 400),
/// email ≤200 + shape-checked, organization/jobTitle ≤200, phone ≤60.
/// Response: `{ok, customer}` with the same payload GET /api/customers/:id
/// serves.
pub async fn create_customer(State(state): State<AppState>, Json(body): Json<Value>) -> Response {
    let mut issues: Vec<String> = Vec::new();

    let first_name = match bounded_text(&body, "firstName", 80, false) {
        Some(Ok(v)) => Some(v),
        Some(Err(e)) => {
            issues.push(e);
            None
        }
        None => None,
    };
    let last_name = match bounded_text(&body, "lastName", 80, false) {
        Some(Ok(v)) => Some(v),
        Some(Err(e)) => {
            issues.push(e);
            None
        }
        None => None,
    };
    let email = match bounded_text(&body, "email", 200, false) {
        Some(Ok(v)) => {
            if let Err(e) = check_email(&v) {
                issues.push(e);
            }
            Some(v)
        }
        Some(Err(e)) => {
            issues.push(e);
            None
        }
        None => None,
    };
    if first_name.is_none() && last_name.is_none() && email.is_none() {
        issues.push("At least one of firstName, lastName or email is required.".to_string());
    }
    let organization = match bounded_text(&body, "organization", 200, false) {
        Some(Ok(v)) => Some(v),
        Some(Err(e)) => {
            issues.push(e);
            None
        }
        None => None,
    };
    let job_title = match bounded_text(&body, "jobTitle", 200, false) {
        Some(Ok(v)) => Some(v),
        Some(Err(e)) => {
            issues.push(e);
            None
        }
        None => None,
    };
    let phone = match bounded_text(&body, "phone", 60, false) {
        Some(Ok(v)) => Some(v),
        Some(Err(e)) => {
            issues.push(e);
            None
        }
        None => None,
    };

    if !issues.is_empty() {
        return bad_request(issues.join(" "));
    }

    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let new = NewCustomer {
        first_name,
        last_name,
        email,
        organization,
        job_title,
        phone,
    };
    match people_store::create_customer(&conn, &new) {
        Ok(id) => match crate::customers::get_customer(&conn, id) {
            Ok(Some(c)) => {
                (StatusCode::OK, Json(json!({"ok": true, "customer": c}))).into_response()
            }
            _ => (StatusCode::OK, Json(json!({"ok": true, "customer_id": id}))).into_response(),
        },
        Err(e) => internal(e),
    }
}

/// PATCH /api/customers/:id — the reference customerPatchSchema semantics:
/// present fields are validated and updated, explicit nulls (or empty
/// strings) clear, absent fields are untouched. An empty patch is a 400;
/// unknown ids 404. Response: `{ok, customer}`.
pub async fn update_customer(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> Response {
    let mut issues: Vec<String> = Vec::new();

    let first_name = match patch_bounded_text(&body, "firstName", 80) {
        Ok(v) => v,
        Err(e) => {
            issues.push(e);
            None
        }
    };
    let last_name = match patch_bounded_text(&body, "lastName", 80) {
        Ok(v) => v,
        Err(e) => {
            issues.push(e);
            None
        }
    };
    let email = match patch_bounded_text(&body, "email", 200) {
        Ok(v) => {
            if let Some(Some(e)) = v.as_ref() {
                if let Err(msg) = check_email(e) {
                    issues.push(msg);
                }
            }
            v
        }
        Err(e) => {
            issues.push(e);
            None
        }
    };
    let organization = match patch_bounded_text(&body, "organization", 200) {
        Ok(v) => v,
        Err(e) => {
            issues.push(e);
            None
        }
    };
    let job_title = match patch_bounded_text(&body, "jobTitle", 200) {
        Ok(v) => v,
        Err(e) => {
            issues.push(e);
            None
        }
    };
    let phone = match patch_bounded_text(&body, "phone", 60) {
        Ok(v) => v,
        Err(e) => {
            issues.push(e);
            None
        }
    };

    if !issues.is_empty() {
        return bad_request(issues.join(" "));
    }

    let patch = CustomerPatch {
        first_name,
        last_name,
        email,
        organization,
        job_title,
        phone,
    };
    if patch.is_empty() {
        return bad_request("No fields to update.".to_string());
    }

    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    match people_store::update_customer(&conn, id, &patch) {
        Ok(true) => match crate::customers::get_customer(&conn, id) {
            Ok(Some(c)) => {
                (StatusCode::OK, Json(json!({"ok": true, "customer": c}))).into_response()
            }
            _ => (StatusCode::OK, Json(json!({"ok": true, "customer_id": id}))).into_response(),
        },
        Ok(false) => not_found("Customer not found."),
        Err(e) => internal(e),
    }
}

/// POST /api/organizations — name required (1..200), domains an optional
/// array of trimmed non-empty strings (≤20 entries, each ≤200 chars,
/// lowercased). Response: `{ok, organization}` with the detail payload.
pub async fn create_organization(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let mut issues: Vec<String> = Vec::new();

    let name = match bounded_text(&body, "name", 200, true) {
        Some(Ok(v)) => Some(v),
        Some(Err(e)) => {
            issues.push(e);
            None
        }
        None => None,
    };
    let domains = match parse_domains(&body) {
        Ok(v) => v,
        Err(e) => {
            issues.push(e);
            None
        }
    };

    if !issues.is_empty() {
        return bad_request(issues.join(" "));
    }
    let Some(name) = name else {
        return bad_request("name is required.".to_string());
    };

    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    match people_store::create_organization(&conn, &name, domains.as_deref().unwrap_or(&[])) {
        Ok(id) => match people_store::get_organization_detail(&conn, id) {
            Ok(Some(org)) => (
                StatusCode::OK,
                Json(json!({"ok": true, "organization": org})),
            )
                .into_response(),
            _ => (
                StatusCode::OK,
                Json(json!({"ok": true, "organization_id": id})),
            )
                .into_response(),
        },
        Err(e) => internal(e),
    }
}

/// PATCH /api/organizations/:id — name (1..200) and/or domains (array)
/// with the same PATCH semantics; renaming keeps the linked customers'
/// legacy text column in step. Empty patch 400; unknown ids 404.
/// Response: `{ok, organization}`.
pub async fn update_organization(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> Response {
    let mut issues: Vec<String> = Vec::new();

    let name = match bounded_text(&body, "name", 200, false) {
        Some(Ok(v)) => Some(v),
        Some(Err(e)) => {
            issues.push(e);
            None
        }
        None => None,
    };
    let domains = match parse_domains(&body) {
        Ok(v) => v,
        Err(e) => {
            issues.push(e);
            None
        }
    };

    if !issues.is_empty() {
        return bad_request(issues.join(" "));
    }
    if name.is_none() && domains.is_none() {
        return bad_request("No fields to update.".to_string());
    }

    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    match people_store::update_organization(&conn, id, name.as_deref(), domains.as_deref()) {
        Ok(true) => match people_store::get_organization_detail(&conn, id) {
            Ok(Some(org)) => (
                StatusCode::OK,
                Json(json!({"ok": true, "organization": org})),
            )
                .into_response(),
            _ => (
                StatusCode::OK,
                Json(json!({"ok": true, "organization_id": id})),
            )
                .into_response(),
        },
        Ok(false) => not_found("Organization not found."),
        Err(e) => internal(e),
    }
}

/// POST /api/timeline/rebuild — reference people.ts:131-135: the full
/// idempotent re-derivation, answering `{ok, created, message}` and
/// writing the `customer_events_rebuilt` audit row with
/// `{created}`. (The port additionally resolves organization links on
/// the way — an idempotent port-side nicety that keeps org timelines
/// resolvable; it stays off the wire like in the reference.)
pub async fn timeline_rebuild(State(state): State<AppState>) -> Response {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    match people_store::timeline_rebuild(&conn) {
        Ok(created) => {
            if let Err(e) = crate::audit::audit(
                &conn,
                &crate::audit::AuditEntry::user("customer_events_rebuilt")
                    .with_after_state(json!({"created": created})),
            ) {
                return internal(e);
            }
            (
                StatusCode::OK,
                Json(json!({
                    "ok": true,
                    "created": created,
                    "message": format!("Timeline rebuilt; {created} new event(s) derived.")
                })),
            )
                .into_response()
        }
        Err(e) => internal(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::IntoResponse as _;
    use rusqlite::Connection;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    fn fresh_db() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::bootstrap::apply_all(&mut conn).unwrap();
        conn
    }

    fn make_state() -> AppState {
        AppState {
            conn: Arc::new(Mutex::new(fresh_db())),
            data_dir: std::path::PathBuf::from("/tmp"),
            port: 3000,
            host: "127.0.0.1".into(),
            demo_mode: false,
            bus: crate::http::EventBus::new(64),
            limiter: crate::http::RateLimiter::new(),
            sync: None,
            real: None,
            provider_kind: "fake".into(),
            workers: None,
            qdrant: std::sync::Arc::new(crate::vectorstore_qdrant::EmbeddedQdrant::new(
                "/tmp/spp-test-qdrant",
                "http://127.0.0.1:6333",
                false,
            )),
        }
    }

    /// AppState is not Clone (Arc contents shared) — build a second handle
    /// over the same connection for sequential route calls in one test.
    fn state_clone(state: &AppState) -> AppState {
        AppState {
            conn: Arc::clone(&state.conn),
            data_dir: std::path::PathBuf::from("/tmp"),
            port: 3000,
            host: "127.0.0.1".into(),
            demo_mode: false,
            bus: crate::http::EventBus::new(64),
            limiter: crate::http::RateLimiter::new(),
            sync: None,
            real: None,
            provider_kind: "fake".into(),
            workers: None,
            qdrant: std::sync::Arc::clone(&state.qdrant),
        }
    }

    async fn body_json(response: Response) -> (StatusCode, Value) {
        let response = response.into_response();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&bytes).unwrap();
        (status, json)
    }

    #[tokio::test]
    async fn customer_create_validates_and_persists() {
        let state = make_state();
        // No identity at all → 400.
        let (status, body) =
            body_json(create_customer(State(state_clone(&state)), Json(json!({}))).await).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            body["message"]
                .as_str()
                .unwrap()
                .contains("At least one of firstName, lastName or email"),
            "{body}"
        );
        // Bad email → 400.
        let (status, body) = body_json(
            create_customer(
                State(state_clone(&state)),
                Json(json!({"email": "not-an-email"})),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body["message"].as_str().unwrap().contains("valid email"));
        // Valid create → the served payload round-trips.
        let (status, body) = body_json(
            create_customer(
                State(state_clone(&state)),
                Json(json!({
                    "firstName": "Ada",
                    "lastName": "Lovelace",
                    "email": "ada@example.com",
                    "jobTitle": "Engineer"
                })),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["customer"]["first_name"], json!("Ada"));
        assert_eq!(body["customer"]["email"], json!("ada@example.com"));
    }

    #[tokio::test]
    async fn customer_patch_semantics() {
        let state = make_state();
        let (status, body) = body_json(
            create_customer(
                State(state_clone(&state)),
                Json(json!({"firstName": "Grace", "lastName": "Hopper"})),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let id = body["customer"]["id"].as_i64().unwrap();

        // Empty patch → 400.
        let (status, _) =
            body_json(update_customer(State(state_clone(&state)), Path(id), Json(json!({}))).await)
                .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        // Present-null clears, absent untouched.
        let (status, body) = body_json(
            update_customer(
                State(state_clone(&state)),
                Path(id),
                Json(json!({"lastName": null, "phone": "+1 555 0100"})),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["customer"]["last_name"], Value::Null);
        assert_eq!(body["customer"]["first_name"], json!("Grace"));
        assert_eq!(body["customer"]["phone"], json!("+1 555 0100"));
        // Unknown id → 404.
        let (status, _) = body_json(
            update_customer(
                State(state_clone(&state)),
                Path(99999),
                Json(json!({"firstName": "X"})),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn organization_crud_and_support_health() {
        let state = make_state();
        // Missing name → 400.
        let (status, _) =
            body_json(create_organization(State(state_clone(&state)), Json(json!({}))).await).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        // Bad domains shape → 400.
        let (status, _) = body_json(
            create_organization(
                State(state_clone(&state)),
                Json(json!({"name": "Acme", "domains": "acme.com"})),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        // Valid create.
        let (status, body) = body_json(
            create_organization(
                State(state_clone(&state)),
                Json(json!({"name": "Acme", "domains": ["Acme.COM"]})),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["organization"]["name"], json!("Acme"));
        // Domains are lowercased (the sync engine's stored format).
        assert_eq!(body["organization"]["domains"], json!(["acme.com"]));
        let id = body["organization"]["id"].as_i64().unwrap();

        // Support health: the no-score report in the { report } envelope —
        // metrics + flags + incident exposure, never a single verdict.
        let (status, health) =
            body_json(organization_support_health(State(state_clone(&state)), Path(id)).await)
                .await;
        assert_eq!(status, StatusCode::OK);
        let report = &health["report"];
        assert_eq!(report["subject_kind"], json!("organization"));
        assert_eq!(report["subject_label"], json!("Acme"));
        assert!(report.get("health").is_none(), "no verdict key by design");
        assert!(
            report["metrics"]
                .as_array()
                .unwrap()
                .iter()
                .any(|m| m["key"] == json!("support_volume_total")),
            "metric set present: {}",
            report["metrics"]
        );
        assert_eq!(report["flags"].as_array().unwrap().len(), 0);
        assert_eq!(report["incident_exposure"].as_array().unwrap().len(), 0);

        // Rename keeps the shape; unknown id → 404.
        let (status, body) = body_json(
            update_organization(
                State(state_clone(&state)),
                Path(id),
                Json(json!({"name": "Acme Corp"})),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["organization"]["name"], json!("Acme Corp"));
        let (status, _) = body_json(
            update_organization(
                State(state_clone(&state)),
                Path(99999),
                Json(json!({"name": "X"})),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn customer_support_health_unknown_id_404s() {
        let state = make_state();
        let (status, _) =
            body_json(customer_support_health(State(state_clone(&state)), Path(99999)).await).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn timeline_rebuild_reports_real_counts() {
        let state = make_state();
        let (status, body) = body_json(timeline_rebuild(State(state_clone(&state))).await).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body["ok"].as_bool().unwrap());
        // `created` is a number (0 on an empty db is fine — the point is
        // the route ran the real sweep, not a fake "queued" message), and
        // the reference message shape.
        assert!(body["created"].is_i64());
        assert_eq!(
            body["message"],
            json!(format!(
                "Timeline rebuilt; {} new event(s) derived.",
                body["created"].as_i64().unwrap()
            ))
        );
        // The reference audit row (one, with the created count).
        {
            let conn = state.conn.lock().unwrap();
            let (action, after): (String, Value) = conn
                .query_row(
                    "SELECT action, after_state FROM audit_log WHERE action = 'customer_events_rebuilt'",
                    [],
                    |r| Ok((r.get(0)?, serde_json::from_str::<Value>(&r.get::<_, String>(1)?).unwrap_or_default())),
                )
                .unwrap();
            assert_eq!(action, "customer_events_rebuilt");
            assert_eq!(after["created"], body["created"]);
        }
    }

    #[tokio::test]
    async fn list_organizations_serves_real_rows() {
        let state = make_state();
        {
            let conn = state.conn.lock().unwrap();
            people_store::create_organization(&conn, "Acme", &["acme.com".into()]).unwrap();
        }
        let mut params = HashMap::new();
        params.insert("limit".to_string(), "50".to_string());
        let (status, body) = body_json(
            list_organizations(State(state_clone(&state)), Query(params))
                .await
                .into_response(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["total"], json!(1));
        assert_eq!(body["organizations"][0]["name"], json!("Acme"));
        assert_eq!(body["organizations"][0]["domains"], json!(["acme.com"]));
    }
}
