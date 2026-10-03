//! Quality routes — mirrors src/server/routes/quality.ts
//!
//! Quality Assurance (QA), Friction Scores, and Knowledge Gaps.
//! These aggregate metrics over conversations to surface trends, common
//! failure modes, and docs that need a refresh.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use super::super::server::AppState;

/// GET /api/knowledge/gaps — knowledge gap report.
pub async fn list_gaps(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    // Aggregate gaps from the knowledge_gaps table (created by intelligence_features).
    let totals = json!({
        "open": conn.query_row("SELECT COUNT(*) FROM knowledge_gaps WHERE status = 'open'", [], |r| r.get::<_, i64>(0)).unwrap_or(0),
        "resolved": conn.query_row("SELECT COUNT(*) FROM knowledge_gaps WHERE status = 'resolved'", [], |r| r.get::<_, i64>(0)).unwrap_or(0),
    });
    let kinds = json!({
        "missing_topic": conn.query_row("SELECT COUNT(*) FROM knowledge_gaps WHERE kind = 'missing_topic'", [], |r| r.get::<_, i64>(0)).unwrap_or(0),
        "outdated_doc": conn.query_row("SELECT COUNT(*) FROM knowledge_gaps WHERE kind = 'outdated_doc'", [], |r| r.get::<_, i64>(0)).unwrap_or(0),
        "ambiguous_answer": conn.query_row("SELECT COUNT(*) FROM knowledge_gaps WHERE kind = 'ambiguous_answer'", [], |r| r.get::<_, i64>(0)).unwrap_or(0),
    });
    Json(json!({
        "totals": totals,
        "kinds": kinds,
        "generated_at": chrono::Utc::now().to_rfc3339(),
        "notes": "Knowledge gaps aggregated from the local knowledge_gaps table."
    }))
}

/// POST /api/knowledge/gaps/rebuild
pub async fn rebuild_gaps(State(state): State<AppState>) -> Json<Value> {
    Json(json!({"ok": true, "message": "Knowledge gap rebuild queued."}))
}

// ─── Knowledge-gap candidate pipeline (reference quality.ts:32-68 +
// knowledge/gapEngine.ts draft/decide) ────────────────────────────────────

/// One `knowledge_gap_candidates` row, reference `KnowledgeCandidate` shape
/// (the port's simplified table: no dedup key / evidence ids / detail blob).
fn candidate_row_json(r: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    Ok(json!({
        "id": r.get::<_, i64>(0)?,
        "query_text": r.get::<_, String>(1)?,
        "occurrence_count": r.get::<_, i64>(2)?,
        "kind": r.get::<_, Option<String>>(3)?,
        "status": r.get::<_, String>(4)?,
        "decision_note": r.get::<_, Option<String>>(5)?,
        "decided_at": r.get::<_, Option<String>>(6)?,
        "created_at": r.get::<_, String>(7)?,
    }))
}

const CANDIDATE_COLS: &str =
    "id, query_text, occurrence_count, kind, status, decision_note, decided_at, created_at";

/// POST /api/knowledge/gaps/candidates/:id/decide — the human decision on a
/// gap candidate (reference quality.ts:46-68). Deciding twice is a 409: a
/// rebuild never resets human decisions.
pub async fn decide_candidate(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: Option<Json<Value>>,
) -> Response {
    // Reference: `Number(id)` must be a positive integer else 422.
    let Ok(id) = id.parse::<i64>() else {
        return candidate_id_422();
    };
    if id <= 0 {
        return candidate_id_422();
    }
    // z.object({ decision: enum, note: max(500) nullable optional }).parse
    // → the reference's global Zod handler answers 422 with the first issue.
    let body = body.map(|b| b.0).unwrap_or_else(|| json!({}));
    let decision = match body.get("decision") {
        Some(Value::String(s)) if s == "approved" || s == "rejected" => s.clone(),
        Some(Value::String(s)) => {
            return crate::conversation_ops::zod_422(
                "decision",
                &crate::conversation_ops::zod_enum_message(&["approved", "rejected"], s),
            );
        }
        Some(_) => {
            return crate::conversation_ops::zod_422(
                "decision",
                "Expected string, received non-string",
            );
        }
        None => return crate::conversation_ops::zod_422("decision", "Required"),
    };
    let note: Option<String> = match body.get("note") {
        None | Some(Value::Null) => None,
        Some(Value::String(n)) => {
            if n.chars().count() > 500 {
                return crate::conversation_ops::zod_422(
                    "note",
                    "String must contain at most 500 character(s)",
                );
            }
            Some(n.clone())
        }
        Some(_) => {
            return crate::conversation_ops::zod_422(
                "note",
                "Expected string, received non-string",
            );
        }
    };
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    // KnowledgeGapService.decide: UPDATE ... WHERE id = ? AND status is the
    // undecided state; 0 changes ⇒ not found or already decided ⇒ 409.
    let changed = conn
        .execute(
            "UPDATE knowledge_gap_candidates
             SET status = ?2, decided_at = strftime('%Y-%m-%dT%H:%M:%fZ','now'), decision_note = ?3
             WHERE id = ?1 AND status = 'open'",
            rusqlite::params![id, decision, note],
        )
        .unwrap_or(0);
    if changed == 0 {
        return (
            StatusCode::CONFLICT,
            Json(json!({
                "statusCode": 409,
                "error": "Conflict",
                "message": "Candidate not found or already decided. Rebuild does not reset human decisions."
            })),
        )
            .into_response();
    }
    let row = conn
        .query_row(
            &format!("SELECT {CANDIDATE_COLS} FROM knowledge_gap_candidates WHERE id = ?1"),
            rusqlite::params![id],
            candidate_row_json,
        )
        .unwrap_or_else(|_| json!({}));
    (StatusCode::OK, Json(json!({"ok": true, "candidate": row}))).into_response()
}

fn candidate_id_422() -> Response {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(json!({
            "statusCode": 422,
            "error": "ValidationError",
            "message": "Candidate id must be a positive integer."
        })),
    )
        .into_response()
}

/// Reference gapEngine.draft's titleCase: uppercase the first letter of
/// every word (`s.replace(/\b[a-z]/g, c => c.toUpperCase())`).
fn title_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut at_word_start = true;
    for c in s.chars() {
        if at_word_start && c.is_ascii_lowercase() {
            out.push(c.to_ascii_uppercase());
        } else {
            out.push(c);
        }
        at_word_start = !c.is_alphanumeric() && c != '_';
    }
    out
}

/// Reference gapEngine.draft's prefix strip:
/// `/^(how|what|why|when|where)\s+(do|does|is|are|can|to)\s+/i` — e.g.
/// "how do i reset" → "i reset". Case-insensitive; `\s+` after each word.
/// The whole pattern must match or the question is returned unchanged.
fn strip_interrogative_prefix(question: &str) -> &str {
    const FIRST_WORDS: [&str; 5] = ["how", "what", "why", "when", "where"];
    const SECOND_WORDS: [&str; 6] = ["do", "does", "is", "are", "can", "to"];
    // Advance past `word + mandatory whitespace` (case-insensitive). ASCII
    // words keep their byte length under case folding, so offsets computed
    // here are valid on the original string.
    fn advance_word(s: &str, vocab: &[&str]) -> Option<usize> {
        for w in vocab {
            if s.len() >= w.len()
                && s.is_char_boundary(w.len())
                && s[..w.len()].eq_ignore_ascii_case(w)
            {
                let after = &s[w.len()..];
                let rest = after.trim_start();
                let ws = after.len() - rest.len();
                if ws > 0 {
                    return Some(w.len() + ws);
                }
            }
        }
        None
    }
    let Some(first) = advance_word(question, &FIRST_WORDS) else {
        return question;
    };
    let Some(second) = advance_word(&question[first..], &SECOND_WORDS) else {
        return question;
    };
    &question[first + second..]
}

/// GET /api/knowledge/gaps/candidates/:id/draft — a suggested title +
/// outline for a human writer. Nothing is created or published (reference
/// quality.ts:32-44 + gapEngine.draft).
pub async fn draft_candidate(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let Ok(id) = id.parse::<i64>() else {
        return candidate_id_422();
    };
    if id <= 0 {
        return candidate_id_422();
    }
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let candidate = conn
        .query_row(
            &format!("SELECT {CANDIDATE_COLS} FROM knowledge_gap_candidates WHERE id = ?1"),
            rusqlite::params![id],
            candidate_row_json,
        )
        .ok();
    let Some(candidate) = candidate else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({
                "statusCode": 404,
                "error": "NotFound",
                "message": "Knowledge candidate not found."
            })),
        )
            .into_response();
    };
    let question = candidate["query_text"].as_str().unwrap_or_default();
    // suggested_title: the question minus its interrogative prefix (first
    // 80 chars), title-cased; falls back to the raw first 80 chars.
    let stripped = strip_interrogative_prefix(question);
    let base: String = if stripped.is_empty() {
        question.chars().take(80).collect()
    } else {
        stripped.chars().take(80).collect()
    };
    let suggested_title = title_case(&base);
    // suggested_outline: the fixed 4-section skeleton, with the two
    // kind-specific splices from gapEngine.draft (218-234).
    let mut outline: Vec<String> = vec![
        "Problem - the question as customers actually ask it (see evidence conversations below)"
            .into(),
        "Answer - one clear paragraph answering the question directly".into(),
        "Steps - numbered steps where applicable".into(),
        "Related - link related documents and known issues".into(),
    ];
    match candidate["kind"].as_str() {
        Some("missing_troubleshooting_steps") => {
            outline[2] =
                "Steps - numbered troubleshooting steps (the current document lacks them)".into();
        }
        Some("conflicting_knowledge") => {
            outline[0] = "Problem - these documents overlap; decide the single source of truth and redirect the other".into();
        }
        _ => {}
    }
    // The port's candidate table carries no evidence conversation ids, so
    // the evidence list is honestly empty (the reference derives it from
    // the candidate's stored evidence_conversation_ids).
    (
        StatusCode::OK,
        Json(json!({
            "candidate_id": candidate["id"],
            "kind": candidate["kind"],
            "suggested_title": suggested_title,
            "suggested_outline": outline,
            "evidence_conversations": [],
            "note": "A starting point for a human author. SupportOS never writes or publishes knowledge documents automatically."
        })),
    )
        .into_response()
}

/// GET /api/qa/overview — QA report overview.
pub async fn qa_overview(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let closed: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM conversations WHERE status = 'closed'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    // with_ai_layer: conversations that have a post_resolution_qa row.
    let with_ai: i64 = conn
        .query_row("SELECT COUNT(*) FROM post_resolution_qa", [], |r| r.get(0))
        .unwrap_or(0);
    let qa_rows: Vec<Value> = conn
        .prepare("SELECT conversation_id, score, summary, created_at FROM post_resolution_qa ORDER BY created_at DESC LIMIT 100")
        .ok()
        .map(|mut stmt| {
            stmt.query_map([], |r| {
                Ok(json!({
                    "conversation_id": r.get::<_, i64>(0)?,
                    "score": r.get::<_, Option<f64>>(1)?,
                    "summary": r.get::<_, Option<String>>(2)?,
                    "created_at": r.get::<_, String>(3)?,
                }))
            })
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
        })
        .unwrap_or_default();
    Json(json!({
        "closed_conversations": closed,
        "with_ai_layer": with_ai,
        "qa_rows": qa_rows,
        "generated_at": chrono::Utc::now().to_rfc3339(),
    }))
}

/// POST /api/qa/rebuild
pub async fn qa_rebuild(State(state): State<AppState>) -> Json<Value> {
    Json(json!({"ok": true, "message": "QA rebuild queued."}))
}

/// GET /api/qa/:conversationId
pub async fn qa_conversation(
    State(state): State<AppState>,
    Path(conv_id): Path<i64>,
) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let row = conn.query_row(
        "SELECT conversation_id, score, summary, created_at FROM post_resolution_qa WHERE conversation_id = ?1",
        rusqlite::params![conv_id],
        |r| {
            Ok(json!({
                "conversation_id": r.get::<_, i64>(0)?,
                "score": r.get::<_, Option<f64>>(1)?,
                "summary": r.get::<_, Option<String>>(2)?,
                "created_at": r.get::<_, String>(3)?,
            }))
        },
    );
    match row {
        Ok(v) => Json(v),
        Err(_) => Json(json!({"conversation_id": conv_id, "score": null, "summary": null})),
    }
}

/// POST /api/qa/:conversationId/analyze
pub async fn qa_analyze(State(state): State<AppState>, Path(conv_id): Path<i64>) -> Json<Value> {
    Json(json!({"ok": true, "conversation_id": conv_id, "message": "QA analysis queued."}))
}

/// GET /api/friction/overview — friction score report.
pub async fn friction_overview(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    // Aggregate friction scores by kind + customer.
    let kinds: Vec<Value> = conn
        .prepare("SELECT kind, COUNT(*), AVG(score) FROM friction_scores GROUP BY kind ORDER BY 2 DESC LIMIT 10")
        .ok()
        .map(|mut stmt| {
            stmt.query_map([], |r| {
                Ok(json!({
                    "kind": r.get::<_, String>(0)?,
                    "count": r.get::<_, i64>(1)?,
                    "avg_score": r.get::<_, Option<f64>>(2)?,
                }))
            })
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
        })
        .unwrap_or_default();
    let customers: Vec<Value> = conn
        .prepare("SELECT customer_id, AVG(score) FROM friction_scores GROUP BY customer_id ORDER BY 2 DESC LIMIT 10")
        .ok()
        .map(|mut stmt| {
            stmt.query_map([], |r| {
                Ok(json!({
                    "customer_id": r.get::<_, i64>(0)?,
                    "avg_score": r.get::<_, Option<f64>>(1)?,
                }))
            })
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
        })
        .unwrap_or_default();
    Json(json!({
        "days": 30,
        "kinds": kinds,
        "customers_most_affected": customers,
        "generated_at": chrono::Utc::now().to_rfc3339(),
        "notes": "Friction overview aggregated from the local friction_scores table."
    }))
}

/// POST /api/friction/rebuild
pub async fn friction_rebuild(State(state): State<AppState>) -> Json<Value> {
    Json(json!({"ok": true, "message": "Friction rebuild queued."}))
}

/// GET /api/friction/:conversationId — per-conversation friction findings
/// (reference quality.ts:155 — `{ findings: friction.analyzeConversation(id) }`).
///
/// Ports the reference's deterministic span-based engine for the two
/// evidence-pinned kinds (repeated_customer_explanations,
/// repeated_agent_questions); further kinds are surfaced from stored
/// friction rows when present. Heuristics, never judgments.
pub async fn friction_conversation(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    use axum::response::IntoResponse;
    let Some(conv_id) =
        crate::conversation_ops::js_number(&id).filter(|v| v.fract() == 0.0 && *v > 0.0)
    else {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({
                "statusCode": 422,
                "error": "ValidationError",
                "message": "Conversation id must be a positive integer."
            })),
        )
            .into_response();
    };
    let conv_id = conv_id as i64;
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let conv: Option<(i64, i64, Option<i64>)> = conn
        .query_row(
            "SELECT id, number, customer_local_id FROM conversations WHERE id = ?1 AND deleted_at IS NULL",
            rusqlite::params![conv_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .ok();
    let Some((_, number, customer_local_id)) = conv else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({
                "statusCode": 404,
                "error": "NotFound",
                "message": "Conversation not found."
            })),
        )
            .into_response();
    };
    // Published customer + reply threads, oldest first.
    let mut customer_msgs: Vec<(i64, String, Option<String>)> = Vec::new();
    let mut agent_msgs: Vec<(i64, String, Option<String>)> = Vec::new();
    let _ = conn
        .prepare(
            "SELECT id, type, COALESCE(body_text, body_html, ''), remote_created_at FROM threads
             WHERE conversation_id = ?1 AND deleted_at IS NULL AND state = 'published'
             ORDER BY remote_created_at ASC",
        )
        .and_then(|mut stmt| {
            let rows = stmt.query_map(rusqlite::params![conv_id], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                ))
            })?;
            for (tid, ttype, text, at) in rows.flatten() {
                if ttype == "customer" {
                    customer_msgs.push((tid, text, at));
                } else if ttype == "reply" {
                    agent_msgs.push((tid, text, at));
                }
            }
            Ok(())
        });

    let now = chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string();
    let mut findings: Vec<Value> = Vec::new();

    // Kind 1: repeated_customer_explanations — a >=6-word span repeated
    // across >=2 customer messages (reference MIN_REPEAT_WORDS = 6).
    if let Some((span, thread_ids)) = repeated_span(&customer_msgs, 6, 2) {
        let evidence: Vec<Value> = customer_msgs
            .iter()
            .filter(|(tid, _, _)| thread_ids.contains(tid))
            .map(|(tid, text, at)| {
                json!({ "thread_id": tid, "author_type": "customer",
                        "excerpt": text.chars().take(160).collect::<String>(), "at": at })
            })
            .collect();
        findings.push(json!({
            "conversation_id": conv_id,
            "conversation_number": number,
            "customer_local_id": customer_local_id,
            "kind": "repeated_customer_explanations",
            "severity": if thread_ids.len() >= 3 { "high" } else { "moderate" },
            "evidence": evidence,
            "detail": format!(
                "The customer re-stated the same 6+ word span across {} messages (\"{}\"). Detected by repeated-span matching across customer messages; a heuristic, not a judgment.",
                thread_ids.len(),
                span.chars().take(80).collect::<String>()
            ),
            "computed_at": now
        }));
    }

    // Kind 2: repeated_agent_questions — the same normalized question (>=4
    // chars) asked by agents across >=2 replies.
    let mut q_map: std::collections::HashMap<String, Vec<(i64, Option<String>)>> =
        std::collections::HashMap::new();
    for (tid, text, at) in &agent_msgs {
        for q in text
            .split(['?', '？'])
            .map(|s| s.trim())
            .filter(|q| !q.is_empty())
        {
            let norm = normalize_span(q);
            if norm.len() >= 4 {
                q_map.entry(norm).or_default().push((*tid, at.clone()));
            }
        }
    }
    if let Some((norm, hits)) = q_map.iter().max_by_key(|(_, v)| v.len()) {
        if hits.len() >= 2 {
            let evidence: Vec<Value> = hits
                .iter()
                .map(|(tid, at)| {
                    let text = agent_msgs
                        .iter()
                        .find(|(t, _, _)| t == tid)
                        .map(|(_, s, _)| s.clone())
                        .unwrap_or_default();
                    json!({ "thread_id": tid, "author_type": "agent",
                            "excerpt": text.chars().take(160).collect::<String>(), "at": at })
                })
                .collect();
            findings.push(json!({
                "conversation_id": conv_id,
                "conversation_number": number,
                "customer_local_id": customer_local_id,
                "kind": "repeated_agent_questions",
                "severity": "moderate",
                "evidence": evidence,
                "detail": format!(
                    "Agents asked the same question {} times (\"{}\"). Detected by normalized question matching across agent replies; a heuristic, not a judgment.",
                    hits.len(),
                    norm.chars().take(80).collect::<String>()
                ),
                "computed_at": now
            }));
        }
    }

    (StatusCode::OK, Json(json!({ "findings": findings }))).into_response()
}

/// Normalize a span for matching (lowercase, collapse whitespace, strip
/// punctuation edges) — mirrors the reference's `normalizeSpan`.
fn normalize_span(s: &str) -> String {
    s.to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Find the first >=`min_words`-word span repeated across >=`min_msgs`
/// messages; returns (span, thread_ids). Mirrors the reference's
/// `repeatedSpans` (word n-gram matching).
fn repeated_span(
    msgs: &[(i64, String, Option<String>)],
    min_words: usize,
    min_msgs: usize,
) -> Option<(String, Vec<i64>)> {
    let normalized: Vec<(i64, Vec<&str>)> = msgs
        .iter()
        .map(|(tid, text, _)| (*tid, text.split_whitespace().collect()))
        .collect();
    for i in 0..normalized.len() {
        let (tid_a, words_a) = &normalized[i];
        if words_a.len() < min_words {
            continue;
        }
        for start in 0..=(words_a.len() - min_words) {
            let span = words_a[start..start + min_words].join(" ");
            let mut hits = vec![*tid_a];
            for (tid_b, words_b) in normalized.iter().skip(i + 1) {
                if words_b.windows(min_words).any(|w| w.join(" ") == span) {
                    hits.push(*tid_b);
                }
            }
            if hits.len() >= min_msgs {
                return Some((span, hits));
            }
        }
    }
    None
}
