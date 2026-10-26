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
    /// Open the first `1b8e:c003` device (either download stage) and
    /// claim interface 0.
    pub fn open() -> Result<Self> {
        let t =
            usbcore::NusbTransport::open_first(crate::USB_VID_AMLOGIC, crate::USB_PID_UPDATE, |_| {
                true
            })?;
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
            self.ctrl_out(
                req::WR_LARGE_MEM,
                block_len as u16,
                blocks as u16,
                &head,
            )?;
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
        if buf.len() % block_len != 0 {
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
            self.ctrl_out(
                req::RD_LARGE_MEM,
                block_len as u16,
                blocks as u16,
                &head,
            )?;
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
    pub fn send_bulk_cmd(&mut self, cmd: &str) -> Result<()> {
        let mut cmd = cmd.to_string();
        cmd.push('\0');
        if cmd.len() >= 128 {
            return Err(Error::CommandTooLong(cmd));
        }
        self.ctrl_out(req::BULKCMD, 0, 2, cmd.as_bytes())
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
