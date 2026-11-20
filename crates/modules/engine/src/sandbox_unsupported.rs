//! The engine-component sandbox needs Linux (Landlock and seccomp); other platforms
//! refuse to start components instead of running them unconfined.
use std::{io, path::Path};

pub struct PreparedSandbox;

pub fn prepare(_program: &Path) -> io::Result<PreparedSandbox> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "engine sandbox requires Linux x86-64",
    ))
}

impl PreparedSandbox {
    /// # Safety
    /// Matches the Linux signature; never reached because `prepare` always fails.
    pub unsafe fn apply(&self) -> io::Result<()> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}
