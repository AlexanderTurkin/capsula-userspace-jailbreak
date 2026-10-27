//! The Amlogic update (USB burning) protocol client.
//!
//! The same vendor-request protocol serves both download stages that
//! enumerate as `1b8e:c003`: the BootROM (IPL) and the u-boot USB-burning
//! gadget ("optimus", AM_REQ_* in `drivers/usb/gadget/v2_burning/`).
//! Wire format cross-checked against the device side (u-boot 2015.01
//! `v2_usb_tool/usb_pcd.c`) and the pyamlboot host implementation.
//!
//! All memory here is the SoC DRAM/registers address space; storage
//! access goes through textual burning commands (`download`/`upload`)
//! executed by the u-boot gadget.

use crate::error::{Error, Result};
use crate::soc::SocId;
use crate::{add_sum, req};
use std::thread::sleep;
use std::time::Duration;
use usbcore::Transport;

/// u16 block count per control transfer (pyamlboot MAX_LARGE_BLOCK_COUNT).
const MAX_LARGE_BLOCKS: usize = 0xffff;
/// Reply checksum algorithm id: add_sum (WRITE_MEDIA_CHEKSUM_ALG_ADDSUM).
const CHECKSUM_ALG_ADDSUM: u16 = 0x00ef;
/// Data block per `download` media packet (the burning tool's 64 KiB).
pub const MEDIA_CHUNK: usize = 0x10000;
/// Ack buffer length for media transfers (pyamlboot ackLen).
pub const MEDIA_ACK_LEN: usize = 0x200;
/// Bulk reply length of bulkcmd/tpl status (AM_BULK_REPLY_LEN).
pub const BULK_REPLY_LEN: usize = 512;
/// The device truncates a bulkcmd payload at 64 bytes silently; keep
/// the string + NUL within that (flasher.sh BULKCMD_MAX = 63).
pub const BULKCMD_MAX: usize = 63;
/// Per-transfer size of the AM_REQ_READ_MEDIA pipe: wValue is 16-bit
/// and the buffer-manager slots are small — 32 KiB is a safe step.
pub const UPLOAD_CHUNK: usize = 0x8000;
/// NAND staging size of the raw-range dump: one `nand read` moves this
/// much NAND->DRAM, then [`UPLOAD_CHUNK`] `upload mem` arms stream it
/// to the host (fewer command round-trips per byte — the per-chunk
/// cost is dominated by round-trips, not the bulk data).
pub const NAND_STAGE: usize = 256 * 1024;
/// Pause between `Continue:` status polls (as pyamlboot).
const CONTINUE_PAUSE: Duration = Duration::from_secs(3);

/// A session with an Amlogic SoC in a USB download mode.
pub struct AmlogicSession<T: Transport> {
    transport: T,
    ep_in: u8,
    ep_out: u8,
}

impl<T: Transport> AmlogicSession<T> {
    /// Access the underlying transport (tests and advanced callers).
    pub fn transport(&self) -> &T {
        &self.transport
    }
}

impl AmlogicSession<usbcore::NusbTransport> {
    /// USB port reset (see [`usbcore::NusbTransport::reset`]): the
    /// session handle dies, the device re-enumerates into a fresh
    /// download window. Part of the automatic dump cycle.
    pub fn reset(&self) -> Result<()> {
        Ok(self.transport.reset()?)
    }

    /// Open the first `1b8e:c003` device (either download stage) and
    /// claim interface 0.
    pub fn open() -> Result<Self> {
        let t = usbcore::NusbTransport::open_first(
            crate::USB_VID_AMLOGIC,
            crate::USB_PID_UPDATE,
            |_| true,
        )?;
        let (ep_in, ep_out) = (t.ep_in, t.ep_out);
        Ok(AmlogicSession::new(t, ep_in, ep_out))
    }
}

impl<T: Transport> AmlogicSession<T> {
    pub fn new(transport: T, ep_in: u8, ep_out: u8) -> Self {
        AmlogicSession {
            transport,
            ep_in,
            ep_out,
        }
    }

    /// EP0 vendor IN (control_in) helper.
    fn ctrl_in(&mut self, request: u8, value: u16, index: u16, buf: &mut [u8]) -> Result<usize> {
        Ok(self.transport.control_in(request, value, index, buf)?)
    }

    /// EP0 vendor OUT (control_out) helper.
    fn ctrl_out(&mut self, request: u8, value: u16, index: u16, data: &[u8]) -> Result<()> {
        Ok(self.transport.control_out(request, value, index, data)?)
    }

    /// Identify the device (AM_REQ_IDENTIFY_HOST): protocol version and
    /// boot stage. Also clears the u-boot auto-burn timeout, so call it
    /// promptly after the device appears.
    pub fn identify(&mut self) -> Result<SocId> {
        let mut buf = [0u8; 8];
        let n = self.ctrl_in(req::IDENTIFY_HOST, 0, 0, &mut buf)?;
        SocId::parse(&buf[..n]).ok_or(Error::Usb("identify reply too short".into()))
    }

    /// Write up to 64 bytes to memory (AM_REQ_WRITE_MEM).
    pub fn write_memory(&mut self, addr: u32, data: &[u8]) -> Result<()> {
        assert!(data.len() <= 64, "chunk must be <= 64 bytes");
        self.ctrl_out(req::WRITE_MEM, (addr >> 16) as u16, addr as u16, data)
    }

    /// Write data to memory in 64-byte EP0 chunks (AM_REQ_WRITE_MEM).
    pub fn write_memory_chunks(&mut self, addr: u32, data: &[u8]) -> Result<()> {
        for (i, chunk) in data.chunks(64).enumerate() {
            self.write_memory(addr + (i * 64) as u32, chunk)?;
        }
        Ok(())
    }

    /// Read up to 64 bytes from memory (AM_REQ_READ_MEM); returns the
    /// bytes actually received.
    pub fn read_memory(&mut self, addr: u32, buf: &mut [u8]) -> Result<usize> {
        assert!(buf.len() <= 64, "chunk must be <= 64 bytes");
        self.ctrl_in(req::READ_MEM, (addr >> 16) as u16, addr as u16, buf)
    }

    /// Read data from memory in 64-byte EP0 chunks (AM_REQ_READ_MEM).
    /// Fills `buf` completely; fails if the device answers short.
    pub fn read_memory_chunks(&mut self, addr: u32, buf: &mut [u8]) -> Result<()> {
        for (i, chunk) in buf.chunks_mut(64).enumerate() {
            let n = self.read_memory(addr + (i * 64) as u32, chunk)?;
            if n != chunk.len() {
                return Err(Error::Usb(format!(
                    "short read at {:#x} ({n} of {} bytes)",
                    addr + (i * 64) as u32,
                    chunk.len()
                )));
            }
        }
        Ok(())
    }

    /// Run code at `addr` (AM_REQ_RUN_IN_ADDR). With `keep_power` the
    /// flag bit asks newer BootROMs to leave the USB phy powered across
    /// the jump (the BL2/TPL loading chain needs it to keep talking).
    pub fn run(&mut self, addr: u32, keep_power: bool) -> Result<()> {
        let data = if keep_power {
            addr | crate::FLAG_KEEP_POWER_ON
        } else {
            addr
        };
        self.ctrl_out(
            req::RUN_IN_ADDR,
            (addr >> 16) as u16,
            addr as u16,
            &data.to_le_bytes(),
        )
    }

    /// Large memory write (AM_REQ_WR_LARGE_MEM): one control transfer
    /// announces the transfer, the payload follows on the bulk-OUT pipe
    /// in `block_len` blocks (device-side cap 0x1000 per block). The
    /// payload is zero-padded to a whole number of blocks.
    pub fn write_large_memory(&mut self, addr: u32, data: &[u8], block_len: usize) -> Result<()> {
        assert!((1..=0x1000).contains(&block_len), "block_len 1..0x1000");
        let padded = data.len().div_ceil(block_len) * block_len;
        let mut off = 0;
        while off < padded {
            let blocks = padded.div_ceil(block_len) - off / block_len;
            let blocks = blocks.min(MAX_LARGE_BLOCKS);
            let len = blocks * block_len;
            let mut head = [0u8; 16];
            head[0..4].copy_from_slice(&(addr + off as u32).to_le_bytes());
            head[4..8].copy_from_slice(&(len as u32).to_le_bytes());
            self.ctrl_out(req::WR_LARGE_MEM, block_len as u16, blocks as u16, &head)?;
            for b in 0..blocks {
                let start = off + b * block_len;
                let end = (start + block_len).min(data.len());
                let mut chunk = vec![0u8; block_len];
                if start < end {
                    chunk[..end - start].copy_from_slice(&data[start..end]);
                }
                self.transport.bulk_write(self.ep_out, &chunk)?;
            }
            off += len;
        }
        Ok(())
    }

    /// Large memory read (AM_REQ_RD_LARGE_MEM): mirror of
    /// [`write_large_memory`], payload arrives on the bulk-IN pipe.
    /// Reliable on the BootROM stage; the u-boot burning gadget of the
    /// old 2015.01 tree mishandles this request — there prefer
    /// [`AmlogicSession::upload_read`] (bulkcmd `upload mem`).
    pub fn read_large_memory(&mut self, addr: u32, buf: &mut [u8], block_len: usize) -> Result<()> {
        assert!((1..=0x1000).contains(&block_len), "block_len 1..0x1000");
        if !buf.len().is_multiple_of(block_len) {
            return Err(Error::Usb("length must be a multiple of block_len".into()));
        }
        let mut off = 0;
        while off < buf.len() {
            let blocks = (buf.len() - off) / block_len;
            let blocks = blocks.min(MAX_LARGE_BLOCKS);
            let len = blocks * block_len;
            let mut head = [0u8; 16];
            head[0..4].copy_from_slice(&(addr + off as u32).to_le_bytes());
            head[4..8].copy_from_slice(&(len as u32).to_le_bytes());
            self.ctrl_out(req::RD_LARGE_MEM, block_len as u16, blocks as u16, &head)?;
            for b in 0..blocks {
                let start = off + b * block_len;
                self.transport
                    .bulk_read(self.ep_in, &mut buf[start..start + block_len])?;
            }
            off += len;
        }
        Ok(())
    }

    /// Execute a textual burning command via AM_REQ_BULKCMD and return
    /// the final status text. The u-boot gadget runs the string through
    /// its burning dispatcher (`optimus_working`). The status is
    /// `success` when the command completed, `Continue:xx` while it is
    /// still running (polled with a pause), anything else is the error
    /// text.
    pub fn bulk_cmd(&mut self, cmd: &str) -> Result<String> {
        self.send_bulk_cmd(cmd)?;
        self.wait_bulk_status()
    }

    /// Send a bulk command without waiting for the status.
    ///
    /// Payload cap: the device truncates at 64 bytes silently and then
    /// answers half a command with a syntax error nobody reports — so
    /// anything longer is an error here (flasher.sh BULKCMD_MAX).
    pub fn send_bulk_cmd(&mut self, cmd: &str) -> Result<()> {
        let mut cmd = cmd.to_string();
        cmd.push('\0');
        if cmd.len() > BULKCMD_MAX + 1 {
            return Err(Error::CommandTooLong(cmd));
        }
        self.ctrl_out(req::BULKCMD, 0, 2, cmd.as_bytes())
    }

    /// Sacrificial command right after identify: the first bulkcmd of a
    /// session can lose its leading bytes (flasher.sh `prime_bulk`), and
    /// on a freshly caught device the bulk pipes may not be ready yet
    /// (IOKit kIOUSBUnknownPipeErr) — so this also retries until the
    /// bulk path answers. The semantic result is ignored either way.
    pub fn prime(&mut self) {
        for attempt in 0..3 {
            match self.bulk_cmd("echo") {
                // the pipe works — the reply text is meaningless here
                Ok(_) | Err(Error::CommandFailed(_)) => return,
                Err(_) => {
                    if attempt == 2 {
                        return;
                    }
                    sleep(Duration::from_millis(100));
                }
            }
        }
    }

    /// Poll the bulk status until the running command finishes.
    /// `Continue:xx` means "still executing".
    pub fn wait_bulk_status(&mut self) -> Result<String> {
        loop {
            let text = self.read_bulk_reply()?;
            if text.starts_with("Continue:") {
                log::info!("amlogic: {text} — waiting");
                sleep(CONTINUE_PAUSE);
                continue;
            }
            if text.starts_with("success") {
                return Ok(text);
            }
            return Err(Error::CommandFailed(text));
        }
    }

    /// One 512-byte bulk-IN status reply, NUL-trimmed.
    pub fn read_bulk_reply(&mut self) -> Result<String> {
        let mut buf = [0u8; BULK_REPLY_LEN];
        self.transport.bulk_read(self.ep_in, &mut buf)?;
        Ok(nul_trim(&buf))
    }

    /// Execute a command via AM_REQ_TPL_CMD (subcode 1) and fetch the
    /// result with AM_REQ_TPL_STAT (a 64-byte EP0 reply instead of the
    /// bulk one). The burning tool uses this channel for `download`.
    pub fn tpl_cmd(&mut self, cmd: &str) -> Result<String> {
        let mut cmd = cmd.to_string();
        cmd.push('\0');
        if cmd.len() >= 128 {
            return Err(Error::CommandTooLong(cmd));
        }
        self.ctrl_out(req::TPL_CMD, 0, 1, cmd.as_bytes())?;
        self.tpl_stat()
    }

    /// Fetch the TPL command result (AM_REQ_TPL_STAT).
    pub fn tpl_stat(&mut self) -> Result<String> {
        let mut buf = [0u8; 0x40];
        self.ctrl_in(req::TPL_STAT, 0, 0, &mut buf)?;
        let text = nul_trim(&buf);
        if text.starts_with("Continue:") {
            log::info!("amlogic: {text} — waiting");
            sleep(CONTINUE_PAUSE);
            return self.tpl_stat();
        }
        if text.starts_with("success") {
            return Ok(text);
        }
        Err(Error::CommandFailed(text))
    }

    /// One `download` media packet (AM_REQ_WRITE_MEDIA): `data` (at most
    /// [`MEDIA_CHUNK`], zero-padded is not applied — pass whole packets)
    /// is streamed over bulk-OUT with an add_sum checksum, then the
    /// device acknowledges on bulk-IN with `OK!!`. Prerequisite: a
    /// `download <store|mem|dtb> <part> <type> <total_size>` command has
    /// armed the transfer.
    pub fn write_media(&mut self, data: &[u8], seq: u32) -> Result<()> {
        assert!(data.len() <= MEDIA_CHUNK, "packet must be <= MEDIA_CHUNK");
        let mut head = [0u8; 0x20];
        head[0..4].copy_from_slice(&0u32.to_le_bytes()); // retryTimes / isPktResended
        head[4..8].copy_from_slice(&(data.len() as u32).to_le_bytes());
        head[8..12].copy_from_slice(&seq.to_le_bytes());
        head[12..16].copy_from_slice(&add_sum(data).to_le_bytes());
        head[16..18].copy_from_slice(&CHECKSUM_ALG_ADDSUM.to_le_bytes());
        head[18..20].copy_from_slice(&(MEDIA_ACK_LEN as u16).to_le_bytes());
        self.ctrl_out(req::WRITE_MEDIA, 1, 0xffff, &head)?;
        self.transport.bulk_write(self.ep_out, data)?;
        loop {
            let ack = self.read_bulk_reply()?;
            if ack.starts_with("Continue:") {
                log::info!("amlogic: {ack} — waiting");
                sleep(CONTINUE_PAUSE);
                continue;
            }
            if ack.starts_with("OK!!") {
                return Ok(());
            }
            return Err(Error::MediaFailed(ack));
        }
    }

    /// Stream a whole buffer as `download` media packets of
    /// [`MEDIA_CHUNK`] (sequence numbers from 0).
    pub fn write_media_all(&mut self, mut data: &[u8]) -> Result<()> {
        let mut seq = 0u32;
        while !data.is_empty() {
            let n = data.len().min(MEDIA_CHUNK);
            self.write_media(&data[..n], seq)?;
            data = &data[n..];
            seq += 1;
        }
        Ok(())
    }

    /// Receive `size` bytes uploaded by the preceding `upload ...`
    /// burning command (AM_REQ_READ_MEDIA): a 16-byte EP0 header reply
    /// followed by the payload on bulk-IN. `size` must fit 16 bits
    /// (wValue); the burning tool uploads in 4 KiB steps.
    pub fn read_media(&mut self, size: usize) -> Result<Vec<u8>> {
        if size == 0 || size > 0xffff {
            return Err(Error::Usb(format!("upload size {size} out of 1..=0xffff")));
        }
        let blocks = size.div_ceil(0x1000) as u16;
        let mut head = [0u8; 16];
        self.ctrl_in(req::READ_MEDIA, size as u16, blocks, &mut head)?;
        let mut data = vec![0u8; size];
        self.transport.bulk_read(self.ep_in, &mut data)?;
        Ok(data)
    }

    /// One AM_REQ_UPLOAD fetch that also returns the device state head:
    /// bytes [4..8] are the data the device still owes, [8..12] the
    /// sequence number (usb_pcd.c do_vendor_in_complete).
    pub fn read_media_head(&mut self, size: usize) -> Result<(Vec<u8>, [u8; 16])> {
        if size == 0 || size > 0xffff {
            return Err(Error::Usb(format!("upload size {size} out of 1..=0xffff")));
        }
        let blocks = size.div_ceil(0x1000) as u16;
        let mut head = [0u8; 16];
        self.ctrl_in(req::READ_MEDIA, size as u16, blocks, &mut head)?;
        let mut data = vec![0u8; size];
        self.transport.bulk_read(self.ep_in, &mut data)?;
        Ok((data, head))
    }

    /// u-boot gadget stage: read DRAM via the `upload mem <addr> normal
    /// <size>` burning command + AM_REQ_READ_MEDIA (works around the old
    /// tree's broken AM_REQ_RD_LARGE_MEM). `size` must fit 16 bits.
    pub fn upload_read(&mut self, addr: u32, size: usize) -> Result<Vec<u8>> {
        let cmd = format!("upload mem {addr:#x} normal {size:#x}");
        self.send_bulk_cmd(&cmd)?;
        // the command completes once the payload is staged; the data
        // itself arrives via read_media
        let status = self.wait_bulk_status()?;
        log::debug!("upload armed: {status}");
        self.read_media(size)
    }

    /// Read a large DRAM range in [`UPLOAD_CHUNK`] steps, each with its
    /// own `upload mem` arm (independent transfers, no device-side
    /// sequence to maintain).
    pub fn read_dram(
        &mut self,
        addr: u32,
        size: usize,
        progress: &dyn Fn(usize),
    ) -> Result<Vec<u8>> {
        let mut out = vec![0u8; size];
        let mut off = 0usize;
        while off < size {
            let n = (size - off).min(UPLOAD_CHUNK);
            let chunk = self.upload_read(addr + off as u32, n)?;
            out[off..off + n].copy_from_slice(&chunk);
            off += n;
            progress(off);
        }
        Ok(out)
    }

    /// `nand read <ram> <off> <len>` — raw-offset NAND read into DRAM
    /// (the logical layout: u-boot skips bad blocks). Pair with
    /// [`AmlogicSession::read_dram`] to bring the data to the host.
    pub fn nand_read(&mut self, ram: u32, off: u32, len: usize) -> Result<()> {
        self.bulk_cmd(&format!("nand read {ram:#x} {off:#x} {len:#x}"))
            .map(|_| ())
    }

    /// Dump a raw NAND range through DRAM (flasher.sh
    /// `read_big_from_flash`): a `nand read` stages up to
    /// [`NAND_STAGE`] in RAM, `upload mem` streams it back in
    /// [`UPLOAD_CHUNK`] steps. The one-shot `upload store` stream
    /// overruns the pipe on this gadget (the device pushes the whole
    /// armed size past the host read), so the per-chunk path is the
    /// reliable one.
    ///
    /// Error split: a `nand read` REJECTED by the device (`failed:`,
    /// a bad block after the good prefix) truncates the result and
    /// returns it; a TRANSPORT error is the session dying and
    /// propagates to the caller, which reconnects and retries the same
    /// range.
    pub fn read_nand_range(
        &mut self,
        off: u32,
        len: usize,
        progress: &dyn Fn(usize),
    ) -> Result<Vec<u8>> {
        self.read_nand_range_until(off, len, progress, &|| false)
    }

    /// [`read_nand_range`] with a cooperative stop: `stop` is polled
    /// before every sub-chunk, so a caller riding a short-lived window
    /// can exit cleanly mid-range and finish the command it needs
    /// before the window closes.
    pub fn read_nand_range_until(
        &mut self,
        off: u32,
        len: usize,
        progress: &dyn Fn(usize),
        stop: &dyn Fn() -> bool,
    ) -> Result<Vec<u8>> {
        let mut out = vec![0u8; len];
        let mut total = 0usize; // bytes received across all stages
        while total < len {
            if stop() {
                break;
            }
            // identify doubles as the keepalive: one cheap control
            // transfer per stage
            let _ = self.identify();
            let stage = (len - total).min(NAND_STAGE);
            if let Err(e) = self.nand_read(crate::RAM_SCRATCH, off + total as u32, stage) {
                match e {
                    // the device rejected the read after the good
                    // prefix — the received data is the truth
                    // (flasher.sh readable_size)
                    Error::CommandFailed(_) if total > 0 => {
                        log::warn!(
                            "nand read rejected at {:#x} (+{total:#x} of {len:#x}): {e} \
                             — keeping the prefix",
                            off + total as u32
                        );
                        break;
                    }
                    // anything else is the session dying
                    other => return Err(other),
                }
            }
            let mut staged = 0usize;
            while staged < stage {
                if stop() {
                    break;
                }
                let n = (stage - staged).min(UPLOAD_CHUNK);
                let chunk = self.upload_read(crate::RAM_SCRATCH + staged as u32, n)?;
                out[total + staged..total + staged + n].copy_from_slice(&chunk);
                staged += n;
                progress(total + staged);
            }
            total += staged;
            if staged < stage {
                break; // stopped mid-stage
            }
        }
        out.truncate(total);
        Ok(out)
    }

    /// Stream a whole partition by name: one `upload store <part>
    /// normal <total>` arm, then sequential AM_REQ_UPLOAD fetches until
    /// the device reports no data left. A short stream (bad blocks
    /// shorten NAND reads) is returned with a warning, not an error —
    /// the vendor flasher treats the received length as the truth.
    pub fn upload_store(
        &mut self,
        part: &str,
        total: usize,
        progress: &dyn Fn(usize),
    ) -> Result<Vec<u8>> {
        if total == 0 {
            return Ok(Vec::new());
        }
        self.bulk_cmd(&format!("upload store {part} normal {total:#x}"))
            .map(|_| ())?;
        let mut out = Vec::with_capacity(total);
        while out.len() < total {
            // 0x8000 steps: a ~64 KiB fetch breaks the pipe on real
            // hardware (IOKit submit failure), 32 KiB is proven fine
            let want = (total - out.len()).min(UPLOAD_CHUNK);
            let (data, head) = self.read_media_head(want)?;
            let left = u32::from_le_bytes(head[4..8].try_into().unwrap()) as usize;
            if data.is_empty() {
                return Err(Error::Usb(format!(
                    "upload store {part}: stalled with {} of {total:#x} bytes",
                    out.len()
                )));
            }
            out.extend_from_slice(&data);
            progress(out.len());
            if left == 0 {
                if out.len() < total {
                    log::warn!(
                        "upload store {part}: device reports no data left at {:#x} of {total:#x} \
                         bytes (bad blocks shorten the read) — keeping the short dump",
                        out.len()
                    );
                }
                break;
            }
        }
        Ok(out)
    }

    /// Fill memory words (AM_REQ_FILL_MEM): (addr, value) pairs.
    pub fn fill_memory(&mut self, pairs: &[(u32, u32)]) -> Result<()> {
        let mut data = Vec::with_capacity(pairs.len() * 8);
        for (addr, value) in pairs {
            data.extend_from_slice(&addr.to_le_bytes());
            data.extend_from_slice(&value.to_le_bytes());
        }
        self.ctrl_out(req::FILL_MEM, 0, 0, &data)
    }
}

/// Trim trailing NULs of a fixed-size reply buffer.
fn nul_trim(buf: &[u8]) -> String {
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).into_owned()
}
