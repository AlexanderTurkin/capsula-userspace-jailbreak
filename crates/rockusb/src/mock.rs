//! Mock transport for scenario tests of the protocols, no hardware.

use crate::error::{Error, Result};
use crate::transport::Transport;

/// One expected scenario step: which control request or bulk transfer
/// should happen and what to answer.
#[derive(Debug, Clone)]
pub enum Step {
    /// Expect control_out(request, value, index) with this data (checked).
    ControlOut(u8, u16, u16, Vec<u8>),
    /// Expect a bulk write to any endpoint with this content.
    BulkOut(Vec<u8>),
    /// Answer a bulk read with these bytes (exhaustion is an error).
    BulkIn(Vec<u8>),
}

#[derive(Default)]
pub struct MockTransport {
    pub steps: Vec<Step>,
    pub pos: usize,
    /// Log of every call, for test assertions.
    pub log: Vec<String>,
}

impl MockTransport {
    pub fn new(steps: Vec<Step>) -> Self {
        MockTransport {
            steps,
            pos: 0,
            log: Vec::new(),
        }
    }

    fn next(&mut self, what: &str) -> Result<Step> {
        if self.pos >= self.steps.len() {
            return Err(Error::Usb(format!("mock: scenario exhausted ({what})")));
        }
        let s = self.steps[self.pos].clone();
        self.pos += 1;
        Ok(s)
    }
}

impl Transport for MockTransport {
    fn control_out(&mut self, request: u8, value: u16, index: u16, data: &[u8]) -> Result<()> {
        match self.next("control_out")? {
            Step::ControlOut(r, v, i, expect) => {
                if r != request || v != value || i != index || expect != data {
                    return Err(Error::Usb(format!(
                        "mock: control_out mismatch\n  expected: {}",
                        hex::encode(&expect),
                    )));
                }
                self.log
                    .push(format!("ctrl {r:#x}/{index:#x} {}b", data.len()));
                Ok(())
            }
            other => Err(Error::Usb(format!(
                "mock: expected {other:?}, got control_out"
            ))),
        }
    }

    fn bulk_write(&mut self, endpoint: u8, data: &[u8]) -> Result<()> {
        match self.next("bulk_write")? {
            Step::BulkOut(expect) => {
                if expect != data {
                    return Err(Error::Usb(format!(
                        "mock: bulk_write mismatch\n  expected: {}",
                        hex::encode(expect),
                    )));
                }
                self.log
                    .push(format!("out ep{endpoint:#x} {}b", data.len()));
                Ok(())
            }
            other => Err(Error::Usb(format!(
                "mock: expected {other:?}, got bulk_write"
            ))),
        }
    }

    fn bulk_read(&mut self, endpoint: u8, buf: &mut [u8]) -> Result<()> {
        let mut filled = 0;
        while filled < buf.len() {
            match self.next("bulk_read")? {
                Step::BulkIn(data) => {
                    if filled + data.len() > buf.len() {
                        return Err(Error::Usb("mock: bulk_read would overflow".into()));
                    }
                    buf[filled..filled + data.len()].copy_from_slice(&data);
                    filled += data.len();
                }
                other => return Err(Error::Usb(format!("mock: expected data, got {other:?}"))),
            }
        }
        self.log.push(format!("in ep{endpoint:#x} {}b", buf.len()));
        Ok(())
    }
}
