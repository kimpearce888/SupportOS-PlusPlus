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

/// Read a bool setting. Returns `default` if the key is absent.
/// Stored as the strings `"true"` / `"false"` (SQLite-friendly, debuggable).
pub fn get_bool(conn: &Connection, key: &str, default: bool) -> Result<bool> {
    Ok(match get_string(conn, key)? {
        None => default,
        Some(s) => match s.as_str() {
            "true" => true,
            "false" => false,
            other => {
                return Err(Error::Config(format!(
                    "settings key {key:?} = {other:?}; expected \"true\" or \"false\""
                )))
            }
        },
    })
}

/// Write a bool setting. Stored as `"true"` / `"false"`.
pub fn set_bool(conn: &Connection, key: &str, value: bool) -> Result<()> {
    set_string(conn, key, if value { "true" } else { "false" })
}

/// Read an i64 setting. Returns `default` if absent or unparseable.
pub fn get_i64(conn: &Connection, key: &str, default: i64) -> Result<i64> {
    Ok(match get_string(conn, key)? {
        None => default,
        Some(s) => s.parse().map_err(|_| {
            Error::Config(format!("settings key {key:?} = {s:?}; expected an integer"))
        })?,
    })
}

/// Write an i64 setting.
pub fn set_i64(conn: &Connection, key: &str, value: i64) -> Result<()> {
    set_string(conn, key, &value.to_string())
}

/// Read a JSON-typed setting, deserializing into `T`. Returns `None` if absent.
pub fn get_json<T: serde::de::DeserializeOwned>(conn: &Connection, key: &str) -> Result<Option<T>> {
    match get_string(conn, key)? {
        None => Ok(None),
        Some(s) => {
            let v = serde_json::from_str(&s).map_err(|e| {
                Error::Config(format!("settings key {key:?} is not valid JSON: {e}"))
            })?;
            Ok(Some(v))
        }
    }
}

/// Write a value as JSON.
pub fn set_json<T: serde::Serialize>(conn: &Connection, key: &str, value: &T) -> Result<()> {
    let s = serde_json::to_string(value)
        .map_err(|e| Error::Config(format!("settings key {key:?} failed to serialize: {e}")))?;
    set_string(conn, key, &s)
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

// --------------------------------------------------------------------------
// First-run flag (single-row app_state table from migration 1)
// --------------------------------------------------------------------------

/// Returns `true` if first-run onboarding has been completed.
pub fn first_run_done(conn: &Connection) -> Result<bool> {
    let v: i64 = conn.query_row(
        "SELECT first_run_done FROM app_state WHERE id = 1",
        [],
        |r| r.get(0),
    )?;
    Ok(v != 0)
}

/// Mark first-run onboarding as complete. Idempotent.
pub fn mark_first_run_done(conn: &Connection) -> Result<()> {
    conn.execute("UPDATE app_state SET first_run_done = 1 WHERE id = 1", [])?;
    Ok(())
}

/// Qdrant health probe — mirrors the reference `QdrantAdapter.health()`
/// (`src/server/integrations/qdrant/qdrantAdapter.ts:62-74`): GET
/// `{url}/collections` with a 5s timeout; graceful failure shape.
pub async fn qdrant_health(url: &str, enabled: bool) -> QdrantHealth {
    if !enabled {
        return QdrantHealth {
            connected: false,
            url: url.to_string(),
            collections: Vec::new(),
            error: Some("Qdrant disabled in settings".to_string()),
        };
    }
    let url = url.trim_end_matches('/').to_string();
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .ok();
    match client {
        Some(client) => match client.get(format!("{url}/collections")).send().await {
            Ok(resp) if resp.status().is_success() => {
                match resp.json::<serde_json::Value>().await {
                    Ok(body) => {
                        let collections = body
                            .get("result")
                            .and_then(|r| r.as_array())
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|c| {
                                        c.get("name").and_then(|n| n.as_str()).map(String::from)
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        QdrantHealth {
                            connected: true,
                            url,
                            collections,
                            error: None,
                        }
                    }
                    Err(e) => QdrantHealth {
                        connected: false,
                        url,
                        collections: Vec::new(),
                        error: Some(e.to_string()),
                    },
                }
            }
            Ok(resp) => QdrantHealth {
                connected: false,
                url,
                collections: Vec::new(),
                error: Some(format!("Qdrant GET /collections -> {}", resp.status())),
            },
            Err(e) => QdrantHealth {
                connected: false,
                url,
                collections: Vec::new(),
                error: Some(e.to_string()),
            },
        },
        None => QdrantHealth {
            connected: false,
            url,
            collections: Vec::new(),
            error: Some("could not build HTTP client".to_string()),
        },
    }
}

/// Reference `QdrantHealth` shape.
#[derive(Debug, Clone)]
pub struct QdrantHealth {
    pub connected: bool,
    pub url: String,
    pub collections: Vec<String>,
    pub error: Option<String>,
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
        crate::db::ensure_migrations_table(&conn).unwrap();
        crate::migrations::run_all(&mut conn).unwrap();
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
    fn bool_round_trip_with_default() {
        let conn = fresh_db();
        assert!(!(get_bool(&conn, "demo_mode", false).unwrap()));
        assert!(get_bool(&conn, "demo_mode", true).unwrap());
        set_bool(&conn, "demo_mode", true).unwrap();
        assert!(get_bool(&conn, "demo_mode", false).unwrap());
        set_bool(&conn, "demo_mode", false).unwrap();
        assert!(!(get_bool(&conn, "demo_mode", true).unwrap()));
    }

    #[test]
    fn bool_invalid_value_errors() {
        let conn = fresh_db();
        set_string(&conn, "demo_mode", "yes").unwrap(); // not "true"/"false"
        let err = get_bool(&conn, "demo_mode", false).unwrap_err();
        assert!(matches!(err, Error::Config(_)));
        assert!(err.to_string().contains("demo_mode"));
    }

    #[test]
    fn i64_round_trip_with_default() {
        let conn = fresh_db();
        assert_eq!(get_i64(&conn, "sync_interval_min", 5).unwrap(), 5);
        set_i64(&conn, "sync_interval_min", 10).unwrap();
        assert_eq!(get_i64(&conn, "sync_interval_min", 5).unwrap(), 10);
    }

    #[test]
    fn i64_invalid_value_errors() {
        let conn = fresh_db();
        set_string(&conn, "sync_interval_min", "ten").unwrap();
        let err = get_i64(&conn, "sync_interval_min", 5).unwrap_err();
        assert!(matches!(err, Error::Config(_)));
    }

    #[test]
    fn json_round_trip() {
        #[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
        struct Window {
            width: u32,
            height: u32,
        }
        let conn = fresh_db();
        assert_eq!(get_json::<Window>(&conn, "window").unwrap(), None);
        let w = Window {
            width: 1280,
            height: 800,
        };
        set_json(&conn, "window", &w).unwrap();
        assert_eq!(get_json::<Window>(&conn, "window").unwrap(), Some(w));
    }

    #[test]
    fn json_invalid_value_errors() {
        let conn = fresh_db();
        set_string(&conn, "window", "{not valid json").unwrap();
        let err = get_json::<serde_json::Value>(&conn, "window").unwrap_err();
        assert!(matches!(err, Error::Config(_)));
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

    #[test]
    fn first_run_flag_starts_false_and_is_markable() {
        let conn = fresh_db();
        assert!(!(first_run_done(&conn).unwrap()));
        mark_first_run_done(&conn).unwrap();
        assert!(first_run_done(&conn).unwrap());
        // Idempotent: marking again doesn't error or change state.
        mark_first_run_done(&conn).unwrap();
        assert!(first_run_done(&conn).unwrap());
    }
}
