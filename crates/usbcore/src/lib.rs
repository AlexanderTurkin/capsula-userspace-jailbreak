//! Cross-platform USB transport layer shared by the vendor protocol
//! crates (rockusb, amlogic). Extracted from the original rockusb
//! transport: a mockable `Transport` trait plus the blocking nusb
//! implementation — no libusb, no vendor DLLs; on Windows nusb talks to
//! the built-in WinUSB driver, so a device only needs a WinUSB-class
//! driver assignment (e.g. the vendor driver package or Zadig).

pub mod error;
pub mod mock;
pub mod transport;

pub use error::{Error, Result};
pub use mock::{MockTransport, Step};
pub use transport::{NusbTransport, Transport};
