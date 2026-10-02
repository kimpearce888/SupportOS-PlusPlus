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
//!   "sha256": <hex of plaintext snapshot>
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
//!   schema-version guard (never import a newer schema), automatic safety
//!   backup of the current DB, then atomic swap + WAL/SHM removal.
//! - Bundles are pruned to the newest 5.

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

/// Local schema version (reference: `MAX(id) FROM schema_migrations`, 0 when
/// unmigrated). The port records applied reference-equivalent migrations in
/// `schema_migrations`; `_migrations` is the legacy port-only table.
fn schema_version(conn: &Connection) -> i64 {
    if let Ok(v) = conn.query_row(
        "SELECT COALESCE(MAX(id), 0) FROM schema_migrations",
        [],
        |r| r.get(0),
    ) {
        return v;
    }
    if let Ok(v) = conn.query_row("SELECT COALESCE(MAX(id), 0) FROM _migrations", [], |r| {
        r.get(0)
    }) {
        return v;
    }
    0
}

/// Schema version of a snapshot file (reference `schemaVersionOfFile`):
/// looks for `schema_migrations` then `migrations`; 0 when absent.
fn schema_version_of_file(path: &Path) -> i64 {
    let Ok(test) = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
    else {
        return 0;
    };
    for table in ["schema_migrations", "migrations"] {
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
                &format!("SELECT COALESCE(MAX(id), 0) FROM {table}"),
                [],
                |r| r.get(0),
            ) {
                return v;
            }
        }
    }
    0
}

/// Ensure the port's DB records its reference-equivalent schema level so the
/// import guard compares like with like. The port implements the reference
/// migration set 001..016 via its boot-time batch migrations; this seeds
/// `schema_migrations` with those ids exactly once (idempotent).
pub fn ensure_schema_migrations_record(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            id INTEGER PRIMARY KEY,
            name TEXT,
            applied_at TEXT NOT NULL DEFAULT (datetime('now'))
        );",
    )?;
    for (id, name) in [
        (1i64, "001_core"),
        (2, "002_sync_jobs"),
        (3, "003_ai_knowledge"),
        (4, "004_fts"),
        (5, "005_interaction_intelligence"),
        (6, "006_interaction_integrity"),
        (7, "007_channels_docs"),
        (8, "008_semantic_docs_sla"),
        (9, "009_outreach_semantic_sync"),
        (10, "010_audit_hardening"),
        (11, "011_activity_engine"),
        (12, "012_m2_collaboration"),
        (13, "013_m3_copilot_attributes"),
        (14, "014_m4_intelligence_workspace"),
        (15, "015_m5_quality_translation_reports"),
        (16, "016_m6_graph_coaching_memory"),
    ] {
        conn.execute(
            "INSERT OR IGNORE INTO schema_migrations (id, name) VALUES (?1, ?2)",
            rusqlite::params![id, name],
        )?;
    }
    Ok(())
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
    let stamp = stamp();
    let snapshot_path = bundles_dir.join(format!(".snapshot-{stamp}.tmp"));
    let target = bundles_dir.join(format!("supportos-sync-{stamp}.sosync"));

    // 1. Consistent snapshot (VACUUM INTO under WAL).
    let escaped = snapshot_path.to_string_lossy().replace('\'', "''");
    conn.execute_batch(&format!("VACUUM INTO '{escaped}'"))?;

    // 2. Metadata from the snapshot.
    let info = snapshot_info(&snapshot_path)?;

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

/// Two-phase import step 1 (reference `verifyBundle`).
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
        let info = snapshot_info(&temp)?;
        let current = schema_version(conn);
        let bundle = schema_version_of_file(&temp);
        let _ = fs::remove_file(&temp);
        if bundle > current {
            return Ok(json!({
                "ok": false,
                "message": format!("The bundle was created by a newer SupportOS (schema {bundle} > local {current}). Update SupportOS on this device first.")
            }));
        }
        Ok(json!({
            "ok": true,
            "message": format!("Bundle verified: {} conversations, {} customers, schema {}.", info.conversations, info.customers, bundle),
            "info": info
        }))
    })();
    result
        .unwrap_or_else(|e| json!({ "ok": false, "message": format!("Verification failed: {e}") }))
}

/// Two-phase import step 2 (reference `importBundle`): decrypt, integrity +
/// schema guard, safety backup, atomic swap, WAL/SHM removal.
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
    let current = schema_version(conn);
    let bundle = schema_version_of_file(&temp);
    if bundle > current {
        let _ = fs::remove_file(&temp);
        return ImportResult {
            ok: false,
            message: format!("The bundle was created by a newer SupportOS (schema {bundle} > local {current}). Update this device first."),
            require_restart: None,
        };
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
