//! rockusb error types.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    /// Device not found / has disappeared.
    #[error("rockchip device not found")]
    DeviceNotFound,
    /// USB transport error.
    #[error("usb: {0}")]
    Usb(String),
    /// Device returned an invalid CSW (signature or tag mismatch).
    #[error("invalid device response (CSW)")]
    BadResponse,
    /// Command completed with an error status in the CSW.
    #[error("device rejected the command (status={0:#x})")]
    CommandFailed(u8),
    /// The loader file is not a valid BOOT/LDR container.
    #[error("loader: {0}")]
    Loader(String),
    /// The device is in the wrong mode for this operation.
    #[error("wrong device mode: {0}")]
    WrongMode(String),
    /// I/O error (loader files etc.).
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

impl From<nusb::Error> for Error {
    fn from(e: nusb::Error) -> Self {
        Error::Usb(e.to_string())
    }
}

impl From<nusb::transfer::TransferError> for Error {
    fn from(e: nusb::transfer::TransferError) -> Self {
        Error::Usb(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;
