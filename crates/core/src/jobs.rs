//! Job queue: enqueue, claim, execute, retry with backoff, dead-letter.
//!
//! Schema and queue semantics mirror the reference `jobRepo.ts`
//! (`src/server/database/repositories/jobRepo.ts`):
//! - `jobs(queue, type, priority, status, payload, attempt, max_attempts,
//!    error, run_at, locked_by, locked_at, created_at, started_at, completed_at)`
//! - status vocabulary: `queued | running | completed | failed | cancelled |
//!    awaiting_approval`
//! - retrying an `awaiting_approval` job patches `approved:true` into the
//!   payload (the approve gesture); 409 when not retryable.
//! - outbound write queue (`outbound_jobs` + `outbound_attempts`) and the
//!   `audit_log` / `application_errors` tables.
//!
//! Per KNOWN PITFALLS: "Job claim loops must be tested end to end (enqueue,
//! claim, execute), not by calling components directly."

use rusqlite::{params, Connection};

use crate::error::{Error, Result};

/// Reference status vocabulary for the `jobs.status` column.
pub mod status {
    pub const QUEUED: &str = "queued";
    pub const RUNNING: &str = "running";
    pub const COMPLETED: &str = "completed";
    pub const FAILED: &str = "failed";
    pub const CANCELLED: &str = "cancelled";
    pub const AWAITING_APPROVAL: &str = "awaiting_approval";
}

/// Create the `jobs` table if it does not exist (reference shape). Idempotent.
pub fn ensure_jobs_table(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS jobs (
            id            INTEGER PRIMARY KEY AUTOINCREMENT,
            queue         TEXT    NOT NULL,
            type          TEXT    NOT NULL,
            priority      INTEGER NOT NULL DEFAULT 2,
            status        TEXT    NOT NULL DEFAULT 'queued',
            payload       TEXT,
            attempt       INTEGER DEFAULT 0,
            max_attempts  INTEGER DEFAULT 3,
            error         TEXT,
            run_at        TEXT,
            locked_by     TEXT,
            locked_at     TEXT,
            created_at    TEXT    NOT NULL DEFAULT (datetime('now')),
            started_at    TEXT,
            completed_at  TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_jobs_status
            ON jobs (status, priority, run_at);
        CREATE INDEX IF NOT EXISTS idx_jobs_queue
            ON jobs (queue, status);",
    )?;
    Ok(())
}

/// Enqueue a job of `kind` with `payload` (JSON) on the `sync` queue
/// (reference default priority 2, default max_attempts 3). Available
/// immediately.
pub fn enqueue(conn: &Connection, kind: &str, payload: &str) -> Result<i64> {
    enqueue_on(conn, "sync", kind, payload, 2, 3)
}

/// Enqueue a job on a specific queue with a priority and a retry budget
/// (reference `enqueue(queue, type, payload, priority = 2, maxAttempts = 3)`
/// shape, jobRepo.ts:9). `run_at` is stored in SQLite's own
/// `datetime('now')` format so `claim_next` compares it lexically (the
/// reference's ISO-8601 pitfall never applies here).
pub fn enqueue_on(
    conn: &Connection,
    queue: &str,
    kind: &str,
    payload: &str,
    priority: i64,
    max_attempts: i64,
) -> Result<i64> {
    ensure_jobs_table(conn)?;
    conn.execute(
        "INSERT INTO jobs (queue, type, priority, payload, max_attempts, run_at)
         VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'))",
        params![queue, kind, priority, payload, max_attempts],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Claim the next available job — optionally scoped to one queue (the
/// reference `claimNext(queue?)` filter, jobRepo.ts:22). Returns `None` if
/// no job is available.
///
/// Ordering is the reference's `priority ASC, id ASC`; a job is runnable
/// when `run_at IS NULL OR run_at <= datetime('now')` (SQLite's own format,
/// never ISO-8601 — see the reference's run_at pitfall comment).
pub fn claim_next(conn: &mut Connection, queue: Option<&str>) -> Result<Option<ClaimedJob>> {
    let tx = conn.transaction()?;
    // Atomic claim: highest priority first, then FIFO by id, never a parked
    // awaiting_approval job (status filter). The queue filter scopes the
    // claim to one worker's lane (reference claimNext(queue)).
    type ClaimRow = (i64, String, String, i64, i64);
    let select_row = |stmt: &mut rusqlite::Statement<'_>,
                      params: &[&dyn rusqlite::ToSql]|
     -> rusqlite::Result<Option<ClaimRow>> {
        let mut rows = stmt.query(params)?;
        match rows.next()? {
            Some(r) => Ok(Some((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
            ))),
            None => Ok(None),
        }
    };
    let row: Option<ClaimRow> = match queue {
        Some(q) => {
            let mut stmt = tx.prepare(
                "SELECT id, type, payload, attempt, max_attempts
                   FROM jobs
                  WHERE status = 'queued'
                    AND (run_at IS NULL OR run_at <= datetime('now'))
                    AND queue = ?
                  ORDER BY priority ASC, id ASC
                  LIMIT 1;",
            )?;
            select_row(&mut stmt, &[&q])?
        }
        None => {
            let mut stmt = tx.prepare(
                "SELECT id, type, payload, attempt, max_attempts
                   FROM jobs
                  WHERE status = 'queued'
                    AND (run_at IS NULL OR run_at <= datetime('now'))
                  ORDER BY priority ASC, id ASC
                  LIMIT 1;",
            )?;
            select_row(&mut stmt, &[])?
        }
    };

    let Some((id, kind, payload, attempt, max_attempts)) = row else {
        return Ok(None);
    };

    tx.execute(
        "UPDATE jobs
            SET status = 'running',
                started_at = datetime('now'),
                attempt = ?1
          WHERE id = ?2",
        params![attempt + 1, id],
    )?;
    tx.commit()?;

    Ok(Some(ClaimedJob {
        id,
        kind,
        payload,
        attempts: attempt + 1,
        max_attempts,
    }))
}

/// A job that has been claimed and is ready to execute.
#[derive(Debug, Clone)]
pub struct ClaimedJob {
    pub id: i64,
    pub kind: String,
    pub payload: String,
    pub attempts: i64,
    pub max_attempts: i64,
}

/// Mark a claimed job as completed (clears any carried-over error, the
/// reference `completeJob` sets `error = NULL`).
pub fn complete(conn: &Connection, id: i64) -> Result<()> {
    let rows = conn.execute(
        "UPDATE jobs SET status = 'completed', completed_at = datetime('now'), error = NULL
          WHERE id = ?1 AND status = 'running'",
        params![id],
    )?;
    if rows == 0 {
        return Err(Error::Other(
            format!("job {id} not in 'running' state; cannot complete").into(),
        ));
    }
    Ok(())
}

/// Mark a claimed job as failed (reference `failJob(id, error, retryable)`,
/// jobRepo.ts:53). If `retryable` and attempt < max_attempts, requeue with
/// the reference's exact exponential backoff — `min(300, 5 * 2^attempt)`
/// SECONDS (5, 10, 20, 40, 80, 160, 300 capped), stored via SQLite
/// `datetime('now', '+N seconds')` so it stays in the claim-comparable
/// format. Otherwise, dead-letter (`failed`). The error text is capped at
/// 1000 characters like the reference's `error.slice(0, 1000)`.
pub fn fail(conn: &Connection, id: i64, error: &str, retryable: bool) -> Result<()> {
    let row: Option<(i64, i64)> = conn
        .prepare("SELECT attempt, max_attempts FROM jobs WHERE id = ?1 AND status = 'running'")?
        .query_row(params![id], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
        })
        .ok();

    let Some((attempt, max_attempts)) = row else {
        return Err(Error::Other(
            format!("job {id} not in 'running' state; cannot fail").into(),
        ));
    };

    let error: String = error.chars().take(1000).collect();
    let can_retry = retryable && attempt < max_attempts;
    if !can_retry {
        conn.execute(
            "UPDATE jobs SET status = 'failed', completed_at = datetime('now'), error = ?1 WHERE id = ?2",
            params![error, id],
        )?;
    } else {
        // Reference backoff (jobRepo.ts:58): Math.min(300, 2 ** attempt * 5).
        let shift = u32::try_from(attempt).unwrap_or(0).min(20);
        let delay_secs = (5i64 << shift).min(300);
        conn.execute(
            "UPDATE jobs
                SET status = 'queued',
                    locked_by = NULL,
                    locked_at = NULL,
                    run_at = datetime('now', '+' || ?1 || ' seconds'),
                    error = ?2
              WHERE id = ?3",
            params![delay_secs, error, id],
        )?;
    }
    Ok(())
}

/// `parkJob(id)` (jobRepo.ts:98, v1.6.0 audit fix): park a job in the
/// `awaiting_approval` state — visible in the Queue panel, never claimed
/// again, approved via `retry_job(id, {"approved":true})` / rejected via
/// `cancel_job(id)`.
pub fn park_job(conn: &Connection, id: i64) -> Result<()> {
    conn.execute(
        "UPDATE jobs SET status = 'awaiting_approval', completed_at = NULL WHERE id = ?1",
        params![id],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Reference queue-panel API (jobRepo.ts parity)
// ---------------------------------------------------------------------------

/// A queued job row as the reference `GET /api/queue` returns it.
#[derive(Debug, Clone, serde::Serialize)]
pub struct QueueJob {
    pub id: i64,
    pub queue: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub priority: i64,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
    pub attempt: i64,
    pub max_attempts: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locked_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locked_at: Option<String>,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
}

/// `listJobs({status, queue, limit})` — payload parsed to JSON.
pub fn list_jobs(
    conn: &Connection,
    status: Option<&str>,
    queue: Option<&str>,
    limit: i64,
) -> Result<Vec<QueueJob>> {
    let mut sql = String::from("SELECT * FROM jobs");
    let mut conds: Vec<&str> = Vec::new();
    if status.is_some() {
        conds.push("status = ?1");
    }
    if queue.is_some() {
        conds.push("queue = ?");
    }
    if !conds.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&conds.join(" AND "));
    }
    sql.push_str(" ORDER BY id DESC LIMIT ?");
    let mut stmt = conn.prepare(&sql)?;
    let limit_str = limit.to_string();
    let rows = stmt.query_map(
        [
            status.unwrap_or(""),
            queue.unwrap_or(""),
            limit_str.as_str(),
        ],
        map_queue_job,
    )?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

fn map_queue_job(r: &rusqlite::Row<'_>) -> rusqlite::Result<QueueJob> {
    let payload: Option<String> = r.get("payload")?;
    Ok(QueueJob {
        id: r.get("id")?,
        queue: r.get("queue")?,
        kind: r.get("type")?,
        priority: r.get("priority")?,
        status: r.get("status")?,
        payload: payload
            .as_deref()
            .and_then(|p| serde_json::from_str(p).ok()),
        attempt: r.get("attempt")?,
        max_attempts: r.get("max_attempts")?,
        error: r.get("error")?,
        run_at: r.get("run_at")?,
        locked_by: r.get("locked_by")?,
        locked_at: r.get("locked_at")?,
        created_at: r.get("created_at")?,
        started_at: r.get("started_at")?,
        completed_at: r.get("completed_at")?,
    })
}

/// `getJob(id)` — one job or None.
pub fn get_job(conn: &Connection, id: i64) -> Result<Option<QueueJob>> {
    let job = conn
        .prepare("SELECT * FROM jobs WHERE id = ?1")?
        .query_row(params![id], map_queue_job)
        .ok();
    Ok(job)
}

/// `queueStats()` — `{queued, running, failed, completed}`.
pub fn queue_stats(conn: &Connection) -> Result<(i64, i64, i64, i64)> {
    let (queued, running, failed, completed): (i64, i64, i64, i64) = conn.query_row(
        "SELECT SUM(CASE WHEN status='queued' THEN 1 ELSE 0 END),
                SUM(CASE WHEN status='running' THEN 1 ELSE 0 END),
                SUM(CASE WHEN status='failed' THEN 1 ELSE 0 END),
                SUM(CASE WHEN status='completed' THEN 1 ELSE 0 END)
         FROM jobs",
        [],
        |r| {
            Ok((
                r.get::<_, Option<i64>>(0)?.unwrap_or(0),
                r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                r.get::<_, Option<i64>>(2)?.unwrap_or(0),
                r.get::<_, Option<i64>>(3)?.unwrap_or(0),
            ))
        },
    )?;
    Ok((queued, running, failed, completed))
}

/// `retryJob(id, payloadPatch)` — requeue a failed/cancelled/awaiting_approval
/// job; `None` when the job does not exist or is not retryable.
pub fn retry_job(conn: &Connection, id: i64, patch: Option<&str>) -> Result<Option<bool>> {
    let existing: Option<(Option<String>, String)> = conn
        .prepare(
            "SELECT payload, status FROM jobs
              WHERE id = ?1 AND status IN ('failed','cancelled','awaiting_approval')",
        )?
        .query_row(params![id], |r| {
            Ok((r.get::<_, Option<String>>(0)?, r.get::<_, String>(1)?))
        })
        .ok();
    let Some((payload, _status)) = existing else {
        // Distinguish "not found" from "not retryable".
        let any: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM jobs WHERE id = ?1)",
            params![id],
            |r| r.get(0),
        )?;
        return Ok(if any { Some(false) } else { None });
    };
    if let Some(patch_json) = patch {
        if let Some(existing_payload) = payload.as_deref() {
            let merged = merge_payloads(existing_payload, patch_json);
            conn.execute(
                "UPDATE jobs SET payload = ?1 WHERE id = ?2",
                params![merged, id],
            )?;
        }
    }
    let rows = conn.execute(
        "UPDATE jobs SET status = 'queued', attempt = 0, error = NULL, run_at = datetime('now')
          WHERE id = ?1",
        params![id],
    )?;
    Ok(Some(rows > 0))
}

/// Approving an awaiting_approval job flags the payload so the worker executes
/// the parked action instead of parking it again (`{"approved":true}` merge).
fn merge_payloads(existing: &str, patch: &str) -> String {
    let mut base = serde_json::from_str::<serde_json::Value>(existing)
        .unwrap_or_else(|_| serde_json::json!({}));
    let add =
        serde_json::from_str::<serde_json::Value>(patch).unwrap_or_else(|_| serde_json::json!({}));
    if let (Some(base_map), Some(add_map)) = (base.as_object_mut(), add.as_object()) {
        for (k, v) in add_map {
            base_map.insert(k.clone(), v.clone());
        }
    }
    base.to_string()
}

/// `cancelJob(id)` — cancel a queued/running/awaiting_approval job.
pub fn cancel_job(conn: &Connection, id: i64) -> Result<bool> {
    let rows = conn.execute(
        "UPDATE jobs SET status = 'cancelled', completed_at = datetime('now')
          WHERE id = ?1 AND status IN ('queued','running','awaiting_approval')",
        params![id],
    )?;
    Ok(rows > 0)
}

/// `clearCompleted()` — delete completed/cancelled jobs older than 24 h.
pub fn clear_completed(conn: &Connection) -> Result<usize> {
    let rows = conn.execute(
        "DELETE FROM jobs
          WHERE status IN ('completed','cancelled')
            AND completed_at < datetime('now', '-1 day')",
        [],
    )?;
    Ok(rows)
}

/// `recoverStaleJobs()` — release jobs stuck in `running` for 30+ minutes
/// (the boot recovery, message matches the reference).
pub fn recover_stale_jobs(conn: &Connection) -> Result<usize> {
    let rows = conn.execute(
        "UPDATE jobs SET status = 'queued', error = 'Recovered after restart'
          WHERE status = 'running' AND started_at < datetime('now', '-30 minutes')",
        [],
    )?;
    Ok(rows)
}

/// `requeueStaleRunningJobs(thresholdMinutes = 30)` (jobRepo.ts:159, v2.2.1
/// audit fix): the periodic sweep companion to the boot recovery — the same
/// conservative 30-minute rule, with the sweep's distinct error message, so
/// a job claimed moments before a restart does not stay `running` for the
/// whole process lifetime.
pub fn requeue_stale_running_jobs(conn: &Connection, threshold_minutes: i64) -> Result<usize> {
    let rows = conn.execute(
        "UPDATE jobs SET status = 'queued',
             error = 'Re-queued by maintenance sweep: running longer than threshold'
          WHERE status = 'running'
            AND started_at < datetime('now', ?1)",
        params![format!("-{threshold_minutes} minutes")],
    )?;
    Ok(rows)
}

/// `listOutboundJobs(status, limit)`.
pub fn list_outbound_jobs(
    conn: &Connection,
    status: Option<&str>,
    limit: i64,
) -> Result<Vec<serde_json::Value>> {
    let mut out = Vec::new();
    let sql = if status.is_some() {
        "SELECT * FROM outbound_jobs WHERE status = ?1 ORDER BY id DESC LIMIT ?2"
    } else {
        "SELECT * FROM outbound_jobs ORDER BY id DESC LIMIT ?2"
    };
    let mut stmt = conn.prepare(sql)?;
    let columns: Vec<String> = stmt
        .column_names()
        .iter()
        .map(|c| (*c).to_string())
        .collect();
    let limit_str = limit.to_string();
    let rows = stmt.query_map([status.unwrap_or(""), limit_str.as_str()], |r| {
        let mut obj = serde_json::Map::new();
        for (idx, col) in columns.iter().enumerate() {
            let v: rusqlite::types::Value = r.get(idx)?;
            obj.insert(
                col.clone(),
                match v {
                    rusqlite::types::Value::Null => serde_json::Value::Null,
                    rusqlite::types::Value::Integer(i) => serde_json::Value::from(i),
                    rusqlite::types::Value::Real(f) => serde_json::Value::from(f),
                    rusqlite::types::Value::Text(s) => {
                        if col == "payload" {
                            serde_json::from_str(&s).unwrap_or(serde_json::Value::String(s))
                        } else {
                            serde_json::Value::String(s)
                        }
                    }
                    rusqlite::types::Value::Blob(b) => {
                        serde_json::Value::String(String::from_utf8_lossy(&b).to_string())
                    }
                },
            );
        }
        Ok(serde_json::Value::Object(obj))
    })?;
    for v in rows.flatten() {
        out.push(v);
    }
    Ok(out)
}

/// `audit(entry)` — append to `audit_log`.
#[allow(clippy::too_many_arguments)]
pub fn audit(
    conn: &Connection,
    actor: &str,
    action: &str,
    conversation_id: Option<i64>,
    before_state: Option<&str>,
    after_state: Option<&str>,
    remote_operation: Option<&str>,
    remote_result: Option<&str>,
    ai_involvement: bool,
) -> Result<()> {
    audit_entry(
        conn,
        actor,
        action,
        conversation_id,
        before_state,
        after_state,
        remote_operation,
        remote_result,
        ai_involvement,
        None,
        None,
    )
}

/// `audit(entry)` with the job/correlation columns the write pipeline fills
/// (jobRepo.ts audit: job_id + correlation_id).
#[allow(clippy::too_many_arguments)]
pub fn audit_entry(
    conn: &Connection,
    actor: &str,
    action: &str,
    conversation_id: Option<i64>,
    before_state: Option<&str>,
    after_state: Option<&str>,
    remote_operation: Option<&str>,
    remote_result: Option<&str>,
    ai_involvement: bool,
    job_id: Option<i64>,
    correlation_id: Option<&str>,
) -> Result<()> {
    conn.execute(
        "INSERT INTO audit_log (timestamp, actor, action, conversation_id, before_state,
             after_state, remote_operation, remote_result, ai_involvement, job_id, correlation_id)
         VALUES (datetime('now'), ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            actor,
            action,
            conversation_id,
            before_state,
            after_state,
            remote_operation,
            remote_result,
            i64::from(ai_involvement),
            job_id,
            correlation_id
        ],
    )?;
    Ok(())
}

/// `recordOutboundAttempt(jobId, attempt, summary, statusCode, responseBody,
/// latencyMs)` — jobRepo.ts:196. Summary caps at 500 chars, body at 2000.
pub fn record_outbound_attempt(
    conn: &Connection,
    job_id: i64,
    attempt: i64,
    summary: &str,
    status_code: Option<i64>,
    response_body: Option<&str>,
    latency_ms: Option<i64>,
) -> Result<()> {
    let summary: String = summary.chars().take(500).collect();
    let body: Option<String> = response_body.map(|b| b.chars().take(2000).collect());
    conn.execute(
        "INSERT INTO outbound_attempts (outbound_job_id, attempt, request_summary,
             status_code, response_body, latency_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![job_id, attempt, summary, status_code, body, latency_ms],
    )?;
    Ok(())
}

/// `logError(service, message)` — append to `application_errors`.
pub fn log_error(conn: &Connection, service: &str, message: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO application_errors (timestamp, service, message) VALUES (datetime('now'), ?1, ?2)",
        params![service, message],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    fn fresh_db() -> Connection {
        let f = NamedTempFile::new()
            .unwrap()
            .into_temp_path()
            .keep()
            .unwrap();
        let conn = crate::db::open(&f).unwrap();
        crate::db::ensure_migrations_table(&conn).unwrap();
        ensure_jobs_table(&conn).unwrap();
        conn
    }

    #[test]
    fn end_to_end_enqueue_claim_complete() {
        // Per KNOWN PITFALLS: test the full claim loop, not just components.
        let mut conn = fresh_db();
        let id = enqueue(&conn, "test.echo", r#"{"msg":"hi"}"#).unwrap();
        assert!(id > 0);

        let claimed = claim_next(&mut conn, None)
            .unwrap()
            .expect("a job is available");
        assert_eq!(claimed.kind, "test.echo");
        assert_eq!(claimed.attempts, 1);

        // No more jobs available.
        assert!(claim_next(&mut conn, None).unwrap().is_none());

        complete(&conn, claimed.id).unwrap();
        let state: String = conn
            .query_row(
                "SELECT status FROM jobs WHERE id = ?1",
                params![claimed.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(state, "completed");
    }

    #[test]
    fn fail_requeues_with_backoff_then_dead() {
        let mut conn = fresh_db();
        let id = enqueue(&conn, "test.failing", "{}").unwrap();

        // Attempt 1: fail -> requeue with backoff (run_at in the future).
        let claimed = claim_next(&mut conn, None).unwrap().unwrap();
        assert_eq!(claimed.attempts, 1);
        fail(&conn, claimed.id, "boom", true).unwrap();
        let state: String = conn
            .query_row("SELECT status FROM jobs WHERE id=?1", params![id], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(state, "queued");

        // Reset run_at to now so claim_next picks it up (backoff elapsed).
        conn.execute(
            "UPDATE jobs SET run_at = datetime('now') WHERE id = ?1",
            params![id],
        )
        .unwrap();

        // Attempt 2: claim again, fail -> still requeued (attempt < max_attempts=3).
        let claimed = claim_next(&mut conn, None).unwrap().unwrap();
        assert_eq!(claimed.attempts, 2);
        fail(&conn, claimed.id, "boom", true).unwrap();
        let state: String = conn
            .query_row("SELECT status FROM jobs WHERE id=?1", params![id], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(state, "queued");

        // Exhaust: attempt 3 == max_attempts 3 -> failed (dead-letter).
        conn.execute(
            "UPDATE jobs SET run_at = datetime('now') WHERE id = ?1",
            params![id],
        )
        .unwrap();
        let claimed = claim_next(&mut conn, None).unwrap().unwrap();
        assert_eq!(claimed.attempts, 3);
        fail(&conn, claimed.id, "boom", true).unwrap();
        let state: String = conn
            .query_row("SELECT status FROM jobs WHERE id=?1", params![id], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(state, "failed");
    }

    /// DB-09: the exact reference backoff seconds — min(300, 5 * 2^attempt)
    /// (jobRepo.ts:58), stored in SQLite datetime format so it is comparable.
    #[test]
    fn fail_backoff_is_exact_reference_seconds() {
        let conn = fresh_db();
        // attempt 0 -> 5s, 1 -> 10s, 2 -> 20s, 4 -> 80s, 6 -> min(320,300) = 300 capped.
        for (attempt, expect_secs) in [(0i64, 5i64), (1, 10), (2, 20), (4, 80), (6, 300)] {
            conn.execute(
                "INSERT INTO jobs (queue, type, status, attempt, max_attempts, run_at)
                 VALUES ('sync', 'b', 'running', ?1, 99, datetime('now'))",
                params![attempt],
            )
            .unwrap();
            let id = conn.last_insert_rowid();
            fail(&conn, id, "boom", true).unwrap();
            let run_at: String = conn
                .query_row("SELECT run_at FROM jobs WHERE id = ?1", params![id], |r| {
                    r.get(0)
                })
                .unwrap();
            // REAL seconds since now (SQLite's datetime() truncates to whole
            // seconds at generation, so the observable delay is expect-1..expect+1).
            let delay: f64 = conn
                .query_row(
                    "SELECT julianday(?1) - julianday('now')",
                    params![run_at],
                    |r| r.get::<_, f64>(0),
                )
                .unwrap()
                * 86400.0;
            assert!(
                (expect_secs as f64 - 1.1..=expect_secs as f64 + 1.1).contains(&delay),
                "attempt {attempt}: backoff {delay:.2}s not within [{}, {}]",
                expect_secs - 1,
                expect_secs + 1
            );
            // SQLite format (space separator), never ISO-8601 ('T').
            assert!(
                !run_at.contains('T'),
                "run_at must stay in SQLite datetime format, got {run_at}"
            );
        }
    }

    /// DB-09: `failJob(id, error, retryable=false)` dead-letters immediately
    /// (the reference's unknown-job-type path never retries).
    #[test]
    fn fail_non_retryable_dead_letters_immediately() {
        let mut conn = fresh_db();
        let id = enqueue(&conn, "test.permanent", "{}").unwrap();
        let claimed = claim_next(&mut conn, None).unwrap().unwrap();
        assert_eq!(claimed.attempts, 1);
        fail(&conn, claimed.id, "Unknown job type", false).unwrap();
        let state: String = conn
            .query_row("SELECT status FROM jobs WHERE id=?1", params![id], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(state, "failed");
    }

    /// DB-09: `claimNext(queue?)` — the queue filter scopes the claim.
    #[test]
    fn claim_next_filters_by_queue() {
        let mut conn = fresh_db();
        enqueue_on(&conn, "sync", "sync.job", "{}", 2, 3).unwrap();
        enqueue_on(&conn, "ai", "ai.job", "{}", 4, 3).unwrap();
        // Unscoped claim follows priority: the ai job (priority 4 vs 2)? No —
        // lower number = higher priority, so sync (2) wins unscoped.
        let claimed = claim_next(&mut conn, None).unwrap().unwrap();
        assert_eq!(claimed.kind, "sync.job");
        // Scoped claim on the ai lane finds its own job.
        let claimed = claim_next(&mut conn, Some("ai")).unwrap().unwrap();
        assert_eq!(claimed.kind, "ai.job");
        // The ai lane is now empty even though the sync job is mid-flight.
        assert!(claim_next(&mut conn, Some("ai")).unwrap().is_none());
    }

    /// DB-09: `parkJob(id)` — the awaiting-approval park (jobRepo.ts:98).
    #[test]
    fn park_job_awaiting_approval_never_claimed_until_approved() {
        let mut conn = fresh_db();
        let id = enqueue(&conn, "automation.parked", "{}").unwrap();
        let claimed = claim_next(&mut conn, None).unwrap().unwrap();
        park_job(&conn, claimed.id).unwrap();
        let (state, completed_at): (String, Option<String>) = conn
            .query_row(
                "SELECT status, completed_at FROM jobs WHERE id=?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(state, "awaiting_approval");
        assert!(completed_at.is_none());
        // Never claimed while parked (any lane).
        assert!(claim_next(&mut conn, None).unwrap().is_none());
        // Approve = retry with the approved patch -> queued again.
        let ok = retry_job(&conn, id, Some(r#"{"approved":true}"#)).unwrap();
        assert_eq!(ok, Some(true));
        let reclaimed = claim_next(&mut conn, Some("sync")).unwrap().unwrap();
        assert_eq!(reclaimed.id, id);
    }

    /// DB-09: the periodic sweep (v2.2.1) uses the reference's distinct
    /// message and honors its threshold parameter.
    #[test]
    fn requeue_stale_running_jobs_uses_sweep_message_and_threshold() {
        let conn = fresh_db();
        conn.execute_batch(
            "INSERT INTO jobs (queue, type, status, started_at)
             VALUES ('sync', 'a', 'running', datetime('now', '-45 minutes'));
             INSERT INTO jobs (queue, type, status, started_at)
             VALUES ('sync', 'b', 'running', datetime('now', '-5 minutes'));",
        )
        .unwrap();
        // Threshold 30: only the 45-minute-old row is requeued.
        let n = requeue_stale_running_jobs(&conn, 30).unwrap();
        assert_eq!(n, 1);
        let (status, error): (String, Option<String>) = conn
            .query_row("SELECT status, error FROM jobs WHERE id = 1", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(status, "queued");
        assert_eq!(
            error.as_deref(),
            Some("Re-queued by maintenance sweep: running longer than threshold")
        );
        let status_b: String = conn
            .query_row("SELECT status FROM jobs WHERE id = 2", [], |r| r.get(0))
            .unwrap();
        assert_eq!(status_b, "running");
    }

    /// DB-09: enqueue stores the caller's max_attempts (jobRepo.ts:9).
    #[test]
    fn enqueue_records_max_attempts() {
        let conn = fresh_db();
        let id = enqueue_on(&conn, "ai", "analyze_ticket", "{}", 3, 2).unwrap();
        let max_attempts: i64 = conn
            .query_row(
                "SELECT max_attempts FROM jobs WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(max_attempts, 2);
        // Default enqueue keeps the reference default of 3.
        let id = enqueue(&conn, "plain", "{}").unwrap();
        let max_attempts: i64 = conn
            .query_row(
                "SELECT max_attempts FROM jobs WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(max_attempts, 3);
    }

    #[test]
    fn retry_failed_job_resets_attempt() {
        let conn = fresh_db();
        conn.execute(
            "INSERT INTO jobs (queue, type, status, attempt, max_attempts, error)
             VALUES ('sync', 'x', 'failed', 3, 3, 'boom')",
            [],
        )
        .unwrap();
        let ok = retry_job(&conn, 1, None).unwrap();
        assert_eq!(ok, Some(true));
        let (status, attempt): (String, i64) = conn
            .query_row("SELECT status, attempt FROM jobs WHERE id = 1", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(status, "queued");
        assert_eq!(attempt, 0);
    }

    #[test]
    fn retry_running_job_is_rejected() {
        let conn = fresh_db();
        conn.execute(
            "INSERT INTO jobs (queue, type, status) VALUES ('sync', 'x', 'running')",
            [],
        )
        .unwrap();
        let ok = retry_job(&conn, 1, None).unwrap();
        assert_eq!(ok, Some(false));
    }

    #[test]
    fn retry_missing_job_returns_none() {
        let conn = fresh_db();
        let ok = retry_job(&conn, 999, None).unwrap();
        assert!(ok.is_none());
    }

    #[test]
    fn retry_awaiting_approval_patches_approved() {
        let conn = fresh_db();
        conn.execute(
            "INSERT INTO jobs (queue, type, status, payload) VALUES ('automation', 'run_action', 'awaiting_approval', '{\"action\":\"close\"}')",
            [],
        )
        .unwrap();
        let ok = retry_job(&conn, 1, Some(r#"{"approved":true}"#)).unwrap();
        assert_eq!(ok, Some(true));
        let payload: String = conn
            .query_row("SELECT payload FROM jobs WHERE id = 1", [], |r| r.get(0))
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(v["approved"], serde_json::json!(true));
        assert_eq!(v["action"], serde_json::json!("close"));
    }

    #[test]
    fn cancel_only_pending_work() {
        let conn = fresh_db();
        conn.execute_batch(
            "INSERT INTO jobs (queue, type, status) VALUES ('sync', 'a', 'queued');
             INSERT INTO jobs (queue, type, status) VALUES ('sync', 'b', 'completed');",
        )
        .unwrap();
        assert!(cancel_job(&conn, 1).unwrap());
        assert!(!cancel_job(&conn, 2).unwrap());
    }

    #[test]
    fn clear_completed_respects_24h_window() {
        let conn = fresh_db();
        conn.execute_batch(
            "INSERT INTO jobs (queue, type, status, completed_at) VALUES ('sync', 'a', 'completed', datetime('now', '-2 day'));
             INSERT INTO jobs (queue, type, status, completed_at) VALUES ('sync', 'b', 'completed', datetime('now'));",
        )
        .unwrap();
        let n = clear_completed(&conn).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn queue_stats_counts_statuses() {
        let conn = fresh_db();
        conn.execute_batch(
            "INSERT INTO jobs (queue, type, status) VALUES ('sync', 'a', 'queued');
             INSERT INTO jobs (queue, type, status) VALUES ('sync', 'b', 'queued');
             INSERT INTO jobs (queue, type, status) VALUES ('sync', 'c', 'running');
             INSERT INTO jobs (queue, type, status) VALUES ('sync', 'd', 'failed');
             INSERT INTO jobs (queue, type, status) VALUES ('sync', 'e', 'completed');",
        )
        .unwrap();
        let (q, r, f, c) = queue_stats(&conn).unwrap();
        assert_eq!((q, r, f, c), (2, 1, 1, 1));
    }

    #[test]
    fn list_jobs_filters_by_status_and_queue() {
        let conn = fresh_db();
        conn.execute_batch(
            "INSERT INTO jobs (queue, type, status) VALUES ('sync', 'a', 'queued');
             INSERT INTO jobs (queue, type, status) VALUES ('maintenance', 'b', 'queued');
             INSERT INTO jobs (queue, type, status) VALUES ('sync', 'c', 'completed');",
        )
        .unwrap();
        let sync_queued = list_jobs(&conn, Some("queued"), Some("sync"), 100).unwrap();
        assert_eq!(sync_queued.len(), 1);
        assert_eq!(sync_queued[0].kind, "a");
        assert!(sync_queued[0].payload.is_none());
    }

    #[test]
    fn priority_ordering_in_claim() {
        let mut conn = fresh_db();
        enqueue_on(&conn, "sync", "low", "{}", 4, 3).unwrap();
        enqueue_on(&conn, "sync", "high", "{}", 0, 3).unwrap();
        let claimed = claim_next(&mut conn, None).unwrap().unwrap();
        assert_eq!(claimed.kind, "high");
    }

    #[test]
    fn awaiting_approval_never_claimed() {
        let mut conn = fresh_db();
        conn.execute(
            "INSERT INTO jobs (queue, type, status, payload) VALUES ('automation', 'x', 'awaiting_approval', '{}')",
            [],
        )
        .unwrap();
        assert!(claim_next(&mut conn, None).unwrap().is_none());
    }

    #[test]
    fn audit_and_error_log_append() {
        let conn = fresh_db();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS audit_log (id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp TEXT, actor TEXT, action TEXT, conversation_id INTEGER,
                before_state TEXT, after_state TEXT, remote_operation TEXT,
                remote_result TEXT, ai_involvement INTEGER, job_id INTEGER, correlation_id TEXT);
             CREATE TABLE IF NOT EXISTS application_errors (id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp TEXT, service TEXT, message TEXT, stack TEXT, context TEXT);",
        )
        .unwrap();
        audit(
            &conn,
            "user",
            "webhook_registered",
            None,
            None,
            Some("{\"url\":\"x\"}"),
            Some("POST /v2/webhooks"),
            None,
            false,
        )
        .unwrap();
        log_error(&conn, "sync", "initial sync failed: boom").unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM audit_log", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM application_errors", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }
}
