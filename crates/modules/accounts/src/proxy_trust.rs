//! Trusted front-proxy settings for [`crate::client_ip`]. The proxy addresses are live
//! configuration shipped with the source (`live-files.json`); the hash of the edge's
//! secret proof header is credential material and stays private in the state root.
use anyhow::Result;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use webagent_configuration::live;

/// The live authentication file, relative to the checkout.
pub const LIVE_AUTHENTICATION: &str = "crates/modules/accounts/authentication.json";
/// SHA-256 (hex) of the `x-webagent-client-proof` header, in the state root.
pub const PROOF_HASH: &str = "proxy-proof.sha256";
const LIMIT: u64 = 16 * 1024;

/// The live file where the process saves through the broker; otherwise (tests) the state root.
fn settings_path(root: &Path) -> PathBuf {
    if live::writable() {
        return PathBuf::from(LIVE_AUTHENTICATION);
    }
    root.join("config/authentication.json")
}

/// A settings object; `client_ip` validates the proxy list on use.
pub fn parse(bytes: &[u8]) -> Result<Value> {
    let value: Value = serde_json::from_slice(bytes)?;
    anyhow::ensure!(value.is_object(), "authentication.json must be an object");
    Ok(value)
}

/// `{"authentication": {trusted_proxy_ips, proxy_header_sha256}}` for `client_ip`. A missing
/// proxy file trusts no proxy; a missing proof hash never trusts a forwarded address.
pub fn trust_settings(root: &Path) -> Result<Value> {
    let mut settings = match live::parsed_path(&settings_path(root), LIMIT, parse) {
        Err(error) if live::missing(&error) => json!({}),
        other => other?,
    };
    match std::fs::read_to_string(root.join(PROOF_HASH)) {
        Ok(hash) => settings["proxy_header_sha256"] = json!(hash.trim()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(json!({"authentication": settings}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxies_come_from_config_and_the_proof_hash_from_state() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(
            trust_settings(root.path()).unwrap(),
            json!({"authentication": {}})
        );
        std::fs::create_dir(root.path().join("config")).unwrap();
        std::fs::write(
            root.path().join("config/authentication.json"),
            br#"{"trusted_proxy_ips":["192.0.2.8"]}"#,
        )
        .unwrap();
        std::fs::write(
            root.path().join(PROOF_HASH),
            format!("{}\n", "a".repeat(64)),
        )
        .unwrap();
        let settings = trust_settings(root.path()).unwrap();
        assert_eq!(
            settings["authentication"]["trusted_proxy_ips"],
            json!(["192.0.2.8"])
        );
        assert_eq!(
            settings["authentication"]["proxy_header_sha256"],
            "a".repeat(64)
        );
        std::fs::write(root.path().join("config/authentication.json"), b"[]").unwrap();
        assert!(trust_settings(root.path()).is_err());
    }

    #[test]
    fn shipped_file_parses_and_holds_no_proof() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/authentication.json");
        let value = parse(&std::fs::read(path).unwrap()).unwrap();
        assert!(value.get("proxy_header_sha256").is_none());
        let settings = json!({"authentication": value});
        let peer = "192.0.2.99:1".parse().unwrap();
        crate::client_ip(Some(peer), &axum::http::HeaderMap::new(), &settings).unwrap();
    }
}
