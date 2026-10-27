//! capsula-jailbreak domain: everything independent of the transport
//! and of the UI.
//!
//! Method: userspace overwrite. userdata is not signed and is freely
//! rewritable; no other partition is touched. The configuration lives
//! in resources/config.toml (embedded into the binary).

pub mod aml;
pub mod backup;
pub mod backup_store;
pub mod config;
pub mod flash;
pub mod gpt;
pub mod ota;
pub mod rkfw;
pub mod session;
pub mod target;
pub mod userdata;

/// SoC backend identity. Implemented by the vendor stacks the tool
/// dispatches between (rockchip today, amlogic for the late minis).
pub trait SocBackend {
    /// Human-readable vendor name.
    fn name(&self) -> &'static str;
}
