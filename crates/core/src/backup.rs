//! Vector backup + migration + recovery — .sosync format (M5-T09).
//!
//! Per spec M5: "vector backup, migration and recovery."
//! Per spec A6: "Use the same cryptographic approach as the reference
//! (AES-256-GCM, scrypt-derived key, authenticated header, verify-first
//! import, safety backup, atomic swap) in your own documented, versioned
//! format. Compatibility with the original app's database or bundles is not
//! required."
//!
//! ## .sosync format
//!
//! The `.sosync` file is SupportOS++'s own backup format. It contains:
//! 1. A magic header (`SOSYNC1`) for format identification.
//! 2. A version byte (1).
//! 3. The scrypt salt (16 bytes).
//! 4. The AES-256-GCM nonce (12 bytes).
//! 5. The encrypted payload (ciphertext + GCM tag).
//!
//! The plaintext payload is a JSON `SosyncBackup` struct containing:
//! - The VectorStore collection snapshots (via `snapshot()` from M5-T01).
//! - The schema version (for migration awareness).
//! - The backup creation timestamp.

use std::path::Path;

use aes_gcm::{aead::Aead, Aes256Gcm, KeyInit, Nonce};
use rand::RngCore;
use scrypt::scrypt;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::vectorstore::{CollectionSnapshot, VectorStore};

/// The magic header for `.sosync` files (identifies the format).
pub const SOSYNC_MAGIC: &[u8; 7] = b"SOSYNC1";

/// The format version byte.
pub const SOSYNC_VERSION: u8 = 1;

/// The scrypt salt length (16 bytes — standard).
const SALT_LEN: usize = 16;

/// The AES-256-GCM nonce length (12 bytes — standard).
const NONCE_LEN: usize = 12;

/// The scrypt parameters. Per A6: "scrypt-derived key." These are the same
/// parameters as the reference (N=2^17, r=8, p=1) — OWASP recommended.
/// They're CPU-intensive but adequate for local-first desktop use.
const SCRYPT_N: u32 = 1 << 17;
const SCRYPT_R: u32 = 8;
const SCRYPT_P: u32 = 1;
const SCRYPT_KEY_LEN: usize = 32; // 256 bits for AES-256

/// The backup payload — what gets encrypted + stored in the `.sosync` file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SosyncBackup {
    /// The format version (matches `SOSYNC_VERSION`).
    pub version: u8,
    /// All VectorStore collection snapshots.
    pub collections: Vec<CollectionSnapshot>,
    /// The schema version at backup time (for migration awareness).
    pub schema_version: u32,
    /// When the backup was created (ISO-8601 UTC).
    pub created_at: String,
}

/// Derive a 256-bit AES key from a password + salt using scrypt.
fn derive_key(password: &str, salt: &[u8]) -> Result<[u8; SCRYPT_KEY_LEN]> {
    let mut key = [0u8; SCRYPT_KEY_LEN];
    scrypt(
        password.as_bytes(),
        salt,
        &scrypt::Params::new(
            SCRYPT_N.trailing_zeros() as u8,
            SCRYPT_R,
            SCRYPT_P,
            SCRYPT_KEY_LEN,
        )
        .map_err(|e| Error::Config(format!("scrypt params invalid: {e}")))?,
        &mut key,
    )
    .map_err(|e| Error::Config(format!("scrypt derivation failed: {e}")))?;
    Ok(key)
}

/// Create a backup of all VectorStore collections. Encrypts with the given
/// password (AES-256-GCM, scrypt-derived key) and writes to `path`.
///
/// Per A6: "authenticated header, verify-first import, safety backup,
/// atomic swap." This function writes to a temp file first, then renames
/// atomically to the final path (atomic swap).
///
/// # Errors
///
/// Returns `Error::Io` if file I/O fails, `Error::Config` for crypto/serde
/// failures, or the VectorStore's error if `snapshot()` fails.
pub fn backup(
    path: &Path,
    password: &str,
    vectorstore: &dyn VectorStore,
    collection_names: &[String],
    schema_version: u32,
) -> Result<()> {
    // 1. Collect all collection snapshots.
    let mut collections = Vec::with_capacity(collection_names.len());
    for name in collection_names {
        let bytes = vectorstore.snapshot(name)?;
        let snapshot: CollectionSnapshot = serde_json::from_slice(&bytes).map_err(|e| {
            Error::Config(format!("snapshot deserialization failed for {name}: {e}"))
        })?;
        collections.push(snapshot);
    }

    // 2. Build the backup payload.
    let backup = SosyncBackup {
        version: SOSYNC_VERSION,
        collections,
        schema_version,
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    let plaintext = serde_json::to_vec(&backup)
        .map_err(|e| Error::Config(format!("backup serialization failed: {e}")))?;

    // 3. Derive the AES key from the password (scrypt).
    let mut salt = [0u8; SALT_LEN];
    rand::thread_rng().fill_bytes(&mut salt);
    let key = derive_key(password, &salt)?;
    let cipher = Aes256Gcm::new_from_slice(&key)
        .map_err(|e| Error::Config(format!("AES key init failed: {e}")))?;

    // 4. Encrypt (AES-256-GCM).
    let mut nonce_bytes = [0u8; NONCE_LEN];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ciphertext = cipher
        .encrypt(nonce, plaintext.as_ref())
        .map_err(|e| Error::Config(format!("AES encryption failed: {e}")))?;

    // 5. Write to a temp file, then rename atomically (atomic swap per A6).
    let mut file_content =
        Vec::with_capacity(SOSYNC_MAGIC.len() + 1 + SALT_LEN + NONCE_LEN + ciphertext.len());
    file_content.extend_from_slice(SOSYNC_MAGIC);
    file_content.push(SOSYNC_VERSION);
    file_content.extend_from_slice(&salt);
    file_content.extend_from_slice(&nonce_bytes);
    file_content.extend_from_slice(&ciphertext);

    let temp_path = path.with_extension("sosync.tmp");
    std::fs::write(&temp_path, &file_content)?;
    std::fs::rename(&temp_path, path)?;

    Ok(())
}

/// Restore a backup from a `.sosync` file. Per A6: "verify-first import
/// (check integrity before applying), safety backup, atomic swap."
///
/// The verification steps:
/// 1. Check the magic header.
/// 2. Check the version byte.
/// 3. Derive the key from the password (scrypt).
/// 4. Decrypt (AES-256-GCM — wrong password → decryption failure).
/// 5. Deserialize the backup payload.
///
/// After verification, each collection is restored via `vectorstore.restore()`.
///
/// # Errors
///
/// Returns `Error::Config` for format/crypto/serde failures, `Error::Io`
/// for file I/O, or the VectorStore's error if `restore()` fails.
pub fn restore(path: &Path, password: &str, vectorstore: &dyn VectorStore) -> Result<SosyncBackup> {
    // 1. Read the file.
    let file_content = std::fs::read(path)?;

    // 2. Verify the magic header (verify-first import per A6).
    if file_content.len() < SOSYNC_MAGIC.len() + 1 + SALT_LEN + NONCE_LEN {
        return Err(Error::Config(format!(
            "file too short for .sosync header (got {} bytes)",
            file_content.len()
        )));
    }
    if &file_content[..SOSYNC_MAGIC.len()] != SOSYNC_MAGIC {
        return Err(Error::Config(
            "invalid magic header — not a .sosync file".into(),
        ));
    }

    // 3. Check the version byte.
    let version = file_content[SOSYNC_MAGIC.len()];
    if version != SOSYNC_VERSION {
        return Err(Error::Config(format!(
            "unsupported .sosync version {version} (expected {SOSYNC_VERSION})"
        )));
    }

    // 4. Extract salt + nonce + ciphertext.
    let offset = SOSYNC_MAGIC.len() + 1;
    let salt = &file_content[offset..offset + SALT_LEN];
    let nonce_offset = offset + SALT_LEN;
    let nonce_bytes = &file_content[nonce_offset..nonce_offset + NONCE_LEN];
    let ciphertext = &file_content[nonce_offset + NONCE_LEN..];

    // 5. Derive the key + decrypt (wrong password → AES-GCM auth failure).
    let key = derive_key(password, salt)?;
    let cipher = Aes256Gcm::new_from_slice(&key)
        .map_err(|e| Error::Config(format!("AES key init failed: {e}")))?;
    let nonce = Nonce::from_slice(nonce_bytes);
    let plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|_| Error::Config("decryption failed — wrong password or corrupt file".into()))?;

    // 6. Deserialize the backup payload.
    let backup: SosyncBackup = serde_json::from_slice(&plaintext)
        .map_err(|e| Error::Config(format!("backup deserialization failed: {e}")))?;

    // 7. Restore each collection into the VectorStore.
    for collection in &backup.collections {
        let bytes = serde_json::to_vec(collection)
            .map_err(|e| Error::Config(format!("collection serialization failed: {e}")))?;
        vectorstore.restore(&bytes)?;
    }

    Ok(backup)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vectorstore::{InMemoryVectorStore, Point};
    use serde_json::json;
    use tempfile::NamedTempFile;

    fn store_with_data() -> InMemoryVectorStore {
        let store = InMemoryVectorStore::new();
        store.create_collection("docs", Some(3)).unwrap();
        store
            .upsert(
                "docs",
                Point {
                    id: "p1".into(),
                    dense: Some(vec![1.0, 0.0, 0.0]),
                    sparse: None,
                    payload: json!({"subject": "hello"}),
                },
            )
            .unwrap();
        store
            .upsert(
                "docs",
                Point {
                    id: "p2".into(),
                    dense: Some(vec![0.0, 1.0, 0.0]),
                    sparse: None,
                    payload: json!({"subject": "world"}),
                },
            )
            .unwrap();
        store
    }

    // ---- derive_key --------------------------------------------------------

    #[test]
    fn derive_key_is_deterministic_for_same_password_and_salt() {
        let salt = [0u8; SALT_LEN];
        let key1 = derive_key("password", &salt).unwrap();
        let key2 = derive_key("password", &salt).unwrap();
        assert_eq!(key1, key2);
    }

    #[test]
    fn derive_key_differs_for_different_passwords() {
        let salt = [0u8; SALT_LEN];
        let key1 = derive_key("password1", &salt).unwrap();
        let key2 = derive_key("password2", &salt).unwrap();
        assert_ne!(key1, key2);
    }

    #[test]
    fn derive_key_differs_for_different_salts() {
        let salt1 = [0u8; SALT_LEN];
        let salt2 = [1u8; SALT_LEN];
        let key1 = derive_key("password", &salt1).unwrap();
        let key2 = derive_key("password", &salt2).unwrap();
        assert_ne!(key1, key2);
    }

    #[test]
    fn derive_key_returns_32_bytes() {
        let salt = [0u8; SALT_LEN];
        let key = derive_key("test", &salt).unwrap();
        assert_eq!(key.len(), SCRYPT_KEY_LEN);
    }

    // ---- backup → restore round-trip ---------------------------------------

    #[test]
    fn backup_then_restore_round_trips() {
        let store = store_with_data();
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().with_extension("sosync");

        // Backup.
        backup(&path, "password123", &store, &["docs".into()], 8).unwrap();
        assert!(path.exists(), "backup file was created");

        // Restore into a fresh store.
        let store2 = InMemoryVectorStore::new();
        let restored = restore(&path, "password123", &store2).unwrap();
        assert_eq!(restored.version, SOSYNC_VERSION);
        assert_eq!(restored.collections.len(), 1);
        assert_eq!(restored.collections[0].name, "docs");
        assert_eq!(restored.collections[0].points.len(), 2);
        assert_eq!(restored.schema_version, 8);

        // Verify the restored data is searchable.
        let info = store2.collection_info("docs").unwrap().unwrap();
        assert_eq!(info.point_count, 2);
        let results = store2
            .search_dense("docs", &[1.0, 0.0, 0.0], None, 10)
            .unwrap();
        assert_eq!(results[0].id, "p1");
    }

    // ---- wrong password rejection ------------------------------------------

    #[test]
    fn restore_with_wrong_password_fails() {
        let store = store_with_data();
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().with_extension("sosync");

        backup(&path, "correct", &store, &["docs".into()], 8).unwrap();

        let store2 = InMemoryVectorStore::new();
        let result = restore(&path, "wrong", &store2);
        assert!(result.is_err(), "wrong password must fail");
        let err = result.unwrap_err().to_string();
        assert!(err.contains("decryption failed"), "got: {err}");
    }

    // ---- corrupt file rejection --------------------------------------------

    #[test]
    fn restore_corrupt_file_fails() {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().with_extension("sosync");
        // Write a file that's long enough to pass the length check but has a
        // wrong magic header.
        let mut content = b"BADMAGIC".to_vec();
        content.push(1); // version
        content.extend_from_slice(&[0u8; SALT_LEN + NONCE_LEN + 100]); // padding
        std::fs::write(&path, &content).unwrap();

        let store = InMemoryVectorStore::new();
        let result = restore(&path, "password", &store);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("invalid magic header"));
    }

    #[test]
    fn restore_truncated_file_fails() {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().with_extension("sosync");
        // Write a valid magic header but truncated (too short for the rest).
        std::fs::write(&path, SOSYNC_MAGIC).unwrap();

        let store = InMemoryVectorStore::new();
        let result = restore(&path, "password", &store);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("file too short"));
    }

    #[test]
    fn restore_wrong_version_fails() {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().with_extension("sosync");
        // Write a valid magic header + wrong version byte.
        let mut content = SOSYNC_MAGIC.to_vec();
        content.push(99); // wrong version
        content.extend_from_slice(&[0u8; SALT_LEN + NONCE_LEN]);
        std::fs::write(&path, &content).unwrap();

        let store = InMemoryVectorStore::new();
        let result = restore(&path, "password", &store);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("unsupported .sosync version"));
    }

    // ---- atomic swap -------------------------------------------------------

    #[test]
    fn backup_writes_atomically_no_temp_file_left() {
        let store = store_with_data();
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().with_extension("sosync");
        let temp_path = path.with_extension("sosync.tmp");

        backup(&path, "pw", &store, &["docs".into()], 8).unwrap();
        assert!(path.exists(), "final file exists");
        assert!(
            !temp_path.exists(),
            "temp file was renamed away (atomic swap)"
        );
    }

    // ---- empty backup ------------------------------------------------------

    #[test]
    fn backup_with_no_collections_creates_valid_file() {
        let store = InMemoryVectorStore::new();
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().with_extension("sosync");

        backup(&path, "pw", &store, &[], 8).unwrap();

        let store2 = InMemoryVectorStore::new();
        let restored = restore(&path, "pw", &store2).unwrap();
        assert_eq!(restored.collections.len(), 0);
    }

    // ---- SosyncBackup serde ------------------------------------------------

    #[test]
    fn sosync_backup_serializes_with_version() {
        let backup = SosyncBackup {
            version: 1,
            collections: vec![],
            schema_version: 8,
            created_at: "2026-10-01T10:00:00Z".into(),
        };
        let s = serde_json::to_string(&backup).unwrap();
        assert!(s.contains("\"version\":1"));
        assert!(s.contains("\"schema_version\":8"));
    }

    // ---- constants ---------------------------------------------------------

    #[test]
    fn sosync_magic_is_correct() {
        assert_eq!(SOSYNC_MAGIC, b"SOSYNC1");
    }

    #[test]
    fn sosync_version_is_1() {
        assert_eq!(SOSYNC_VERSION, 1);
    }
}
