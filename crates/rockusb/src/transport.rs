//! Rockchip device-mode enumeration on the bus. The transport itself
//! ([`usbcore::Transport`] / [`usbcore::NusbTransport`]) lives in the
//! shared usbcore crate; this module adds the Rockchip-specific mode
//! detection (loader vs MaskROM vs the live Linux gadget).

use crate::error::Result;
use nusb::MaybeFuture;
use usbcore::NusbTransport;

/// Snapshot of a Rockchip device present on the bus.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DeviceMode {
    /// 2207:330d — u-boot loader (rockusb).
    Loader,
    /// 2207:330e — raw MaskROM (BootROM).
    MaskRom,
    /// 2207:330e with iProduct == "USB-MSC" — usbplug runs in RAM.
    /// (Told apart by iProduct, not by the PID.)
    UsbPlug,
    /// 2207:0006 — Linux gadget (adb); NOT a download mode.
    LinuxGadget,
    /// Any other PID of the vendor (diagnostics).
    Other(u16),
}

impl DeviceMode {
    pub fn pid(&self) -> u16 {
        match self {
            DeviceMode::Loader => 0x330d,
            DeviceMode::MaskRom | DeviceMode::UsbPlug => 0x330e,
            DeviceMode::LinuxGadget => 0x0006,
            DeviceMode::Other(p) => *p,
        }
    }

    /// The "best" of the present modes: 330d > usbplug > maskrom > 0006 >
    /// others. None if the bus is empty.
    pub fn best(modes: &[DeviceMode]) -> Option<DeviceMode> {
        if modes.contains(&DeviceMode::Loader) {
            return Some(DeviceMode::Loader);
        }
        if modes.contains(&DeviceMode::UsbPlug) {
            return Some(DeviceMode::UsbPlug);
        }
        if modes.contains(&DeviceMode::MaskRom) {
            return Some(DeviceMode::MaskRom);
        }
        if modes.contains(&DeviceMode::LinuxGadget) {
            return Some(DeviceMode::LinuxGadget);
        }
        modes.first().cloned()
    }

    /// Short human-readable mode name for logs.
    pub fn label(&self) -> String {
        match self {
            DeviceMode::Loader => "u-boot loader (330d)".into(),
            DeviceMode::MaskRom => "MaskROM (330e)".into(),
            DeviceMode::UsbPlug => "usbplug (330e, USB-MSC)".into(),
            DeviceMode::LinuxGadget => "Linux gadget (0006, not a download mode)".into(),
            DeviceMode::Other(p) => format!("other ({p:04x})"),
        }
    }
}

/// Open the Rockchip device in the given mode (claim interface 0).
/// Errors if the device is not present.
pub fn open_mode(mode: &DeviceMode) -> Result<NusbTransport> {
    Ok(NusbTransport::open_first(crate::USB_VID_ROCKCHIP, mode.pid(), |d| match mode {
        // usbplug and MaskROM share the PID — told apart by iProduct
        DeviceMode::UsbPlug => d.product_string() == Some("USB-MSC"),
        DeviceMode::MaskRom => d.product_string() != Some("USB-MSC"),
        _ => true,
    })?)
}

/// All Rockchip modes currently on the bus (deduplicated).
pub fn list_modes() -> Result<Vec<DeviceMode>> {
    let mut modes: Vec<DeviceMode> = Vec::new();
    for d in nusb::list_devices().wait()? {
        if d.vendor_id() != crate::USB_VID_ROCKCHIP {
            continue;
        }
        let m = match d.product_id() {
            0x330d => DeviceMode::Loader,
            0x330e if d.product_string() == Some("USB-MSC") => DeviceMode::UsbPlug,
            0x330e => DeviceMode::MaskRom,
            0x0006 => DeviceMode::LinuxGadget,
            p => DeviceMode::Other(p),
        };
        if !modes.contains(&m) {
            modes.push(m);
        }
    }
    Ok(modes)
}

/// The best mode present right now (see [`DeviceMode::best`]).
pub fn current_mode() -> Result<Option<DeviceMode>> {
    Ok(DeviceMode::best(&list_modes()?))
}
