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
}

/// Transport over nusb (blocking IO via the MaybeFuture/EndpointRead/
/// EndpointWrite adapters).
pub struct NusbTransport {
    #[allow(dead_code)] // kept for the interface lifetime
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
        // Explicitly SET CONFIGURATION(1). Linux does it during
        // enumeration; macOS does not — and download-mode gadgets rely
        // on it to cancel their "wait for PC" timeout (the Amlogic
        // u-boot burning gadget exits ~700 ms after start unless it
        // sees SetConfig, killing the session mid-transfer).
        if let Err(e) = device.set_configuration(1).wait() {
            // already-configured devices reject this; anything else is
            // still worth a try before claiming
            log::debug!("set_configuration(1): {e} (continuing)");
        }
        let interface = match device.claim_interface(0).wait() {
            Ok(i) => i,
            Err(_) => device.detach_and_claim_interface(0).wait()?,
        };

        // Find the bulk IN/OUT endpoints in the active configuration
        let config = device
            .active_configuration()
            .map_err(|e| Error::Usb(e.to_string()))?;
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
        let (ep_in, ep_out) = (ep_in, ep_out);
        let (Some(ep_in), Some(ep_out)) = (ep_in, ep_out) else {
            return Err(Error::Usb("device has no bulk endpoints".into()));
        };

        Ok(NusbTransport {
            device,
            interface,
            ep_in,
            ep_out,
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
        let ep = self
            .interface
            .endpoint::<nusb::transfer::Bulk, nusb::transfer::In>(endpoint)
            .map_err(|e| Error::Usb(e.to_string()))?;
        let mut reader = ep.reader(buf.len().max(512));
        reader
            .read_exact(buf)
            .map_err(|e| Error::Usb(e.to_string()))?;
        Ok(())
    }
}
