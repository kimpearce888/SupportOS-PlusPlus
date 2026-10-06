//! Cross-compat execution test for the .sosync schema guard (BK-04).
//!
//! Audit C1: the old guard compared fabricated migration numbers, so a
//! REFERENCE-created bundle (MAIN's 123-table DDL, migration 16) verified and
//! imported into the port — swapping in an incompatible schema. The new
//! guard compares the REAL canonical DDL fingerprint, so a reference bundle
//! must now be REJECTED with the incompatibility message, while port bundles
//! must still decrypt on the reference side (JSON parsers ignore the extra
//! `schema_fingerprint` header field).
//!
//! Run with:
//!   cargo test -p supportos-plusplus-core --lib cross_compat -- --ignored --nocapture

use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::encrypted_sync;

const DIR: &str = "/home/z/my-project/sosync-test";
const PASS: &str = "cross-compat-passphrase";

#[test]
#[ignore]
fn reference_bundle_is_rejected_by_the_real_schema_guard() {
    // A minimal PORT-shaped local schema — the guard compares real DDL, not
    // fabricated migration numbers.
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE conversations (
             id INTEGER PRIMARY KEY, number INTEGER, subject TEXT,
             status TEXT, mailbox_id INTEGER, customer_id INTEGER,
             created_at TEXT, closed_at TEXT
         );
         CREATE TABLE customers (id INTEGER PRIMARY KEY, email TEXT);",
    )
    .unwrap();

    let bundles = Path::new(DIR);
    let bundle = bundles.join("ref-created.sosync");
    assert!(bundle.exists(), "run scripts/sosync_cross_test.js first");

    // Verify with the right passphrase: decryption succeeds, but the schema
    // guard must refuse the reference's divergent DDL (BK-04 / audit C1).
    let v = encrypted_sync::verify_bundle(&conn, bundles, &bundle, PASS);
    assert!(
        !v["ok"].as_bool().unwrap(),
        "reference bundle must NOT verify against the port schema: {v}"
    );
    let msg = v["message"].as_str().unwrap();
    assert!(
        msg.contains("incompatible"),
        "expected the incompatibility message, got: {msg}"
    );
    println!("REFERENCE->PORT VERIFY (rejected): {msg}");

    // Wrong passphrase still fails with the reference's exact message.
    let bad = encrypted_sync::verify_bundle(&conn, bundles, &bundle, "wrong-wrong-wrong");
    assert!(!bad["ok"].as_bool().unwrap());
    assert!(bad["message"]
        .as_str()
        .unwrap()
        .contains("Wrong passphrase"));

    // Now export a port bundle for the reference side to decrypt.
    let db_path = Path::new(DIR).join("port-test.db");
    let port_conn = Connection::open(&db_path).unwrap();
    port_conn
        .execute_batch(
            "CREATE TABLE conversations (
                 id INTEGER PRIMARY KEY, number INTEGER, subject TEXT,
                 status TEXT, mailbox_id INTEGER, customer_id INTEGER,
                 created_at TEXT, closed_at TEXT
             );
             CREATE TABLE customers (id INTEGER PRIMARY KEY, email TEXT);
             INSERT INTO conversations VALUES (1, 101, 'a', 'active', 1, 1, '2026-01-01', NULL);
             INSERT INTO customers VALUES (1, 'a@example.com');",
        )
        .unwrap();
    let res = encrypted_sync::export_bundle(&port_conn, bundles, PASS);
    assert!(res.ok, "{}", res.message);
    // Rename to the name the node script looks for.
    let created = PathBuf::from(res.path.clone().unwrap());
    let target = bundles.join("port-created.sosync");
    let _ = fs::copy(&created, &target);
    println!(
        "PORT BUNDLE: {} ({} bytes)",
        target.display(),
        res.size_bytes.unwrap()
    );
}
