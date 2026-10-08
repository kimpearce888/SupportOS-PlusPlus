//! Icon-set verification for the desktop release pipeline (BU-02).
//!
//! The Tauri bundler picks up `bundle.icon` from `tauri.conf.json`; a broken
//! `.ico`/`.icns` only surfaces at packaging time on the target OS. This
//! module parses the containers locally so `cargo xtask verify-icons` (and
//! the release workflow) can fail fast with a precise message:
//!
//! - the PNG sources: exact square dimensions
//! - `icon.ico`: ICONDIR header, every directory entry is a PNG whose IHDR
//!   matches the entry size, the expected size ladder 16..256
//! - `icon.icns`: the `icns` magic, declared length, a chunk walk that must
//!   consume the file exactly, every expected entry type present with a
//!   PNG payload of the right dimensions
//! - `tauri.conf.json`: every path in `bundle.icon` exists

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde_json::Value;

const PNG_SIG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// PNG dimensions from the IHDR chunk (no CRC verification — this checks
/// structure, not encoding validity).
fn png_dimensions(bytes: &[u8]) -> Result<(u32, u32)> {
    if bytes.len() < 24 || bytes[..8] != PNG_SIG {
        bail!("not a PNG (bad signature)");
    }
    let ihdr_len = u32::from_be_bytes(bytes[8..12].try_into()?);
    if ihdr_len != 13 || &bytes[12..16] != b"IHDR" {
        bail!("first chunk is not a 13-byte IHDR");
    }
    let w = u32::from_be_bytes(bytes[16..20].try_into()?);
    let h = u32::from_be_bytes(bytes[20..24].try_into()?);
    Ok((w, h))
}

fn check_png(path: &Path, want: u32) -> Result<()> {
    let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let (w, h) = png_dimensions(&bytes).with_context(|| format!("{}", path.display()))?;
    if (w, h) != (want, want) {
        bail!("{} is {w}x{h}, expected {want}x{want}", path.display());
    }
    Ok(())
}

/// The expected `icon.ico` size ladder (mirrors what `tauri icon` emits).
const ICO_SIZES: [u32; 7] = [16, 24, 32, 48, 64, 128, 256];

fn check_ico(path: &Path) -> Result<()> {
    let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    if bytes.len() < 6 {
        bail!("{} is too short for an ICONDIR", path.display());
    }
    let reserved = u16::from_le_bytes(bytes[0..2].try_into()?);
    let kind = u16::from_le_bytes(bytes[2..4].try_into()?);
    let count = u16::from_le_bytes(bytes[4..6].try_into()?);
    if reserved != 0 || kind != 1 {
        bail!(
            "{}: bad ICO header (reserved={reserved}, type={kind})",
            path.display()
        );
    }
    if bytes.len() < 6 + 16 * count as usize {
        bail!("{}: truncated directory ({count} entries)", path.display());
    }

    let mut found = BTreeSet::new();
    for i in 0..count as usize {
        let e = &bytes[6 + i * 16..6 + i * 16 + 16];
        let w = e[0] as u32;
        let w = if w == 0 { 256 } else { w };
        let h = e[1] as u32;
        let h = if h == 0 { 256 } else { h };
        let length = u32::from_le_bytes(e[8..12].try_into()?) as usize;
        let offset = u32::from_le_bytes(e[12..16].try_into()?) as usize;
        let payload = bytes
            .get(offset..offset + length)
            .with_context(|| format!("{}: entry {w}x{h} payload out of bounds", path.display()))?;
        let (pw, ph) = png_dimensions(payload)
            .with_context(|| format!("{}: entry {w}x{h} payload", path.display()))?;
        if (pw, ph) != (w, h) {
            bail!(
                "{}: entry claims {w}x{h} but the payload PNG is {pw}x{ph}",
                path.display()
            );
        }
        found.insert(w);
    }

    let expected: BTreeSet<u32> = ICO_SIZES.into();
    if found != expected {
        bail!(
            "{}: expected exactly {ICO_SIZES:?} entries, found {:?}",
            path.display(),
            found.into_iter().collect::<Vec<_>>()
        );
    }
    Ok(())
}

/// The expected `icon.icns` entries: (type, pixel size). All PNG-encoded
/// (macOS 10.7+ accepts PNG payloads for every entry).
const ICNS_ENTRIES: &[(&[u8; 4], u32)] = &[
    (b"icp4", 16),
    (b"icp5", 32),
    (b"ic07", 128),
    (b"ic08", 256),
    (b"ic09", 512),
    (b"ic10", 1024),
    (b"ic11", 32),
    (b"ic12", 64),
    (b"ic13", 256),
    (b"ic14", 512),
];

fn check_icns(path: &Path) -> Result<()> {
    let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    if bytes.len() < 8 || &bytes[0..4] != b"icns" {
        bail!("{}: bad ICNS magic", path.display());
    }
    let total = u32::from_be_bytes(bytes[4..8].try_into()?) as usize;
    if total != bytes.len() {
        bail!(
            "{}: declared length {total} != file length {}",
            path.display(),
            bytes.len()
        );
    }

    let mut seen = BTreeSet::new();
    let mut pos = 8usize;
    while pos < bytes.len() {
        if pos + 8 > bytes.len() {
            bail!("{}: truncated chunk header at offset {pos}", path.display());
        }
        let ctype: [u8; 4] = bytes[pos..pos + 4].try_into()?;
        let length = u32::from_be_bytes(bytes[pos + 4..pos + 8].try_into()?) as usize;
        if length < 8 || pos + length > bytes.len() {
            bail!(
                "{}: chunk {} at offset {pos} has invalid length {length}",
                path.display(),
                String::from_utf8_lossy(&ctype)
            );
        }
        let payload = &bytes[pos + 8..pos + length];
        let Some(&(_, want)) = ICNS_ENTRIES.iter().find(|(t, _)| **t == ctype) else {
            bail!(
                "{}: unknown chunk type {}",
                path.display(),
                String::from_utf8_lossy(&ctype)
            );
        };
        let (pw, ph) = png_dimensions(payload).with_context(|| {
            format!(
                "{}: chunk {} payload",
                path.display(),
                String::from_utf8_lossy(&ctype)
            )
        })?;
        if (pw, ph) != (want, want) {
            bail!(
                "{}: chunk {} payload is {pw}x{ph}, expected {want}x{want}",
                path.display(),
                String::from_utf8_lossy(&ctype)
            );
        }
        seen.insert(ctype);
        pos += length;
    }
    if pos != bytes.len() {
        bail!(
            "{}: chunk walk left {} trailing bytes",
            path.display(),
            bytes.len() - pos
        );
    }

    let expected: BTreeSet<[u8; 4]> = ICNS_ENTRIES.iter().map(|(t, _)| **t).collect();
    let missing: Vec<String> = expected
        .difference(&seen)
        .map(|t| String::from_utf8_lossy(t).into_owned())
        .collect();
    if !missing.is_empty() {
        bail!("{}: missing ICNS entries {missing:?}", path.display());
    }
    Ok(())
}

/// Verify the whole icon set of the Tauri app at `tauri_dir`
/// (`crates/app/src-tauri`).
pub fn verify_icons(tauri_dir: &Path) -> Result<()> {
    let icons = tauri_dir.join("icons");

    for (name, size) in [
        ("32x32.png", 32u32),
        ("128x128.png", 128),
        ("128x128@2x.png", 256),
        ("256x256.png", 256),
        ("1024x1024.png", 1024),
    ] {
        check_png(&icons.join(name), size)?;
    }
    check_ico(&icons.join("icon.ico"))?;
    check_icns(&icons.join("icon.icns"))?;

    // every path listed in bundle.icon must exist
    let conf: Value = serde_json::from_str(&fs::read_to_string(tauri_dir.join("tauri.conf.json"))?)
        .context("parsing tauri.conf.json")?;
    let icon_paths = conf
        .get("bundle")
        .and_then(|b| b.get("icon"))
        .and_then(Value::as_array)
        .with_context(|| "tauri.conf.json has no bundle.icon array")?;
    for p in icon_paths {
        let p = p.as_str().context("bundle.icon entries must be strings")?;
        let full = tauri_dir.join(p);
        if !full.is_file() {
            bail!("bundle.icon lists {} but {} is missing", p, full.display());
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal structurally-valid PNG (signature + 13-byte IHDR, no IDAT):
    /// `png_dimensions` checks structure only, which is exactly what the
    /// fixtures need.
    fn fake_png(size: u32) -> Vec<u8> {
        let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        out.extend_from_slice(&13u32.to_be_bytes());
        out.extend_from_slice(b"IHDR");
        out.extend_from_slice(&size.to_be_bytes());
        out.extend_from_slice(&size.to_be_bytes());
        out.extend_from_slice(&[8, 6, 0, 0, 0]); // depth, RGBA, deflate, adaptive, no filter
        out
    }

    fn build_set(dir: &Path) {
        let icons = dir.join("icons");
        fs::create_dir_all(&icons).unwrap();
        for (name, size) in [
            ("32x32.png", 32u32),
            ("128x128.png", 128),
            ("128x128@2x.png", 256),
            ("256x256.png", 256),
            ("1024x1024.png", 1024),
        ] {
            fs::write(icons.join(name), fake_png(size)).unwrap();
        }

        // ICO with the full ladder
        let mut ico = Vec::new();
        ico.extend_from_slice(&[0, 0, 1, 0]); // reserved=0, type=1
        let sizes = [16u32, 24, 32, 48, 64, 128, 256];
        ico.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
        let dir_len = 6 + 16 * sizes.len(); // payloads start after the directory
        let mut payload = Vec::new();
        for s in sizes {
            let b = if s == 256 { 0u8 } else { s as u8 };
            let png = fake_png(s);
            ico.extend_from_slice(&[b, b, 0, 0, 1, 0, 32, 0]);
            ico.extend_from_slice(&(png.len() as u32).to_le_bytes());
            ico.extend_from_slice(&((dir_len + payload.len()) as u32).to_le_bytes());
            payload.extend_from_slice(&png);
        }
        ico.extend(payload);
        fs::write(icons.join("icon.ico"), ico).unwrap();

        // ICNS with all expected entry types
        let mut chunks = Vec::new();
        for (t, size) in ICNS_ENTRIES {
            let png = fake_png(*size);
            chunks.extend_from_slice(t.as_slice());
            chunks.extend_from_slice(&((png.len() + 8) as u32).to_be_bytes());
            chunks.extend_from_slice(&png);
        }
        let mut icns = Vec::new();
        icns.extend_from_slice(b"icns");
        icns.extend_from_slice(&((chunks.len() + 8) as u32).to_be_bytes());
        icns.extend_from_slice(&chunks);
        fs::write(icons.join("icon.icns"), icns).unwrap();

        fs::write(
            dir.join("tauri.conf.json"),
            r#"{"bundle": {"icon": ["icons/32x32.png", "icons/128x128.png", "icons/128x128@2x.png", "icons/icon.icns", "icons/icon.ico"]}}"#,
        )
        .unwrap();
    }

    #[test]
    fn accepts_a_well_formed_set() {
        let dir = tempfile::tempdir().unwrap();
        build_set(dir.path());
        verify_icons(dir.path()).expect("a well-formed set must verify");
    }

    #[test]
    fn rejects_a_missing_png() {
        let dir = tempfile::tempdir().unwrap();
        build_set(dir.path());
        fs::remove_file(dir.path().join("icons/128x128.png")).unwrap();
        assert!(verify_icons(dir.path()).is_err());
    }

    #[test]
    fn rejects_a_corrupted_icns() {
        let dir = tempfile::tempdir().unwrap();
        build_set(dir.path());
        let p = dir.path().join("icons/icon.icns");
        let mut bad = fs::read(&p).unwrap();
        // corrupt the declared total length
        bad[4] ^= 0xFF;
        fs::write(&p, bad).unwrap();
        assert!(verify_icons(dir.path()).is_err());
    }

    #[test]
    fn rejects_an_ico_with_a_size_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        build_set(dir.path());
        let p = dir.path().join("icons/icon.ico");
        let mut bad = fs::read(&p).unwrap();
        // first entry payload claims 16x16; flip the PNG's width byte to 17
        let offset = u32::from_le_bytes(bad[18..22].try_into().unwrap()) as usize;
        bad[offset + 16] = 17;
        fs::write(&p, bad).unwrap();
        assert!(verify_icons(dir.path()).is_err());
    }

    #[test]
    fn rejects_an_icon_listed_but_missing_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        build_set(dir.path());
        fs::remove_file(dir.path().join("icons/icon.icns")).unwrap();
        assert!(verify_icons(dir.path()).is_err());
    }
}
