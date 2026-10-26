//! Rockchip OTA package (RKFW 1.x): extraction of the signed BOOT
//! loader container — the only part of the package the tool needs (the
//! usbplug loader for the MaskROM download). The container format
//! itself is parsed by `rockusb::LoaderContainer`; here we locate it
//! inside the package and verify the framing.

#[derive(Debug, thiserror::Error)]
pub enum RkfwError {
    #[error("not an RKFW package: {0}")]
    NotRkfw(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Extract the signed loader (BOOT container, byte-exact) from an RKFW
/// package. The container's own CRC32 is verified by the parser.
pub fn extract_loader(pkg: &[u8]) -> Result<Vec<u8>, RkfwError> {
    if pkg.len() < 4 || &pkg[0..4] != b"RKFW" {
        return Err(RkfwError::NotRkfw("no RKFW magic".into()));
    }
    // the BOOT loader: the first 'BOOT' within the first 0x200 bytes
    let ldr_off = pkg[..pkg.len().min(0x200)]
        .windows(4)
        .position(|w| w == b"BOOT")
        .ok_or_else(|| RkfwError::NotRkfw("no BOOT container in the first 0x200 bytes".into()))?;
    let container = rockusb::LoaderContainer::parse(pkg[ldr_off..].to_vec())
        .map_err(|e| RkfwError::NotRkfw(format!("BOOT container: {e}")))?;
    let total = {
        let last = container.entries.last().unwrap();
        last.data_offset as usize + last.data_size as usize + 4
    };
    Ok(pkg[ldr_off..ldr_off + total].to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rockusb::util::crc32_xrock;

    /// A minimal synthetic package: RKFW header + a valid BOOT container
    /// (102-byte header + one entry + data + CRC32).
    fn synth_pkg() -> Vec<u8> {
        let entry_data = b"TPL\x00\x00\x00\x00".to_vec();
        let mut boot = vec![0u8; 102];
        boot[0..4].copy_from_slice(b"BOOT");
        boot[4..6].copy_from_slice(&102u16.to_le_bytes());
        boot[0x19] = 1; // code471_num
        let mut e = vec![0u8; 57];
        e[1..5].copy_from_slice(&1u32.to_le_bytes()); // type 471
        for (i, ch) in "Tst".encode_utf16().enumerate() {
            e[5 + i * 2..7 + i * 2].copy_from_slice(&ch.to_le_bytes());
        }
        e[45..49].copy_from_slice(&(102u32 + 57).to_le_bytes()); // data_offset
        e[49..53].copy_from_slice(&(entry_data.len() as u32).to_le_bytes());
        boot.extend_from_slice(&e);
        boot.extend_from_slice(&entry_data);
        let crc = crc32_xrock(0, &boot);
        boot.extend_from_slice(&crc.to_le_bytes());

        let mut pkg = vec![0u8; 8]; // RKFW header
        pkg[0..4].copy_from_slice(b"RKFW");
        pkg.extend_from_slice(&boot);
        pkg
    }

    #[test]
    fn synthetic_loader_extraction() {
        let pkg = synth_pkg();
        let ldr = extract_loader(&pkg).expect("extract");
        // the container parses and the extraction is byte-exact
        let container = rockusb::LoaderContainer::parse(ldr.clone()).unwrap();
        assert_eq!(container.entries.len(), 1);
        let boot_off = pkg.windows(4).position(|w| w == b"BOOT").unwrap();
        assert_eq!(ldr, &pkg[boot_off..boot_off + ldr.len()]);
    }

    #[test]
    fn rejects_non_rkfw() {
        assert!(extract_loader(b"NOPE____").is_err());
        assert!(extract_loader(b"RKFW").is_err()); // no BOOT container
    }

    /// Real OTA from the VK CDN: download (temp cache), extract the
    /// loader, parse the container. Firmware binaries are not kept in
    /// the repository — the test downloads them itself, so it is
    /// disabled by default: `cargo test -p jb-core --lib -- --ignored`.
    #[test]
    #[ignore = "needs network: downloads the real OTA from the VK CDN"]
    fn real_ota_loader_downloads_and_parses() {
        let code = crate::config::Config::embedded().ota.platform;
        let meta = crate::ota::fetch_latest(&code).expect("firmware API query");
        let cache = std::env::temp_dir().join(format!("jb-rkfw-ota-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&cache);
        let (pkg, _) =
            crate::ota::ensure_cached(&meta, &code, &cache, &|_, _| {}).expect("OTA download");
        let data = std::fs::read(&pkg).expect("package read");

        let loader = extract_loader(&data).expect("loader extraction");
        let c = rockusb::LoaderContainer::parse(loader).expect("BOOT container parse");
        assert!(c.is_signed, "the loader is signed");
        assert!(!c.is_newidb, "BOOT form factor, not LDR");
        // rc4_flag != 0 means RC4 is disabled for the download
        assert!(!c.is_rc4on, "rc4_flag=1 -> RC4 disabled");
        assert_eq!(c.chiptype.to_le_bytes(), *b"8033", "rk3308 chip");
        // MaskROM download stages: DDR init (0x471) and usbplug (0x472)
        let stages = c.download_stages();
        assert!(stages.len() >= 2, "stages 471+472 required");
        assert_eq!(stages.first().unwrap().0, 0x471);
        assert_eq!(stages.last().unwrap().0, 0x472);
        let total: usize = stages.iter().map(|(_, _, d, _)| d.len()).sum();
        assert!(total > 0 && total < c.data.len());
        let _ = std::fs::remove_dir_all(&cache);
    }
}
