//! App saves of live files (`live-files.json`). The app never writes the source tree:
//! the broker replaces the file only while it still has the bytes the caller loaded,
//! and its maintenance loop commits the edit.
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Fixed broker workspace for configuration saves; it never selects a source task.
const WORKSPACE: &str = "5f1c0a3e-7d2b-4c1e-9a6f-3b8d2e4c7a10";
const REPLY_LIMIT: u64 = 64 * 1024;

/// Route this process's live-file writes (`webagent_configuration::live::write`) to the broker.
pub(crate) fn install(socket: PathBuf) {
    webagent_configuration::live::install_writer(move |relative, body, expected| {
        write(&socket, relative, body, expected)
    });
}

/// One bounded `live_file_write` request: a short blocking call made under the caller's lock.
fn write(socket: &Path, relative: &str, body: &[u8], expected: &[u8]) -> Result<()> {
    use sha2::Digest;
    let request = json!({
        "id": uuid::Uuid::new_v4().to_string(),
        "name": "live_file_write",
        "workspace": WORKSPACE,
        "timeout_seconds": 30,
        "trusted": {"user_id": "live-config", "session_id": WORKSPACE},
        "arguments": {
            "path": relative,
            "content": std::str::from_utf8(body)?,
            "expected_sha256": hex::encode(sha2::Sha256::digest(expected)),
        },
    });
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    stream.set_write_timeout(Some(Duration::from_secs(30)))?;
    let mut line = serde_json::to_vec(&request)?;
    line.push(b'\n');
    stream.write_all(&line)?;
    let mut reply = String::new();
    BufReader::new(stream.take(REPLY_LIMIT)).read_line(&mut reply)?;
    let reply: Value =
        serde_json::from_str(&reply).map_err(|_| anyhow!("Malformed broker reply"))?;
    if reply["status"] != "success" {
        bail!(
            "Live file save refused: {}",
            reply["output"].as_str().unwrap_or("broker error")
        );
    }
    Ok(())
}

/// Current bytes of a live file, unvalidated, for digest-fenced saves.
pub(crate) fn current(relative: &str, limit: u64) -> Result<Vec<u8>> {
    webagent_configuration::live::read(relative, limit, |_| Ok(()))
}

/// Replace `relative` with `body` if it still holds `expected` (the bytes the caller read).
pub(crate) async fn save(
    _app: &crate::App,
    relative: &str,
    body: &[u8],
    expected: &[u8],
) -> Result<()> {
    let (relative, body, expected) = (relative.to_owned(), body.to_vec(), expected.to_vec());
    tokio::task::spawn_blocking(move || {
        webagent_configuration::live::write(&relative, &body, &expected)
    })
    .await?
}

/// Serialize a JSON object the way live JSON files are kept: pretty, newline-terminated.
pub(crate) fn pretty(value: &Value) -> Result<Vec<u8>> {
    let mut body = serde_json::to_vec_pretty(value)?;
    body.push(b'\n');
    Ok(body)
}
