//! A toy module: one interface it serves and one host port it calls back.
use anyhow::{bail, Result};
use std::time::Duration;
use webagent_module_service::module_interface;

#[module_interface(version = 1)]
pub trait Toy {
    async fn add(&self, a: u64, b: u64) -> anyhow::Result<u64>;
    /// Greets `name` with the host's label, calling the host back.
    async fn greet(&self, name: &str) -> anyhow::Result<String>;
    async fn echo(&self, text: String) -> anyhow::Result<String>;
    async fn sleep(&self, millis: u64) -> anyhow::Result<()>;
    async fn fail(&self, reason: &str) -> anyhow::Result<()>;
    /// Ends the service process (crash test); linked builds refuse.
    async fn exit(&self) -> anyhow::Result<()>;
    async fn generation(&self) -> anyhow::Result<u32>;
    /// Opens a file and returns its length; a service process is denied.
    async fn open(&self, path: &str) -> anyhow::Result<u64>;
}

/// What the host provides to the toy module.
#[module_interface(version = 1)]
pub trait ToyHost {
    async fn label(&self) -> anyhow::Result<String>;
}

pub struct Implementation<H> {
    pub host: H,
    /// Whether this runs in its own process, where `exit` may end it.
    pub own_process: bool,
    pub generation: u32,
}

impl<H: ToyHost> Toy for Implementation<H> {
    async fn add(&self, a: u64, b: u64) -> Result<u64> {
        a.checked_add(b)
            .ok_or_else(|| anyhow::anyhow!("toy overflow"))
    }

    async fn greet(&self, name: &str) -> Result<String> {
        Ok(format!("{}, {name}", self.host.label().await?))
    }

    async fn echo(&self, text: String) -> Result<String> {
        Ok(text)
    }

    async fn sleep(&self, millis: u64) -> Result<()> {
        tokio::time::sleep(Duration::from_millis(millis)).await;
        Ok(())
    }

    async fn fail(&self, reason: &str) -> Result<()> {
        bail!("toy refused: {reason}")
    }

    async fn exit(&self) -> Result<()> {
        if !self.own_process {
            bail!("toy exit is service-only");
        }
        std::process::exit(3)
    }

    async fn generation(&self) -> Result<u32> {
        Ok(self.generation)
    }

    async fn open(&self, path: &str) -> Result<u64> {
        Ok(std::fs::File::open(path)?.metadata()?.len())
    }
}
