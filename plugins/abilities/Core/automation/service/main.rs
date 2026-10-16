//! Optional Automation package. Host owns encrypted persistence and atomic admission.
//! This executable has no database, credentials, network clients or core linkage.
pub mod contracts;
mod engine;
use std::io::{self, BufRead, Write};
fn main() {
    let input = io::stdin();
    let mut output = io::BufWriter::new(io::stdout());
    let mut reader = input.lock();
    loop {
        let mut bytes = Vec::new();
        let mut too_large = false;
        loop {
            let buffer = match reader.fill_buf() {
                Ok(b) => b,
                Err(_) => return,
            };
            if buffer.is_empty() {
                if bytes.is_empty() {
                    return;
                } else {
                    break;
                }
            }
            let take = buffer
                .iter()
                .position(|b| *b == b'\n')
                .map(|p| p + 1)
                .unwrap_or(buffer.len());
            let ended = buffer[take - 1] == b'\n';
            if bytes.len().saturating_add(take) > 8_388_608 {
                too_large = true;
            }
            if !too_large {
                bytes.extend_from_slice(&buffer[..take]);
            }
            reader.consume(take);
            if ended {
                break;
            }
        }
        let line: io::Result<Vec<u8>> = if too_large {
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "request_too_large",
            ))
        } else {
            Ok(bytes)
        };
        let reply = match line {
            Ok(bytes) if bytes.len() <= 8_388_608 => match serde_json::from_slice(&bytes) {
                Ok(request) => engine::dispatch(request),
                Err(_) => serde_json::json!({"ok":false,"error":"invalid_json"}),
            },
            _ => serde_json::json!({"ok":false,"error":"request_too_large"}),
        };
        if serde_json::to_writer(&mut output, &reply).is_err()
            || output.write_all(b"\n").is_err()
            || output.flush().is_err()
        {
            break;
        }
    }
}
