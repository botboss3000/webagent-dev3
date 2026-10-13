//! Optional MCP package. The host owns credentials, agent grants and approvals.
//! This process accepts only UID-authenticated local frames and public HTTPS MCP.
use anyhow::{anyhow, bail, Result};
use reqwest::{header, Client, Response, Url};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    net::{IpAddr, SocketAddr},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream},
};

const MAX: usize = 1024 * 1024;
const PROTOCOL: &str = "2025-06-18";
const REGISTRY: &str = "https://registry.modelcontextprotocol.io/v0.1/servers";
const DEADLINE: Duration = Duration::from_secs(30);

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    version: u32,
    operation: String,
    endpoint: Option<String>,
    query: Option<String>,
    tool: Option<String>,
    arguments: Option<Value>,
    bearer: Option<String>,
    expected_schema: Option<Value>,
}
fn endpoint(raw: &str) -> Result<Url> {
    if raw.len() > 2048 || raw.chars().any(char::is_control) {
        bail!("Endpoint rejected")
    }
    let url = Url::parse(raw).map_err(|_| anyhow!("Endpoint rejected"))?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        bail!("Endpoint rejected")
    }
    if let Some(ip) = literal_ip(&url) {
        if !public_ip(ip) {
            bail!("Destination rejected")
        }
    }
    Ok(url)
}
fn literal_ip(url: &Url) -> Option<IpAddr> {
    url.host_str()?.trim_matches(['[', ']']).parse().ok()
}
fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            // Fail closed for non-global ranges, including CGNAT/Tailscale and metadata.
            !(a == 0
                || a == 10
                || a == 127
                || a >= 224
                || (a == 100 && (64..=127).contains(&b))
                || (a == 169 && b == 254)
                || (a == 172 && (16..=31).contains(&b))
                || (a == 192 && b == 168)
                || (a == 192 && b == 0)
                || (a == 192 && b == 88 && c == 99)
                || (a == 198 && (b == 18 || b == 19))
                || (a == 198 && b == 51 && c == 100)
                || (a == 203 && b == 0 && c == 113))
        }
        IpAddr::V6(ip) => {
            let s = ip.segments();
            // Only native global unicast; mapped, translation, tunnels, documentation
            // and special-purpose prefixes cannot reach a private destination.
            (s[0] & 0xe000) == 0x2000
                && s[0] != 0x2002
                && !(s[0] == 0x2001
                    && (s[1] == 0
                        || s[1] == 2
                        || s[1] == 0xdb8
                        || (s[1] & 0xfff0) == 0x10
                        || (s[1] & 0xfff0) == 0x20))
                && !(s[0] == 0x3fff && (s[1] & 0xf000) == 0)
        }
    }
}
fn validate(req: &Request) -> Result<()> {
    if req.version != 1 || !matches!(req.operation.as_str(), "search" | "inspect" | "call") {
        bail!("Request rejected")
    }
    if let Some(bearer) = &req.bearer {
        if bearer.is_empty()
            || bearer.len() > 8192
            || !bearer.bytes().all(|b| (0x21..=0x7e).contains(&b))
        {
            bail!("Credential rejected")
        }
    }
    if req
        .query
        .as_ref()
        .is_some_and(|q| q.len() > 200 || q.chars().any(char::is_control))
    {
        bail!("Query rejected")
    }
    if req.operation != "search" {
        endpoint(
            req.endpoint
                .as_deref()
                .ok_or_else(|| anyhow!("Endpoint required"))?,
        )?;
    }
    if req.operation == "call" {
        let tool = req
            .tool
            .as_deref()
            .ok_or_else(|| anyhow!("Tool required"))?;
        if tool.is_empty()
            || tool.len() > 128
            || tool.chars().any(char::is_control)
            || !req.arguments.as_ref().is_some_and(Value::is_object)
            || !req.expected_schema.as_ref().is_some_and(Value::is_object)
        {
            bail!("Call rejected")
        }
    }
    Ok(())
}
async fn pinned_client(url: &Url) -> Result<Client> {
    let host = url.host_str().ok_or_else(|| anyhow!("Host required"))?;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| anyhow!("Port required"))?;
    let addresses: Vec<SocketAddr> = if let Some(ip) = literal_ip(url) {
        vec![SocketAddr::new(ip, port)]
    } else {
        tokio::net::lookup_host((host, port))
            .await
            .map_err(|_| anyhow!("DNS lookup failed"))?
            .collect()
    };
    if addresses.is_empty() || addresses.len() > 32 || addresses.iter().any(|a| !public_ip(a.ip()))
    {
        bail!("Destination rejected")
    }
    // Disable environment proxies and pin the checked DNS answer for this client.
    Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .timeout(DEADLINE)
        .resolve_to_addrs(host, &addresses)
        .build()
        .map_err(|_| anyhow!("Client rejected"))
}
fn clean(raw: &str, limit: usize) -> String {
    raw.chars()
        .filter(|c| !c.is_control())
        .take(limit)
        .collect()
}
fn sanitize_tool(tool: &Value) -> Result<Value> {
    let name = tool["name"]
        .as_str()
        .ok_or_else(|| anyhow!("Invalid tool"))?;
    if name.is_empty()
        || name.len() > 128
        || name.chars().any(char::is_control)
        || !tool["inputSchema"].is_object()
        || serde_json::to_vec(&tool["inputSchema"])?.len() > 64 * 1024
    {
        bail!("Invalid tool")
    }
    Ok(
        json!({"name":name,"description":clean(tool["description"].as_str().unwrap_or(""),4096),
        "inputSchema":tool["inputSchema"]}),
    )
}
fn scrub(value: &mut Value, bearer: Option<&str>) {
    match value {
        Value::String(s) => {
            if let Some(secret) = bearer {
                *s = s.replace(secret, "[redacted]");
            }
        }
        Value::Array(items) => {
            for item in items {
                scrub(item, bearer);
            }
        }
        Value::Object(map) => {
            // Server output is untrusted, including keys that reflect submitted credentials.
            let old = std::mem::take(map);
            for (mut key, mut item) in old {
                if let Some(secret) = bearer {
                    key = key.replace(secret, "[redacted]");
                }
                scrub(&mut item, bearer);
                map.insert(key, item);
            }
        }
        _ => {}
    }
}
fn envelope(status: &str, output: Value) -> Value {
    json!({"version":1,"status":status,"output":output})
}
fn failure(operation: &str, uncertain: bool) -> Value {
    envelope(
        "error",
        json!({"code":"mcp_operation_failed",
        "message":if operation == "call" && uncertain {
            "MCP call outcome is uncertain; reconcile before retrying an external write."
        } else { "MCP operation failed." }, "uncertain":uncertain}),
    )
}
async fn bounded_body(mut response: Response) -> Result<Vec<u8>> {
    if response.content_length().is_some_and(|n| n > MAX as u64) {
        bail!("Response too large")
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| anyhow!("Read failed"))? {
        if body.len().saturating_add(chunk.len()) > MAX {
            bail!("Response too large")
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}
#[derive(Debug)]
struct KnownRpcFailure;
impl std::fmt::Display for KnownRpcFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Remote RPC rejected the operation")
    }
}
impl std::error::Error for KnownRpcFailure {}
fn rpc_result(value: &Value, id: u64) -> Result<Option<Value>> {
    if value["jsonrpc"] != "2.0" {
        bail!("Invalid RPC")
    }
    if value["id"].as_u64() != Some(id) {
        if value.get("id").is_some() {
            bail!("Unexpected RPC")
        }
        return Ok(None);
    }
    if let Some(error) = value.get("error") {
        if value.get("result").is_some()
            || error["code"].as_i64().is_none()
            || error["message"].as_str().is_none()
        {
            bail!("Invalid RPC error")
        }
        return Err(KnownRpcFailure.into());
    }
    Ok(Some(
        value
            .get("result")
            .ok_or_else(|| anyhow!("Missing result"))?
            .clone(),
    ))
}
fn sse_result(body: &[u8], id: u64) -> Result<Option<Value>> {
    let text = match std::str::from_utf8(body) {
        Ok(t) => t,
        Err(e) if e.error_len().is_none() => return Ok(None),
        Err(_) => bail!("Invalid SSE"),
    };
    let normalized = text.replace("\r\n", "\n");
    for event in normalized
        .split("\n\n")
        .take(normalized.matches("\n\n").count())
    {
        let data = event
            .lines()
            .filter_map(|line| line.strip_prefix("data:"))
            .map(|s| s.strip_prefix(' ').unwrap_or(s))
            .collect::<Vec<_>>()
            .join("\n");
        if data.is_empty() {
            continue;
        }
        let message: Value = serde_json::from_str(&data).map_err(|_| anyhow!("Invalid SSE"))?;
        if let Some(result) = rpc_result(&message, id)? {
            return Ok(Some(result));
        }
    }
    Ok(None)
}
async fn response_rpc(mut response: Response, id: u64) -> Result<Value> {
    if !response.status().is_success() {
        bail!("HTTP operation failed")
    }
    let mime = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_owned();
    if mime == "application/json" {
        let value: Value = serde_json::from_slice(&bounded_body(response).await?)
            .map_err(|_| anyhow!("Invalid JSON"))?;
        return rpc_result(&value, id)?.ok_or_else(|| anyhow!("Missing RPC"));
    }
    if mime != "text/event-stream" {
        bail!("Unsupported response")
    }
    if response.content_length().is_some_and(|n| n > MAX as u64) {
        bail!("Response too large")
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| anyhow!("Read failed"))? {
        if body.len().saturating_add(chunk.len()) > MAX {
            bail!("Response too large")
        }
        body.extend_from_slice(&chunk);
        // Return on the response event; servers may keep an SSE connection open.
        if let Some(result) = sse_result(&body, id)? {
            return Ok(result);
        }
    }
    bail!("Missing SSE response")
}
struct Connection {
    client: Client,
    url: Url,
    bearer: Option<String>,
    session: Option<String>,
}
impl Connection {
    fn request(&self, method: reqwest::Method) -> reqwest::RequestBuilder {
        let mut req = self
            .client
            .request(method, self.url.clone())
            .header(header::ACCEPT, "application/json, text/event-stream")
            .header("MCP-Protocol-Version", PROTOCOL);
        if let Some(bearer) = &self.bearer {
            req = req.bearer_auth(bearer);
        }
        if let Some(session) = &self.session {
            req = req.header("Mcp-Session-Id", session);
        }
        req
    }
    async fn rpc(&self, id: u64, method: &str, params: Value) -> Result<Value> {
        let response = self
            .request(reqwest::Method::POST)
            .json(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
            .send()
            .await
            .map_err(|_| anyhow!("RPC transport failed"))?;
        response_rpc(response, id).await
    }
    async fn initialize(&mut self) -> Result<()> {
        let response = self
            .request(reqwest::Method::POST)
            .json(&json!({
            "jsonrpc":"2.0","id":1,"method":"initialize","params":{
                "protocolVersion":PROTOCOL,"capabilities":{},
                "clientInfo":{"name":"webagent-mcp","version":"1.0.0"}}}))
            .send()
            .await
            .map_err(|_| anyhow!("Initialize failed"))?;
        if let Some(raw) = response.headers().get("Mcp-Session-Id") {
            let s = raw.to_str().map_err(|_| anyhow!("Invalid session"))?;
            if s.is_empty() || s.len() > 256 || !s.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
                bail!("Invalid session")
            }
            self.session = Some(s.to_owned());
        }
        let result = response_rpc(response, 1).await?;
        if result["protocolVersion"] != PROTOCOL || !result["capabilities"]["tools"].is_object() {
            bail!("Unsupported protocol or capabilities")
        }
        let response = self
            .request(reqwest::Method::POST)
            .json(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
            .send()
            .await
            .map_err(|_| anyhow!("Initialize failed"))?;
        if response.status() != reqwest::StatusCode::ACCEPTED {
            bail!("Notification rejected")
        }
        Ok(())
    }
    async fn tools(&self) -> Result<Vec<Value>> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        for page in 0..8 {
            let params = cursor
                .as_ref()
                .map(|c| json!({"cursor":c}))
                .unwrap_or(json!({}));
            let result = self.rpc(2 + page, "tools/list", params).await?;
            for tool in result["tools"]
                .as_array()
                .ok_or_else(|| anyhow!("Tools missing"))?
            {
                let mut tool = sanitize_tool(tool)?;
                scrub(&mut tool, self.bearer.as_deref());
                if tools.iter().any(|t: &Value| t["name"] == tool["name"]) {
                    bail!("Duplicate tool")
                }
                tools.push(tool);
                if tools.len() > 128 || serde_json::to_vec(&tools)?.len() > MAX / 2 {
                    bail!("Tool catalog too large")
                }
            }
            if result
                .get("nextCursor")
                .is_some_and(|v| !v.is_null() && !v.is_string())
            {
                bail!("Invalid cursor")
            }
            let next = result
                .get("nextCursor")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty());
            match next {
                None => return Ok(tools),
                Some(n) if n.len() <= 2048 && cursor.as_deref() != Some(n) => {
                    cursor = Some(n.to_owned())
                }
                _ => bail!("Invalid cursor"),
            }
        }
        bail!("Tool catalog truncated")
    }
    async fn call_checked(
        &self,
        name: &str,
        arguments: &Value,
        expected: &Value,
        dispatched: &mut bool,
    ) -> Result<Value> {
        let tools = self.tools().await?;
        let tool = tools
            .iter()
            .find(|t| t["name"] == name)
            .ok_or_else(|| anyhow!("Tool not available"))?;
        if expected != tool {
            bail!("Tool schema changed")
        }
        // This is the external-effect boundary. No automatic retries after dispatch.
        *dispatched = true;
        let mut output = match self
            .rpc(20, "tools/call", json!({"name":name,"arguments":arguments}))
            .await
        {
            Ok(output) => output,
            Err(error) => {
                return Err(error);
            }
        };
        if !output["content"].is_array() {
            bail!("Invalid tool result")
        }
        scrub(&mut output, self.bearer.as_deref());
        Ok(output)
    }
    async fn close(&self) {
        if self.session.is_some() {
            let _ = tokio::time::timeout(
                Duration::from_secs(1),
                self.request(reqwest::Method::DELETE).send(),
            )
            .await;
        }
    }
}
fn catalog_entries(entries: &Value, query: &str, source: &str) -> Vec<Value> {
    let mut found = Vec::new();
    let empty = Vec::new();
    for item in entries.as_array().unwrap_or(&empty).iter().take(100) {
        let item = item.get("server").unwrap_or(item);
        let name = clean(item["name"].as_str().unwrap_or(""), 200);
        let description = clean(item["description"].as_str().unwrap_or(""), 2000);
        if name.is_empty()
            || !(name.to_lowercase().contains(query) || description.to_lowercase().contains(query))
        {
            continue;
        }
        for remote in item["remotes"].as_array().unwrap_or(&empty).iter().take(8) {
            if remote["type"] != "streamable-http" {
                continue;
            }
            let Some(raw) = remote["url"].as_str() else {
                continue;
            };
            let Ok(url) = endpoint(raw) else {
                continue;
            };
            found.push(
                json!({"name":name,"description":description,"endpoint":url.as_str(),
                "transport":"streamable-http","source":source}),
            );
        }
    }
    found
}
async fn search(root: &Path, query: &str) -> Result<Value> {
    let query = query.trim().to_lowercase();
    let mut servers = Vec::new();
    let catalog_path = root.join("mcp.json");
    if let Ok(metadata) = tokio::fs::metadata(&catalog_path).await {
        if metadata.len() > MAX as u64 {
            bail!("Catalog rejected")
        }
        let bytes = tokio::fs::read(catalog_path).await?;
        let catalog: Value = serde_json::from_slice(&bytes)?;
        if catalog["version"] != 1 {
            bail!("Catalog rejected")
        }
        servers.extend(catalog_entries(&catalog["servers"], &query, "curated"));
    }
    let mut url = endpoint(REGISTRY)?;
    url.query_pairs_mut()
        .append_pair("limit", "50")
        .append_pair("version", "latest")
        .append_pair("search", &query);
    let fetched: Result<Value> = async {
        let client = pinned_client(&url).await?;
        let response = client
            .get(url)
            .header(header::ACCEPT, "application/json")
            .send()
            .await?;
        if !response.status().is_success() {
            bail!("Registry unavailable")
        }
        Ok(serde_json::from_slice(&bounded_body(response).await?)?)
    }
    .await;
    let registry_available = fetched.is_ok();
    if let Ok(catalog) = fetched {
        servers.extend(catalog_entries(&catalog["servers"], &query, "registry"));
    }
    let mut seen = std::collections::HashSet::new();
    servers.retain(|s| seen.insert(s["endpoint"].as_str().unwrap_or("").to_owned()));
    servers.truncate(100);
    Ok(json!({"servers":servers,"registry_available":registry_available}))
}
async fn operate(root: &Path, req: &Request, dispatched: &mut bool) -> Result<Value> {
    validate(req)?;
    if req.operation == "search" {
        return search(root, req.query.as_deref().unwrap_or("")).await;
    }
    let url = endpoint(req.endpoint.as_deref().unwrap())?;
    let mut connection = Connection {
        client: pinned_client(&url).await?,
        url,
        bearer: req.bearer.clone(),
        session: None,
    };
    let result: Result<Value> = async {
        connection.initialize().await?;
        if req.operation == "inspect" {
            let tools = connection.tools().await?;
            return Ok(json!({"protocol_version":PROTOCOL,"tools":tools}));
        }
        // Tool errors may follow partial external effects; the host must hold recovery.
        connection
            .call_checked(
                req.tool.as_deref().unwrap(),
                req.arguments.as_ref().unwrap(),
                req.expected_schema.as_ref().unwrap(),
                dispatched,
            )
            .await
    }
    .await;
    connection.close().await;
    result
}
async fn until_disconnect<F: std::future::Future>(
    reader: &mut tokio::net::unix::OwnedReadHalf,
    operation: F,
) -> Option<F::Output> {
    tokio::select! {
        biased;
        // The host owns cancellation/revocation. EOF (or a second frame) drops
        // preflight before it can proceed to a new consequential remote request.
        _=reader.read_u8()=>None,
        result=operation=>Some(result),
    }
}
async fn handle(mut stream: UnixStream, root: &Path) -> Result<()> {
    if stream.peer_cred()?.uid() != unsafe { getuid() } {
        bail!("Peer rejected")
    }
    let n = stream.read_u32().await? as usize;
    if n == 0 || n > MAX {
        bail!("Frame rejected")
    }
    let mut bytes = vec![0; n];
    stream.read_exact(&mut bytes).await?;
    let req: Request = match serde_json::from_slice(&bytes) {
        Ok(r) => r,
        Err(_) => {
            let bytes = serde_json::to_vec(&failure("", false))?;
            stream.write_u32(bytes.len() as u32).await?;
            stream.write_all(&bytes).await?;
            return Ok(());
        }
    };
    let (mut reader, mut writer) = stream.into_split();
    let mut dispatched = false;
    let Some(result) = until_disconnect(
        &mut reader,
        tokio::time::timeout(DEADLINE, operate(root, &req, &mut dispatched)),
    )
    .await
    else {
        return Ok(());
    };
    let reply = match result {
        Ok(Ok(mut output)) => {
            scrub(&mut output, req.bearer.as_deref());
            completed(&req.operation, output)
        }
        _ => failure(&req.operation, dispatched),
    };
    let mut bytes = serde_json::to_vec(&reply)?;
    if bytes.len() > MAX {
        bytes = serde_json::to_vec(&failure(&req.operation, dispatched))?;
    }
    writer.write_u32(bytes.len() as u32).await?;
    writer.write_all(&bytes).await?;
    Ok(())
}
fn completed(operation: &str, output: Value) -> Value {
    if operation == "call" && output["isError"] == true {
        failure("call", true)
    } else {
        envelope("success", output)
    }
}
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
unsafe extern "C" {
    fn getuid() -> u32;
}
#[tokio::main]
async fn main() -> Result<()> {
    let root = PathBuf::from(std::env::var("V3_PLUGIN_PACKAGE")?);
    if !root.is_dir() {
        bail!("Package missing")
    }
    let runtime = PathBuf::from(std::env::var("V3_PLUGIN_STATE")?).join("mcp");
    tokio::fs::create_dir_all(&runtime).await?;
    tokio::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700)).await?;
    let socket = runtime.join("plugin.sock");
    if socket.exists() {
        tokio::fs::remove_file(&socket).await?;
    }
    let listener = UnixListener::bind(&socket)?;
    tokio::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).await?;
    tokio::fs::write(runtime.join("plugin.pid"), std::process::id().to_string()).await?;
    let ready = runtime.join("plugin.ready");
    tokio::spawn(async move {
        let mut ticks = tokio::time::interval(Duration::from_secs(2));
        loop {
            ticks.tick().await;
            let temp = ready.with_extension("next");
            if tokio::fs::write(&temp, now().to_string()).await.is_ok() {
                let _ = tokio::fs::rename(&temp, &ready).await;
            }
        }
    });
    let permits = std::sync::Arc::new(tokio::sync::Semaphore::new(4));
    loop {
        let permit = permits.clone().acquire_owned().await?;
        let (stream, _) = listener.accept().await?;
        let root = root.clone();
        tokio::spawn(async move {
            let _permit = permit;
            // Includes frame reads so a local idle client cannot monopolize a slot.
            let _ = tokio::time::timeout(Duration::from_secs(35), handle(stream, &root)).await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn endpoint_rejects_private_and_credential_urls() {
        for url in [
            "http://example.com/mcp",
            "https://user:password@example.invalid/mcp",
            "https://example.com/mcp?token=secret",
            "https://example.com/mcp#secret",
            "https://127.0.0.1/mcp",
            "https://100.112.23.15/mcp",
            "https://169.254.169.254/mcp",
            "https://[::1]/mcp",
            "https://[::ffff:127.0.0.1]/mcp",
            "https://[fc00::1]/mcp",
            "https://[2002:7f00:1::]/mcp",
        ] {
            assert!(endpoint(url).is_err(), "accepted {url}");
        }
        assert!(endpoint("https://example.com/mcp").is_ok());
    }
    #[test]
    fn public_network_boundary_includes_reserved_ipv4_ipv6() {
        for raw in [
            "0.1.2.3",
            "10.0.0.1",
            "172.31.0.1",
            "192.168.0.1",
            "192.0.0.9",
            "192.0.2.1",
            "198.18.0.1",
            "198.51.100.1",
            "203.0.113.1",
            "224.0.0.1",
            "255.255.255.255",
            "::",
            "fe80::1",
            "2001:db8::1",
            "2001::1",
            "2001:20::1",
            "3fff::1",
            "64:ff9b::7f00:1",
        ] {
            assert!(!public_ip(raw.parse().unwrap()), "accepted {raw}");
        }
        assert!(public_ip("8.8.8.8".parse().unwrap()));
        assert!(public_ip("2606:4700:4700::1111".parse().unwrap()));
    }
    #[test]
    fn validation_requires_pinned_schema_and_object_arguments() {
        let base = json!({"version":1,"operation":"call","endpoint":"https://example.com/mcp",
            "tool":"read","arguments":{},"expected_schema":{"name":"read","inputSchema":{}}});
        assert!(validate(&serde_json::from_value(base.clone()).unwrap()).is_ok());
        for (key, value) in [
            ("version", json!(2)),
            ("arguments", json!([])),
            ("expected_schema", Value::Null),
            ("tool", json!("")),
            ("bearer", json!("secret\nheader")),
        ] {
            let mut bad = base.clone();
            bad[key] = value;
            assert!(validate(&serde_json::from_value(bad).unwrap()).is_err());
        }
    }
    #[test]
    fn sse_accepts_chunked_multiline_data_and_ignores_notifications() {
        let body=b"data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/message\"}\r\n\r\ndata: {\"jsonrpc\":\"2.0\",\r\ndata: \"id\":2,\"result\":{\"tools\":[]}}\r\n\r\n";
        assert!(sse_result(&body[..body.len() - 4], 2).unwrap().is_none());
        assert_eq!(sse_result(body, 2).unwrap(), Some(json!({"tools":[]})));
        assert!(rpc_result(
            &json!({"jsonrpc":"2.0","id":2,"error":{"message":"secret"}}),
            2
        )
        .is_err());
        let known = rpc_result(
            &json!({"jsonrpc":"2.0","id":2,
            "error":{"code":-32602,"message":"secret"}}),
            2,
        )
        .unwrap_err();
        assert!(known.is::<KnownRpcFailure>());
        assert!(!known.to_string().contains("secret"));
    }
    #[test]
    fn registry_only_exposes_remote_https_metadata() {
        let servers = json!([{"server":{"name":"demo","description":"Remote\nserver",
            "packages":[{"identifier":"malicious-command"}],"remotes":[
                {"type":"stdio","url":"https://ignored.com"},
                {"type":"streamable-http","url":"https://127.0.0.1"},
                {"type":"streamable-http","url":"https://example.com/mcp"}]}}]);
        let entries = catalog_entries(&servers, "demo", "registry");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["endpoint"], "https://example.com/mcp");
        assert!(entries[0].get("packages").is_none());
    }
    #[test]
    fn sanitize_schema_omits_authority_and_scrubs_secrets() {
        let mut tool = sanitize_tool(&json!({"name":"read","description":"secret",
            "inputSchema":{"type":"object"},"annotations":{"readOnlyHint":true}}))
        .unwrap();
        scrub(&mut tool, Some("secret"));
        assert_eq!(tool["description"], "[redacted]");
        assert!(tool.get("annotations").is_none());
    }
    #[test]
    fn tool_error_holds_uncertain_effects_in_the_supported_envelope() {
        let result = completed("call", json!({"isError":true,"content":[]}));
        assert_eq!(result["status"], "error");
        assert_eq!(result["output"]["uncertain"], true);
        assert_eq!(
            completed("call", json!({"content":[]}))["status"],
            "success"
        );
    }
    #[tokio::test]
    async fn framed_protocol_returns_sanitized_error_and_rejects_oversize() {
        let root = tempfile::tempdir().unwrap();
        let (mut client, server) = UnixStream::pair().unwrap();
        let path = root.path().to_owned();
        let task = tokio::spawn(async move { handle(server, &path).await });
        let request = br#"{"version":9,"operation":"call","bearer":"private-secret"}"#;
        client.write_u32(request.len() as u32).await.unwrap();
        client.write_all(request).await.unwrap();
        let n = client.read_u32().await.unwrap() as usize;
        let mut bytes = vec![0; n];
        client.read_exact(&mut bytes).await.unwrap();
        let reply: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(reply["version"], 1);
        assert_eq!(reply["status"], "error");
        assert_eq!(reply["output"]["uncertain"], false);
        assert!(!String::from_utf8(bytes).unwrap().contains("private-secret"));
        task.await.unwrap().unwrap();
        let (mut client, server) = UnixStream::pair().unwrap();
        let path = root.path().to_owned();
        let task = tokio::spawn(async move { handle(server, &path).await });
        client.write_u32(MAX as u32 + 1).await.unwrap();
        assert!(task.await.unwrap().is_err());
    }

    #[tokio::test]
    async fn streamable_http_fixture_negotiates_session_and_finishes_open_sse() {
        use axum::{extract::State, response::IntoResponse, routing::post, Json, Router};
        use futures_util::stream;
        use std::sync::{Arc, Mutex};
        async fn serve(
            State(seen): State<Arc<Mutex<Vec<String>>>>,
            headers: axum::http::HeaderMap,
            Json(body): Json<Value>,
        ) -> axum::response::Response {
            let method = body["method"].as_str().unwrap().to_owned();
            seen.lock().unwrap().push(method.clone());
            assert_eq!(headers["mcp-protocol-version"], PROTOCOL);
            assert!(headers["accept"]
                .to_str()
                .unwrap()
                .contains("text/event-stream"));
            assert_eq!(headers["authorization"], "Bearer fixture-secret");
            if method == "initialize" {
                assert_eq!(body["params"]["protocolVersion"], PROTOCOL);
                return (
                    axum::http::StatusCode::OK,
                    [
                        ("content-type", "application/json"),
                        ("mcp-session-id", "fixture-session"),
                    ],
                    json!({"jsonrpc":"2.0","id":1,"result":{"protocolVersion":PROTOCOL,
                        "capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}}})
                    .to_string(),
                )
                    .into_response();
            }
            assert_eq!(headers["mcp-session-id"], "fixture-session");
            if method == "notifications/initialized" {
                return axum::http::StatusCode::ACCEPTED.into_response();
            }
            if method == "tools/list" {
                let event = format!(
                    "data: {}\n\n",
                    json!({"jsonrpc":"2.0","id":body["id"],
                    "result":{"tools":[{"name":"read","description":"Read fixture",
                        "inputSchema":{"type":"object"},"annotations":{"readOnlyHint":true}}]}})
                );
                // A valid response followed by an indefinitely open connection.
                let body = axum::body::Body::from_stream(
                    stream::once(async move { Ok::<_, std::io::Error>(event) })
                        .chain(stream::pending()),
                );
                let mut response = axum::response::Response::new(body);
                response.headers_mut().insert(
                    "content-type",
                    axum::http::HeaderValue::from_static("text/event-stream"),
                );
                return response;
            }
            assert_eq!(method, "tools/call");
            assert_eq!(body["params"]["name"], "read");
            (
                axum::http::StatusCode::OK,
                [("content-type", "application/json")],
                json!({"jsonrpc":"2.0","id":body["id"],"result":{"isError":true,
                    "content":[{"type":"text","text":"explicit fixture failure"}]}})
                .to_string(),
            )
                .into_response()
        }
        use futures_util::StreamExt;
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/mcp", post(serve))
            .with_state(seen.clone());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        // Fixture transport injection is confined to tests; endpoint() and
        // pinned_client() never allow this loopback HTTP URL in production.
        let mut connection = Connection {
            client: Client::builder().no_proxy().build().unwrap(),
            url: Url::parse(&format!("http://{address}/mcp")).unwrap(),
            bearer: Some("fixture-secret".into()),
            session: None,
        };
        connection.initialize().await.unwrap();
        let tools = tokio::time::timeout(Duration::from_secs(2), connection.tools())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(tools.len(), 1);
        assert!(tools[0].get("annotations").is_none());
        let mut drifted = tools[0].clone();
        drifted["inputSchema"]["required"] = json!(["new"]);
        let mut dispatched = false;
        assert!(connection
            .call_checked("read", &json!({}), &drifted, &mut dispatched)
            .await
            .is_err());
        assert!(!dispatched);
        let output = connection
            .call_checked("read", &json!({}), &tools[0], &mut dispatched)
            .await
            .unwrap();
        assert!(dispatched);
        assert_eq!(output["isError"], true);
        assert_eq!(
            seen.lock().unwrap().as_slice(),
            [
                "initialize",
                "notifications/initialized",
                "tools/list",
                "tools/list",
                "tools/list",
                "tools/call"
            ]
        );
        server.abort();
    }

    #[tokio::test]
    async fn host_disconnect_drops_preflight_before_external_dispatch() {
        let (client, server) = UnixStream::pair().unwrap();
        let (mut reader, _writer) = server.into_split();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let dispatched = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let writes = dispatched.clone();
        let task = tokio::spawn(async move {
            until_disconnect(&mut reader, async move {
                let _ = entered_tx.send(());
                // Simulates the remote tools/list preflight waiting for a reply.
                tokio::time::sleep(Duration::from_secs(30)).await;
                writes.store(true, std::sync::atomic::Ordering::SeqCst);
            })
            .await
        });
        entered_rx.await.unwrap();
        drop(client);
        assert!(tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .is_none());
        assert!(!dispatched.load(std::sync::atomic::Ordering::SeqCst));
    }
}
