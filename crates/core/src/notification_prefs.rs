//! Notification preferences (M4-T06, reshaped to the reference in S17).
//!
//! Per spec M4 + CHANGELOG v2.1.x: "Notification Center v2 (15 types, per-type
//! preferences, retention pruning)."
//!
//! ## Per-type preferences
//!
//! Each of the 15 notification types can be independently opted in/out.
//! The preference is stored in the existing typed settings store
//! (`application_settings` table) keyed by `notifications.<type>.enabled`
//! — the same key the PUT /api/notifications/prefs/:type route writes, so
//! the sweep's BEFORE-insert gate (`notifications::pref_for`) and the
//! settings UI observe ONE switch. The default value comes from
//! `NotificationType::default_enabled()` (the catalog's source of truth).
//!
//! ## Retention pruning
//!
//! There is NO separate 30-day prune job: the reference prunes notifications
//! through the SAME data-retention window as the other local operational
//! data (Settings > Data → `maintenance::enforce_retention` →
//! `notifications::prune_older_than`, the reference `pruneOlderThan`).
//! The dead `notification.prune` job machinery was removed in S17 (no
//! reference counterpart).

use rusqlite::Connection;

use crate::catalog::NotificationType;
use crate::error::Result;
use crate::settings;

/// The settings key prefix for per-type preferences.
/// The full key is `notifications.<type>.enabled`.
const PREF_KEY_PREFIX: &str = "notifications.";
const PREF_KEY_SUFFIX: &str = ".enabled";

/// Build the settings key for a notification type:
/// `notifications.<type>.enabled`. Stored via the typed settings store
/// (`get_bool`/`set_bool`).
fn pref_key(notif_type: NotificationType) -> String {
    format!("{PREF_KEY_PREFIX}{}{PREF_KEY_SUFFIX}", notif_type.as_str())
}

/// A user's preference for a notification type: enabled (will see) or
/// disabled (will be filtered out at read time).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
    _user_id: i64,
    notif_type: NotificationType,
) -> Result<NotificationPreference> {
    let key = pref_key(notif_type);
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
    _user_id: i64,
    notif_type: NotificationType,
    preference: NotificationPreference,
) -> Result<()> {
    let key = pref_key(notif_type);
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
    _user_id: i64,
    types: &[NotificationType],
) -> Result<Vec<NotificationType>> {
    let mut filtered = Vec::with_capacity(types.len());
    for &t in types {
        if let NotificationPreference::Enabled = get_user_preference(conn, _user_id, t)? {
            filtered.push(t);
        }
    }
    Ok(filtered)
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
        let mut conn = crate::db::open(&f).unwrap();
        // The canonical boot chain — tests must exercise the REAL schema,
        // never a partial one.
        crate::bootstrap::apply_all(&mut conn).unwrap();
        conn
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
    fn preferences_are_per_type() {
        let conn = fresh_db();
        // Disable Mentioned; SlaBreach stays at default (enabled).
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

    // ---- pref_key sanity -----------------------------------------------

    #[test]
    fn pref_key_includes_the_type() {
        let key = pref_key(NotificationType::SlaBreach);
        assert_eq!(key, "notifications.sla_breach.enabled");
        assert!(key.starts_with("notifications."), "key: {key}");
        assert!(key.ends_with(".enabled"), "key: {key}");
    }

    #[test]
    fn pref_key_matches_the_route_and_sweep_gate() {
        // The PUT /api/notifications/prefs/:type route writes
        // `notifications.<type>.enabled`; the sweep's pref gate must read
        // the SAME key or the two switches diverge.
        let conn = fresh_db();
        let key = pref_key(NotificationType::SlaBreach);
        settings::set_bool(&conn, &key, false).unwrap();
        assert!(!crate::notifications::pref_for(&conn, NotificationType::SlaBreach).unwrap());
        assert_eq!(
            get_user_preference(&conn, 7, NotificationType::SlaBreach).unwrap(),
            NotificationPreference::Disabled
        );
    }
}
