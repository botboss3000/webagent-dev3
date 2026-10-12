use serde_json::{json, Value};
use std::{os::unix::fs::PermissionsExt, path::PathBuf};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixListener,
};
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let state = PathBuf::from(std::env::var("V3_PLUGIN_STATE")?).join("optimization");
    let generation = std::env::var("V3_PLUGIN_GENERATION")?;
    if generation.len() != 64 || !generation.bytes().all(|b| b.is_ascii_hexdigit()) {
        anyhow::bail!("Invalid generation")
    }
    let socket = state.join("plugin.sock");
    let _ = std::fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket)?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
    loop {
        let (mut stream, _) = listener.accept().await?;
        let g = generation.clone();
        let state = state.clone();
        tokio::spawn(async move {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(2), async {
                if stream.peer_cred()?.uid()
                    != std::fs::metadata(&state)
                        .map(|m| {
                            use std::os::unix::fs::MetadataExt;
                            m.uid()
                        })
                        .unwrap_or(u32::MAX)
                {
                    return Ok::<_, anyhow::Error>(());
                }
                let size = stream.read_u32().await?;
                if size > 65536 {
                    return Ok(());
                }
                let mut bytes = vec![0; size as usize];
                stream.read_exact(&mut bytes).await?;
                let request: Value = serde_json::from_slice(&bytes)?;
                if request != json!({"version":1,"operation":"probe","generation":g}) {
                    return Ok(());
                }
                let reply =
                    serde_json::to_vec(&json!({"version":1,"status":"ready","generation":g}))?;
                stream.write_u32(reply.len() as u32).await?;
                stream.write_all(&reply).await?;
                Ok(())
            })
            .await;
        });
    }
}
