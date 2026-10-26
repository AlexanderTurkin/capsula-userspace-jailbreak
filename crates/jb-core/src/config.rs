//! Tool configuration (resources/config.toml, embedded into the binary).
//!
//! One configuration for all supported boards: the bring-up loader comes
//! from the Capsule-14 OTA and fits every board, and the RAM-patch
//! constants belong to that loader build.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub ota: OtaConfig,
    /// usbplug RAM-patch constants, bound to the loader build from the OTA.
    pub usbplug: UsbplugPatch,
    /// Loader-mode instructions, shown while the tool waits for the device.
    pub hint: Hint,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OtaConfig {
    /// Platform code for the VK API — the loader comes from its OTA.
    pub platform: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UsbplugPatch {
    /// Address of the u32 LE read limit (in sectors) inside the running
    /// usbplug; the stock value is 0x10000, everything above reads as 0xCC.
    pub read_limit_addr: u32,
    pub signature1_addr: u32,
    pub signature1_hex: String,
    pub signature2_addr: u32,
    pub signature2_hex: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Hint {
    /// How to bring each supported board into a download mode; shown
    /// once while the tool waits for the device.
    pub text: String,
    /// Extra note for the Amlogic boards (the late minis).
    #[serde(default)]
    pub amlogic_text: String,
}

/// Path of the source TOML in the repository (for the sync test).
pub const SOURCE_PATH: &str = "resources/config.toml";
const EMBEDDED: &str = include_str!("../../../resources/config.toml");

impl Config {
    pub fn embedded() -> Config {
        toml::from_str(EMBEDDED).expect("resources/config.toml must parse")
    }

    /// The embedded copy must stay in sync with the file in resources/.
    pub fn embedded_raw() -> &'static str {
        EMBEDDED
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_config_parses_with_expected_constants() {
        let c = Config::embedded();
        assert_eq!(c.ota.platform, "Capsule-14");
        assert_eq!(c.usbplug.read_limit_addr, 0x0334_3EC4);
        assert_eq!(c.usbplug.signature1_addr, 0x0301_2290);
        assert_eq!(c.usbplug.signature1_hex, "29c54eb91f01096b02030054");
        assert_eq!(c.usbplug.signature2_addr, 0x0301_2390);
        assert_eq!(c.usbplug.signature2_hex, "29c54eb91f01096bc2000054");
        assert!(c.hint.text.contains("2207:330d"));
        assert!(c.hint.text.contains("1b8e:c003"));
        assert!(c.hint.amlogic_text.contains("capsula-jb aml"));
    }

    #[test]
    fn embedded_matches_resources_dir() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../resources");
        let from_dir = std::fs::read_to_string(dir.join("config.toml"))
            .unwrap_or_else(|e| panic!("resources/config.toml: {e}"));
        assert_eq!(EMBEDDED, from_dir, "embedded TOML differs from resources/");
    }
}
