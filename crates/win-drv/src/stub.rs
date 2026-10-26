//! Non-Windows stub: USB needs no driver binding on macOS/Linux.

use std::path::Path;

pub struct Session;

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

impl Session {
    pub fn new() -> Self {
        Session
    }
    pub fn log_path(&self) -> &Path {
        Path::new("")
    }
    pub fn elevated(&self) -> bool {
        true
    }
    pub fn ensure_supervision(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
}

pub fn helper_main(_done: &Path) -> i32 {
    0
}
