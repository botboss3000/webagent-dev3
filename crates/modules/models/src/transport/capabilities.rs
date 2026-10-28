//! Public model metadata catalogs for discovery enrichment. Normalization runs in
//! the model-discovery module (`crate::discovery`); this only fetches.
use serde_json::Value;

// Fixed public endpoints carry no provider credentials. Failures degrade to live metadata.
#[cfg(not(any(test,feature="test-support")))]
pub(super) async fn metadata(client: &reqwest::Client) -> (Value, Value) {
    use futures_util::StreamExt;
    use std::{sync::OnceLock, time::{Duration, Instant}};
    static CACHE: OnceLock<tokio::sync::Mutex<Option<(Instant, Value, Value)>>> = OnceLock::new();
    let mut cache = CACHE.get_or_init(||tokio::sync::Mutex::new(None)).lock().await;
    if let Some((at,a,b)) = &*cache { if at.elapsed() < Duration::from_secs(3600) { return (a.clone(),b.clone()); } }
    async fn fetch(client: &reqwest::Client, url: &str) -> Option<Value> {
        let read = async {
            let response = client.get(url).send().await.ok()?.error_for_status().ok()?;
            let mut stream = response.bytes_stream(); let mut bytes = Vec::new();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.ok()?;
                if bytes.len() + chunk.len() > 16 * 1024 * 1024 { return None; }
                bytes.extend_from_slice(&chunk);
            }
            serde_json::from_slice(&bytes).ok()
        };
        tokio::time::timeout(Duration::from_secs(8), read).await.ok().flatten()
    }
    let (a,b) = tokio::join!(fetch(client,"https://models.dev/api.json"),fetch(client,"https://openrouter.ai/api/v1/models"));
    let old = cache.as_ref();
    let a = a.unwrap_or_else(||old.map(|(_,a,_)|a.clone()).unwrap_or(Value::Null));
    let b = b.unwrap_or_else(||old.map(|(_,_,b)|b.clone()).unwrap_or(Value::Null));
    *cache = Some((Instant::now(),a.clone(),b.clone())); (a,b)
}
#[cfg(any(test,feature="test-support"))]
pub(super) async fn metadata(_: &reqwest::Client) -> (Value, Value) { (Value::Null,Value::Null) }
