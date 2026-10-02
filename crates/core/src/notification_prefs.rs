//! Notification preferences + retention pruning (M4-T06).
//!
//! Per spec M4 + CHANGELOG v2.1.x: "Notification Center v2 (15 types, per-type
//! preferences, retention pruning)."
//!
//! ## Per-type preferences
//!
//! Each user can independently opt in/out of each of the 15 notification
//! types. The preference is stored in the existing typed settings store
//! (`application_settings` table) keyed by
//! `notifications.<type>.enabled.<user_id>`. The default value comes from
//! `NotificationType::default_enabled()` (the catalog's source of truth).
//!
//! Preferences are checked at READ time (when listing unread for a user),
//! not at WRITE time (when recording). This keeps the sweep simple — it
//! always records; the user's preference filter is applied when the UI
//! asks for the unread list.
//!
//! ## Retention pruning
//!
//! A `notification.prune` job (enqueued via `jobs::enqueue`) deletes
//! notifications older than the configurable TTL (default 30 days). The
//! comparison uses `julianday()` per KNOWN PITFALLS. Pruning is scoped
//! globally (no per-user dimension — old notifications are old for everyone).

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::catalog::NotificationType;
use crate::error::Result;
use crate::settings;

/// The default retention TTL in days. Per spec v2.1.x: "retention pruning."
pub const DEFAULT_RETENTION_TTL_DAYS: i64 = 30;

/// The settings key prefix for per-user, per-type preferences.
/// The full key is `notifications.<user_id>.<type>.enabled`.
const PREF_KEY_PREFIX: &str = "notifications.";
const PREF_KEY_SUFFIX: &str = ".enabled";

/// Build the settings key for a (user_id, type) pair:
/// `notifications.<user_id>.<type>.enabled`. Stored via the typed
/// settings store (`get_bool`/`set_bool`).
fn pref_key(user_id: i64, notif_type: NotificationType) -> String {
    format!(
        "{PREF_KEY_PREFIX}{user_id}.{}{PREF_KEY_SUFFIX}",
        notif_type.as_str()
    )
}

/// A user's preference for a notification type: enabled (will see) or
/// disabled (will be filtered out at read time).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationPreference {
    /// The user will see this notification type.
    Enabled,
    /// The user has opted out of this notification type.
    Disabled,
}

impl NotificationPreference {
    /// Convert to a bool for storage in the settings store.
    #[must_use]
    pub fn as_bool(self) -> bool {
        match self {
            Self::Enabled => true,
            Self::Disabled => false,
        }
    }

    /// Convert from a stored bool.
    #[must_use]
    pub fn from_bool(b: bool) -> Self {
        if b {
            Self::Enabled
        } else {
            Self::Disabled
        }
    }
}

/// Get a user's preference for a notification type. Falls back to the
/// catalog's `NotificationType::default_enabled()` if the user has never
/// explicitly set a preference.
///
/// # Errors
///
/// Returns `Error::Sqlite` if the settings store read fails.
pub fn get_user_preference(
    conn: &Connection,
    user_id: i64,
    notif_type: NotificationType,
) -> Result<NotificationPreference> {
    let key = pref_key(user_id, notif_type);
    let default = notif_type.default_enabled();
    let stored = settings::get_bool(conn, &key, default)?;
    Ok(NotificationPreference::from_bool(stored))
}

/// Set a user's preference for a notification type.
///
/// # Errors
///
/// Returns `Error::Sqlite` if the settings store write fails.
pub fn set_user_preference(
    conn: &Connection,
    user_id: i64,
    notif_type: NotificationType,
    preference: NotificationPreference,
) -> Result<()> {
    let key = pref_key(user_id, notif_type);
    settings::set_bool(conn, &key, preference.as_bool())
}

/// Filter a list of (NotificationType, count) pairs by the user's
/// preferences. Used by the UI to display only the notification types
/// the user has opted in to.
///
/// # Errors
///
/// Returns `Error::Sqlite` if any preference read fails.
pub fn filter_by_preferences(
    conn: &Connection,
    user_id: i64,
    types: &[NotificationType],
) -> Result<Vec<NotificationType>> {
    let mut filtered = Vec::with_capacity(types.len());
    for &t in types {
        if let NotificationPreference::Enabled = get_user_preference(conn, user_id, t)? {
            filtered.push(t);
        }
    }
    Ok(filtered)
}

/// The result of a retention-pruning pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PruneResult {
    /// The TTL in days that was used.
    pub ttl_days: i64,
    /// The number of notifications deleted.
    pub deleted: u32,
}

/// Prune (delete) notifications older than `ttl_days`. Per KNOWN PITFALLS:
/// the comparison uses `julianday()` (no lexical ISO-8601 comparison).
///
/// Returns the count of deleted rows.
///
/// # Errors
///
/// Returns `Error::Sqlite` if the delete fails.
pub fn prune_old_notifications(conn: &Connection, ttl_days: i64) -> Result<PruneResult> {
    let ttl_days = ttl_days.max(1); // never prune everything in one pass
    let rows = conn.execute(
        "DELETE FROM notifications
         WHERE julianday(created_at) < julianday('now', ?1)",
        params![format!("-{ttl_days} days")],
    )?;
    Ok(PruneResult {
        ttl_days,
        deleted: u32::try_from(rows).unwrap_or(0),
    })
}

/// Convenience: prune with the default TTL (30 days).
///
/// # Errors
///
/// Returns `Error::Sqlite` if the delete fails.
pub fn prune_with_default_ttl(conn: &Connection) -> Result<PruneResult> {
    prune_old_notifications(conn, DEFAULT_RETENTION_TTL_DAYS)
}

/// The job-handler kind name for the `notification.prune` job. Enqueued
/// via `jobs::enqueue(conn, NOTIFICATION_PRUNE_JOB_KIND, "{}")`.
pub const NOTIFICATION_PRUNE_JOB_KIND: &str = "notification.prune";

/// Enqueue a `notification.prune` job. The Tauri shell's job runner picks
/// it up and calls `prune_with_default_ttl`. The payload is empty (`{}`).
///
/// # Errors
///
/// Returns `Error::Sqlite` if the enqueue fails.
pub fn enqueue_prune_job(conn: &Connection) -> Result<i64> {
    crate::jobs::enqueue(conn, NOTIFICATION_PRUNE_JOB_KIND, "{}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activity::apply_m003;
    use crate::notifications::{apply_m005, record_notification};
    use crate::ticket_states::apply_m004;
    use chrono::{Duration, Utc};
    use tempfile::NamedTempFile;

    fn fresh_db() -> Connection {
        let f = NamedTempFile::new()
            .unwrap()
            .into_temp_path()
            .keep()
            .unwrap();
        let mut conn = crate::db::open(&f).unwrap();
        crate::db::ensure_migrations_table(&conn).unwrap();
        crate::migrations::run_all(&mut conn).unwrap();
        apply_m003(&conn).unwrap();
        apply_m004(&conn).unwrap();
        apply_m005(&conn).unwrap();
        conn
    }

    fn iso_days_ago(days: i64) -> String {
        (Utc::now() - Duration::days(days))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string()
    }

    fn insert_old_notification(conn: &Connection, days_old: i64) -> i64 {
        let id =
            record_notification(conn, &NotificationType::SlaBreach, Some(42), None, None).unwrap();
        let ts = iso_days_ago(days_old);
        conn.execute(
            "UPDATE notifications SET created_at = ?1 WHERE id = ?2",
            params![ts, id],
        )
        .unwrap();
        id
    }

    fn insert_recent_notification(conn: &Connection) -> i64 {
        // Default created_at = now (the migration's strftime default).
        record_notification(conn, &NotificationType::Mentioned, Some(42), None, None).unwrap()
    }

    // ---- Preferences --------------------------------------------------------

    #[test]
    fn default_preference_matches_catalog_default_enabled() {
        let conn = fresh_db();
        for t in NotificationType::ALL {
            let pref = get_user_preference(&conn, 42, t).unwrap();
            let expected = if t.default_enabled() {
                NotificationPreference::Enabled
            } else {
                NotificationPreference::Disabled
            };
            assert_eq!(
                pref, expected,
                "default for {t:?} should be {expected:?} (matches catalog)"
            );
        }
    }

    #[test]
    fn set_user_preference_round_trips() {
        let conn = fresh_db();
        let t = NotificationType::CampaignReply; // ALL types default-enabled (reference)
        assert_eq!(
            get_user_preference(&conn, 42, t).unwrap(),
            NotificationPreference::Enabled,
            "all notification types are default-enabled (reference)"
        );

        set_user_preference(&conn, 42, t, NotificationPreference::Enabled).unwrap();
        assert_eq!(
            get_user_preference(&conn, 42, t).unwrap(),
            NotificationPreference::Enabled
        );

        set_user_preference(&conn, 42, t, NotificationPreference::Disabled).unwrap();
        assert_eq!(
            get_user_preference(&conn, 42, t).unwrap(),
            NotificationPreference::Disabled
        );
    }

    #[test]
    fn preferences_are_per_user() {
        let conn = fresh_db();
        let t = NotificationType::Mentioned;

        // User 42 disables Mentioned; user 43 leaves it at default (enabled).
        set_user_preference(&conn, 42, t, NotificationPreference::Disabled).unwrap();
        assert_eq!(
            get_user_preference(&conn, 42, t).unwrap(),
            NotificationPreference::Disabled
        );
        assert_eq!(
            get_user_preference(&conn, 43, t).unwrap(),
            NotificationPreference::Enabled,
            "user 43's preference is independent"
        );
    }

    #[test]
    fn preferences_are_per_type() {
        let conn = fresh_db();
        // Disable Mentioned for user 42; SlaBreach stays at default (enabled).
        set_user_preference(
            &conn,
            42,
            NotificationType::Mentioned,
            NotificationPreference::Disabled,
        )
        .unwrap();
        assert_eq!(
            get_user_preference(&conn, 42, NotificationType::Mentioned).unwrap(),
            NotificationPreference::Disabled
        );
        assert_eq!(
            get_user_preference(&conn, 42, NotificationType::SlaBreach).unwrap(),
            NotificationPreference::Enabled,
            "SlaBreach preference is independent"
        );
    }

    #[test]
    fn filter_by_preferences_excludes_disabled_types() {
        let conn = fresh_db();
        // Disable CampaignReply + CustomerEvent (both default-disabled anyway).
        set_user_preference(
            &conn,
            42,
            NotificationType::Mentioned,
            NotificationPreference::Disabled,
        )
        .unwrap();
        let types: Vec<NotificationType> = NotificationType::ALL.to_vec();
        let filtered = filter_by_preferences(&conn, 42, &types).unwrap();
        // Mentioned should be excluded; all others default to their catalog defaults.
        assert!(
            !filtered.contains(&NotificationType::Mentioned),
            "Mentioned is disabled → excluded"
        );
        // SlaBreach (default enabled) should be present.
        assert!(filtered.contains(&NotificationType::SlaBreach));
    }

    #[test]
    fn preference_serializes_with_kind_tag() {
        let s = serde_json::to_string(&NotificationPreference::Enabled).unwrap();
        assert_eq!(s, "\"enabled\"");
        let s = serde_json::to_string(&NotificationPreference::Disabled).unwrap();
        assert_eq!(s, "\"disabled\"");
    }

    #[test]
    fn preference_bool_round_trip() {
        assert!(NotificationPreference::Enabled.as_bool());
        assert!(!NotificationPreference::Disabled.as_bool());
        assert_eq!(
            NotificationPreference::from_bool(true),
            NotificationPreference::Enabled
        );
        assert_eq!(
            NotificationPreference::from_bool(false),
            NotificationPreference::Disabled
        );
    }

    // ---- Retention pruning --------------------------------------------------

    #[test]
    fn prune_old_notifications_deletes_only_old_rows() {
        let conn = fresh_db();
        // 3 old notifications (40 days) + 2 recent.
        insert_old_notification(&conn, 40);
        insert_old_notification(&conn, 40);
        insert_old_notification(&conn, 40);
        insert_recent_notification(&conn);
        insert_recent_notification(&conn);

        let result = prune_old_notifications(&conn, 30).unwrap();
        assert_eq!(result.ttl_days, 30);
        assert_eq!(
            result.deleted, 3,
            "only the 3 old notifications should be deleted"
        );

        let remaining: i64 = conn
            .query_row("SELECT COUNT(*) FROM notifications", [], |r| r.get(0))
            .unwrap();
        assert_eq!(remaining, 2);
    }

    #[test]
    fn prune_with_default_ttl_uses_30_days() {
        let conn = fresh_db();
        insert_old_notification(&conn, 31); // 31 days old → prune
        insert_old_notification(&conn, 29); // 29 days old → keep
        let result = prune_with_default_ttl(&conn).unwrap();
        assert_eq!(result.ttl_days, DEFAULT_RETENTION_TTL_DAYS);
        assert_eq!(
            result.deleted, 1,
            "only the 31-day-old notification is pruned"
        );
    }

    #[test]
    fn prune_with_zero_ttl_clamps_to_one_day() {
        let conn = fresh_db();
        insert_recent_notification(&conn);
        let result = prune_old_notifications(&conn, 0).unwrap();
        // ttl_days is clamped to 1, so only notifications older than 1 day
        // are deleted. The recent one (just inserted) survives.
        assert_eq!(result.ttl_days, 1, "ttl_days clamps to 1");
        assert_eq!(result.deleted, 0, "recent notification survives");
    }

    #[test]
    fn prune_on_empty_db_returns_zero() {
        let conn = fresh_db();
        let result = prune_old_notifications(&conn, 30).unwrap();
        assert_eq!(result.deleted, 0);
    }

    #[test]
    fn prune_uses_julianday_not_lexical_comparison() {
        // Smoke test: a notification with created_at = "2099-01-01T10:00:00Z"
        // (far future) should NOT be pruned even with a large TTL.
        let conn = fresh_db();
        let id =
            record_notification(&conn, &NotificationType::SlaBreach, Some(42), None, None).unwrap();
        conn.execute(
            "UPDATE notifications SET created_at = ?1 WHERE id = ?2",
            params!["2099-01-01T10:00:00Z", id],
        )
        .unwrap();
        let result = prune_old_notifications(&conn, 30).unwrap();
        assert_eq!(result.deleted, 0, "future timestamps survive pruning");
    }

    // ---- Prune job enqueue --------------------------------------------------

    #[test]
    fn enqueue_prune_job_creates_a_pending_job() {
        let conn = fresh_db();
        let id = enqueue_prune_job(&conn).unwrap();
        assert!(id > 0);

        let (kind, state): (String, String) = conn
            .query_row(
                "SELECT kind, state FROM jobs WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(kind, NOTIFICATION_PRUNE_JOB_KIND);
        assert_eq!(state, "pending");
    }

    #[test]
    fn enqueue_prune_job_payload_is_empty_json() {
        let conn = fresh_db();
        let id = enqueue_prune_job(&conn).unwrap();
        let payload: String = conn
            .query_row("SELECT payload FROM jobs WHERE id = ?1", params![id], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(payload, "{}");
    }

    // ---- pref_key sanity -----------------------------------------------

    #[test]
    fn pref_key_includes_user_id_and_type() {
        let key = pref_key(42, NotificationType::SlaBreach);
        assert!(key.contains("42"), "key: {key}");
        assert!(key.contains("sla_breach"), "key: {key}");
        assert!(key.starts_with("notifications."), "key: {key}");
        assert!(key.ends_with(".enabled"), "key: {key}");
    }
}
