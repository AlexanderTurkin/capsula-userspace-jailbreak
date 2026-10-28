//! USB transport: the `Transport` trait (mockable for tests) and the
//! blocking nusb implementation. nusb rides the OS USB stack directly on
//! Linux/macOS and the built-in WinUSB driver on Windows, so vendor
//! protocols on top need no libusb and no kernel patches.

use crate::error::{Error, Result};
use nusb::descriptors::TransferType;
use nusb::transfer::{ControlIn, ControlOut, ControlType, Direction, Recipient};
use nusb::MaybeFuture;
use std::io::{Read as _, Write as _};
use std::time::Duration;

/// Timeout for control transfers.
pub const CONTROL_TIMEOUT: Duration = Duration::from_secs(5);
/// Timeout for one bulk transfer completion. Without it a bulk read on
/// a device that vanished mid-command (the Amlogic gadget tears its USB
/// stack down on re-init) hangs on IOKit forever — the reference
/// flasher uses 5 s for bulk reads too.
pub const BULK_TIMEOUT: Duration = Duration::from_secs(5);

/// Maximum bytes per host bulk-IN request. nusb enables WinUSB RAW_IO,
/// which does not split oversized requests; use the same 1 MiB ceiling
/// for both vendor protocols. Bulk-OUT is split by WinUSB itself.
pub const BULK_IN_MAX: usize = 1024 * 1024;

/// Minimal synchronous transport needed by the vendor protocols.
/// Production: [`NusbTransport`]; tests: [`crate::MockTransport`].
pub trait Transport {
    /// Vendor OUT control transfer on endpoint 0.
    fn control_out(&mut self, request: u8, value: u16, index: u16, data: &[u8]) -> Result<()>;

    /// Vendor IN control transfer on endpoint 0; `buf.len()` is wLength.
    /// Returns the number of bytes actually received (the device may
    /// legitimately answer short, e.g. the Amlogic BootROM identify).
    fn control_in(&mut self, request: u8, value: u16, index: u16, buf: &mut [u8]) -> Result<usize>;

    /// Write the whole buffer to a bulk-OUT endpoint (split into chunks).
    fn bulk_write(&mut self, endpoint: u8, data: &[u8]) -> Result<()>;

    /// Read exactly buf.len() bytes from a bulk-IN endpoint (loop until
    /// full).
    fn bulk_read(&mut self, endpoint: u8, buf: &mut [u8]) -> Result<()>;

    /// Read UP TO buf.len() bytes from a bulk-IN endpoint: one transfer
    /// capped at [`BULK_IN_MAX`], short packets included. Reply frames may
    /// be shorter than the buffer (the reference flasher tolerates
    /// short status reads); blocking for the full buffer would wedge
    /// the pipe after a short reply.
    fn bulk_read_upto(&mut self, endpoint: u8, buf: &mut [u8]) -> Result<usize>;
}

/// Transport over nusb (blocking IO via the MaybeFuture/EndpointRead/
/// EndpointWrite adapters).
pub struct NusbTransport {
    device: nusb::Device,
    interface: nusb::Interface,
    /// bulk-IN endpoint (usually 0x81).
    pub ep_in: u8,
    /// bulk-OUT endpoint (usually 0x02).
    pub ep_out: u8,
}

impl NusbTransport {
    /// Open the first (vid, pid) device that passes `filter` (inspect
    /// strings/serials there to tell co-existing modes apart) and claim
    /// interface 0.
    pub fn open_first(
        vid: u16,
        pid: u16,
        filter: impl Fn(&nusb::DeviceInfo) -> bool,
    ) -> Result<Self> {
        let info = nusb::list_devices()
            .wait()?
            .filter(|d| d.vendor_id() == vid && d.product_id() == pid)
            .find(|d| filter(d))
            .ok_or(Error::DeviceNotFound)?;

        Self::open_device_info(&info)
    }

    /// Open an exact DeviceInfo (to pick a specific instance).
    pub fn open_device_info(info: &nusb::DeviceInfo) -> Result<Self> {
        let device = info.open().wait()?;
        // The endpoints are read from the ACTIVE configuration; if the
        // OS left the device unconfigured (rare — macOS/Linux configure
        // during enumeration), do it explicitly: download-mode gadgets
        // like the Amlogic burning one cancel their "wait for PC"
        // timeout only on SetConfig. A redundant SetConfig would reset
        // the port and kill such a gadget, so it is strictly a fallback.
        if Self::find_bulk_endpoints(&device)?.is_none() {
            log::info!("usb: device is unconfigured — setting configuration 1");
            device
                .set_configuration(1)
                .wait()
                .map_err(|e| Error::Usb(format!("set_configuration(1): {e}")))?;
        }
        let interface = match device.claim_interface(0).wait() {
            Ok(i) => i,
            Err(_) => device.detach_and_claim_interface(0).wait()?,
        };
        let (ep_in, ep_out) = Self::find_bulk_endpoints(&device)?
            .ok_or_else(|| Error::Usb("device has no bulk endpoints".into()))?;

        Ok(NusbTransport {
            device,
            interface,
            ep_in,
            ep_out,
        })
    }

    /// USB port reset: the device re-enumerates (this handle dies), the
    /// caller re-opens it. On the Amlogic gadget this re-attaches the
    /// burning mode without a board reboot — the fast session cycle.
    pub fn reset(&self) -> Result<()> {
        Ok(self.device.reset().wait()?)
    }

    /// Bulk endpoint addresses of the active configuration, if any.
    fn find_bulk_endpoints(device: &nusb::Device) -> Result<Option<(u8, u8)>> {
        let config = match device.active_configuration() {
            Ok(c) => c,
            Err(_) => return Ok(None),
        };
        let mut ep_in = None;
        let mut ep_out = None;
        'outer: for iface in config.interfaces() {
            for alt in iface.alt_settings() {
                for ep in alt.endpoints() {
                    if ep.transfer_type() != TransferType::Bulk {
                        continue;
                    }
                    match ep.direction() {
                        Direction::In => ep_in = Some(ep.address()),
                        Direction::Out => ep_out = Some(ep.address()),
                    }
                }
                if ep_in.is_some() && ep_out.is_some() {
                    break 'outer;
                }
            }
        }
        Ok(match (ep_in, ep_out) {
            (Some(i), Some(o)) => Some((i, o)),
            _ => None,
        })
    }
}

impl Transport for NusbTransport {
    fn control_out(&mut self, request: u8, value: u16, index: u16, data: &[u8]) -> Result<()> {
        self.interface
            .control_out(
                ControlOut {
                    control_type: ControlType::Vendor,
                    recipient: Recipient::Device,
                    request,
                    value,
                    index,
                    data,
                },
                CONTROL_TIMEOUT,
            )
            .wait()?;
        Ok(())
    }

    fn control_in(&mut self, request: u8, value: u16, index: u16, buf: &mut [u8]) -> Result<usize> {
        let data = self
            .interface
            .control_in(
                ControlIn {
                    control_type: ControlType::Vendor,
                    recipient: Recipient::Device,
                    request,
                    value,
                    index,
                    length: buf.len() as u16,
                },
                CONTROL_TIMEOUT,
            )
            .wait()?;
        let n = data.len().min(buf.len());
        buf[..n].copy_from_slice(&data[..n]);
        Ok(n)
    }

    fn bulk_write(&mut self, endpoint: u8, data: &[u8]) -> Result<()> {
        let ep = self
            .interface
            .endpoint::<nusb::transfer::Bulk, nusb::transfer::Out>(endpoint)
            .map_err(|e| Error::Usb(e.to_string()))?;
        let mut writer = ep.writer(data.len().max(512));
        writer.set_write_timeout(BULK_TIMEOUT);
        writer
            .write_all(data)
            .map_err(|e| Error::Usb(e.to_string()))?;
        // EndpointWrite buffers until full: without an explicit flush a
        // short write (e.g. a 31-byte CBW or a 16-byte command block) is
        // never submitted.
        writer.flush().map_err(|e| Error::Usb(e.to_string()))?;
        Ok(())
    }

    fn bulk_read(&mut self, endpoint: u8, buf: &mut [u8]) -> Result<()> {
        read_bulk_chunks(buf, |chunk| {
            let ep = self
                .interface
                .endpoint::<nusb::transfer::Bulk, nusb::transfer::In>(endpoint)
                .map_err(|e| Error::Usb(e.to_string()))?;
            let mut reader = ep.reader(chunk.len().max(512));
            reader.set_read_timeout(BULK_TIMEOUT);
            reader
                .read_exact(chunk)
                .map_err(|e| Error::Usb(e.to_string()))
        })
    }

    fn bulk_read_upto(&mut self, endpoint: u8, buf: &mut [u8]) -> Result<usize> {
        let len = buf.len().min(BULK_IN_MAX);
        if len == 0 {
            return Ok(0);
        }
        let buf = &mut buf[..len];
        let ep = self
            .interface
            .endpoint::<nusb::transfer::Bulk, nusb::transfer::In>(endpoint)
            .map_err(|e| Error::Usb(e.to_string()))?;
        let mut reader = ep.reader(buf.len().max(512));
        reader.set_read_timeout(BULK_TIMEOUT);
        // one transfer: a short packet ends it, and that IS the reply
        reader.read(buf).map_err(|e| Error::Usb(e.to_string()))
    }
}

/// Read a data phase through individual host requests.
fn read_bulk_chunks(
    buf: &mut [u8],
    mut read_chunk: impl FnMut(&mut [u8]) -> Result<()>,
) -> Result<()> {
    for chunk in buf.chunks_mut(BULK_IN_MAX) {
        read_chunk(chunk)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bulk_in_splits_above_one_mib() {
        // Model the host's RAW_IO ceiling, leaving the actual chunking
        // and reconstruction to the production data-phase reader.
        let source: Vec<u8> = (0..1_049_088).map(|i| (i % 251) as u8).collect();
        let mut output = vec![0; source.len()];
        let mut offset = 0;
        let mut requests = Vec::new();
        read_bulk_chunks(&mut output, |chunk| {
            if chunk.len() > 1_048_576 {
                return Err(Error::Usb("RAW_IO request exceeds host limit".into()));
            }
            requests.push(chunk.len());
            chunk.copy_from_slice(&source[offset..offset + chunk.len()]);
            offset += chunk.len();
            Ok(())
        })
        .unwrap();

        assert_eq!(requests, [1_048_576, 512]);
        assert_eq!(output, source);
    }

    #[test]
    fn bulk_in_empty_data_phase_skips_usb() {
        read_bulk_chunks(&mut [], |_| {
            Err(Error::Usb("an empty data phase must not access USB".into()))
        })
        .unwrap();
    }
}
