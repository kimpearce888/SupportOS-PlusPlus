//! Query-invalidation bus — the Leptos counterpart of the reference's
//! TanStack Query `invalidateQueries` calls (UI-26).
//!
//! The reference `ServerEventsBridge` (src/client/api/events.ts) turns SSE
//! events into query invalidations so every open view refreshes within
//! seconds without polling. Leptos has no query cache, so the port keeps one
//! global version counter per query key: [`init`] creates the counters
//! inside the reactive runtime, SSE handlers bump them via [`invalidate`],
//! and pages read the counter inside their `create_effect` dependencies so
//! a bump re-runs the load effect — the same refetch-on-invalidate
//! semantics with the reference's exact key names.
//!
//! Keys are the reference's query-key roots (`'dashboard'`, `'customer'`,
//! `'interaction-profile'`, `'conversations'`, `'conversation'`,
//! `'nav-counts'`, `'operations-center'`, `'operations-workload'`, `'docs'`,
//! `'outreach-campaigns'`, `'notifications'`, `'notification-unread'`,
//! `'mention-queue'`).

use std::collections::HashMap;
use std::sync::OnceLock;

use leptos::*;

/// The reference query-key roots the bridge invalidates.
pub const KEYS: [&str; 13] = [
    "dashboard",
    "customer",
    "interaction-profile",
    "conversations",
    "conversation",
    "nav-counts",
    "operations-center",
    "operations-workload",
    "docs",
    "outreach-campaigns",
    "notifications",
    "notification-unread",
    "mention-queue",
];

/// Global counter table (created once by [`init`] inside the reactive
/// runtime, like `toasts::init`).
static COUNTERS: OnceLock<HashMap<&'static str, RwSignal<u32>>> = OnceLock::new();

/// Create the counter table. Must run inside the reactive runtime (called
/// from `app_view` before the shell mounts).
pub fn init() {
    let mut map = HashMap::new();
    for key in KEYS {
        map.insert(key, create_rw_signal(0u32));
    }
    let _ = COUNTERS.set(map);
}

/// The version counter for one key. Pages read this inside `create_effect`
/// dependencies: `let _ = queries::version("dashboard").get();`
///
/// Panics when the key is unknown or [`init`] has not run — both are
/// programming errors caught on the first render, not runtime conditions.
#[must_use]
pub fn version(key: &str) -> RwSignal<u32> {
    let counters = COUNTERS.get().unwrap_or_else(|| {
        panic!("queries::init() must run in app_view before any page reads a version")
    });
    *counters
        .get(key)
        .unwrap_or_else(|| panic!("unknown query key: {key} (not in KEYS)"))
}

/// Invalidate one key (the reference `qc.invalidateQueries({queryKey: [k]})`).
/// No-op before [`init`] (nothing has subscribed yet — same as invalidating a
/// cache nobody mounted).
pub fn invalidate(key: &str) {
    let Some(counters) = COUNTERS.get() else {
        return;
    };
    if let Some(signal) = counters.get(key) {
        signal.update(|v| *v = v.wrapping_add(1));
    }
}

/// The reference's event → invalidation map (ServerEventsBridge), as a pure
/// function so the mapping is unit-testable:
///
/// - `ratings` → dashboard, customer, interaction-profile
///   (+ the rating toast, which lives in `sse_bridge.rs`)
/// - `sync` → conversations, conversation, dashboard, nav-counts,
///   operations-center (+ docs when kind=incremental AND processed>0)
/// - `conversation` → conversations, conversation, nav-counts, dashboard,
///   operations-center, operations-workload
/// - `campaign` → outreach-campaigns
/// - `notification` → notifications, notification-unread, mention-queue,
///   operations-center, operations-workload
#[must_use]
pub fn keys_for_event(
    event: &str,
    sync_kind: Option<&str>,
    sync_processed: u32,
) -> Vec<&'static str> {
    match event {
        "ratings" => vec!["dashboard", "customer", "interaction-profile"],
        "sync" => {
            let mut keys = vec![
                "conversations",
                "conversation",
                "dashboard",
                "nav-counts",
                "operations-center",
            ];
            // v1.4.0+ parity: docs sync can import knowledge, so an
            // incremental sync with processed rows invalidates docs views.
            if sync_kind == Some("incremental") && sync_processed > 0 {
                keys.push("docs");
            }
            keys
        }
        "conversation" => vec![
            "conversations",
            "conversation",
            "nav-counts",
            "dashboard",
            "operations-center",
            "operations-workload",
        ],
        "campaign" => vec!["outreach-campaigns"],
        "notification" => vec![
            "notifications",
            "notification-unread",
            "mention-queue",
            "operations-center",
            "operations-workload",
        ],
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contains_all(keys: &[&str], want: &[&str]) -> bool {
        want.iter().all(|w| keys.contains(w))
    }

    #[test]
    fn keys_match_the_reference_query_roots() {
        for key in KEYS {
            assert!(!key.is_empty());
        }
        // The exact key set the bridge invalidates.
        assert_eq!(KEYS.len(), 13);
    }

    #[test]
    fn ratings_invalidates_dashboard_customer_interaction() {
        let keys = keys_for_event("ratings", None, 0);
        assert_eq!(keys, vec!["dashboard", "customer", "interaction-profile"]);
    }

    #[test]
    fn sync_invalidates_conversation_views() {
        let keys = keys_for_event("sync", Some("incremental"), 3);
        assert!(contains_all(
            &keys,
            &[
                "conversations",
                "conversation",
                "dashboard",
                "nav-counts",
                "operations-center"
            ]
        ));
        assert!(
            keys.contains(&"docs"),
            "incremental with processed>0 also invalidates docs"
        );
    }

    #[test]
    fn sync_without_processed_rows_skips_docs() {
        let keys = keys_for_event("sync", Some("incremental"), 0);
        assert!(
            !keys.contains(&"docs"),
            "processed=0 → no docs invalidation"
        );
        let keys = keys_for_event("sync", Some("initial"), 10);
        assert!(
            !keys.contains(&"docs"),
            "only incremental syncs invalidate docs"
        );
    }

    #[test]
    fn conversation_invalidates_both_operations_keys() {
        let keys = keys_for_event("conversation", None, 0);
        assert!(contains_all(
            &keys,
            &[
                "conversations",
                "conversation",
                "nav-counts",
                "dashboard",
                "operations-center",
                "operations-workload"
            ]
        ));
    }

    #[test]
    fn campaign_invalidates_outreach() {
        assert_eq!(
            keys_for_event("campaign", None, 0),
            vec!["outreach-campaigns"]
        );
    }

    #[test]
    fn notification_invalidates_center_and_mention_queue() {
        let keys = keys_for_event("notification", None, 0);
        assert!(contains_all(
            &keys,
            &["notifications", "notification-unread", "mention-queue"]
        ));
    }

    #[test]
    fn unknown_event_invalidates_nothing() {
        assert!(keys_for_event("hello", None, 0).is_empty());
        assert!(keys_for_event("error", None, 0).is_empty());
    }

    #[test]
    fn keys_are_unique_and_nonempty() {
        // The counter map is keyed by these names — a duplicate would
        // silently shadow one counter.
        let mut sorted: Vec<&str> = KEYS.to_vec();
        sorted.sort_unstable();
        let before = sorted.len();
        sorted.dedup();
        assert_eq!(before, sorted.len(), "KEYS must be unique: {sorted:?}");
    }

    // NOTE: the version-counter bump (`invalidate` → `version`) is global
    // OnceLock state created inside one reactive runtime; native tests each
    // create and drop their own runtime, so exercising it cross-test would
    // hit disposed signals. The per-event → keys mapping above is the
    // behavior under test; the bump itself is a one-line signal update.
}
