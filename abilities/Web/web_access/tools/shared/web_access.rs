//! Trusted, compiled Web Access adapter. Manifests never select executable code.
//! Fixed public providers only; no worker, credentials, proxy, redirects or retries.
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::{sync::OnceLock, time::Duration};
use tokio_util::sync::CancellationToken;

#[path = "../get_weather/get_weather.rs"]
mod get_weather;
#[path = "../maps_geocode/maps_geocode.rs"]
mod maps_geocode;
/// Argument types of this family's tools, checked against their tool.json by the crate test.
pub const ARGUMENT_TYPES: &[webagent_abilities::tool_arguments::ArgumentType] = &[
    webagent_abilities::argument_type!("get_weather", get_weather::Weather),
    webagent_abilities::argument_type!("maps_geocode", maps_geocode::Maps),
];

const MAX_BODY: usize = 512 * 1024;
const USER_AGENT: &str = "WebAgent-v3/1.0 (+https://webagent.live)";

pub fn owns(name: &str) -> bool {
    matches!(name, "get_weather" | "maps_geocode")
}
/// Tools this adapter executes; their schemas live in the ability's `tools/<tool>/<tool>.tool.json`.
pub const TOOLS: &[&str] = &["get_weather", "maps_geocode"];
pub(super) fn text(s: &str, max: usize) -> Result<()> {
    if s.trim().is_empty() || s.len() > max || s.chars().any(char::is_control) {
        bail!("Invalid text argument");
    }
    Ok(())
}
pub(super) fn coords(lat: Option<f64>, lon: Option<f64>) -> Result<(f64, f64)> {
    let (Some(a), Some(b)) = (lat, lon) else {
        bail!("Coordinates required");
    };
    if !a.is_finite()
        || !b.is_finite()
        || !(-90.0..=90.0).contains(&a)
        || !(-180.0..=180.0).contains(&b)
    {
        bail!("Coordinates out of range");
    }
    Ok((a, b))
}
fn client() -> Result<&'static reqwest::Client> {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    if let Some(c) = CLIENT.get() {
        return Ok(c);
    }
    let c = reqwest::Client::builder()
        .https_only(true)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(15))
        .user_agent(USER_AGENT)
        .build()?;
    let _ = CLIENT.set(c);
    Ok(CLIENT.get().unwrap())
}
async fn body(response: reqwest::Response) -> Result<String> {
    let status = response.status();
    if status != reqwest::StatusCode::OK {
        bail!("Public provider HTTP {}", status.as_u16());
    }
    if response
        .content_length()
        .is_some_and(|n| n > MAX_BODY as u64)
    {
        bail!("Provider response exceeds limit");
    }
    let mut response = response;
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len() + chunk.len() > MAX_BODY {
            bail!("Provider response exceeds limit");
        }
        bytes.extend_from_slice(&chunk);
    }
    String::from_utf8(bytes).context("Invalid provider text")
}
pub(super) async fn get(url: &'static str, params: &[(&str, String)]) -> Result<Value> {
    let response = client()?
        .get(url)
        .query(params)
        .send()
        .await
        .context("Public provider unavailable")?;
    serde_json::from_str(&body(response).await?).context("Malformed provider response")
}
pub async fn execute(name: &str, args: &Value, cancel: CancellationToken) -> Result<Value> {
    if cancel.is_cancelled() {
        bail!("tool execution cancelled");
    }
    let work = async {
        match name {
            "get_weather" => {
                get_weather::weather(
                    serde_json::from_value(args.clone()).context("Invalid weather arguments")?,
                )
                .await
            }
            "maps_geocode" => {
                maps_geocode::maps(
                    serde_json::from_value(args.clone()).context("Invalid geocoding arguments")?,
                )
                .await
            }
            _ => bail!("Unknown web access tool"),
        }
    };
    let output = tokio::select! {
        _=cancel.cancelled()=>bail!("tool execution cancelled"),
        result=tokio::time::timeout(Duration::from_secs(35),work)=>match result {Ok(v)=>v,Err(_)=>Err(anyhow::anyhow!("Public provider timed out"))},
    };
    Ok(match output {
        Ok(value) => json!({"status":"success","output":value}),
        Err(error) => json!({"status":"error","output":error.to_string()}),
    })
}
#[cfg(test)]
#[path = "../../tests/web_access.rs"]
mod tests;
