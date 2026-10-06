//! Encrypted multi-device sync (.sosync) — byte-compatible with the
//! reference `EncryptedSyncService` (`src/server/services/encryptedSyncService.ts`).
//!
//! ## File format (identical bytes on the wire)
//!
//! ```text
//! MAGIC "SOSYNC" (6 bytes, utf8)
//! u32 big-endian header length
//! header JSON (utf8): {
//!   "v": 1,
//!   "kdf": { "name": "scrypt", "N": 32768, "r": 8, "p": 1, "salt": <base64 16B> },
//!   "cipher": "aes-256-gcm",
//!   "iv": <base64 12B>,
//!   "created_at": ISO-8601,
//!   "app_version": <string|null>,
//!   "conversations": <count>,
//!   "customers": <count>,
//!   "sha256": <hex of plaintext snapshot>,
//!   "schema_fingerprint": <hex sha256 of the canonical DDL>  (BK-04)
//! }
//! ciphertext (AES-256-GCM over the SQLite VACUUM INTO snapshot)
//! GCM tag (16 bytes, appended after the ciphertext)
//! ```
//!
//! - scrypt work factor N = 2^15, r = 8, p = 1 (reference `SCRYPT_N`;
//!   ~0.5s — deliberately slow for offline brute force).
//! - Content: a `VACUUM INTO` snapshot of the SQLite mirror (the port's
//!   mirror carries the same conversation/customer data model).
//! - Import is safe-by-default: decrypt to temp, `PRAGMA integrity_check`,
//!   then the **real schema guard** (BK-04): the decrypted snapshot's
//!   canonical DDL fingerprint must match the local schema exactly, or be
//!   a compatible *ancestor* (older same-lineage schema the boot batches
//!   can migrate). Fabricated migration numbers are never trusted — the
//!   fingerprint is recomputed from the decrypted bytes, not read from the
//!   (unauthenticated) header. A divergent schema (e.g. a bundle created
//!   by the reference app) is refused before anything is changed;
//!   afterwards: automatic safety backup of the current DB, atomic swap,
//!   WAL/SHM removal.
//! - Bundles are pruned to the newest 5.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

/// Magic header: identifies a SupportOS encrypted sync bundle.
pub const MAGIC: &[u8; 6] = b"SOSYNC";
/// Format version (reference `FORMAT_VERSION`).
pub const FORMAT_VERSION: u32 = 1;
/// scrypt work factor — reference `SCRYPT_N = 1 << 15`.
const SCRYPT_N: u32 = 1 << 15;
const SCRYPT_R: u32 = 8;
const SCRYPT_P: u32 = 1;
const KEY_LEN: usize = 32; // AES-256

/// Info about a decrypted snapshot (reference `SyncBundleInfo`).
#[derive(Debug, Clone, Serialize)]
pub struct SyncBundleInfo {
    pub version: u32,
    pub created_at: String,
    pub conversations: i64,
    pub customers: i64,
    pub app_version: Option<String>,
    pub sha256: String,
}

/// Reference `ExportResult`.
#[derive(Serialize)]
pub struct ExportResult {
    pub ok: bool,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub info: Option<SyncBundleInfo>,
}

/// Reference `ImportResult`.
#[derive(Serialize)]
pub struct ImportResult {
    pub ok: bool,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub require_restart: Option<bool>,
}

/// The unencrypted header JSON of a bundle.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct BundleHeader {
    v: u32,
    kdf: KdfParams,
    cipher: String,
    iv: String,
    created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    app_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    conversations: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    customers: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sha256: Option<String>,
    /// Canonical DDL fingerprint of the snapshot (BK-04). Informational in
    /// the header — the import guard always recomputes it from the decrypted
    /// bytes (the header is not authenticated).
    #[serde(skip_serializing_if = "Option::is_none")]
    schema_fingerprint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct KdfParams {
    name: String,
    #[serde(rename = "N")]
    n: u32,
    r: u32,
    p: u32,
    salt: String,
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// The app version recorded in bundle headers (the reference reads
/// package.json; the port records its Cargo workspace version).
fn app_version() -> Option<String> {
    Some(env!("CARGO_PKG_VERSION").to_string())
}

/// ISO stamp with `:` and `.` replaced by `-` (reference filename convention).
fn stamp() -> String {
    now_iso().replace([':', '.'], "-")
}

fn derive_key(passphrase: &str, salt: &[u8]) -> Result<[u8; KEY_LEN]> {
    let mut key = [0u8; KEY_LEN];
    let params = scrypt::Params::new(SCRYPT_N.trailing_zeros() as u8, SCRYPT_R, SCRYPT_P, KEY_LEN)
        .map_err(|e| Error::Config(format!("scrypt params invalid: {e}")))?;
    scrypt::scrypt(passphrase.as_bytes(), salt, &params, &mut key)
        .map_err(|e| Error::Config(format!("scrypt derivation failed: {e}")))?;
    Ok(key)
}

/// Ensure the `encrypted_sync_log` ledger table exists (reference migration
/// 009_outreach_semantic_sync.ts:129).
pub fn ensure_log_table(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS encrypted_sync_log (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            direction TEXT NOT NULL,
            file_path TEXT NOT NULL,
            size_bytes INTEGER NOT NULL,
            conversations INTEGER,
            customers INTEGER,
            sha256 TEXT,
            at TEXT NOT NULL DEFAULT (datetime('now'))
        );",
    )?;
    Ok(())
}

/// Metadata from a snapshot file: conversation/customer counts + sha256 of
/// the file bytes (reference `snapshotInfo`).
fn snapshot_info(snapshot_path: &Path) -> Result<SyncBundleInfo> {
    let test =
        Connection::open_with_flags(snapshot_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let conversations: i64 =
        test.query_row("SELECT COUNT(*) FROM conversations", [], |r| r.get(0))?;
    let customers: i64 = test.query_row("SELECT COUNT(*) FROM customers", [], |r| r.get(0))?;
    drop(test);
    let bytes = fs::read(snapshot_path)?;
    let sha256 = hex(&Sha256::digest(&bytes));
    Ok(SyncBundleInfo {
        version: FORMAT_VERSION,
        created_at: now_iso(),
        conversations,
        customers,
        app_version: app_version(),
        sha256,
    })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Schema version of a snapshot file: looks for `schema_migrations` then
/// `migrations` (the reference's tables) then `_migrations` (the port's
/// real history); 0 when absent. Informational only — the import decision
/// is made by the DDL fingerprint guard below (BK-04).
fn schema_version_of_file(path: &Path) -> i64 {
    let Ok(test) = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
    else {
        return 0;
    };
    for (table, col) in [
        ("schema_migrations", "id"),
        ("migrations", "id"),
        ("_migrations", "version"),
    ] {
        let exists: bool = test
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name = ?1",
                [table],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n > 0)
            .unwrap_or(false);
        if exists {
            if let Ok(v) = test.query_row(
                &format!("SELECT COALESCE(MAX({col}), 0) FROM {table}"),
                [],
                |r| r.get(0),
            ) {
                return v;
            }
        }
    }
    0
}

// ---------------- Real schema guard (BK-04) ----------------

/// fts5 shadow-table suffixes — the generated DDL of these companion tables
/// depends on the bundled SQLite build, so they are excluded from the
/// canonical fingerprint (the virtual table itself is still included).
const FTS_SHADOW_SUFFIXES: [&str; 5] = ["_data", "_idx", "_docsize", "_content", "_config"];

/// Names of the virtual tables (e.g. fts5) in this database.
fn virtual_tables(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT name FROM sqlite_master
         WHERE type = 'table' AND sql LIKE 'CREATE VIRTUAL%'",
    )?;
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// True when a sqlite_master object is excluded from the canonical schema
/// fingerprint: SQLite internals, migration bookkeeping (whose presence
/// depends on the database's lineage — fabricated or real — not on the app
/// schema), and fts5 shadow tables (SQLite-version-dependent DDL).
fn fingerprint_excluded(name: &str, virtual_tables: &[String]) -> bool {
    if name.starts_with("sqlite_") {
        return true;
    }
    if name == "_migrations" || name == "schema_migrations" || name == "migrations" {
        return true;
    }
    for vt in virtual_tables {
        for suffix in FTS_SHADOW_SUFFIXES {
            if name == format!("{vt}{suffix}") {
                return true;
            }
        }
    }
    false
}

/// Canonical DDL fingerprint (BK-04): SHA-256 over every schema object in
/// `sqlite_master` (tables, indexes, views, triggers) with the object's SQL
/// whitespace-normalized, sorted by (type, name). Two databases share a
/// fingerprint **iff** their application schema is identical — regardless of
/// any migration-number claims in bookkeeping tables.
pub fn schema_fingerprint(conn: &Connection) -> Result<String> {
    let vtabs = virtual_tables(conn)?;
    let mut objects: Vec<(String, String, String)> = Vec::new();
    {
        let mut stmt = conn
            .prepare("SELECT type, name, sql FROM sqlite_master WHERE name NOT LIKE 'sqlite_%'")?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (typ, name, sql) in rows {
            if fingerprint_excluded(&name, &vtabs) {
                continue;
            }
            objects.push((typ, name, sql));
        }
    }
    objects.sort();
    let mut canonical = String::new();
    for (typ, name, sql) in &objects {
        let normalized: String = sql.split_whitespace().collect::<Vec<_>>().join(" ");
        canonical.push_str(&format!("{typ}\u{1f}{name}\u{1f}{normalized}\n"));
    }
    Ok(hex(&Sha256::digest(canonical.as_bytes())))
}

/// Canonical DDL fingerprint of a database file (opened read-only).
fn fingerprint_of_file(path: &Path) -> Result<String> {
    let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    schema_fingerprint(&conn)
}

/// Columns of a table as (name, normalized type, notnull, pk) tuples.
fn table_columns(conn: &Connection, table: &str) -> Option<Vec<(String, String, i64, i64)>> {
    let escaped = table.replace('"', "\"\"");
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info(\"{escaped}\")"))
        .ok()?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .to_lowercase(),
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(5)?,
            ))
        })
        .ok()?;
    Some(rows.flatten().collect())
}

/// True when the snapshot's schema is a compatible **ancestor** of the local
/// schema: every snapshot table exists locally with all of the snapshot's
/// columns (same name, type, NOT NULL and primary-key flags). The local
/// schema may have MORE tables/columns — that is ordinary additive evolution
/// the idempotent boot batches migrate on restart. Any table or column the
/// snapshot has that the local schema lacks means the bundle comes from a
/// different (divergent or newer) lineage.
fn snapshot_is_compatible_ancestor(local: &Connection, snapshot: &Connection) -> bool {
    let vtabs = match virtual_tables(snapshot) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let names: Vec<String> = match snapshot
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
        .and_then(|mut s| {
            s.query_map([], |r| r.get::<_, String>(0))
                .map(|rows| rows.flatten().collect::<Vec<_>>())
        }) {
        Ok(v) => v,
        Err(_) => return false,
    };
    for name in names {
        if fingerprint_excluded(&name, &vtabs) {
            continue;
        }
        let Some(snap_cols) = table_columns(snapshot, &name) else {
            return false;
        };
        let Some(local_cols) = table_columns(local, &name) else {
            return false; // table does not exist locally
        };
        let local_set: HashSet<(String, String, i64, i64)> = local_cols.into_iter().collect();
        for col in snap_cols {
            if !local_set.contains(&col) {
                return false; // divergent or newer column
            }
        }
    }
    true
}

/// Outcome of the .sosync schema guard (BK-04).
enum SchemaCheck {
    /// Snapshot DDL is byte-identical to the local schema.
    Identical,
    /// Snapshot DDL is an older, compatible same-lineage schema (boot
    /// batches migrate it on restart).
    CompatibleAncestor,
    /// Snapshot DDL is divergent — refuse the import.
    Incompatible { bundle: String, local: String },
}

fn check_schema_compatibility(local: &Connection, snapshot: &Connection) -> Result<SchemaCheck> {
    let local_fp = schema_fingerprint(local)?;
    let bundle_fp = schema_fingerprint(snapshot)?;
    if local_fp == bundle_fp {
        return Ok(SchemaCheck::Identical);
    }
    if snapshot_is_compatible_ancestor(local, snapshot) {
        Ok(SchemaCheck::CompatibleAncestor)
    } else {
        Ok(SchemaCheck::Incompatible {
            bundle: bundle_fp,
            local: local_fp,
        })
    }
}

/// Short display form of a fingerprint (first 12 hex chars).
fn fp_short(fp: &str) -> &str {
    &fp[..fp.len().min(12)]
}

// ---------------- Export ----------------

/// Export an encrypted bundle (reference `exportBundle`). Returns the
/// reference-shaped `ExportResult`.
pub fn export_bundle(conn: &Connection, bundles_dir: &Path, passphrase: &str) -> ExportResult {
    try_export_bundle(conn, bundles_dir, passphrase).unwrap_or_else(|e| ExportResult {
        ok: false,
        message: format!("Export failed: {e}"),
        path: None,
        size_bytes: None,
        info: None,
    })
}

fn try_export_bundle(
    conn: &Connection,
    bundles_dir: &Path,
    passphrase: &str,
) -> Result<ExportResult> {
    if passphrase.len() < 8 {
        return Ok(ExportResult {
            ok: false,
            message: "Passphrase must be at least 8 characters.".into(),
            path: None,
            size_bytes: None,
            info: None,
        });
    }
    fs::create_dir_all(bundles_dir)?;
    // BK-04: ensure the ledger table exists BEFORE the snapshot so the
    // exported fingerprint matches the live schema (the first-ever export
    // would otherwise ship a snapshot without `encrypted_sync_log` and
    // force every later import through the ancestor path).
    ensure_log_table(conn)?;
    let stamp = stamp();
    let snapshot_path = bundles_dir.join(format!(".snapshot-{stamp}.tmp"));
    let target = bundles_dir.join(format!("supportos-sync-{stamp}.sosync"));

    // 1. Consistent snapshot (VACUUM INTO under WAL).
    let escaped = snapshot_path.to_string_lossy().replace('\'', "''");
    conn.execute_batch(&format!("VACUUM INTO '{escaped}'"))?;

    // 2. Metadata from the snapshot — including the REAL canonical DDL
    //    fingerprint of the bytes actually shipped (BK-04).
    let info = snapshot_info(&snapshot_path)?;
    let fingerprint = fingerprint_of_file(&snapshot_path)?;

    // 3. Encrypt.
    let mut salt = [0u8; 16];
    let mut iv = [0u8; 12];
    use rand::RngCore;
    rand::thread_rng().fill_bytes(&mut salt);
    rand::thread_rng().fill_bytes(&mut iv);
    let key = derive_key(passphrase, &salt)?;
    let header = BundleHeader {
        v: FORMAT_VERSION,
        kdf: KdfParams {
            name: "scrypt".into(),
            n: SCRYPT_N,
            r: SCRYPT_R,
            p: SCRYPT_P,
            salt: B64.encode(salt),
        },
        cipher: "aes-256-gcm".into(),
        iv: B64.encode(iv),
        created_at: now_iso(),
        app_version: app_version(),
        conversations: Some(info.conversations),
        customers: Some(info.customers),
        sha256: Some(info.sha256.clone()),
        schema_fingerprint: Some(fingerprint.clone()),
    };
    let header_bytes = serde_json::to_vec(&header)?;
    let mut header_len = [0u8; 4];
    header_len[0] = (header_bytes.len() >> 24) as u8;
    header_len[1] = (header_bytes.len() >> 16) as u8;
    header_len[2] = (header_bytes.len() >> 8) as u8;
    header_len[3] = header_bytes.len() as u8;

    let plaintext = fs::read(&snapshot_path)?;
    let cipher = Aes256Gcm::new((&key).into());
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&iv), plaintext.as_slice())
        .map_err(|_| Error::Config("encryption failed".into()))?;
    // aes-gcm crate appends the 16-byte tag to the ciphertext — exactly the
    // reference layout (ciphertext || tag).
    let mut out = Vec::with_capacity(MAGIC.len() + 4 + header_bytes.len() + ciphertext.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&header_len);
    out.extend_from_slice(&header_bytes);
    out.extend_from_slice(&ciphertext);
    fs::write(&target, &out)?;
    let _ = fs::remove_file(&snapshot_path);

    let size = fs::metadata(&target)?.len();
    log_sync(conn, "export", &target, size, &info)?;
    prune_old_bundles(bundles_dir, 5);
    Ok(ExportResult {
        ok: true,
        message: format!(
            "Encrypted sync bundle created ({:.1} MB). Move it to the other device and import it there with the same passphrase.",
            size as f64 / 1024.0 / 1024.0
        ),
        path: Some(target.to_string_lossy().into_owned()),
        size_bytes: Some(size),
        info: Some(info),
    })
}

// ---------------- Verify / Import ----------------

/// Decrypt a bundle to a temp file (reference `decryptToTemp`). Returns
/// `Err(message)` with the reference's user-facing failure strings.
fn decrypt_to_temp(
    bundles_dir: &Path,
    bundle_path: &Path,
    passphrase: &str,
) -> std::result::Result<PathBuf, String> {
    if !bundle_path.exists() {
        return Err("Bundle file not found.".into());
    }
    let raw = fs::read(bundle_path).map_err(|e| e.to_string())?;
    if raw.len() < MAGIC.len() + 4 {
        return Err("File is too small to be a SupportOS sync bundle.".into());
    }
    if &raw[0..MAGIC.len()] != MAGIC {
        return Err("This is not a SupportOS encrypted sync bundle (.sosync).".into());
    }
    let mut offset = MAGIC.len();
    let header_len = u32::from_be_bytes([
        raw[offset],
        raw[offset + 1],
        raw[offset + 2],
        raw[offset + 3],
    ]);
    offset += 4;
    if header_len == 0
        || header_len > 65536
        || (offset as u64) + u64::from(header_len) >= raw.len() as u64
    {
        return Err("Bundle header is corrupt.".into());
    }
    let header: BundleHeader = serde_json::from_slice(&raw[offset..offset + header_len as usize])
        .map_err(|_| "Bundle header is corrupt.".to_string())?;
    if header.v != FORMAT_VERSION {
        return Err(format!("Unsupported bundle format version {}.", header.v));
    }
    if header.cipher != "aes-256-gcm" || header.kdf.name != "scrypt" {
        return Err("Bundle uses an unsupported cipher.".into());
    }
    let ciphertext = &raw[offset + header_len as usize..raw.len() - 16];
    let tag = &raw[raw.len() - 16..];
    // Reconstruct ciphertext||tag for the aes-gcm crate (it expects the tag
    // appended).
    let mut ct_with_tag = Vec::with_capacity(ciphertext.len() + 16);
    ct_with_tag.extend_from_slice(ciphertext);
    ct_with_tag.extend_from_slice(tag);

    let salt = B64
        .decode(&header.kdf.salt)
        .map_err(|_| "Bundle header is corrupt.".to_string())?;
    let key =
        scrypt_derive_with_params(passphrase, &salt, header.kdf.n, header.kdf.r, header.kdf.p)
            .map_err(|e| format!("Key derivation failed: {e}"))?;
    let iv = B64
        .decode(&header.iv)
        .map_err(|_| "Bundle header is corrupt.".to_string())?;
    let cipher = Aes256Gcm::new((&key).into());
    let plain = cipher
        .decrypt(Nonce::from_slice(&iv), ct_with_tag.as_slice())
        .map_err(|_| {
            "Wrong passphrase (or the bundle was modified). Nothing was changed.".to_string()
        })?;
    let temp_path = bundles_dir.join(format!(
        ".import-{}.tmp",
        chrono::Utc::now().timestamp_millis()
    ));
    fs::create_dir_all(bundles_dir).map_err(|e| e.to_string())?;
    fs::write(&temp_path, plain).map_err(|e| e.to_string())?;
    Ok(temp_path)
}

fn scrypt_derive_with_params(
    passphrase: &str,
    salt: &[u8],
    n: u32,
    r: u32,
    p: u32,
) -> Result<[u8; KEY_LEN]> {
    let mut key = [0u8; KEY_LEN];
    let params = scrypt::Params::new(n.trailing_zeros() as u8, r, p, KEY_LEN)
        .map_err(|e| Error::Config(format!("scrypt params invalid: {e}")))?;
    scrypt::scrypt(passphrase.as_bytes(), salt, &params, &mut key)
        .map_err(|e| Error::Config(format!("scrypt derivation failed: {e}")))?;
    Ok(key)
}

fn integrity_check(path: &Path) -> bool {
    Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .and_then(|test| test.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0)))
        .map(|v| v == "ok")
        .unwrap_or(false)
}

/// Two-phase import step 1 (reference `verifyBundle`). The schema guard is
/// the REAL one (BK-04): the decrypted snapshot's canonical DDL must match
/// the local schema, or be a compatible older schema — migration-number
/// claims in the snapshot are not trusted (they can be fabricated).
pub fn verify_bundle(
    conn: &Connection,
    bundles_dir: &Path,
    bundle_path: &Path,
    passphrase: &str,
) -> Value {
    let temp = match decrypt_to_temp(bundles_dir, bundle_path, passphrase) {
        Ok(t) => t,
        Err(msg) => return json!({ "ok": false, "message": format!("Verification failed: {msg}") }),
    };
    let result = (|| -> Result<Value> {
        // The on-demand ledger table (idempotent) — every port DB carries it
        // as soon as it syncs, so a fresh local DB must not fail the
        // fingerprint guard for lacking it (BK-04).
        ensure_log_table(conn)?;
        let info = snapshot_info(&temp)?;
        let schema = schema_version_of_file(&temp);
        let snap = Connection::open_with_flags(&temp, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let check = check_schema_compatibility(conn, &snap)?;
        let _ = fs::remove_file(&temp);
        match check {
            SchemaCheck::Incompatible { bundle, local } => Ok(json!({
                "ok": false,
                "message": format!(
                    "The bundle was created by an incompatible database schema (bundle fingerprint {}, local {}). Importing it could corrupt your data - nothing was changed.",
                    fp_short(&bundle),
                    fp_short(&local)
                )
            })),
            SchemaCheck::CompatibleAncestor => Ok(json!({
                "ok": true,
                "message": format!(
                    "Bundle verified: {} conversations, {} customers, schema {} — an older compatible schema that will be brought up to date on restart.",
                    info.conversations, info.customers, schema
                ),
                "info": info
            })),
            SchemaCheck::Identical => Ok(json!({
                "ok": true,
                "message": format!(
                    "Bundle verified: {} conversations, {} customers, schema {}.",
                    info.conversations, info.customers, schema
                ),
                "info": info
            })),
        }
    })();
    result
        .unwrap_or_else(|e| json!({ "ok": false, "message": format!("Verification failed: {e}") }))
}

/// Two-phase import step 2 (reference `importBundle`): decrypt, integrity
/// check, REAL schema-fingerprint guard (BK-04 — refuse divergent schemas
/// before anything is touched), safety backup, atomic swap, WAL/SHM
/// removal.
pub fn import_bundle(
    conn: &Connection,
    db_path: &Path,
    bundles_dir: &Path,
    bundle_path: &Path,
    passphrase: &str,
) -> ImportResult {
    let temp = match decrypt_to_temp(bundles_dir, bundle_path, passphrase) {
        Ok(t) => t,
        Err(msg) => {
            return ImportResult {
                ok: false,
                message: format!("Import failed: {msg}"),
                require_restart: None,
            }
        }
    };
    if !integrity_check(&temp) {
        let _ = fs::remove_file(&temp);
        return ImportResult {
            ok: false,
            message: "The decrypted database failed the integrity check. The bundle is corrupt - nothing was changed.".into(),
            require_restart: None,
        };
    }
    // The real schema guard (BK-04): compare the decrypted snapshot's actual
    // DDL against the local schema. Migration-number claims inside the
    // snapshot are not trusted — they can be fabricated (audit C1). The
    // on-demand ledger table is ensured first (idempotent) so a fresh local
    // DB is not penalized for not having logged a sync yet.
    let schema_guard = (|| -> Result<SchemaCheck> {
        ensure_log_table(conn)?;
        let snap = Connection::open_with_flags(&temp, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        check_schema_compatibility(conn, &snap)
    })();
    match schema_guard {
        Ok(SchemaCheck::Identical) | Ok(SchemaCheck::CompatibleAncestor) => {}
        Ok(SchemaCheck::Incompatible { bundle, local }) => {
            let _ = fs::remove_file(&temp);
            return ImportResult {
                ok: false,
                message: format!(
                    "The bundle's database schema is incompatible with this build (bundle fingerprint {}, local {}). Importing it could corrupt your data - nothing was changed.",
                    fp_short(&bundle),
                    fp_short(&local)
                ),
                require_restart: None,
            };
        }
        Err(e) => {
            let _ = fs::remove_file(&temp);
            return ImportResult {
                ok: false,
                message: format!("Import failed: schema check failed: {e}"),
                require_restart: None,
            };
        }
    }
    let info = match snapshot_info(&temp) {
        Ok(i) => i,
        Err(e) => {
            let _ = fs::remove_file(&temp);
            return ImportResult {
                ok: false,
                message: format!("Import failed: {e}"),
                require_restart: None,
            };
        }
    };

    // Safety net: automatic backup of the CURRENT data before the swap.
    let backup_path = format!(
        "{}.pre-import-{}.db",
        db_path.to_string_lossy(),
        chrono::Utc::now().timestamp_millis()
    );
    let escaped = backup_path.replace('\'', "''");
    if conn
        .execute_batch(&format!("VACUUM INTO '{escaped}'"))
        .is_err()
    {
        let _ = fs::remove_file(&temp);
        return ImportResult {
            ok: false,
            message: "Import failed: could not write the safety backup.".into(),
            require_restart: None,
        };
    }

    // Swap (same model as BackupService.restore; caller restarts the app).
    let restore_tmp = format!("{}.restore-tmp", db_path.to_string_lossy());
    if fs::copy(&temp, &restore_tmp).is_err() {
        let _ = fs::remove_file(&temp);
        return ImportResult {
            ok: false,
            message: "Import failed: could not stage the restored database.".into(),
            require_restart: None,
        };
    }
    let _ = fs::remove_file(&temp);
    if fs::rename(&restore_tmp, db_path).is_err() {
        return ImportResult {
            ok: false,
            message: "Import failed: could not swap the database into place.".into(),
            require_restart: None,
        };
    }
    for ext in ["-wal", "-shm"] {
        let p = format!("{}{ext}", db_path.to_string_lossy());
        if Path::new(&p).exists() {
            let _ = fs::remove_file(&p);
        }
    }
    let size = fs::metadata(bundle_path).map(|m| m.len()).unwrap_or(0);
    let _ = log_sync(conn, "import", bundle_path, size, &info);
    ImportResult {
        ok: true,
        message: format!(
            "Encrypted bundle imported ({} conversations, {} customers). A safety backup of the previous data was written next to the database. Restart SupportOS to use the restored mirror.",
            info.conversations, info.customers
        ),
        require_restart: Some(true),
    }
}

// ---------------- Listing ----------------

/// Reference `listBundles`: .sosync files in the bundle dir, newest first.
pub fn list_bundles(bundles_dir: &Path) -> Vec<Value> {
    let mut out: Vec<(String, Value)> = Vec::new();
    let Ok(entries) = fs::read_dir(bundles_dir) else {
        return Vec::new();
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".sosync") {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let Ok(modified) = meta.modified() else {
            continue;
        };
        let created_at = modified
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| {
                chrono::DateTime::<chrono::Utc>::from_timestamp(
                    d.as_secs() as i64,
                    d.subsec_nanos(),
                )
                .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
                .unwrap_or_default()
            })
            .unwrap_or_default();
        out.push((
            created_at.clone(),
            json!({
                "file": name,
                "size_bytes": meta.len(),
                "created_at": created_at,
            }),
        ));
    }
    out.sort_by_key(|(k, _)| std::cmp::Reverse(k.clone()));
    out.into_iter().map(|(_, v)| v).collect()
}

/// Reference `syncLog`: last 50 ledger rows.
pub fn sync_log(conn: &Connection) -> Vec<Value> {
    if ensure_log_table(conn).is_err() {
        return Vec::new();
    }
    let Ok(mut stmt) = conn.prepare(
        "SELECT id, direction, file_path, size_bytes, conversations, customers, at
         FROM encrypted_sync_log ORDER BY id DESC LIMIT 50",
    ) else {
        return Vec::new();
    };
    let rows = stmt.query_map([], |r| {
        Ok(json!({
            "id": r.get::<_, i64>(0)?,
            "direction": r.get::<_, String>(1)?,
            "file_path": r.get::<_, String>(2)?,
            "size_bytes": r.get::<_, i64>(3)?,
            "conversations": r.get::<_, Option<i64>>(4)?,
            "customers": r.get::<_, Option<i64>>(5)?,
            "at": r.get::<_, String>(6)?,
        }))
    });
    match rows {
        Ok(rows) => rows.flatten().collect(),
        Err(_) => Vec::new(),
    }
}

fn log_sync(
    conn: &Connection,
    direction: &str,
    path: &Path,
    size: u64,
    info: &SyncBundleInfo,
) -> Result<()> {
    if ensure_log_table(conn).is_err() {
        return Ok(()); // the ledger must never break the operation
    }
    conn.execute(
        "INSERT INTO encrypted_sync_log (direction, file_path, size_bytes, conversations, customers, sha256)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            direction,
            path.to_string_lossy(),
            size as i64,
            info.conversations,
            info.customers,
            info.sha256,
        ],
    )?;
    Ok(())
}

/// Keep at most the `keep` newest bundles (reference `pruneOldBundles`).
fn prune_old_bundles(bundles_dir: &Path, keep: usize) {
    let Ok(entries) = fs::read_dir(bundles_dir) else {
        return;
    };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".sosync"))
        .filter_map(|e| {
            e.metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .map(|t| (t, e.path()))
        })
        .collect();
    files.sort_by_key(|(t, _)| std::cmp::Reverse(*t));
    for (_, path) in files.iter().skip(keep.max(1)) {
        let _ = fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (Connection, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("test.db");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE conversations (id INTEGER PRIMARY KEY);
             CREATE TABLE customers (id INTEGER PRIMARY KEY);
             INSERT INTO conversations VALUES (1),(2),(3);
             INSERT INTO customers VALUES (1),(2);",
        )
        .unwrap();
        (conn, dir)
    }

    // ---- BK-04: real schema fingerprint -------------------------------------

    #[test]
    fn fingerprint_is_stable_and_ordering_independent() {
        let (conn, _dir) = setup();
        let fp1 = schema_fingerprint(&conn).unwrap();
        let fp2 = schema_fingerprint(&conn).unwrap();
        assert_eq!(fp1, fp2, "same DDL must give the same fingerprint");
        assert_eq!(fp1.len(), 64, "sha-256 hex");

        // A database that creates the same tables in a DIFFERENT order must
        // produce the same fingerprint (objects are sorted by name).
        let conn2 = Connection::open_in_memory().unwrap();
        conn2
            .execute_batch(
                "CREATE TABLE customers (id INTEGER PRIMARY KEY);
                 CREATE TABLE conversations (id INTEGER PRIMARY KEY);",
            )
            .unwrap();
        assert_eq!(schema_fingerprint(&conn2).unwrap(), fp1);
    }

    #[test]
    fn fingerprint_changes_when_ddl_changes() {
        let (conn, _dir) = setup();
        let fp1 = schema_fingerprint(&conn).unwrap();
        conn.execute_batch("ALTER TABLE conversations ADD COLUMN subject TEXT")
            .unwrap();
        let fp2 = schema_fingerprint(&conn).unwrap();
        assert_ne!(fp1, fp2, "adding a column must change the fingerprint");
        conn.execute_batch("CREATE TABLE tags (id INTEGER PRIMARY KEY)")
            .unwrap();
        assert_ne!(fp2, schema_fingerprint(&conn).unwrap());
    }

    #[test]
    fn fingerprint_ignores_migration_bookkeeping_and_fts_shadows() {
        let a = Connection::open_in_memory().unwrap();
        a.execute_batch(
            "CREATE TABLE conversations (id INTEGER PRIMARY KEY, body TEXT);
             CREATE VIRTUAL TABLE fts_threads USING fts5(body);",
        )
        .unwrap();
        let fp_a = schema_fingerprint(&a).unwrap();

        // Same app schema, but with fabricated migration bookkeeping rows and
        // a differently-named migration table — the fingerprint must not care.
        let b = Connection::open_in_memory().unwrap();
        b.execute_batch(
            "CREATE TABLE conversations (id INTEGER PRIMARY KEY, body TEXT);
             CREATE VIRTUAL TABLE fts_threads USING fts5(body);
             CREATE TABLE schema_migrations (id INTEGER PRIMARY KEY, name TEXT);
             INSERT INTO schema_migrations VALUES (16, '016_m6_graph_coaching_memory');
             CREATE TABLE _migrations (version INTEGER PRIMARY KEY, applied_at TEXT, label TEXT);
             INSERT INTO _migrations (version) VALUES (1),(2);",
        )
        .unwrap();
        assert_eq!(fp_a, schema_fingerprint(&b).unwrap());
    }

    #[test]
    fn fingerprint_sees_through_fabricated_migration_numbers() {
        // Audit C1's trap: a DB claiming "migration 16" via fabricated
        // schema_migrations rows while shipping the port's DDL. The
        // fingerprint reflects the DDL only — a MAIN-shaped schema claiming
        // migration 16 must NOT fingerprint like a port schema, no matter
        // what its migration numbers say.
        let port_like = Connection::open_in_memory().unwrap();
        port_like
            .execute_batch(
                "CREATE TABLE conversations (id INTEGER PRIMARY KEY, number INTEGER, mailbox_id INTEGER);
                 CREATE TABLE schema_migrations (id INTEGER PRIMARY KEY, name TEXT);
                 INSERT INTO schema_migrations VALUES (16, '016_m6_graph_coaching_memory');",
            )
            .unwrap();
        let main_like = Connection::open_in_memory().unwrap();
        main_like
            .execute_batch(
                "CREATE TABLE conversations (id INTEGER PRIMARY KEY, number INTEGER, subject TEXT, mailboxId INTEGER, createdAt TEXT);
                 CREATE TABLE schema_migrations (id INTEGER PRIMARY KEY, name TEXT);
                 INSERT INTO schema_migrations VALUES (16, '016_m6_graph_coaching_memory');",
            )
            .unwrap();
        assert_ne!(
            schema_fingerprint(&port_like).unwrap(),
            schema_fingerprint(&main_like).unwrap(),
            "same claimed migration number, different real DDL — the fingerprint must differ"
        );
    }

    #[test]
    fn compatible_ancestor_allows_additive_local_evolution() {
        let older = Connection::open_in_memory().unwrap();
        older
            .execute_batch(
                "CREATE TABLE conversations (id INTEGER PRIMARY KEY, subject TEXT);
                 CREATE TABLE customers (id INTEGER PRIMARY KEY);",
            )
            .unwrap();
        let newer = Connection::open_in_memory().unwrap();
        newer
            .execute_batch(
                "CREATE TABLE conversations (id INTEGER PRIMARY KEY, subject TEXT, status TEXT);
                 CREATE TABLE customers (id INTEGER PRIMARY KEY);
                 CREATE TABLE tags (id INTEGER PRIMARY KEY);",
            )
            .unwrap();
        assert!(snapshot_is_compatible_ancestor(&newer, &older));
        // The reverse is NOT compatible: the newer schema has columns/tables
        // the older one lacks.
        assert!(!snapshot_is_compatible_ancestor(&older, &newer));
    }

    #[test]
    fn divergent_schema_is_not_an_ancestor() {
        let port = Connection::open_in_memory().unwrap();
        port.execute_batch(
            "CREATE TABLE conversations (id INTEGER PRIMARY KEY, number INTEGER, mailbox_id INTEGER);",
        )
        .unwrap();
        let reference = Connection::open_in_memory().unwrap();
        // MAIN-shaped conversations: many columns with divergent names.
        reference.execute_batch(
            "CREATE TABLE conversations (id INTEGER PRIMARY KEY, number INTEGER UNIQUE, subject TEXT, status TEXT, mailboxId INTEGER, createdAt TEXT, closedAt TEXT, closedByUserId INTEGER, closedByUserEmail TEXT, preview TEXT, ccEmails TEXT, bccEmails TEXT, assignedTo INTEGER, assignedForSeconds INTEGER, tags TEXT, customerWaitingSince TEXT, firstResponseTimeSeconds INTEGER, supportHoursSupportedSeconds INTEGER, createdAtDays INTEGER, modifiedAt TEXT, sourceType TEXT, sourceVia TEXT, overrideCustomFields TEXT, spamCount INTEGER, hasAttachments INTEGER, embedUrl TEXT, reviewScore INTEGER, reviewComment TEXT, reviewLink TEXT);
             CREATE TABLE schema_migrations (id INTEGER PRIMARY KEY, name TEXT);
             INSERT INTO schema_migrations VALUES (16, '016_m6_graph_coaching_memory');",
        )
        .unwrap();
        assert!(!snapshot_is_compatible_ancestor(&port, &reference));
        assert!(!snapshot_is_compatible_ancestor(&reference, &port));
    }

    #[test]
    fn export_bundle_writes_real_schema_fingerprint_header() {
        let (conn, dir) = setup();
        let bundles = dir.path().join("bundles");
        let res = export_bundle(&conn, &bundles, "long-enough-pass");
        assert!(res.ok, "{}", res.message);
        let raw = fs::read(res.path.clone().unwrap()).unwrap();
        let hl = u32::from_be_bytes([raw[6], raw[7], raw[8], raw[9]]);
        let header: BundleHeader = serde_json::from_slice(&raw[10..10 + hl as usize]).unwrap();
        let fp = header.schema_fingerprint.expect("fingerprint in header");
        assert_eq!(
            fp,
            schema_fingerprint(&conn).unwrap(),
            "header fingerprint matches the real DDL"
        );
        assert_eq!(fp.len(), 64);
    }

    #[test]
    fn import_refuses_divergent_schema_and_leaves_db_untouched() {
        // Local: port-shaped schema.
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("local.db");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE conversations (id INTEGER PRIMARY KEY, number INTEGER, mailbox_id INTEGER);
             INSERT INTO conversations VALUES (1, 101, 1);",
        )
        .unwrap();

        // Bundle exported from a MAIN-shaped schema claiming migration 16 —
        // exactly the fabricated-numbers trap of audit C1.
        let other = Connection::open_in_memory().unwrap();
        other.execute_batch(
            "CREATE TABLE conversations (id INTEGER PRIMARY KEY, number INTEGER, subject TEXT, mailboxId INTEGER, createdAt TEXT);
             CREATE TABLE customers (id INTEGER PRIMARY KEY, email TEXT);
             CREATE TABLE schema_migrations (id INTEGER PRIMARY KEY, name TEXT);
             INSERT INTO schema_migrations VALUES (16, '016_m6_graph_coaching_memory');",
        )
        .unwrap();
        let bundles = dir.path().join("bundles");
        let res = export_bundle(&other, &bundles, "long-enough-pass");
        assert!(res.ok, "{}", res.message);
        let bundle = PathBuf::from(res.path.unwrap());

        // Verify must fail with the incompatibility message.
        let v = verify_bundle(&conn, &bundles, &bundle, "long-enough-pass");
        assert!(!v["ok"].as_bool().unwrap(), "{v}");
        assert!(
            v["message"].as_str().unwrap().contains("incompatible"),
            "{}",
            v
        );

        // Snapshot AFTER verify (which ensures the on-demand ledger table —
        // an expected, idempotent write). What follows must change nothing.
        let before = fs::read(&db_path).unwrap();

        // Import must refuse AND leave the database bytes untouched.
        let r = import_bundle(&conn, &db_path, &bundles, &bundle, "long-enough-pass");
        assert!(!r.ok, "{}", r.message);
        assert!(r.message.contains("incompatible"), "{}", r.message);
        // No safety backup was written (nothing was changed).
        let backups: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("local.db.pre-import-"))
            .collect();
        assert!(
            backups.is_empty(),
            "no safety backup for a refused import: {backups:?}"
        );
        let after = fs::read(&db_path).unwrap();
        assert_eq!(
            before, after,
            "the local DB must be byte-identical after a refused import"
        );
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM conversations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 1, "local data untouched");
    }

    #[test]
    fn import_accepts_older_compatible_schema() {
        // Local: newer port schema (extra table + column).
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("local.db");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE conversations (id INTEGER PRIMARY KEY, subject TEXT, status TEXT);
             CREATE TABLE customers (id INTEGER PRIMARY KEY);
             CREATE TABLE tags (id INTEGER PRIMARY KEY);",
        )
        .unwrap();

        // Bundle from an OLDER same-lineage schema (subset of local).
        let older = Connection::open_in_memory().unwrap();
        older
            .execute_batch(
                "CREATE TABLE conversations (id INTEGER PRIMARY KEY, subject TEXT);
             CREATE TABLE customers (id INTEGER PRIMARY KEY);
             INSERT INTO conversations VALUES (1, 'old');
             INSERT INTO customers VALUES (1);",
            )
            .unwrap();
        let bundles = dir.path().join("bundles");
        let res = export_bundle(&older, &bundles, "long-enough-pass");
        assert!(res.ok, "{}", res.message);
        let bundle = PathBuf::from(res.path.unwrap());

        let v = verify_bundle(&conn, &bundles, &bundle, "long-enough-pass");
        assert!(v["ok"].as_bool().unwrap(), "{v}");
        assert!(
            v["message"]
                .as_str()
                .unwrap()
                .contains("older compatible schema"),
            "{}",
            v
        );

        let r = import_bundle(&conn, &db_path, &bundles, &bundle, "long-enough-pass");
        assert!(r.ok, "{}", r.message);
        let restored = Connection::open(&db_path).unwrap();
        let subject: String = restored
            .query_row("SELECT subject FROM conversations WHERE id = 1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(subject, "old", "older bundle data is swapped in");
        // A safety backup of the previous data was written.
        let backups: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("local.db.pre-import-"))
            .collect();
        assert_eq!(backups.len(), 1, "one safety backup: {backups:?}");
    }

    #[test]
    fn export_verify_roundtrip() {
        let (conn, dir) = setup();
        let bundles = dir.path().join("bundles");
        let res = export_bundle(&conn, &bundles, "short");
        assert!(!res.ok);
        assert_eq!(res.message, "Passphrase must be at least 8 characters.");
        let res = export_bundle(&conn, &bundles, "long-enough-pass");
        assert!(res.ok, "{}", res.message);
        let path = PathBuf::from(res.path.clone().unwrap());
        assert!(path.exists());
        let info = res.info.unwrap();
        assert_eq!(info.conversations, 3);
        assert_eq!(info.customers, 2);

        // magic + layout
        let raw = fs::read(&path).unwrap();
        assert_eq!(&raw[0..6], b"SOSYNC");
        let hl = u32::from_be_bytes([raw[6], raw[7], raw[8], raw[9]]);
        let header: BundleHeader = serde_json::from_slice(&raw[10..10 + hl as usize]).unwrap();
        assert_eq!(header.kdf.n, 32768);

        // verify with the right passphrase
        let v = verify_bundle(&conn, &bundles, &path, "long-enough-pass");
        assert!(v["ok"].as_bool().unwrap(), "{v}");
        // same-schema round-trip: identical fingerprint, plain verified message
        let msg = v["message"].as_str().unwrap();
        assert!(msg.contains("3 conversations"), "{msg}");
        assert!(msg.contains("schema"), "{msg}");
        // wrong passphrase
        let v = verify_bundle(&conn, &bundles, &path, "wrong-passphrase");
        assert!(!v["ok"].as_bool().unwrap());
        assert!(v["message"].as_str().unwrap().contains("Wrong passphrase"));
        // listing + log
        let list = list_bundles(&bundles);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0]["size_bytes"], raw.len() as i64);
        assert_eq!(sync_log(&conn).len(), 1);
    }

    #[test]
    fn corrupted_bundle_rejected() {
        let (conn, dir) = setup();
        let bundles = dir.path().join("bundles");
        let res = export_bundle(&conn, &bundles, "long-enough-pass");
        let path = PathBuf::from(res.path.unwrap());
        let mut raw = fs::read(&path).unwrap();
        let last = raw.len() - 1;
        raw[last] ^= 0xFF; // flip a tag byte
        fs::write(&path, &raw).unwrap();
        let v = verify_bundle(&conn, &bundles, &path, "long-enough-pass");
        assert!(!v["ok"].as_bool().unwrap());
    }
}
