//! Native Rockchip protocol stack for VK Capsula (RK3308) and other
//! Rockchip boards. A port of `xrock` v1.1.3 (github.com/xboot/xrock,
//! MIT) without libusb or subprocesses — transport via the shared
//! `usbcore` crate (nusb, WinUSB on Windows).
//!
//! Layers:
//!
//! - [`transport`] — Rockchip device-mode enumeration (2207:*); the
//!   USB transport trait itself lives in usbcore.
//! - [`loader_container`] — BOOT/LDR loader container parser (RKBOOT).
//! - [`maskrom`] — BootROM stage: DownloadBoot (control 0x0C, stage
//!   codes 0x471/0x472, CRC16-CCITT, RC4, 4096-byte chunks).
//! - [`loader`] — usbplug stage: rockusb over bulk CBW/CSW ('USBC'/
//!   'USBS'): flash read/write/erase by LBA, RAM read/write, reset, info.
//! - [`monitor`] — DeviceMonitor: bus events with timestamps, priority
//!   330d > usbplug > maskrom, debounce (port of common.sh).

pub mod error;
pub mod loader;
pub mod loader_container;
pub mod maskrom;
pub mod monitor;
pub mod transport;
pub mod util;

// The USB transport layer is shared with the other vendor crates.
pub use usbcore::mock;
pub use usbcore::transport::{NusbTransport, Transport};
pub use usbcore::MockTransport;

pub use error::{Error, Result};
pub use loader::LoaderSession;
pub use loader_container::LoaderContainer;
pub use maskrom::MaskRomSession;
pub use monitor::DeviceMonitor;
pub use transport::{current_mode, list_modes, open_mode, DeviceMode};

/// USB VID of all Rockchip modes.
pub const USB_VID_ROCKCHIP: u16 = 0x2207;
/// MaskROM (BootROM) PID — the same after `db` (usbplug): told apart by
/// iProduct == "USB-MSC".
pub const USB_PID_MASKROM: u16 = 0x330e;
/// Loader mode PID (u-boot rockusb).
pub const USB_PID_LOADER: u16 = 0x330d;
/// Live Linux gadget PID (adb); NOT a download mode.
pub const USB_PID_LINUX_GADGET: u16 = 0x0006;
