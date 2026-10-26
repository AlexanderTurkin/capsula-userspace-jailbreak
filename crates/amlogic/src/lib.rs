//! Native Amlogic update (USB burning) protocol stack — the download
//! mode of the Amlogic SoCs (VK Capsula Mini rev. MRC02B on A113X and
//! other boards). Cross-platform over `usbcore`/nusb (WinUSB on
//! Windows); a port of the wire protocol of the vendor `update` tool:
//!
//! - device side: u-boot `drivers/usb/gadget/v2_burning/` (2015.01
//!   vendor trees) and the BootROM requests documented in pyamlboot
//!   `PROTOCOL.md` (github.com/superna9999/pyamlboot);
//! - host side reference: pyamlboot (`pyamlboot.py`, `optimus.py`).
//!
//! Both download stages enumerate as `1b8e:c003` and speak the same
//! vendor requests on EP0 plus two bulk endpoints:
//!
//! - **BootROM (IPL)**: memory read/write/fill, run, large memory
//!   transfers, identify (with the OTP password flags).
//! - **u-boot burning gadget (TPL)**: everything above plus textual
//!   burning commands (AM_REQ_BULKCMD / AM_REQ_TPL_CMD → the
//!   `optimus_working` dispatcher: `download`, `upload`, `verify`,
//!   `disk_initial`, `save_setting`, `burn_complete`, ...), which give
//!   full storage access.
//!
//! On the VK Capsula Mini the vendor u-boot raises this download mode
//! on every power-up for ~450 ms (the factory flashing window, not
//! password-protected); once the host identifies, the device stays in
//! the mode. Secure boot does not gate this window on A113X.

pub mod checksum;
pub mod error;
pub mod session;
pub mod soc;

pub use checksum::add_sum;
pub use error::{Error, Result};
pub use session::{
    AmlogicSession, BULK_REPLY_LEN, MEDIA_ACK_LEN, MEDIA_CHUNK,
};
pub use soc::{SocId, Stage, STAGE_MINOR_IPL, STAGE_MINOR_SPL, STAGE_MINOR_TPL};

/// USB VID of all Amlogic download modes.
pub const USB_VID_AMLOGIC: u16 = 0x1b8e;
/// Download-mode PID (BootROM and the u-boot burning gadget alike).
pub const USB_PID_UPDATE: u16 = 0xc003;

/// Vendor request codes of the update protocol (EP0, recipient device).
pub mod req {
    pub const WRITE_MEM: u8 = 0x01;
    pub const READ_MEM: u8 = 0x02;
    pub const FILL_MEM: u8 = 0x03;
    pub const MODIFY_MEM: u8 = 0x04;
    pub const RUN_IN_ADDR: u8 = 0x05;
    pub const WRITE_AUX: u8 = 0x06;
    pub const READ_AUX: u8 = 0x07;

    pub const WR_LARGE_MEM: u8 = 0x11;
    pub const RD_LARGE_MEM: u8 = 0x12;

    pub const IDENTIFY_HOST: u8 = 0x20;

    pub const TPL_CMD: u8 = 0x30;
    pub const TPL_STAT: u8 = 0x31;
    pub const WRITE_MEDIA: u8 = 0x32;
    pub const READ_MEDIA: u8 = 0x33;
    pub const BULKCMD: u8 = 0x34;
    pub const PASSWORD: u8 = 0x35;
    pub const NOP: u8 = 0x36;

    pub const GET_AMLC: u8 = 0x50;
    pub const WRITE_AMLC: u8 = 0x60;
}

/// RUN_IN_ADDR data flag: keep the USB phy powered across the jump.
pub const FLAG_KEEP_POWER_ON: u32 = 0x10;
