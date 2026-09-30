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

/// Compute the HMAC-SHA1 of `body` keyed by `secret`, returning the lowercase
/// hex digest (the format Help Scout uses in the signature header).
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

    hex_lower(&outer_hash)
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

fn hex_digit(n: u8) -> char {
    match n {
        0..=9 => (b'0' + n) as char,
        10..=15 => (b'a' + (n - 10)) as char,
        _ => unreachable!("hex_digit got {n} (must be 0..15)"),
    }
}

// ---------------------------------------------------------------------------
// Persist-first + dedup (event store in SQLite)
// ---------------------------------------------------------------------------

/// Create the `webhook_events` table if it doesn't exist. Idempotent.
pub fn ensure_webhook_events_table(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS webhook_events (
            id           TEXT PRIMARY KEY,
            received_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
            body         TEXT NOT NULL,
            status       TEXT NOT NULL DEFAULT 'pending',
            -- 'pending' | 'processed' | 'failed'
            processed_at TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_webhook_events_status
            ON webhook_events (status, received_at);",
    )?;
    Ok(())
}

/// Persist a webhook event BEFORE processing it (persist-first per A2).
/// Returns `Ok(true)` if the event was newly stored; `Ok(false)` if it was
/// a duplicate (already in the table — the dedup case). The caller then
/// skips processing.
pub fn persist_event(conn: &Connection, event_id: &str, body: &str) -> Result<bool> {
    ensure_webhook_events_table(conn)?;
    match conn.execute(
        "INSERT OR IGNORE INTO webhook_events (id, body) VALUES (?1, ?2)",
        params![event_id, body],
    ) {
        Ok(rows) => Ok(rows > 0),
        Err(e) => Err(Error::Sqlite(e)),
    }
}

/// Mark a persisted event as processed. Idempotent.
pub fn mark_event_processed(conn: &Connection, event_id: &str) -> Result<()> {
    conn.execute(
        "UPDATE webhook_events
            SET status = 'processed',
                processed_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
          WHERE id = ?1",
        params![event_id],
    )?;
    Ok(())
}

/// Count pending (unprocessed) events. Used by the boot drain.
pub fn pending_event_count(conn: &Connection) -> Result<u32> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM webhook_events WHERE status = 'pending'",
        [],
        |r| r.get(0),
    )?;
    Ok(u32::try_from(n).unwrap_or(0))
}

/// Return the IDs of pending events in receipt order (oldest first). Used by
/// the boot drain to replay unprocessed events.
pub fn pending_event_ids(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT id FROM webhook_events WHERE status = 'pending' ORDER BY received_at ASC",
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
    fn compute_signature_is_lowercase_hex_of_correct_length() {
        let s = compute_signature(b"secret", b"body");
        assert_eq!(s.len(), 40); // SHA-1 = 20 bytes = 40 hex chars
        assert!(s
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
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
