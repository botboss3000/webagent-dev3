//! App saves of live files (`live-files.json`). The app never writes the source tree:
//! the broker replaces the file only while it still has the bytes the caller loaded,
//! and its maintenance loop commits the edit.
use crate::*;
use anyhow::bail;

/// Fixed broker workspace for configuration saves; it never selects a source task.
const WORKSPACE: &str = "5f1c0a3e-7d2b-4c1e-9a6f-3b8d2e4c7a10";

/// Current bytes of a live file, unvalidated, for digest-fenced saves.
pub(crate) fn current(relative: &str, limit: u64) -> Result<Vec<u8>> {
    webagent_configuration::live::read(relative, limit, |_| Ok(()))
}

/// Replace `relative` with `body` if it still holds `expected` (the bytes the caller read).
pub(crate) async fn save(app: &App, relative: &str, body: &[u8], expected: &[u8]) -> Result<()> {
    use sha2::Digest;
    let args = json!({"path": relative, "content": std::str::from_utf8(body)?,
        "expected_sha256": hex::encode(sha2::Sha256::digest(expected))});
    let trusted = json!({"user_id": app.store.current_user_id(), "session_id": WORKSPACE});
    let reply = tools::execute_host_trusted("live_file_write", &args, WORKSPACE,
        &tools::administrator::build_socket(app), Some(trusted), CancellationToken::new()).await?;
    if reply["status"] != "success" {
        bail!("Live file save refused: {}", reply["output"].as_str().unwrap_or("broker error"));
    }
    Ok(())
}

/// Serialize a JSON object the way live JSON files are kept: pretty, newline-terminated.
pub(crate) fn pretty(value: &Value) -> Result<Vec<u8>> {
    let mut body = serde_json::to_vec_pretty(value)?;
    body.push(b'\n');
    Ok(body)
}
