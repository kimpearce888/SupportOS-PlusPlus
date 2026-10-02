//! Webhook HMAC-SHA1 signature verification (A2, A9).
//!
//! Per spec A2: webhook verification must be "timing-safe HMAC-SHA1".
//! Per KNOWN PITFALLS: persist-first with deduplication.
//!
//! This module is pure logic + a tiny SQLite-backed event store. The actual
//! axum route handler that wires these together lands in `loopback.rs`.

use rusqlite::{params, Connection};
use subtle::ConstantTimeEq;

use crate::error::{Error, Result};

/// The header Help Scout uses to send the webhook signature.
pub const HELPSCOUT_SIGNATURE_HEADER: &str = "X-Helpscout-Signature";

/// Compute the HMAC-SHA1 of `body` keyed by `secret`, returning the
/// **base64** digest — the exact format Help Scout uses in the signature
/// header (reference: `crypto.createHmac('sha1', secret).update(rawBody).digest('base64')`).
///
/// This is the canonical computation; [`verify_signature`] does the
/// timing-safe comparison.
#[must_use]
pub fn compute_signature(secret: &[u8], body: &[u8]) -> String {
    use std::io::Write;
    // HMAC-SHA1 from scratch — small enough not to need a dep (we already
    // pull in `sha1` via aes-gcm? No, we don't. Add it minimally inline.)
    //
    // For M1 the spec requires HMAC-SHA1 specifically (Help Scout's choice).
    // The block size for SHA-1 is 64 bytes.
    let block_size = 64;
    let key: Vec<u8> = if secret.len() > block_size {
        // Hash the key first (per HMAC spec) when it's longer than the block.
        sha1(secret).to_vec()
    } else {
        secret.to_vec()
    };
    let mut padded = key.clone();
    padded.resize(block_size, 0);
    let i_pad: Vec<u8> = padded.iter().map(|b| b ^ 0x36).collect();
    let o_pad: Vec<u8> = padded.iter().map(|b| b ^ 0x5c).collect();

    let mut inner = Vec::with_capacity(i_pad.len() + body.len());
    inner.write_all(&i_pad).ok();
    inner.write_all(body).ok();
    let inner_hash = sha1(&inner);

    let mut outer = Vec::with_capacity(o_pad.len() + inner_hash.len());
    outer.write_all(&o_pad).ok();
    outer.write_all(&inner_hash).ok();
    let outer_hash = sha1(&outer);

    base64_std(&outer_hash)
}

/// Verify that `provided_signature` matches the HMAC-SHA1 of `body` keyed
/// by `secret`. Timing-safe (constant-time comparison via `subtle`).
///
/// Returns `Ok(())` if the signature is valid; `Err(Error::WebhookSignature)`
/// otherwise. Never returns the secret or the computed signature.
pub fn verify_signature(secret: &[u8], body: &[u8], provided_signature: &str) -> Result<()> {
    let expected = compute_signature(secret, body);
    // Compare in constant time. Both must be the same length; if the provided
    // signature has a different length, the verification fails (and the
    // comparison is still constant-time over the shorter of the two).
    if expected.len() != provided_signature.len() {
        return Err(Error::WebhookSignature);
    }
    let expected_bytes = expected.as_bytes();
    let provided_bytes = provided_signature.as_bytes();
    if expected_bytes.ct_eq(provided_bytes).into() {
        Ok(())
    } else {
        Err(Error::WebhookSignature)
    }
}

// ---------------------------------------------------------------------------
// Minimal SHA-1 + hex (no external dep needed for this small use case)
// ---------------------------------------------------------------------------

/// Compute SHA-1 of `data` and return the 20-byte digest.
fn sha1(data: &[u8]) -> [u8; 20] {
    let mut state: [u32; 5] = [0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476, 0xc3d2e1f0];

    // Pre-processing: pad the message.
    let bit_len: u64 = (data.len() as u64).wrapping_mul(8);
    let mut padded = data.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());

    // Process each 512-bit (64-byte) chunk.
    for chunk in padded.chunks(64) {
        let mut w = [0u32; 80];
        // Indexed assignment to a fixed-size array is clearer here than the
        // iterator-based form clippy suggests.
        #[allow(clippy::needless_range_loop)]
        for i in 0..16 {
            let j = i * 4;
            w[i] = u32::from_be_bytes([chunk[j], chunk[j + 1], chunk[j + 2], chunk[j + 3]]);
        }
        #[allow(clippy::needless_range_loop)]
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }

        let mut a = state[0];
        let mut b = state[1];
        let mut c = state[2];
        let mut d = state[3];
        let mut e = state[4];

        // SHA-1 main loop: 80 rounds, each picking a different (f, k) based on
        // the round index. Indexed form is clearer here than the iterator form.
        #[allow(clippy::needless_range_loop)]
        for i in 0..80 {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5a827999),
                20..=39 => (b ^ c ^ d, 0x6ed9eba1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8f1bbcdc),
                _ => (b ^ c ^ d, 0xca62c1d6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(w[i]);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }

        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
        state[4] = state[4].wrapping_add(e);
    }

    let mut out = [0u8; 20];
    for (i, word) in state.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

/// Lowercase hex encoding.
fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(hex_digit(b >> 4));
        out.push(hex_digit(b & 0x0f));
    }
    out
}

/// Standard base64 encoding (RFC 4648, with `=` padding) — the format
/// `crypto.createHmac(...).digest('base64')` produces in the reference.
fn base64_std(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[(triple >> 18) as usize & 0x3f] as char);
        out.push(ALPHABET[(triple >> 12) as usize & 0x3f] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[(triple >> 6) as usize & 0x3f] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[triple as usize & 0x3f] as char);
        } else {
            out.push('=');
        }
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

// ---------------------------------------------------------------------------
// Event store (reference-compatible schema + dedup + state machine)
// ---------------------------------------------------------------------------

/// A persisted webhook event row (mirrors the reference `webhook_events` shape).
#[derive(Debug, Clone)]
pub struct WebhookEventRecord {
    pub id: i64,
    pub event_id: Option<String>,
    pub event_type: String,
    pub payload: String,
    pub processing_state: String,
}

/// Create the `webhook_events` table if it doesn't exist, and forward-migrate
/// the pre-parity-audit schema (`id TEXT PK, body, status, processed_at`) to
/// the reference-compatible one. Idempotent.
pub fn ensure_webhook_events_table(conn: &Connection) -> Result<()> {
    // Forward migration: if the old-format table exists (TEXT id + `body`
    // column), rename it out of the way. Its rows were never processable by
    // the reference pipeline (the old route 500'd on every insert), so they
    // are not carried over.
    let old_schema: Option<String> = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='webhook_events'",
            [],
            |r| r.get(0),
        )
        .ok();
    if let Some(schema) = old_schema {
        if schema.contains("body") && !schema.contains("event_hash") {
            conn.execute_batch(
                "ALTER TABLE webhook_events RENAME TO webhook_events_legacy;
                 DROP TABLE IF EXISTS webhook_events_legacy;",
            )?;
        }
    }

    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS webhook_events (
            id                INTEGER PRIMARY KEY AUTOINCREMENT,
            event_id          TEXT,
            event_hash        TEXT NOT NULL UNIQUE,
            event_type        TEXT NOT NULL,
            received_at       TEXT NOT NULL,
            payload           TEXT NOT NULL,
            processing_state  TEXT NOT NULL DEFAULT 'pending',
            processing_error  TEXT,
            attempts          INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS idx_webhook_events_state
            ON webhook_events (processing_state, id);",
    )?;
    Ok(())
}

/// Insert a webhook event with dedup by `sha256("{eventType}:{payload}")`
/// (exact reference dedup key). Returns `(row_id, duplicate)`.
pub fn insert_webhook_event(
    conn: &Connection,
    event_type: &str,
    payload: &str,
    event_id: Option<&str>,
) -> Result<(i64, bool)> {
    use sha2::{Digest, Sha256};
    ensure_webhook_events_table(conn)?;
    let mut hasher = Sha256::new();
    hasher.update(format!("{event_type}:{payload}"));
    let hash = hex_lower(&hasher.finalize());

    let existing: Option<i64> = conn
        .query_row(
            "SELECT id FROM webhook_events WHERE event_hash = ?1",
            params![hash],
            |r| r.get(0),
        )
        .ok();
    if let Some(id) = existing {
        return Ok((id, true));
    }

    let now = chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string();
    conn.execute(
        "INSERT INTO webhook_events (event_id, event_hash, event_type, received_at, payload, processing_state)
         VALUES (?1, ?2, ?3, ?4, ?5, 'pending')",
        params![event_id, hash, event_type, now, payload],
    )?;
    Ok((conn.last_insert_rowid(), false))
}

/// Set the processing state of an event (increments `attempts`, like the
/// reference `setWebhookEventState`).
pub fn set_webhook_event_state(
    conn: &Connection,
    id: i64,
    state: &str,
    error: Option<&str>,
) -> Result<()> {
    conn.execute(
        "UPDATE webhook_events SET processing_state = ?1, processing_error = ?2, attempts = attempts + 1 WHERE id = ?3",
        params![state, error, id],
    )?;
    Ok(())
}

/// Keep only the newest `keep` rows (reference: 5,000). Bounds the
/// rate-limit-exempt endpoint against unbounded growth.
pub fn prune_webhook_events(conn: &Connection, keep: u32) -> Result<()> {
    conn.execute(
        "DELETE FROM webhook_events WHERE id NOT IN (SELECT id FROM webhook_events ORDER BY id DESC LIMIT ?1)",
        params![keep.max(1)],
    )?;
    Ok(())
}

/// Pending/failed events with attempts below the cap (reference drain query).
pub fn get_pending_webhook_events(
    conn: &Connection,
    limit: u32,
) -> Result<Vec<WebhookEventRecord>> {
    ensure_webhook_events_table(conn)?;
    let mut stmt = conn.prepare(
        "SELECT id, event_id, event_type, payload FROM webhook_events
          WHERE processing_state IN ('pending','failed') AND attempts < 5
          ORDER BY id LIMIT ?1",
    )?;
    let rows = stmt
        .query_map(params![limit], |r| {
            Ok(WebhookEventRecord {
                id: r.get(0)?,
                event_id: r.get(1)?,
                event_type: r.get(2)?,
                payload: r.get(3)?,
                processing_state: "pending".into(),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Legacy compat shim: persist by explicit event id (used by demo tools and
/// older tests). Dedups on the event id itself.
pub fn persist_event(conn: &Connection, event_id: &str, body: &str) -> Result<bool> {
    ensure_webhook_events_table(conn)?;
    let existing: Option<i64> = conn
        .query_row(
            "SELECT id FROM webhook_events WHERE event_id = ?1",
            params![event_id],
            |r| r.get(0),
        )
        .ok();
    if existing.is_some() {
        return Ok(false);
    }
    let now = chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string();
    // Deterministic hash keyed on the explicit event id (stable across replays).
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(format!("legacy:{event_id}:{body}"));
    let hash = hex_lower(&hasher.finalize());
    conn.execute(
        "INSERT INTO webhook_events (event_id, event_hash, event_type, received_at, payload, processing_state)
         VALUES (?1, ?2, 'unknown', ?3, ?4, 'pending')",
        params![event_id, hash, now, body],
    )?;
    Ok(true)
}

/// Mark a persisted event as processed. Idempotent.
pub fn mark_event_processed(conn: &Connection, event_id: &str) -> Result<()> {
    conn.execute(
        "UPDATE webhook_events
            SET processing_state = 'processed',
                processing_error = NULL
          WHERE event_id = ?1",
        params![event_id],
    )?;
    Ok(())
}

/// Count pending (unprocessed) events. Used by the boot drain.
pub fn pending_event_count(conn: &Connection) -> Result<u32> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM webhook_events WHERE processing_state = 'pending'",
        [],
        |r| r.get(0),
    )?;
    Ok(u32::try_from(n).unwrap_or(0))
}

/// Return the IDs of pending events in receipt order (oldest first). Used by
/// the boot drain to replay unprocessed events.
pub fn pending_event_ids(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT event_id FROM webhook_events WHERE processing_state = 'pending' AND event_id IS NOT NULL ORDER BY id ASC",
    )?;
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
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

    // --- HMAC tests ---

    #[test]
    fn compute_signature_is_deterministic() {
        let s1 = compute_signature(b"secret", b"body");
        let s2 = compute_signature(b"secret", b"body");
        assert_eq!(s1, s2);
    }

    #[test]
    fn compute_signature_changes_with_secret() {
        let s1 = compute_signature(b"secret1", b"body");
        let s2 = compute_signature(b"secret2", b"body");
        assert_ne!(s1, s2);
    }

    #[test]
    fn compute_signature_changes_with_body() {
        let s1 = compute_signature(b"secret", b"body1");
        let s2 = compute_signature(b"secret", b"body2");
        assert_ne!(s1, s2);
    }

    #[test]
    fn compute_signature_is_standard_base64_of_correct_length() {
        let s = compute_signature(b"secret", b"body");
        // SHA-1 = 20 bytes → 28 base64 chars (with one '=' pad).
        assert_eq!(s.len(), 28);
        assert!(s
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '='));
    }

    #[test]
    fn compute_signature_matches_openssl_vectors() {
        // Cross-check against real OpenSSL base64 HMAC-SHA1 vectors:
        // `printf 'body' | openssl dgst -sha1 -hmac secret -binary | base64`
        assert_eq!(
            compute_signature(b"secret", b"body"),
            "oYmR/35FE6HC0u5R46jpnKiR2c0="
        );
        // `printf 'hello world' | openssl dgst -sha1 -hmac key -binary | base64`
        assert_eq!(
            compute_signature(b"key", b"hello world"),
            "NN0jS5JoNZNWBSj2GT6mjIAF9hU="
        );
        // `printf '{"event":"customer.created"}' | openssl dgst -sha1 -hmac super_secret -binary | base64`
        assert_eq!(
            compute_signature(b"super_secret", b"{\"event\":\"customer.created\"}"),
            "O2niiFFi6O3L82txSdmbIiXxHqM="
        );
    }

    #[test]
    fn verify_signature_accepts_correct_signature() {
        let secret = b"super_secret";
        let body = br#"{"event":"customer.created"}"#;
        let sig = compute_signature(secret, body);
        verify_signature(secret, body, &sig).expect("valid signature must verify");
    }

    #[test]
    fn verify_signature_rejects_wrong_secret() {
        let sig = compute_signature(b"real_secret", b"body");
        verify_signature(b"wrong_secret", b"body", &sig).unwrap_err();
    }

    #[test]
    fn verify_signature_rejects_wrong_body() {
        let sig = compute_signature(b"secret", b"real body");
        verify_signature(b"secret", b"tampered body", &sig).unwrap_err();
    }

    #[test]
    fn verify_signature_rejects_tampered_signature() {
        let mut sig = compute_signature(b"secret", b"body");
        // Flip the first character.
        let first = sig.chars().next().unwrap();
        let replacement = if first == 'a' { 'b' } else { 'a' };
        sig.replace_range(0..1, &replacement.to_string());
        verify_signature(b"secret", b"body", &sig).unwrap_err();
    }

    #[test]
    fn verify_signature_rejects_wrong_length_signature() {
        // A truncated signature must fail (not panic, not pass).
        verify_signature(b"secret", b"body", "abc").unwrap_err();
        // An over-long signature must also fail.
        verify_signature(b"secret", b"body", &"0".repeat(80)).unwrap_err();
    }

    #[test]
    fn sha1_known_vectors() {
        // Test vectors from the FIPS 180-1 spec.
        assert_eq!(
            hex_lower(&sha1(b"")),
            "da39a3ee5e6b4b0d3255bfef95601890afd80709"
        );
        assert_eq!(
            hex_lower(&sha1(b"abc")),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
        assert_eq!(
            hex_lower(&sha1(
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
            )),
            "84983e441c3bd26ebaae4aa1f95129e5e54670f1"
        );
    }

    #[test]
    fn sha1_handles_long_input_requiring_padding_extension() {
        // Input longer than 64 bytes — exercises the chunk loop and final
        // padding extension. Verified against `echo -n "$input" | sha1sum`.
        assert_eq!(
            hex_lower(&sha1(b"The quick brown fox jumps over the lazy dog")),
            "2fd4e1c67a2d28fced849ee1bb76e7391b93eb12"
        );
        // A second chunk (more than 64 bytes total).
        let long_input = b"The quick brown fox jumps over the lazy dog The quick brown fox jumps over the lazy dog";
        let _ = hex_lower(&sha1(long_input)); // smoke: doesn't panic
    }

    // --- Persist-first + dedup tests ---

    #[test]
    fn persist_event_first_call_stores_event() {
        let conn = fresh_db();
        let stored = persist_event(&conn, "evt_001", r#"{"a":1}"#).unwrap();
        assert!(stored, "first call should store the event");
        assert_eq!(pending_event_count(&conn).unwrap(), 1);
    }

    #[test]
    fn persist_event_second_call_with_same_id_dedups() {
        let conn = fresh_db();
        assert!(persist_event(&conn, "evt_001", r#"{"a":1}"#).unwrap());
        let stored = persist_event(&conn, "evt_001", r#"{"a":1}"#).unwrap();
        assert!(
            !stored,
            "second call with same id must dedup (return false)"
        );
        assert_eq!(
            pending_event_count(&conn).unwrap(),
            1,
            "row count must stay at 1"
        );
    }

    #[test]
    fn mark_event_processed_changes_status() {
        let conn = fresh_db();
        persist_event(&conn, "evt_001", r#"{}"#).unwrap();
        assert_eq!(pending_event_count(&conn).unwrap(), 1);
        mark_event_processed(&conn, "evt_001").unwrap();
        assert_eq!(pending_event_count(&conn).unwrap(), 0);
    }

    #[test]
    fn pending_event_ids_returns_oldest_first() {
        let conn = fresh_db();
        persist_event(&conn, "evt_001", r#"{}"#).unwrap();
        // Sleep briefly so the timestamps differ.
        std::thread::sleep(std::time::Duration::from_millis(20));
        persist_event(&conn, "evt_002", r#"{}"#).unwrap();
        let ids = pending_event_ids(&conn).unwrap();
        assert_eq!(ids, vec!["evt_001".to_string(), "evt_002".to_string()]);
    }

    #[test]
    fn pending_event_count_handles_empty_table() {
        let conn = fresh_db();
        ensure_webhook_events_table(&conn).unwrap();
        assert_eq!(pending_event_count(&conn).unwrap(), 0);
    }
}
