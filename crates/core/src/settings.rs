//! Settings store: typed, secrets redacted on read.
//!
//! Per spec: secrets never reach the UI layer and are always redacted in reads.
//! The `application_settings` table stores key/value rows; secrets are stored
//! encrypted in a separate `secrets` table and never appear in settings reads.

use rusqlite::{params, Connection};

use crate::error::{Error, Result};

/// Read a string setting by key. Returns `None` if the key does not exist.
pub fn get_string(conn: &Connection, key: &str) -> Result<Option<String>> {
    let v: Option<String> = conn
        .prepare("SELECT value FROM application_settings WHERE key = ?1")?
        .query_row(params![key], |r| r.get::<_, String>(0))
        .ok();
    Ok(v)
}

/// Write a string setting by key. Upsert (insert or update).
pub fn set_string(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO application_settings (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

/// Read the encrypted secret with `key`. Returns the raw bytes; used by internal callers only.
///
/// NEVER expose the result to the UI layer. Use [`redacted_secret`] for UI display.
pub fn get_secret(conn: &Connection, key: &str) -> Result<Option<Vec<u8>>> {
    let v: Option<Vec<u8>> = conn
        .prepare("SELECT value FROM secrets WHERE key = ?1")?
        .query_row(params![key], |r| r.get::<_, Vec<u8>>(0))
        .ok();
    Ok(v)
}

/// Returns `true` if a secret is stored for the key, without revealing the value.
pub fn has_secret(conn: &Connection, key: &str) -> Result<bool> {
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM secrets WHERE key = ?1)",
        params![key],
        |r| r.get(0),
    )?;
    Ok(exists)
}

/// The redacted form returned to the UI: empty if absent, `••••` if present.
pub fn redacted_secret(conn: &Connection, key: &str) -> Result<String> {
    if has_secret(conn, key)? {
        Ok("••••".into())
    } else {
        Ok(String::new())
    }
}

/// Delete a secret. Returns true if a row was deleted.
pub fn delete_secret(conn: &Connection, key: &str) -> Result<bool> {
    let rows = conn.execute("DELETE FROM secrets WHERE key = ?1", params![key])?;
    Ok(rows > 0)
}

/// Error returned when a caller tries to read a secret that isn't stored.
pub fn secret_or_error(conn: &Connection, key: &str) -> Result<Vec<u8>> {
    get_secret(conn, key)?.ok_or_else(|| Error::SecretNotFound { key: key.into() })
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
        conn.execute_batch(
            "CREATE TABLE application_settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE secrets (key TEXT PRIMARY KEY, value BLOB NOT NULL);",
        )
        .unwrap();
        conn
    }

    #[test]
    fn set_get_round_trip() {
        let conn = fresh_db();
        assert_eq!(get_string(&conn, "theme").unwrap(), None);
        set_string(&conn, "theme", "dark").unwrap();
        assert_eq!(get_string(&conn, "theme").unwrap(), Some("dark".into()));
        set_string(&conn, "theme", "light").unwrap(); // upsert
        assert_eq!(get_string(&conn, "theme").unwrap(), Some("light".into()));
    }

    #[test]
    fn redacted_secret_never_reveals_value() {
        let conn = fresh_db();
        assert_eq!(redacted_secret(&conn, "hs_client_secret").unwrap(), "");
        conn.execute(
            "INSERT INTO secrets (key, value) VALUES (?1, ?2)",
            params!["hs_client_secret", b"super_secret_value" as &[u8]],
        )
        .unwrap();
        assert_eq!(redacted_secret(&conn, "hs_client_secret").unwrap(), "••••");
        // The raw value is still retrievable internally:
        assert_eq!(
            secret_or_error(&conn, "hs_client_secret").unwrap(),
            b"super_secret_value"
        );
    }

    #[test]
    fn secret_or_error_when_absent() {
        let conn = fresh_db();
        let err = secret_or_error(&conn, "missing").unwrap_err();
        assert!(matches!(err, Error::SecretNotFound { .. }));
    }
}
