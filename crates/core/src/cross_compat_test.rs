//! Cross-compat execution test: decrypt a REFERENCE-created .sosync bundle
//! with the port's encrypted_sync module, and create a port bundle for the
//! reference to decrypt. Run with:
//!   cargo test -p supportos-plusplus-core --lib cross_compat -- --ignored --nocapture

use std::fs;
use std::path::Path;

use rusqlite::Connection;

use crate::encrypted_sync;

const DIR: &str = "/home/z/my-project/sosync-test";
const PASS: &str = "cross-compat-passphrase";

#[test]
#[ignore]
fn decrypt_reference_created_bundle() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE schema_migrations (id INTEGER PRIMARY KEY, name TEXT);
         INSERT INTO schema_migrations VALUES (16, '016_m6_graph_coaching_memory');",
    )
    .unwrap();
    encrypted_sync::ensure_schema_migrations_record(&conn).unwrap();

    let bundles = Path::new(DIR);
    let bundle = bundles.join("ref-created.sosync");
    assert!(bundle.exists(), "run scripts/sosync_cross_test.js first");

    // Verify (decrypts + reports counts) — must succeed on the reference format.
    let v = encrypted_sync::verify_bundle(&conn, bundles, &bundle, PASS);
    assert!(
        v["ok"].as_bool().unwrap(),
        "reference bundle failed port verify: {v}"
    );
    let msg = v["message"].as_str().unwrap();
    assert!(msg.contains("5 conversations"), "unexpected message: {msg}");
    assert!(msg.contains("2 customers"), "unexpected message: {msg}");
    assert!(msg.contains("schema 16"), "unexpected message: {msg}");
    println!("REFERENCE->PORT VERIFY: {msg}");

    // Wrong passphrase must fail with the reference's exact message.
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
            "CREATE TABLE conversations (id INTEGER PRIMARY KEY);
             CREATE TABLE customers (id INTEGER PRIMARY KEY);
             INSERT INTO conversations VALUES (1),(2),(3),(4),(5),(6),(7);
             INSERT INTO customers VALUES (1),(2),(3);",
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
use std::path::PathBuf;
