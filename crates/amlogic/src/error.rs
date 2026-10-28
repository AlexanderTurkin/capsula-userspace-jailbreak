//! amlogic error types.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    /// Device not found / has disappeared (e.g. the u-boot download
    /// window closed before identify).
    #[error("amlogic device not found")]
    DeviceNotFound,
    /// USB transport error.
    #[error("usb: {0}")]
    Usb(String),
    /// A bulk command (AM_REQ_BULKCMD / TPL command) was answered with an
    /// error text instead of `success`.
    #[error("device rejected the command: {0}")]
    CommandFailed(String),
    /// A media transfer (download/upload) was rejected: the reply was
    /// neither `OK!!` nor `Continue:`.
    #[error("media transfer failed: {0}")]
    MediaFailed(String),
    /// A command string is too long for the 128-byte EP0 buffer.
    #[error("command too long (max 63 chars): {0}")]
    CommandTooLong(String),
    /// I/O error.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

impl From<usbcore::Error> for Error {
    fn from(e: usbcore::Error) -> Self {
        match e {
            usbcore::Error::DeviceNotFound => Error::DeviceNotFound,
            other => Error::Usb(other.to_string()),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
