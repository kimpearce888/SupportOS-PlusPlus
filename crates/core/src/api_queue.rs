//! ApiQueue — the centralized API command queue (reference
//! `integrations/helpscout/apiQueue.ts`, spec #7).
//!
//! All Help Scout HTTP goes through here: **priority ordered, rate-limited,
//! bounded concurrency** (default 2). The reference's priority ladder is
//! preserved verbatim:
//!
//! - `USER_SEND` (0) — the user is sending a reply / note / new conversation
//! - `INTERACTIVE` (1) — interactive ops (status/tags/fields/snooze, reads
//!   serving an open conversation)
//! - `SYNC` (2) — background sync listings
//! - `ANALYTICS` (3) — ratings and analytics sweeps
//! - `INDEXING` (4) — attachment downloads / bulk indexing
//!
//! Dispatch (`pump`) is the exact port of the reference algorithm:
//!
//! 1. while `active < concurrency` and items are pending,
//! 2. sort by `(priority, seq)` — lower priority number first, FIFO within
//!    a level,
//! 3. ask the shared [`HsRateLimiter`] how long to wait; if the wait is
//!    positive the item goes back to the front and a single timer
//!    (capped at 5 s, reference `Math.min(wait, 5000)`) re-pumps,
//! 4. otherwise the item is dispatched: `active`/`dispatched` tick up and
//!    the waiter is released to run its future.
//!
//! The waiter's future is created lazily by the caller (a Rust future is
//! inert until first polled), so gating happens entirely in `pump` before
//! any HTTP starts — exactly like the reference's deferred `run` closure.
//! On completion `completed`/`failed` tick up, `active` ticks down and
//! `pump` runs again — the reference's `.then/.catch/.finally` chain.
//! The active slot travels THROUGH the permit channel, so a cancelled
//! caller always releases its slot.
//!
//! Stats snapshot shape (`statsSnapshot()`):
//! `{queued, active, dispatched, completed, failed, highWater}` — served
//! verbatim by `GET /api/sync/status` (`api_queue`).

use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::helpscout_real::HsRateLimiter;

/// Reference `PRIORITY.USER_SEND` — the user sending a reply (highest).
pub const PRIORITY_USER_SEND: u8 = 0;
/// Reference `PRIORITY.INTERACTIVE` — interactive ops.
pub const PRIORITY_INTERACTIVE: u8 = 1;
/// Reference `PRIORITY.SYNC` — sync listings.
pub const PRIORITY_SYNC: u8 = 2;
/// Reference `PRIORITY.ANALYTICS` — analytics reads.
pub const PRIORITY_ANALYTICS: u8 = 3;
/// Reference `PRIORITY.INDEXING` — indexing/bulk downloads.
pub const PRIORITY_INDEXING: u8 = 4;

/// Default concurrency (reference `new ApiQueue(limiter, concurrency = 2)`).
pub const DEFAULT_CONCURRENCY: usize = 2;

/// Queue stats (`apiQueue.ts` stats + `statsSnapshot` parity). Gauge/counter
/// holders shared with the queue; `snapshot()` is the wire shape.
#[derive(Default)]
pub struct ApiQueueStats {
    pub queued: AtomicI64,
    pub active: AtomicI64,
    pub dispatched: AtomicI64,
    pub completed: AtomicI64,
    pub failed: AtomicI64,
    pub high_water: AtomicI64,
}

impl ApiQueueStats {
    /// `statsSnapshot()` — the JSON served by `GET /api/sync/status`.
    pub fn snapshot(&self) -> Value {
        json!({
            "queued": self.queued.load(Ordering::Relaxed),
            "active": self.active.load(Ordering::Relaxed),
            "dispatched": self.dispatched.load(Ordering::Relaxed),
            "completed": self.completed.load(Ordering::Relaxed),
            "failed": self.failed.load(Ordering::Relaxed),
            "highWater": self.high_water.load(Ordering::Relaxed),
        })
    }
}

/// RAII active slot: guarantees `active` is released (and the queue
/// re-pumped) even if the caller's future is cancelled or panics — the
/// Rust analogue of the reference's `.finally(() => { active--; pump(); })`.
/// It travels through the permit channel, so the slot is owned by whoever
/// receives it (and dropped with them).
struct ActiveSlot {
    queue: Weak<ApiQueue>,
}

impl Drop for ActiveSlot {
    fn drop(&mut self) {
        if let Some(q) = self.queue.upgrade() {
            q.stats.active.fetch_sub(1, Ordering::Relaxed);
            q.pump();
        }
    }
}

/// One waiting request (`apiQueue.ts` `QueueItem`). The oneshot permit
/// carries the active slot: fulfilling it releases the waiter to run.
struct QueueItem {
    priority: u8,
    seq: u64,
    is_write: bool,
    permit: tokio::sync::oneshot::Sender<ActiveSlot>,
}

/// Centralized API command queue — the port of the reference `ApiQueue`.
pub struct ApiQueue {
    items: Mutex<Vec<QueueItem>>,
    stats: ApiQueueStats,
    limiter: Arc<HsRateLimiter>,
    concurrency: AtomicUsize,
    seq: AtomicU64,
    timer_armed: AtomicBool,
    /// Self-weak reference so `pump` (sync, `&self`) can arm its timer task.
    self_ref: Mutex<Weak<ApiQueue>>,
}

impl ApiQueue {
    /// Reference constructor: `new ApiQueue(limiter, concurrency = 2)`.
    pub fn new(limiter: Arc<HsRateLimiter>, concurrency: usize) -> Arc<Self> {
        let q = Arc::new(Self {
            items: Mutex::new(Vec::new()),
            stats: ApiQueueStats::default(),
            limiter,
            concurrency: AtomicUsize::new(concurrency.max(1)),
            seq: AtomicU64::new(0),
            timer_armed: AtomicBool::new(false),
            self_ref: Mutex::new(Weak::new()),
        });
        *q.self_ref.lock().unwrap_or_else(|p| p.into_inner()) = Arc::downgrade(&q);
        q
    }

    fn weak(&self) -> Weak<ApiQueue> {
        self.self_ref
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// Reference `setConcurrency(n)`: `Math.max(1, n)` then `pump()`.
    pub fn set_concurrency(&self, n: usize) {
        self.concurrency.store(n.max(1), Ordering::Relaxed);
        self.pump();
    }

    /// Stats snapshot — `statsSnapshot()` parity.
    pub fn snapshot(&self) -> Value {
        self.stats.snapshot()
    }

    /// `enqueue(priority, isWrite, fn)` — the single entry point for every
    /// provider call.
    pub async fn enqueue<T>(
        &self,
        priority: u8,
        is_write: bool,
        fut: impl Future<Output = Result<T>>,
    ) -> Result<T> {
        let (permit_tx, mut permit_rx) = tokio::sync::oneshot::channel::<ActiveSlot>();
        {
            let mut items = self.items.lock().unwrap_or_else(|p| p.into_inner());
            items.push(QueueItem {
                priority,
                seq: self.seq.fetch_add(1, Ordering::Relaxed),
                is_write,
                permit: permit_tx,
            });
            self.stats
                .queued
                .store(items.len() as i64, Ordering::Relaxed);
            self.stats
                .high_water
                .fetch_max(items.len() as i64, Ordering::Relaxed);
        }
        self.pump();
        // Wait for dispatch. A dropped queue (provider shut down while
        // requests were still waiting) surfaces as a retryable error.
        let slot = match permit_rx.try_recv() {
            Ok(s) => Some(s),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => permit_rx.await.ok(),
            Err(tokio::sync::oneshot::error::TryRecvError::Closed) => None,
        };
        let _slot = match slot {
            Some(s) => s,
            None => return Err(Error::Other("API queue shut down before dispatch".into())),
        };
        // Dispatched: hold the active slot while the future runs.
        let res = fut.await;
        match res {
            Ok(_) => {
                self.stats.completed.fetch_add(1, Ordering::Relaxed);
            }
            Err(_) => {
                self.stats.failed.fetch_add(1, Ordering::Relaxed);
            }
        }
        res
    }

    /// `pump()` — dispatch while capacity remains, honoring the rate
    /// limiter (a positive wait defers the head item and arms one timer,
    /// capped at 5 s like the reference's `Math.min(wait, 5000)`).
    fn pump(&self) {
        loop {
            let mut items = self.items.lock().unwrap_or_else(|p| p.into_inner());
            let active = self.stats.active.load(Ordering::Relaxed);
            if active >= self.concurrency.load(Ordering::Relaxed) as i64 || items.is_empty() {
                return;
            }
            items.sort_by(|a, b| a.priority.cmp(&b.priority).then(a.seq.cmp(&b.seq)));
            let item = items.remove(0);
            let wait = self.limiter.wait_time_ms(item.is_write);
            if wait > 0 {
                items.insert(0, item);
                self.stats
                    .queued
                    .store(items.len() as i64, Ordering::Relaxed);
                if !self.timer_armed.swap(true, Ordering::Relaxed) {
                    if let Some(q) = self.weak().upgrade() {
                        tokio::spawn(async move {
                            tokio::time::sleep(Duration::from_millis(wait.min(5_000) as u64)).await;
                            q.timer_armed.store(false, Ordering::Relaxed);
                            q.pump();
                        });
                    } else {
                        self.timer_armed.store(false, Ordering::Relaxed);
                    }
                }
                return;
            }
            self.stats.dispatched.fetch_add(1, Ordering::Relaxed);
            self.stats.active.fetch_add(1, Ordering::Relaxed);
            self.stats
                .queued
                .store(items.len() as i64, Ordering::Relaxed);
            drop(items);
            let slot = ActiveSlot { queue: self.weak() };
            if item.permit.send(slot).is_err() {
                // The waiter went away (caller cancelled): give the slot
                // straight back and keep pumping.
                self.stats.active.fetch_sub(1, Ordering::Relaxed);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests (apiQueue semantics: priority order, concurrency bound, stats)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn queue(concurrency: usize) -> Arc<ApiQueue> {
        ApiQueue::new(Arc::new(HsRateLimiter::new(None)), concurrency)
    }

    async fn wait_for(f: impl Fn() -> bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !f() {
            assert!(
                std::time::Instant::now() < deadline,
                "condition never became true"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Concurrency 2 (the default): exactly two futures run at once; the
    /// third waits for a slot.
    #[tokio::test]
    async fn concurrency_bound_is_two() {
        let q = queue(DEFAULT_CONCURRENCY);
        let release = Arc::new(tokio::sync::Notify::new());
        let mut holders = Vec::new();
        for _ in 0..2 {
            let qq = Arc::clone(&q);
            let rel = Arc::clone(&release);
            holders.push(tokio::spawn(async move {
                qq.enqueue(PRIORITY_SYNC, false, async {
                    rel.notified().await;
                    Ok(())
                })
                .await
            }));
        }
        wait_for(|| q.stats.active.load(Ordering::Relaxed) == 2).await;
        assert_eq!(q.stats.queued.load(Ordering::Relaxed), 0);

        // A third item must NOT run while both slots are held.
        let qq = Arc::clone(&q);
        let third =
            tokio::spawn(async move { qq.enqueue(PRIORITY_SYNC, false, async { Ok(()) }).await });
        wait_for(|| q.stats.queued.load(Ordering::Relaxed) == 1).await;
        assert_eq!(q.stats.active.load(Ordering::Relaxed), 2);
        assert!(q.stats.high_water.load(Ordering::Relaxed) >= 1);

        // Release both holders: the third dispatches and completes.
        release.notify_one();
        release.notify_one();
        for h in holders {
            h.await.unwrap().unwrap();
        }
        third.await.unwrap().unwrap();
        assert_eq!(q.stats.completed.load(Ordering::Relaxed), 3);
        assert_eq!(q.stats.active.load(Ordering::Relaxed), 0);
    }

    /// Priority order: with the single slot busy, a later USER_SEND jumps
    /// the queue ahead of earlier SYNC items (reference `items.sort`).
    #[tokio::test]
    async fn user_send_jumps_ahead_of_sync() {
        let q = queue(1); // one slot: deterministic ordering
        let release = Arc::new(tokio::sync::Notify::new());
        let running: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));

        let mut tasks = Vec::new();
        // Hold the single slot with a SYNC item.
        {
            let qq = Arc::clone(&q);
            let rel = Arc::clone(&release);
            let run = Arc::clone(&running);
            tasks.push(tokio::spawn(async move {
                qq.enqueue(PRIORITY_SYNC, false, async {
                    run.lock().unwrap().push(2);
                    rel.notified().await;
                    Ok(())
                })
                .await
            }));
        }
        wait_for(|| q.stats.active.load(Ordering::Relaxed) == 1).await;

        // Queue two SYNC items, then one USER_SEND (P0).
        for _ in 0..2 {
            let qq = Arc::clone(&q);
            let run = Arc::clone(&running);
            tasks.push(tokio::spawn(async move {
                qq.enqueue(PRIORITY_SYNC, false, async {
                    run.lock().unwrap().push(2);
                    Ok(())
                })
                .await
            }));
        }
        {
            let qq = Arc::clone(&q);
            let run = Arc::clone(&running);
            tasks.push(tokio::spawn(async move {
                qq.enqueue(PRIORITY_USER_SEND, false, async {
                    run.lock().unwrap().push(0);
                    Ok(())
                })
                .await
            }));
        }
        wait_for(|| q.stats.queued.load(Ordering::Relaxed) == 3).await;
        assert_eq!(q.stats.high_water.load(Ordering::Relaxed), 3);

        // Release: the USER_SEND must run before the two SYNC stragglers.
        release.notify_one();
        for t in tasks {
            t.await.unwrap().unwrap();
        }
        let order = running.lock().unwrap().clone();
        assert_eq!(order, vec![2, 0, 2, 2]);
        let snap = q.snapshot();
        assert_eq!(snap["completed"], 4);
        assert_eq!(snap["failed"], 0);
        assert_eq!(snap["active"], 0);
        assert_eq!(snap["queued"], 0);
        assert_eq!(snap["highWater"], 3);
    }

    /// FIFO within a priority level (reference `seq` tie-break).
    #[tokio::test]
    async fn fifo_within_same_priority() {
        let q = queue(1);
        let release = Arc::new(tokio::sync::Notify::new());
        let order: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
        let mut tasks = Vec::new();
        // holder
        {
            let qq = Arc::clone(&q);
            let rel = Arc::clone(&release);
            tasks.push(tokio::spawn(async move {
                qq.enqueue(PRIORITY_SYNC, false, async {
                    rel.notified().await;
                    Ok(())
                })
                .await
            }));
        }
        for i in 0..3u64 {
            let qq = Arc::clone(&q);
            let ord = Arc::clone(&order);
            tasks.push(tokio::spawn(async move {
                qq.enqueue(PRIORITY_SYNC, false, async {
                    ord.lock().unwrap().push(i);
                    Ok(())
                })
                .await
            }));
        }
        wait_for(|| q.stats.queued.load(Ordering::Relaxed) == 3).await;
        release.notify_one();
        for t in tasks {
            t.await.unwrap().unwrap();
        }
        assert_eq!(*order.lock().unwrap(), vec![0, 1, 2]);
    }

    /// Failures count in `failed`, successes in `completed` — and a failed
    /// item still releases its slot (reference `.catch` + `.finally`).
    #[tokio::test]
    async fn failed_items_release_their_slot_and_count() {
        let q = queue(2);
        let a = q.enqueue(PRIORITY_SYNC, false, async {
            Err::<i64, _>(Error::Other("boom".into()))
        });
        let b = q.enqueue(PRIORITY_SYNC, false, async { Ok(7) });
        let (ra, rb) = tokio::join!(a, b);
        assert!(ra.is_err());
        assert_eq!(rb.unwrap(), 7);
        assert_eq!(q.stats.failed.load(Ordering::Relaxed), 1);
        assert_eq!(q.stats.completed.load(Ordering::Relaxed), 1);
        assert_eq!(q.stats.active.load(Ordering::Relaxed), 0);
    }

    /// `set_concurrency(1..n)` — raising capacity immediately pumps waiting
    /// items (reference `setConcurrency` -> `pump`).
    #[tokio::test]
    async fn set_concurrency_raises_capacity_and_pumps() {
        let q = queue(1);
        let release = Arc::new(tokio::sync::Notify::new());
        let qq = Arc::clone(&q);
        let rel = Arc::clone(&release);
        let holder = tokio::spawn(async move {
            qq.enqueue(PRIORITY_SYNC, false, async {
                rel.notified().await;
                Ok(())
            })
            .await
        });
        wait_for(|| q.stats.active.load(Ordering::Relaxed) == 1).await;
        let q2 = Arc::clone(&q);
        let waiter =
            tokio::spawn(async move { q2.enqueue(PRIORITY_SYNC, false, async { Ok(()) }).await });
        wait_for(|| q.stats.queued.load(Ordering::Relaxed) == 1).await;
        q.set_concurrency(2);
        wait_for(|| q.stats.active.load(Ordering::Relaxed) == 2).await;
        assert_eq!(q.stats.queued.load(Ordering::Relaxed), 0);
        release.notify_one();
        holder.await.unwrap().unwrap();
        waiter.await.unwrap().unwrap();
    }

    /// Snapshot shape parity with the reference `statsSnapshot()`.
    #[test]
    fn snapshot_shape() {
        let q = queue(2);
        let snap = q.snapshot();
        for key in [
            "queued",
            "active",
            "dispatched",
            "completed",
            "failed",
            "highWater",
        ] {
            assert!(snap.get(key).is_some(), "missing key {key}");
        }
    }

    /// Priority constants keep the reference ladder.
    #[test]
    fn priority_ladder_matches_reference() {
        assert_eq!(PRIORITY_USER_SEND, 0);
        assert_eq!(PRIORITY_INTERACTIVE, 1);
        assert_eq!(PRIORITY_SYNC, 2);
        assert_eq!(PRIORITY_ANALYTICS, 3);
        assert_eq!(PRIORITY_INDEXING, 4);
        assert_eq!(DEFAULT_CONCURRENCY, 2);
    }
}
