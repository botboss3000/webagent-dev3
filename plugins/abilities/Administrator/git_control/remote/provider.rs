//! Only fixed provider origins receive private credentials. No repository code executes here.
use super::model::*;
use crate::*;
use std::future::Future;
#[derive(Clone, Debug, PartialEq)]
pub enum Provider {
    Github,
    Cloudflare,
}
#[derive(Clone, Debug)]
pub struct ProviderRequest {
    pub provider: Provider,
    pub method: &'static str,
    pub path: String,
    pub query: Vec<(String, String)>,
    pub body: Value,
    pub multipart: bool,
}
impl ProviderRequest {
    pub fn gh(method: &'static str, path: String, body: Value) -> Self {
        Self {
            provider: Provider::Github,
            method,
            path,
            query: vec![],
            body,
            multipart: false,
        }
    }
    pub fn cf(method: &'static str, path: String, body: Value) -> Self {
        Self {
            provider: Provider::Cloudflare,
            method,
            path,
            query: vec![],
            body,
            multipart: false,
        }
    }
}
#[derive(Clone, Debug)]
pub struct Reply {
    pub status: u16,
    pub value: Value,
}
pub trait Transport: Sync {
    fn credential_digest(&self) -> Option<String> {
        None
    }
    fn request(&self, request: ProviderRequest) -> impl Future<Output = Result<Reply>> + Send;
    fn reachable(&self, url: &str) -> impl Future<Output = Result<bool>> + Send;
    fn healthy(&self, url: &str) -> impl Future<Output = Result<bool>> + Send;
}
pub struct Http {
    pub secret: Credentials,
    pub cancel: CancellationToken,
    client: reqwest::Client,
}
impl Http {
    pub fn new(secret: Credentials, cancel: CancellationToken) -> Result<Self> {
        anyhow::ensure!(
            !secret.github.is_empty() && !secret.cloudflare.is_empty(),
            "Both provider credentials must be configured"
        );
        Ok(Self {
            secret,
            cancel,
            client: reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(25))
                .build()?,
        })
    }
}
pub fn scrub(value: &mut Value, secret: &Credentials) {
    match value {
        Value::String(s) => {
            for token in [&secret.github, &secret.cloudflare] {
                if !token.is_empty() {
                    *s = s.replace(token, "[redacted]");
                }
            }
        }
        Value::Array(a) => a.iter_mut().for_each(|v| scrub(v, secret)),
        Value::Object(o) => o.values_mut().for_each(|v| scrub(v, secret)),
        _ => {}
    }
}
pub fn preview_url(raw: &str) -> bool {
    reqwest::Url::parse(raw).is_ok_and(|u| {
        u.scheme() == "https"
            && u.port_or_known_default() == Some(443)
            && u.username().is_empty()
            && u.password().is_none()
            && u.host_str().is_some_and(|h| h.ends_with(".workers.dev"))
            && u.fragment().is_none()
    })
}
pub fn collect_urls(value: &Value, urls: &mut std::collections::BTreeSet<String>) -> Result<()> {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                if matches!(
                    key.as_str(),
                    "url"
                        | "preview_url"
                        | "preview_urls"
                        | "deployment_url"
                        | "deployment_urls"
                        | "urls"
                ) {
                    match value {
                        Value::String(url) => {
                            anyhow::ensure!(
                                preview_url(url),
                                "Unexpected preview URL in provider response"
                            );
                            urls.insert(url.clone());
                        }
                        Value::Array(items) => {
                            for url in items.iter().filter_map(Value::as_str) {
                                anyhow::ensure!(
                                    preview_url(url),
                                    "Unexpected preview URL in provider response"
                                );
                                urls.insert(url.into());
                            }
                        }
                        _ => {}
                    }
                } else {
                    collect_urls(value, urls)?;
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_urls(item, urls)?;
            }
        }
        _ => {}
    }
    anyhow::ensure!(urls.len() <= 256, "Preview URL inventory exceeds bound");
    Ok(())
}
impl Transport for Http {
    fn credential_digest(&self) -> Option<String> {
        Some(digest(
            &serde_json::to_value(&self.secret).expect("credential serialization"),
        ))
    }
    async fn request(&self, request: ProviderRequest) -> Result<Reply> {
        anyhow::ensure!(!self.cancel.is_cancelled(), "Provider dispatch cancelled");
        let (base, token) = match request.provider {
            Provider::Github => ("https://api.github.com", &self.secret.github),
            Provider::Cloudflare => ("https://api.cloudflare.com", &self.secret.cloudflare),
        };
        anyhow::ensure!(
            request.path.starts_with('/')
                && !request.path.starts_with("//")
                && !request.path.contains(['?', '#']),
            "Invalid provider path"
        );
        let mut url = reqwest::Url::parse(base)?;
        url.set_path(&request.path);
        let mut req = self
            .client
            .request(reqwest::Method::from_bytes(request.method.as_bytes())?, url)
            .query(&request.query)
            .bearer_auth(token)
            .header("user-agent", "WebAgent-Source-Control");
        if request.provider == Provider::Github {
            req = req
                .header("accept", "application/vnd.github+json")
                .header("X-GitHub-Api-Version", "2022-11-28");
        }
        if request.multipart {
            // The only multipart upload is a fixed unpublished placeholder, never user source.
            let boundary = format!("webagent{}", uuid::Uuid::new_v4().simple());
            let metadata = json!({"main_module":"index.js","compatibility_date":"2026-10-01"});
            let content=format!("--{boundary}\r\nContent-Disposition: form-data; name=\"metadata\"\r\nContent-Type: application/json\r\n\r\n{metadata}\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"index.js\"; filename=\"index.js\"\r\nContent-Type: application/javascript+module\r\n\r\nexport default {{fetch(){{return new Response('Site not published',{{status:503}})}}}};\r\n--{boundary}--\r\n");
            req = req
                .header(
                    "content-type",
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .body(content);
        } else if request.method != "GET" && request.method != "DELETE" {
            req = req.json(&request.body);
        }
        let mut response = tokio::select! {_=self.cancel.cancelled()=>return Err(anyhow!("Provider call interrupted; reconcile its receipt")),result=req.send()=>result.map_err(|_|anyhow!("Provider transport interrupted; effects may be uncertain"))?};
        let status = response.status().as_u16();
        let mut bytes = Vec::new();
        loop {
            let chunk = tokio::select! {_=self.cancel.cancelled()=>return Err(anyhow!("Provider result interrupted; reconcile its receipt")),result=response.chunk()=>result.map_err(|_|anyhow!("Provider result interrupted; effects may be uncertain"))?};
            match chunk {
                Some(chunk) => {
                    anyhow::ensure!(
                        bytes.len() + chunk.len() <= 2 * 1024 * 1024,
                        "Provider response exceeds bound; reconcile its receipt"
                    );
                    bytes.extend_from_slice(&chunk);
                }
                None => break,
            }
        }
        let mut value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)
                .map_err(|_| anyhow!("Provider response was not JSON; reconcile its receipt"))?
        };
        scrub(&mut value, &self.secret);
        Ok(Reply { status, value })
    }
    async fn healthy(&self, url: &str) -> Result<bool> {
        anyhow::ensure!(preview_url(url), "Unexpected site hostname");
        let response = tokio::select! {_=self.cancel.cancelled()=>return Err(anyhow!("Site verification interrupted")),response=self.client.get(url).header("cache-control","no-cache").send()=>response.map_err(|_|anyhow!("Site verification unavailable"))?};
        Ok(response.status().is_success())
    }
    async fn reachable(&self, url: &str) -> Result<bool> {
        anyhow::ensure!(
            preview_url(url),
            "Unexpected preview hostname; cleanup cannot be verified"
        );
        let mut url = reqwest::Url::parse(url)?;
        url.query_pairs_mut()
            .append_pair("webagent_cleanup_check", &uuid());
        let response = tokio::select! {_=self.cancel.cancelled()=>return Err(anyhow!("Preview verification interrupted")),response=self.client.get(url).header("cache-control","no-cache").send()=>response.map_err(|_|anyhow!("Preview reachability unavailable"))?};
        // Only the provider's known missing-preview marker proves that draft code stopped serving.
        Ok(!(response.status() == reqwest::StatusCode::NOT_FOUND
            && response
                .headers()
                .get("x-preview-user-error")
                .is_some_and(|v| v == "true")))
    }
}
pub fn success(reply: Reply) -> Result<Value> {
    anyhow::ensure!(
        (200..300).contains(&reply.status),
        "Provider rejected operation (HTTP {}); inspect status before retry",
        reply.status
    );
    anyhow::ensure!(
        reply.value.get("success").is_none_or(|v| v == true),
        "Cloudflare rejected operation"
    );
    Ok(reply.value.get("result").cloned().unwrap_or(reply.value))
}
