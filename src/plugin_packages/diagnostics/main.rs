//! Stable root binary entry; Diagnostics owns the whole production implementation.
#[tokio::main]
async fn main()->anyhow::Result<()> {webagent_diagnostics::server::run().await}
