//! Single-use OAuth state store (A2).
//!
//! Per spec A2: OAuth state must be "single-use" — generated before the
//! redirect, verified exactly once on callback, then deleted. A replay
//! attempt (same state presented twice) must fail with `Error::OauthStateInvalid`.

use rusqlite::{params, Connection};

use crate::error::{Error, Result};

/// Create the `oauth_states` table if it doesn't exist. Idempotent.
pub fn ensure_oauth_states_table(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS oauth_states (
            state        TEXT PRIMARY KEY,
            created_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
            consumed_at  TEXT,
            redirect_uri TEXT,
            -- The Help Scout OAuth flow needs the redirect URI to be the same
            -- on authorize and callback; we store it here to verify.
            -- States are deleted after consumption AND after a TTL (M2 will
            -- add the TTL sweeper; for M1 we only need single-use).
            scope        TEXT
        );",
    )?;
    Ok(())
}

/// Issue a new OAuth state. The state is a random hex string (32 chars from
/// 16 random bytes). Returns the state. The caller passes it to Help Scout's
/// authorize URL.
///
/// `redirect_uri` and `scope` are stored alongside for callback verification.
pub fn issue_state(conn: &Connection, redirect_uri: &str, scope: Option<&str>) -> Result<String> {
    ensure_oauth_states_table(conn)?;
    let state = random_state();
    conn.execute(
        "INSERT INTO oauth_states (state, redirect_uri, scope) VALUES (?1, ?2, ?3)",
        params![state, redirect_uri, scope],
    )?;
    Ok(state)
}

/// Consume the OAuth state — verifies it exists and hasn't been consumed yet.
/// If valid, the state is marked consumed (single-use: a second call with the
/// same state returns `Error::OauthStateInvalid`).
///
/// Returns `Ok(())` on success; `Err(Error::OauthStateInvalid)` if the state
/// is unknown, already consumed, or expired (M2 will add expiry).
pub fn consume_state(conn: &Connection, state: &str) -> Result<()> {
    ensure_oauth_states_table(conn)?;
    let exists_unconsumed: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM oauth_states WHERE state = ?1 AND consumed_at IS NULL)",
        params![state],
        |r| r.get(0),
    )?;
    if !exists_unconsumed {
        return Err(Error::OauthStateInvalid);
    }
    // Mark consumed.
    conn.execute(
        "UPDATE oauth_states SET consumed_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE state = ?1",
        params![state],
    )?;
    Ok(())
}

/// Look up the redirect URI associated with the state. Used during the
/// callback to verify the URI matches the one sent to Help Scout at
/// authorize time. Returns `None` for unknown states (the caller then
/// rejects the callback).
pub fn redirect_uri_for(conn: &Connection, state: &str) -> Result<Option<String>> {
    ensure_oauth_states_table(conn)?;
    let v: Option<String> = conn
        .prepare("SELECT redirect_uri FROM oauth_states WHERE state = ?1")?
        .query_row(params![state], |r| r.get::<_, String>(0))
        .ok();
    Ok(v)
}

/// Delete consumed states older than the given number of seconds. Returns
/// the count of deleted rows. M1 doesn't call this (no expiry yet); M2 will.
pub fn sweep_consumed(conn: &Connection, older_than_seconds: i64) -> Result<u32> {
    let rows = conn.execute(
        "DELETE FROM oauth_states
          WHERE consumed_at IS NOT NULL
            AND julianday('now') - julianday(consumed_at) >= (?1 / 86400.0)",
        params![older_than_seconds],
    )?;
    Ok(u32::try_from(rows).unwrap_or(0))
}

/// Generate a 32-char hex state (16 random bytes).
fn random_state() -> String {
    let bytes: [u8; 16] = rand::random();
    hex_lower(&bytes)
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(hex_digit(b >> 4));
        out.push(hex_digit(b & 0x0f));
    }
    out
}

fn hex_digit(n: u8) -> char {
    match n {
        0..=9 => (b'0' + n) as char,
        10..=15 => (b'a' + (n - 10)) as char,
        _ => unreachable!("hex_digit got {n} (must be 0..15)"),
    }
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
        crate::db::open(&f).unwrap()
    }

    #[test]
    fn issue_state_returns_a_32_char_hex_string() {
        let conn = fresh_db();
        let s = issue_state(&conn, "http://127.0.0.1:1420/oauth/callback", None).unwrap();
        assert_eq!(s.len(), 32);
        assert!(s
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    #[test]
    fn issue_state_produces_unique_values() {
        let conn = fresh_db();
        let s1 = issue_state(&conn, "http://x", None).unwrap();
        let s2 = issue_state(&conn, "http://x", None).unwrap();
        assert_ne!(s1, s2);
    }

    #[test]
    fn consume_state_succeeds_for_issued_state() {
        let conn = fresh_db();
        let s = issue_state(&conn, "http://x", None).unwrap();
        consume_state(&conn, &s).expect("issued state must be consumable");
    }

    #[test]
    fn consume_state_rejects_unknown_state() {
        let conn = fresh_db();
        let err = consume_state(&conn, "nonexistent").unwrap_err();
        assert!(matches!(err, Error::OauthStateInvalid));
    }

    #[test]
    fn consume_state_rejects_replay_after_consumption() {
        // The single-use guarantee: a state presented twice must fail on the
        // second presentation.
        let conn = fresh_db();
        let s = issue_state(&conn, "http://x", None).unwrap();
        consume_state(&conn, &s).unwrap();
        let err = consume_state(&conn, &s).unwrap_err();
        assert!(matches!(err, Error::OauthStateInvalid));
    }

    #[test]
    fn redirect_uri_for_returns_stored_value() {
        let conn = fresh_db();
        let s = issue_state(
            &conn,
            "http://127.0.0.1:1420/oauth/callback",
            Some("conversations"),
        )
        .unwrap();
        assert_eq!(
            redirect_uri_for(&conn, &s).unwrap(),
            Some("http://127.0.0.1:1420/oauth/callback".to_string())
        );
    }

    #[test]
    fn redirect_uri_for_returns_none_for_unknown_state() {
        let conn = fresh_db();
        assert_eq!(redirect_uri_for(&conn, "missing").unwrap(), None);
    }

    #[test]
    fn ensure_oauth_states_table_is_idempotent() {
        let conn = fresh_db();
        ensure_oauth_states_table(&conn).unwrap();
        ensure_oauth_states_table(&conn).unwrap();
        ensure_oauth_states_table(&conn).unwrap();
    }

    #[test]
    fn sweep_consumed_deletes_only_consumed_states() {
        let conn = fresh_db();
        let s_active = issue_state(&conn, "http://x", None).unwrap();
        let s_consumed = issue_state(&conn, "http://x", None).unwrap();
        consume_state(&conn, &s_consumed).unwrap();

        // Nothing is older than 86400s, so the sweep deletes nothing yet.
        let deleted = sweep_consumed(&conn, 86_400).unwrap();
        assert_eq!(deleted, 0);

        // A 0-second TTL deletes all consumed states.
        let deleted = sweep_consumed(&conn, 0).unwrap();
        assert_eq!(deleted, 1);

        // The unconsumed state is still there.
        assert!(redirect_uri_for(&conn, &s_active).unwrap().is_some());
    }
}
