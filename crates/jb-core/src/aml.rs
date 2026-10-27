//! High-level Amlogic operations, ported from the community flasher.sh
//! for the Capsula Mini (A113X): env capture over USB, partition dumps.
//! Everything here is read-only.

use crate::session::{Progress, ProgressFn};
use amlogic::AmlogicSession;
use std::io::Write as _;
use std::path::Path;

/// Capture the u-boot environment as text — the only way to get it
/// without the UART: bulkcmd answers go to the serial console, so the
/// env is exported into DRAM and the DRAM is read back over USB
/// (flasher.sh `capture_env`). The buffer is zeroed first, or the tail
/// would carry the 0x55 wall of the BL2 memory test.
pub fn capture_env<T: usbcore::Transport>(s: &mut AmlogicSession<T>) -> anyhow::Result<String> {
    s.bulk_cmd(&format!(
        "mw.b {:#x} 0 {:#x}",
        amlogic::ENV_ADDR,
        amlogic::ENV_SIZE
    ))
    .map_err(|e| anyhow::anyhow!("mw.b: {e}"))?;
    s.bulk_cmd(&format!("env export -t {:#x}", amlogic::ENV_ADDR))
        .map_err(|e| anyhow::anyhow!("env export: {e}"))?;
    let buf = s
        .read_dram(amlogic::ENV_ADDR, amlogic::ENV_SIZE, &|_| {})
        .map_err(|e| anyhow::anyhow!("read env from DRAM: {e}"))?;
    let end = buf.iter().position(|&b| b == 0).ok_or_else(|| {
        anyhow::anyhow!(
            "env text is not NUL-terminated within {:#x} bytes",
            amlogic::ENV_SIZE
        )
    })?;
    let text = String::from_utf8_lossy(&buf[..end]).into_owned();
    // trim the 0x55 wall too, in case mw.b was rejected
    let text = text.trim_matches('\u{55}').trim_end();
    Ok(text.to_string())
}

/// Dump a vendor partition by name into `dir/<part>.bin`. A short read
/// (bad blocks) is kept, matching the flasher: the received length is
/// the truth. Returns the number of bytes written.
pub fn dump_partition<T: usbcore::Transport>(
    s: &mut AmlogicSession<T>,
    part: &str,
    dir: &Path,
    progress: ProgressFn,
) -> anyhow::Result<u64> {
    let (_, _, size) = amlogic::partition(part)
        .ok_or_else(|| anyhow::anyhow!("unknown partition {part:?}; known: {}", known_parts()))?;
    progress(Progress::PartitionStart {
        name: part.into(),
        sectors: size as u64 / 512,
    });
    let data = s
        .upload_store(part, size as usize, &|done| {
            progress(Progress::PartitionProgress {
                name: part.into(),
                done: done as u64,
                total: size as u64,
            });
        })
        .map_err(|e| anyhow::anyhow!("upload store {part}: {e}"))?;
    std::fs::create_dir_all(dir)?;
    let path = dir.join(format!("{part}.bin"));
    let mut f = std::fs::File::create(&path)?;
    f.write_all(&data)?;
    progress(Progress::PartitionDone { name: part.into() });
    Ok(data.len() as u64)
}

fn known_parts() -> String {
    amlogic::PARTITIONS
        .iter()
        .map(|(n, _, _)| *n)
        .collect::<Vec<_>>()
        .join(", ")
}
