//! Job queue: enqueue, claim, execute, retry with backoff, dead-letter.
//!
//! Per KNOWN PITFALLS: "Job claim loops must be tested end to end (enqueue, claim, execute),
//! not by calling components directly."
//!
//! This module is the foundation; concrete job handlers will be added in M2+.

use std::time::Duration;

use rusqlite::{params, Connection};

use crate::error::{Error, Result};

/// The state of a job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JobState {
    /// Waiting to be claimed.
    Pending,
    /// Claimed by a worker; in progress.
    Claimed,
    /// Completed successfully.
    Done,
    /// Exhausted its retry budget; will not be retried.
    Dead,
}

impl JobState {
    #[cfg(test)]
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Claimed => "claimed",
            Self::Done => "done",
            Self::Dead => "dead",
        }
    }
}

/// Create the `jobs` table if it does not exist. Idempotent.
pub fn ensure_jobs_table(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS jobs (
            id            INTEGER PRIMARY KEY AUTOINCREMENT,
            kind          TEXT    NOT NULL,
            payload       TEXT    NOT NULL,
            state         TEXT    NOT NULL DEFAULT 'pending',
            attempts      INTEGER NOT NULL DEFAULT 0,
            max_attempts  INTEGER NOT NULL DEFAULT 5,
            available_at  TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
            claimed_at    TEXT,
            completed_at  TEXT,
            last_error    TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_jobs_state_available_at
            ON jobs (state, available_at);",
    )?;
    Ok(())
}

/// Enqueue a job of `kind` with `payload` (JSON). Available immediately.
pub fn enqueue(conn: &Connection, kind: &str, payload: &str) -> Result<i64> {
    ensure_jobs_table(conn)?;
    conn.execute(
        "INSERT INTO jobs (kind, payload) VALUES (?1, ?2)",
        params![kind, payload],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Claim the next available job for `worker_id` (recorded in `claimed_at`).
/// Returns `None` if no job is available.
pub fn claim_next(conn: &mut Connection) -> Result<Option<ClaimedJob>> {
    let tx = conn.transaction()?;
    // Atomic claim: select the oldest pending job whose available_at is now or earlier,
    // mark it claimed, and bump attempts.
    let now = format!("{}", chrono::Utc::now().format("%Y-%m-%dT%H:%M:%fZ"));
    let row: Option<(i64, String, String, i64, i64)> = tx
        .prepare(
            "SELECT id, kind, payload, attempts, max_attempts
               FROM jobs
              WHERE state = 'pending'
                AND available_at <= ?1
              ORDER BY available_at ASC
              LIMIT 1;",
        )?
        .query_row(params![now], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
            ))
        })
        .ok();

    let Some((id, kind, payload, attempts, max_attempts)) = row else {
        return Ok(None);
    };

    tx.execute(
        "UPDATE jobs
            SET state = 'claimed',
                claimed_at = ?1,
                attempts = ?2
          WHERE id = ?3",
        params![now, attempts + 1, id],
    )?;
    tx.commit()?;

    Ok(Some(ClaimedJob {
        id,
        kind,
        payload,
        attempts: attempts + 1,
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

/// Mark a claimed job as done.
pub fn complete(conn: &Connection, id: i64) -> Result<()> {
    let now = format!("{}", chrono::Utc::now().format("%Y-%m-%dT%H:%M:%fZ"));
    let rows = conn.execute(
        "UPDATE jobs SET state = 'done', completed_at = ?1 WHERE id = ?2 AND state = 'claimed'",
        params![now, id],
    )?;
    if rows == 0 {
        return Err(Error::Other(
            format!("job {id} not in 'claimed' state; cannot complete").into(),
        ));
    }
    Ok(())
}

/// Mark a claimed job as failed. If attempts < max_attempts, requeue with exponential backoff.
/// Otherwise, mark dead.
pub fn fail(conn: &Connection, id: i64, error: &str) -> Result<()> {
    let now = chrono::Utc::now();
    let now_str = format!("{}", now.format("%Y-%m-%dT%H:%M:%fZ"));

    let row: Option<(i64, i64)> = conn
        .prepare("SELECT attempts, max_attempts FROM jobs WHERE id = ?1 AND state = 'claimed'")?
        .query_row(params![id], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
        })
        .ok();

    let Some((attempts, max_attempts)) = row else {
        return Err(Error::Other(
            format!("job {id} not in 'claimed' state; cannot fail").into(),
        ));
    };

    if attempts >= max_attempts {
        // Dead-letter.
        conn.execute(
            "UPDATE jobs SET state = 'dead', completed_at = ?1, last_error = ?2 WHERE id = ?3",
            params![now_str, error, id],
        )?;
    } else {
        // Requeue with exponential backoff: 2^attempts minutes (1, 2, 4, 8, 16).
        let shift = ((attempts - 1).max(0) as u32).min(10);
        let delay_secs = 60u64 << shift;
        let delay = Duration::from_secs(delay_secs);
        let next = now + chrono::Duration::from_std(delay).unwrap_or(chrono::Duration::minutes(60));
        let next_str = format!("{}", next.format("%Y-%m-%dT%H:%M:%fZ"));
        conn.execute(
            "UPDATE jobs
                SET state = 'pending',
                    claimed_at = NULL,
                    available_at = ?1,
                    last_error = ?2
              WHERE id = ?3",
            params![next_str, error, id],
        )?;
    }
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
        conn
    }

    #[test]
    fn end_to_end_enqueue_claim_complete() {
        // Per KNOWN PITFALLS: test the full claim loop, not just components.
        let mut conn = fresh_db();
        let id = enqueue(&conn, "test.echo", r#"{"msg":"hi"}"#).unwrap();
        assert!(id > 0);

        let claimed = claim_next(&mut conn).unwrap().expect("a job is available");
        assert_eq!(claimed.kind, "test.echo");
        assert_eq!(claimed.attempts, 1);

        // No more jobs available.
        assert!(claim_next(&mut conn).unwrap().is_none());

        complete(&conn, claimed.id).unwrap();
        let state: String = conn
            .query_row(
                "SELECT state FROM jobs WHERE id = ?1",
                params![claimed.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(state, JobState::Done.as_str());
    }

    #[test]
    fn fail_requeues_with_backoff_then_dead() {
        let mut conn = fresh_db();
        let id = enqueue(&conn, "test.failing", "{}").unwrap();

        // Attempt 1: fail → requeue with backoff (available_at is now in the future).
        let claimed = claim_next(&mut conn).unwrap().unwrap();
        assert_eq!(claimed.attempts, 1);
        fail(&conn, claimed.id, "boom").unwrap();
        let state: String = conn
            .query_row("SELECT state FROM jobs WHERE id=?1", params![id], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(state, "pending");

        // Reset available_at to now so claim_next picks it up (simulating the backoff elapsing).
        conn.execute(
            "UPDATE jobs SET available_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = ?1",
            params![id],
        )
        .unwrap();

        // Attempt 2: claim again, fail → still requeued (attempts < max_attempts=5).
        let claimed = claim_next(&mut conn).unwrap().unwrap();
        assert_eq!(claimed.attempts, 2);
        fail(&conn, claimed.id, "boom").unwrap();
        let state: String = conn
            .query_row("SELECT state FROM jobs WHERE id=?1", params![id], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(state, "pending");

        // Force-exhaust by setting max_attempts low for the test.
        conn.execute(
            "UPDATE jobs SET max_attempts = 1 WHERE id = ?1",
            params![id],
        )
        .unwrap();
        conn.execute(
            "UPDATE jobs SET available_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = ?1",
            params![id],
        )
        .unwrap();
        let claimed = claim_next(&mut conn).unwrap().unwrap();
        assert_eq!(claimed.attempts, 3);
        fail(&conn, claimed.id, "boom").unwrap();
        let state: String = conn
            .query_row("SELECT state FROM jobs WHERE id=?1", params![id], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(state, "dead");
    }
}
