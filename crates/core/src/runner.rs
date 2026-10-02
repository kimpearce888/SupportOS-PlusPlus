//! Job runner — the `execute` half of the job queue.
//!
//! Per KNOWN PITFALLS: "Job claim loops must be tested end to end (enqueue,
//! claim, execute), not by calling components directly."
//!
//! The storage layer (`crate::jobs`) handles enqueue / claim / complete / fail.
//! This module adds the missing piece: a `JobHandler` trait that the runner
//! calls after `claim_next`, plus a `JobRegistry` that maps job `kind` strings
//! to handlers. Together they form the full claim loop.
//!
//! Handlers are async-aware via `Box<dyn Fn>` so they can spawn Tokio tasks if
//! they need to (M2 sync jobs, M5 embedding jobs). For M1 we only need a
//! synchronous handler shape; async wiring arrives with the first real job
//! kind in M2.

use std::collections::HashMap;
use std::sync::Arc;

use rusqlite::Connection;

use crate::error::Result;
use crate::jobs::{self, ClaimedJob};

/// The outcome of running a handler. The runner maps this to the right
/// storage-layer call (`complete` or `fail`).
#[derive(Debug, Clone)]
pub enum HandlerOutcome {
    /// The job succeeded; mark it `done`.
    Success,
    /// The job failed; requeue with backoff (or dead-letter if attempts
    /// exhausted). The caller-visible error message is in `message`.
    Failure { message: String },
}

impl HandlerOutcome {
    /// Convenience constructor for the success case.
    #[must_use]
    pub fn success() -> Self {
        Self::Success
    }

    /// Convenience constructor for the failure case.
    #[must_use]
    pub fn failure(message: impl Into<String>) -> Self {
        Self::Failure {
            message: message.into(),
        }
    }
}

/// A handler for one job kind. The registry stores these as `Arc<dyn JobHandler>`
/// so a single registry can be shared across runner instances (one per
/// background worker) without copying the handler table.
pub trait JobHandler: Send + Sync {
    /// Execute the claimed job. `payload` is the JSON string stored at enqueue
    /// time. Return [`HandlerOutcome`] — the runner does the storage-layer
    /// bookkeeping.
    fn handle(&self, payload: &str) -> HandlerOutcome;
}

/// A registry mapping job `kind` strings to handlers.
///
/// Built once at app startup and shared (`Arc`) with the runner(s). Adding a
/// new job kind = one `register` call + the handler implementation. There is
/// no unregister — kinds are static for the lifetime of the process.
#[derive(Clone, Default)]
pub struct JobRegistry {
    handlers: Arc<HashMap<String, Arc<dyn JobHandler>>>,
}

impl JobRegistry {
    /// Create an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a handler for `kind`. Returns a new registry (the original is
    /// unchanged); callers typically chain these at startup:
    ///
    /// ```ignore
    /// let registry = JobRegistry::new()
    ///     .register("sync.conversations", Arc::new(SyncConversationsHandler))
    ///     .register("embed.text", Arc::new(EmbedTextHandler));
    /// ```
    #[must_use]
    pub fn register(mut self, kind: &str, handler: Arc<dyn JobHandler>) -> Self {
        Arc::make_mut(&mut self.handlers).insert(kind.to_string(), handler);
        self
    }

    /// Look up the handler for `kind`. Returns `None` for unknown kinds —
    /// the runner then marks the job as failed with a clear message so the
    /// operator sees the misconfiguration.
    #[must_use]
    pub fn get(&self, kind: &str) -> Option<Arc<dyn JobHandler>> {
        self.handlers.get(kind).cloned()
    }

    /// Returns the number of registered kinds. Useful for diagnostics.
    #[must_use]
    pub fn len(&self) -> usize {
        self.handlers.len()
    }

    /// Returns true if no handlers are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.handlers.is_empty()
    }
}

/// The runner — owns a DB connection and a registry, runs the claim loop
/// until either (a) no more jobs are available, or (b) `max_iterations` is
/// reached.
///
/// Each iteration:
///   1. `jobs::claim_next` — atomically claim the oldest pending job.
///   2. Look up the handler in the registry.
///   3. Call the handler with the payload.
///   4. On `Success`: `jobs::complete`.
///   5. On `Failure`: `jobs::fail` (requeue with backoff or dead-letter).
///   6. On unknown kind: `jobs::fail` with a clear message (no silent skip).
pub struct Runner<'a> {
    conn: &'a mut Connection,
    registry: &'a JobRegistry,
}

/// A summary of a single `run_until_idle` invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunSummary {
    /// How many jobs were claimed and executed.
    pub processed: u32,
    /// How many of those completed successfully.
    pub succeeded: u32,
    /// How many of those failed (requeued or dead-lettered).
    pub failed: u32,
    /// How many were dead-lettered (exhausted their retry budget).
    pub dead_lettered: u32,
}

impl std::fmt::Display for RunSummary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} processed, {} succeeded, {} failed ({} dead-lettered)",
            self.processed, self.succeeded, self.failed, self.dead_lettered
        )
    }
}

impl<'a> Runner<'a> {
    /// Create a runner that uses `conn` for claim/complete/fail and `registry`
    /// for handler lookups.
    pub fn new(conn: &'a mut Connection, registry: &'a JobRegistry) -> Self {
        Self { conn, registry }
    }

    /// Run the claim loop until either no more jobs are available or
    /// `max_iterations` is reached. Returns a summary.
    ///
    /// `max_iterations` is a safety bound so a runaway enqueue source can't
    /// keep the runner busy forever in a single call. Production callers
    /// typically pass `u32::MAX`; tests pass a small number to bound runtime.
    pub fn run_until_idle(&mut self, max_iterations: u32) -> Result<RunSummary> {
        jobs::ensure_jobs_table(self.conn)?;
        let mut summary = RunSummary {
            processed: 0,
            succeeded: 0,
            failed: 0,
            dead_lettered: 0,
        };

        for _ in 0..max_iterations {
            let Some(job) = jobs::claim_next(self.conn)? else {
                break; // No more pending jobs.
            };
            summary.processed += 1;

            let outcome = self.execute(&job);
            match outcome {
                HandlerOutcome::Success => {
                    jobs::complete(self.conn, job.id)?;
                    summary.succeeded += 1;
                }
                HandlerOutcome::Failure { message } => {
                    let dead_before = count_dead(self.conn)?;
                    jobs::fail(self.conn, job.id, &message)?;
                    let dead_after = count_dead(self.conn)?;
                    summary.failed += 1;
                    if dead_after > dead_before {
                        summary.dead_lettered += 1;
                    }
                }
            }
        }
        Ok(summary)
    }

    /// Execute one claimed job by looking up its handler and calling it.
    /// Unknown kinds produce a failure outcome with a clear message — never
    /// silently skips.
    fn execute(&self, job: &ClaimedJob) -> HandlerOutcome {
        match self.registry.get(&job.kind) {
            Some(handler) => handler.handle(&job.payload),
            None => HandlerOutcome::failure(format!(
                "no handler registered for job kind {:?}",
                job.kind
            )),
        }
    }
}

/// Count jobs in the `dead` state. Used by the runner to detect dead-lettering.
fn count_dead(conn: &Connection) -> Result<u32> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM jobs WHERE status = 'failed'",
        [],
        |r| r.get(0),
    )?;
    Ok(u32::try_from(n).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    /// A handler that always succeeds. Used in tests where we don't care about
    /// the payload.
    struct AlwaysSucceed;

    impl JobHandler for AlwaysSucceed {
        fn handle(&self, _payload: &str) -> HandlerOutcome {
            HandlerOutcome::success()
        }
    }

    /// A handler that fails N times then succeeds on the (N+1)th call.
    /// Used to test the retry-then-succeed path.
    struct FailNTimes {
        n: std::sync::Mutex<u32>,
    }

    impl JobHandler for FailNTimes {
        fn handle(&self, _payload: &str) -> HandlerOutcome {
            let mut count = self.n.lock().unwrap();
            if *count > 0 {
                *count -= 1;
                HandlerOutcome::failure(format!("attempt failed, {} more retries expected", *count))
            } else {
                HandlerOutcome::success()
            }
        }
    }

    /// A handler that always fails. Used to test the dead-letter path.
    struct AlwaysFail;

    impl JobHandler for AlwaysFail {
        fn handle(&self, _payload: &str) -> HandlerOutcome {
            HandlerOutcome::failure("permanent failure")
        }
    }

    /// A handler that echoes the payload back as the failure message — used
    /// to verify the payload is passed through correctly.
    struct EchoPayload;

    impl JobHandler for EchoPayload {
        fn handle(&self, payload: &str) -> HandlerOutcome {
            HandlerOutcome::failure(format!("payload was: {payload}"))
        }
    }

    fn fresh_db() -> Connection {
        let f = NamedTempFile::new()
            .unwrap()
            .into_temp_path()
            .keep()
            .unwrap();
        let mut conn = crate::db::open(&f).unwrap();
        crate::db::ensure_migrations_table(&conn).unwrap();
        crate::migrations::run_all(&mut conn).unwrap();
        jobs::ensure_jobs_table(&conn).unwrap();
        conn
    }

    #[test]
    fn end_to_end_enqueue_claim_execute_success() {
        // Per KNOWN PITFALLS: test the full claim loop end to end, not by
        // calling components directly.
        let mut conn = fresh_db();
        let registry = JobRegistry::new().register("test.echo", Arc::new(AlwaysSucceed));
        let id = jobs::enqueue(&conn, "test.echo", r#"{"msg":"hi"}"#).unwrap();
        assert!(id > 0);

        let summary = {
            let mut runner = Runner::new(&mut conn, &registry);
            runner.run_until_idle(10).unwrap()
        };
        assert_eq!(summary.processed, 1);
        assert_eq!(summary.succeeded, 1);
        assert_eq!(summary.failed, 0);
        assert_eq!(summary.dead_lettered, 0);

        // The job is now in `done` state.
        let state: String = conn
            .query_row(
                "SELECT status FROM jobs WHERE id = ?1",
                rusqlite::params![id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(state, "completed");
    }

    #[test]
    fn run_until_idle_returns_zero_when_queue_empty() {
        let mut conn = fresh_db();
        let registry = JobRegistry::new().register("test.echo", Arc::new(AlwaysSucceed));
        let summary = {
            let mut runner = Runner::new(&mut conn, &registry);
            runner.run_until_idle(10).unwrap()
        };
        assert_eq!(summary.processed, 0);
    }

    #[test]
    fn payload_is_passed_to_handler() {
        let mut conn = fresh_db();
        // EchoPayload returns Failure with the payload as the message — we
        // can then read the last_error from the jobs row to confirm the
        // payload arrived intact.
        let registry = JobRegistry::new().register("test.echo", Arc::new(EchoPayload));
        let id = jobs::enqueue(&conn, "test.echo", r#"{"k":"v"}"#).unwrap();
        {
            let mut runner = Runner::new(&mut conn, &registry);
            let _ = runner.run_until_idle(10).unwrap();
        }

        // The job should have failed (and been requeued with backoff).
        let (state, last_error): (String, Option<String>) = conn
            .query_row(
                "SELECT status, error FROM jobs WHERE id = ?1",
                rusqlite::params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(state, "queued"); // requeued, not dead-lettered (attempt=1 < max=3)
        assert!(last_error.unwrap().contains(r#"{"k":"v"}"#));
    }

    #[test]
    fn unknown_kind_is_failed_with_clear_message() {
        let mut conn = fresh_db();
        let registry = JobRegistry::new(); // empty — no handlers
        let id = jobs::enqueue(&conn, "test.unknown", "{}").unwrap();
        let summary = {
            let mut runner = Runner::new(&mut conn, &registry);
            runner.run_until_idle(10).unwrap()
        };

        assert_eq!(summary.processed, 1);
        assert_eq!(summary.failed, 1);
        assert_eq!(summary.succeeded, 0);

        // last_error mentions the unknown kind.
        let last_error: Option<String> = conn
            .query_row(
                "SELECT error FROM jobs WHERE id = ?1",
                rusqlite::params![id],
                |r| r.get(0),
            )
            .unwrap();
        let msg = last_error.unwrap();
        assert!(msg.contains("no handler registered"));
        assert!(msg.contains("test.unknown"));
    }

    #[test]
    fn always_fail_handler_eventually_dead_letters() {
        let mut conn = fresh_db();
        // Force a low max_attempts so the test doesn't have to wait through
        // the exponential backoff schedule.
        let registry = JobRegistry::new().register("test.always_fail", Arc::new(AlwaysFail));
        let id = jobs::enqueue(&conn, "test.always_fail", "{}").unwrap();
        // Override max_attempts to 1 so the FIRST failure dead-letters.
        conn.execute(
            "UPDATE jobs SET max_attempts = 1 WHERE id = ?1",
            rusqlite::params![id],
        )
        .unwrap();
        // Also reset available_at to now so the runner can claim it immediately
        // (the enqueue inserts with now, but claim_next uses julianday which is
        // robust to format differences).
        conn.execute(
            "UPDATE jobs SET run_at = datetime('now') WHERE id = ?1",
            rusqlite::params![id],
        )
        .unwrap();

        let summary = {
            let mut runner = Runner::new(&mut conn, &registry);
            runner.run_until_idle(10).unwrap()
        };

        // First iteration: claim + fail → dead-letter (attempts=1 >= max_attempts=1).
        assert_eq!(summary.processed, 1);
        assert_eq!(summary.failed, 1);
        assert_eq!(summary.dead_lettered, 1);

        let state: String = conn
            .query_row(
                "SELECT status FROM jobs WHERE id = ?1",
                rusqlite::params![id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(state, "failed");
    }

    #[test]
    fn fail_then_succeed_eventually_completes() {
        let mut conn = fresh_db();
        // Fail 2 times, then succeed on the 3rd attempt.
        let handler = Arc::new(FailNTimes {
            n: std::sync::Mutex::new(2),
        });
        let registry = JobRegistry::new().register("test.flaky", handler);
        let id = jobs::enqueue(&conn, "test.flaky", "{}").unwrap();

        // First run: claims, fails (attempt 1). Requeued with backoff.
        // We need to reset available_at between runs because the backoff
        // would otherwise push it minutes into the future.
        {
            let mut runner = Runner::new(&mut conn, &registry);
            let s1 = runner.run_until_idle(1).unwrap();
            assert_eq!(s1.failed, 1);
            assert_eq!(s1.succeeded, 0);
        }

        // Reset available_at to now (simulate backoff elapsing).
        conn.execute(
            "UPDATE jobs SET run_at = datetime('now') WHERE id = ?1",
            rusqlite::params![id],
        )
        .unwrap();
        {
            let mut runner = Runner::new(&mut conn, &registry);
            let s2 = runner.run_until_idle(1).unwrap();
            assert_eq!(s2.failed, 1);
            assert_eq!(s2.succeeded, 0);
        }

        // Reset and try again — should succeed this time.
        conn.execute(
            "UPDATE jobs SET run_at = datetime('now') WHERE id = ?1",
            rusqlite::params![id],
        )
        .unwrap();
        let s3 = {
            let mut runner = Runner::new(&mut conn, &registry);
            runner.run_until_idle(1).unwrap()
        };
        assert_eq!(s3.succeeded, 1);
        assert_eq!(s3.failed, 0);

        // Final state is `done`.
        let state: String = conn
            .query_row(
                "SELECT status FROM jobs WHERE id = ?1",
                rusqlite::params![id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(state, "completed");
    }

    #[test]
    fn run_summary_display_includes_counts() {
        let s = RunSummary {
            processed: 5,
            succeeded: 3,
            failed: 2,
            dead_lettered: 1,
        };
        let display = format!("{}", s);
        assert!(display.contains("5 processed"));
        assert!(display.contains("3 succeeded"));
        assert!(display.contains("2 failed"));
        assert!(display.contains("1 dead-lettered"));
    }

    #[test]
    fn registry_len_and_is_empty() {
        let r = JobRegistry::new();
        assert!(r.is_empty());
        assert_eq!(r.len(), 0);

        let r2 = r.clone().register("a", Arc::new(AlwaysSucceed));
        assert!(!r2.is_empty());
        assert_eq!(r2.len(), 1);

        // Original is unchanged (immutable register).
        assert!(r.is_empty());
    }

    #[test]
    fn handler_outcome_constructors() {
        let s = HandlerOutcome::success();
        assert!(matches!(s, HandlerOutcome::Success));

        let f = HandlerOutcome::failure("boom");
        match f {
            HandlerOutcome::Failure { message } => assert_eq!(message, "boom"),
            _ => panic!("expected Failure"),
        }
    }

    #[test]
    fn job_handler_trait_object_is_send_sync() {
        // The registry stores Arc<dyn JobHandler>; the trait must be Send+Sync
        // so the registry can cross thread boundaries (Tokio worker pool).
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Arc<dyn JobHandler>>();
    }
}
