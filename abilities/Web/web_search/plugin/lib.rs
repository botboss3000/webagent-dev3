//! Web Search as an ability plugin. It never touches the network or a key: every
//! provider call is a declared `web.provider_request` that the app makes for it as the
//! chatting user, adding that user's vault key and counting the month's use. The same
//! code runs as a standalone process in dev builds (`main.rs`) and linked into stable
//! builds (root feature `stable-abilities`).
use serde_json::{json, Map, Value};
use std::future::Future;

/// Tools this plugin runs.
pub const TOOLS: [&str; 2] = ["web_search", "web_read"];
/// Providers in their default order; each is declared in `plugin.json`.
pub const PROVIDERS: [&str; 4] = ["tavily", "brave", "wikipedia", "duckduckgo"];
/// This ability's manifest: its settings declarations are the one source of defaults.
const MANIFEST: &str = include_str!("../web_search.json");
const DEFAULT_READ_CHARS: u64 = 8_000;
const MAX_SNIPPET: usize = 500;
const WIKIPEDIA: &str = "https://en.wikipedia.org/wiki/";

/// The app, as one plugin call sees it.
pub trait Host {
    /// Make the declared request `name` with `params` and a JSON `body`.
    fn request(
        &self,
        name: &str,
        params: Value,
        body: Value,
    ) -> impl Future<Output = Result<Value, String>> + Send;
}

/// One tool call and the facts the host supplies with it.
pub struct Call<'a> {
    pub tool: &'a str,
    pub arguments: &'a Value,
    /// Stable for this call and its retries.
    pub request_id: &'a str,
    pub now_ms: u64,
}

/// A failed call: one shared error code, what happened, and that code's recovery hint.
#[derive(Debug)]
pub struct Failure {
    pub code: &'static str,
    pub message: String,
}

impl Failure {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// What the agent should do next.
    fn hint(&self) -> &'static str {
        match self.code {
            "invalid_arguments" => "Fix the arguments to match the tool schema, then retry.",
            "credential_missing" => {
                "Ask the user to add the named key with request_credential, or let another enabled provider serve."
            }
            "credential_invalid" => {
                "The saved key was rejected; ask the user to re-enter it with request_credential."
            }
            "quota_exhausted" => {
                "This provider's monthly cap is reached; use another provider or tell the user."
            }
            "rate_limited" => "Wait before retrying this provider, or use another enabled provider.",
            "denied" => {
                "The provider is switched off in this agent's Web Search settings; use an enabled one or ask the user to switch it on."
            }
            "not_found" => {
                "Nothing was found at that address; check the URL or search for the page first."
            }
            "unavailable" => {
                "No enabled provider could serve this; tell the user and do not invent results."
            }
            _ => "Retry once later at most; if it persists, tell the user instead of guessing.",
        }
    }

    /// Whether the next enabled provider may still serve the call.
    fn falls_through(&self) -> bool {
        matches!(
            self.code,
            "credential_missing"
                | "credential_invalid"
                | "quota_exhausted"
                | "rate_limited"
                | "provider_error"
                | "unavailable"
        )
    }

    fn output(&self) -> Value {
        json!({"status": "error", "error_code": self.code, "output": self.message, "hint": self.hint()})
    }
}

fn invalid(message: &str) -> Failure {
    Failure::new("invalid_arguments", message)
}

/// Run one tool. Tool failures are returned as error outputs with a code and a hint.
pub async fn call<H: Host + Sync>(host: &H, call: Call<'_>) -> Result<Value, String> {
    let saved = host
        .request("ability.settings", Value::Null, Value::Null)
        .await?;
    let settings = Settings::resolve(&saved)?;
    let outcome = match call.tool {
        "web_search" => search(host, &settings, call.arguments).await,
        "web_read" => read(host, &settings, call.arguments).await,
        _ => return Err("Unknown Web Search tool".into()),
    };
    Ok(outcome.unwrap_or_else(|failure| failure.output()))
}

/// The agent's settings: saved values over the manifest's declared defaults.
#[derive(Debug)]
pub struct Settings {
    /// Switched-on providers, in the order to try them.
    order: Vec<&'static str>,
    fallback: bool,
    max_results: u64,
    caps: Vec<(&'static str, u64)>,
}

impl Settings {
    /// A saved value of the wrong type is ignored in favor of its default.
    pub fn resolve(saved: &Value) -> Result<Self, String> {
        let manifest: Value =
            serde_json::from_str(MANIFEST).map_err(|_| "Invalid Web Search manifest")?;
        let declared = manifest["config"]["settings"]
            .as_array()
            .ok_or("Web Search settings are not declared")?;
        let value = |key: &str| {
            let default = declared
                .iter()
                .find(|setting| setting["key"] == key)
                .map(|setting| setting["default"].clone())
                .unwrap_or(Value::Null);
            match saved.get(key) {
                Some(value)
                    if std::mem::discriminant(value) == std::mem::discriminant(&default) =>
                {
                    value.clone()
                }
                _ => default,
            }
        };
        let enabled = |provider: &str| value(&format!("{provider}_enabled")) == true;
        let mut order = Vec::new();
        let listed = value("provider_order");
        let names = listed.as_str().unwrap_or_default().split(',');
        // Listed providers first, then any switched-on provider the list left out.
        for name in names
            .map(|name| name.trim().to_ascii_lowercase())
            .chain(PROVIDERS.iter().map(|p| (*p).to_owned()))
        {
            if let Some(provider) = PROVIDERS.iter().copied().find(|p| *p == name) {
                if enabled(provider) && !order.contains(&provider) {
                    order.push(provider);
                }
            }
        }
        let number = |key: &str| value(key).as_u64().unwrap_or_default();
        Ok(Self {
            order,
            fallback: value("fallback") == true,
            max_results: number("max_results").clamp(1, 10),
            caps: vec![
                ("tavily", number("tavily_monthly_cap")),
                ("brave", number("brave_monthly_cap")),
            ],
        })
    }

    fn enabled(&self, provider: &str) -> bool {
        self.order.contains(&provider)
    }

    /// The provider's monthly cap for the host to enforce; 0 means none.
    fn cap(&self, provider: &str) -> u64 {
        self.caps
            .iter()
            .find(|(name, _)| *name == provider)
            .map_or(0, |(_, cap)| *cap)
    }
}

async fn search<H: Host + Sync>(
    host: &H,
    settings: &Settings,
    arguments: &Value,
) -> Result<Value, Failure> {
    let args = object(arguments, &["query", "max_results", "provider"])?;
    let query = text(args, "query", 512)?.ok_or_else(|| invalid("query is required"))?;
    let max = match args.get("max_results") {
        None | Some(Value::Null) => settings.max_results,
        Some(value) => value
            .as_u64()
            .filter(|count| (1..=10).contains(count))
            .ok_or_else(|| invalid("max_results must be 1 to 10"))?,
    };
    let providers = match args.get("provider").and_then(Value::as_str) {
        Some(name) => {
            let provider = PROVIDERS
                .iter()
                .copied()
                .find(|provider| *provider == name)
                .ok_or_else(|| invalid("unknown provider"))?;
            if !settings.enabled(provider) {
                return Err(Failure::new(
                    "denied",
                    format!("{provider} is switched off in this agent's Web Search settings"),
                ));
            }
            vec![provider]
        }
        None => settings.order.clone(),
    };
    if providers.is_empty() {
        return Err(Failure::new(
            "unavailable",
            "No search provider is switched on for this agent",
        ));
    }
    let mut skipped = Vec::new();
    let mut last = None;
    for provider in providers.iter().copied() {
        match search_with(host, settings, provider, &query, max).await {
            Ok(results) => {
                return Ok(json!({"provider": provider, "results": results, "skipped": skipped}))
            }
            Err(failure) => {
                skipped.push(json!({"provider": provider, "error_code": failure.code, "message": failure.message}));
                if !settings.fallback || !failure.falls_through() {
                    return Err(failure);
                }
                last = Some(failure);
            }
        }
    }
    match last {
        Some(failure) if skipped.len() == 1 => Err(failure),
        _ => {
            let tried = skipped
                .iter()
                .map(|row| {
                    format!(
                        "{}: {}",
                        row["provider"].as_str().unwrap_or("?"),
                        row["error_code"].as_str().unwrap_or("?")
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            Err(Failure::new(
                "unavailable",
                format!("No enabled provider could serve this search ({tried})"),
            ))
        }
    }
}

async fn search_with<H: Host + Sync>(
    host: &H,
    settings: &Settings,
    provider: &'static str,
    query: &str,
    max: u64,
) -> Result<Vec<Value>, Failure> {
    let count = max.to_string();
    match provider {
        "tavily" => {
            let body = json!({"query": query, "max_results": max, "search_depth": "basic"});
            let reply = provider_call(host, settings, provider, "search", &[], body).await?;
            parse_tavily(&reply, max)
        }
        "brave" => {
            let pairs = [("q", query), ("count", count.as_str())];
            let reply =
                provider_call(host, settings, provider, "search", &pairs, Value::Null).await?;
            parse_brave(&reply, max)
        }
        "wikipedia" => {
            let pairs = [("q", query), ("limit", count.as_str())];
            let reply =
                provider_call(host, settings, provider, "search", &pairs, Value::Null).await?;
            parse_wikipedia(&reply, max)
        }
        "duckduckgo" => {
            let reply = provider_call(
                host,
                settings,
                provider,
                "search",
                &[("q", query)],
                Value::Null,
            )
            .await?;
            parse_duckduckgo(&reply, max)
        }
        _ => Err(invalid("unknown provider")),
    }
}

async fn read<H: Host + Sync>(
    host: &H,
    settings: &Settings,
    arguments: &Value,
) -> Result<Value, Failure> {
    let args = object(arguments, &["url", "max_chars"])?;
    let url = text(args, "url", 2048)?.ok_or_else(|| invalid("url is required"))?;
    let url = safe_url(&url)
        .ok_or_else(|| invalid("url must be a public http or https address without credentials"))?;
    let max = match args.get("max_chars") {
        None | Some(Value::Null) => DEFAULT_READ_CHARS,
        Some(value) => value
            .as_u64()
            .filter(|count| (500..=50_000).contains(count))
            .ok_or_else(|| invalid("max_chars must be 500 to 50,000"))?,
    };
    let (provider, title, content) = match wikipedia_title(&url) {
        Some(title) if settings.enabled("wikipedia") => {
            let pairs = [
                ("action", "query"),
                ("prop", "extracts"),
                ("explaintext", "1"),
                ("redirects", "1"),
                ("format", "json"),
                ("formatversion", "2"),
                ("titles", title.as_str()),
            ];
            let reply =
                provider_call(host, settings, "wikipedia", "extract", &pairs, Value::Null).await?;
            let (title, content) = parse_wikipedia_extract(&reply)?;
            ("wikipedia", title, content)
        }
        _ if settings.enabled("tavily") => {
            let body = json!({"urls": [url], "extract_depth": "basic"});
            let reply = provider_call(host, settings, "tavily", "extract", &[], body).await?;
            ("tavily", Value::Null, parse_tavily_extract(&reply)?)
        }
        _ => {
            return Err(Failure::new(
                "unavailable",
                "Reading pages needs Tavily switched on in this agent's Web Search settings (Wikipedia articles need Wikipedia switched on)",
            ))
        }
    };
    let text: String = content.chars().take(max as usize).collect();
    let truncated = text.len() < content.len();
    Ok(
        json!({"provider": provider, "url": url, "title": title, "content": text, "truncated": truncated}),
    )
}

/// One declared provider call made by the host. Returns the body of a 200 reply.
async fn provider_call<H: Host + Sync>(
    host: &H,
    settings: &Settings,
    provider: &'static str,
    endpoint: &str,
    pairs: &[(&str, &str)],
    json_body: Value,
) -> Result<String, Failure> {
    let query: Map<String, Value> = pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), Value::String((*value).to_owned())))
        .collect();
    let mut body = json!({"query": query, "monthly_cap": settings.cap(provider)});
    if !json_body.is_null() {
        body["json"] = json_body;
    }
    let reply = host
        .request(
            "web.provider_request",
            json!({"provider": provider, "endpoint": endpoint}),
            body,
        )
        .await
        .map_err(|error| host_failure(provider, &error))?;
    let status = reply["status"].as_u64().unwrap_or_default();
    let text = reply["body"].as_str().unwrap_or_default().to_owned();
    match status {
        200 => Ok(text),
        401 | 403 => Err(Failure::new(
            "credential_invalid",
            format!("{provider} rejected the saved API key"),
        )),
        404 => Err(Failure::new(
            "not_found",
            format!("{provider} found nothing at that address"),
        )),
        429 => Err(Failure::new(
            "rate_limited",
            format!("{provider} is rate limiting requests"),
        )),
        202 => Err(Failure::new(
            "provider_error",
            format!("{provider} answered HTTP 202, usually a bot challenge rather than results"),
        )),
        _ => Err(Failure::new(
            "provider_error",
            format!("{provider} answered HTTP {status}"),
        )),
    }
}

/// Host refusals read `code: message`; only codes the agent can act on are kept.
fn host_failure(provider: &str, error: &str) -> Failure {
    let (code, message) = error.split_once(": ").unwrap_or(("", error));
    let code = match code {
        "credential_missing" => "credential_missing",
        "quota_exhausted" => "quota_exhausted",
        _ => "provider_error",
    };
    Failure::new(code, format!("{provider}: {message}"))
}

fn parse_json(body: &str, provider: &str) -> Result<Value, Failure> {
    serde_json::from_str(body).map_err(|_| {
        Failure::new(
            "provider_error",
            format!("{provider} returned an unreadable response"),
        )
    })
}

fn row(title: &str, url: &str, snippet: &str) -> Option<Value> {
    let url = safe_url(url)?;
    let title = clean_text(title, 300);
    (!title.is_empty())
        .then(|| json!({"title": title, "url": url, "snippet": clean_text(snippet, MAX_SNIPPET)}))
}

fn parse_tavily(body: &str, max: u64) -> Result<Vec<Value>, Failure> {
    let value = parse_json(body, "tavily")?;
    Ok(value["results"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            row(
                item["title"].as_str().unwrap_or_default(),
                item["url"].as_str().unwrap_or_default(),
                item["content"].as_str().unwrap_or_default(),
            )
        })
        .take(max as usize)
        .collect())
}

fn parse_brave(body: &str, max: u64) -> Result<Vec<Value>, Failure> {
    let value = parse_json(body, "brave")?;
    Ok(value["web"]["results"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            row(
                item["title"].as_str().unwrap_or_default(),
                item["url"].as_str().unwrap_or_default(),
                item["description"].as_str().unwrap_or_default(),
            )
        })
        .take(max as usize)
        .collect())
}

fn parse_wikipedia(body: &str, max: u64) -> Result<Vec<Value>, Failure> {
    let value = parse_json(body, "wikipedia")?;
    Ok(value["pages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|page| {
            let key = page["key"].as_str()?;
            let snippet = page["description"]
                .as_str()
                .filter(|text| !text.is_empty())
                .or_else(|| page["excerpt"].as_str())
                .unwrap_or_default();
            row(
                page["title"].as_str().unwrap_or_default(),
                &format!("{WIKIPEDIA}{}", encode(key)),
                snippet,
            )
        })
        .take(max as usize)
        .collect())
}

fn parse_wikipedia_extract(body: &str) -> Result<(Value, String), Failure> {
    let value = parse_json(body, "wikipedia")?;
    let page = &value["query"]["pages"][0];
    let content = page["extract"]
        .as_str()
        .filter(|text| !text.trim().is_empty());
    match content {
        Some(content) if page["missing"] != true => Ok((page["title"].clone(), content.to_owned())),
        _ => Err(Failure::new(
            "not_found",
            "Wikipedia has no article at that address",
        )),
    }
}

fn parse_tavily_extract(body: &str) -> Result<String, Failure> {
    let value = parse_json(body, "tavily")?;
    if let Some(content) = value["results"][0]["raw_content"]
        .as_str()
        .filter(|text| !text.trim().is_empty())
    {
        return Ok(content.to_owned());
    }
    let reason = value["failed_results"][0]["error"]
        .as_str()
        .unwrap_or("the page could not be read");
    Err(Failure::new("not_found", format!("tavily: {reason}")))
}

/// DuckDuckGo's HTML results. Markup with no result links is a challenge or a changed
/// page, never an empty result set.
fn parse_duckduckgo(html: &str, max: u64) -> Result<Vec<Value>, Failure> {
    let mut rows = Vec::new();
    let mut rest = html;
    while let Some(start) = rest.find("<a ") {
        rest = &rest[start..];
        let Some(end) = rest.find('>') else { break };
        let tag = &rest[..end];
        let relevant = attribute(tag, "class")
            .unwrap_or_default()
            .split_whitespace()
            .any(|class| class == "result__a" || class == "result-link");
        let href = attribute(tag, "href").map(|href| href.replace("&amp;", "&"));
        rest = &rest[end + 1..];
        if !relevant {
            continue;
        }
        let Some(close) = rest.find("</a>") else {
            break;
        };
        let title = &rest[..close];
        if let Some(href) = href {
            let href = if href.starts_with("//") {
                format!("https:{href}")
            } else {
                href
            };
            let target = if url_host(&href)
                .is_some_and(|host| host == "duckduckgo.com" || host.ends_with(".duckduckgo.com"))
            {
                query_value(&href, "uddg").unwrap_or(href)
            } else {
                href
            };
            if let Some(row) = row(title, &target, "") {
                rows.push(row);
            }
        }
        if rows.len() as u64 >= max {
            break;
        }
    }
    if rows.is_empty() {
        return Err(Failure::new(
            "provider_error",
            "duckduckgo returned a challenge or an unrecognised page, not results",
        ));
    }
    Ok(rows)
}

/// A quoted attribute preceded by whitespace in one tag.
fn attribute<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let needle = format!("{name}=");
    for (index, _) in tag.match_indices(&needle) {
        if index == 0 || !tag.as_bytes()[index - 1].is_ascii_whitespace() {
            continue;
        }
        let rest = &tag[index + needle.len()..];
        let quote = rest.chars().next()?;
        if quote != '"' && quote != '\'' {
            continue;
        }
        let rest = &rest[1..];
        return rest.find(quote).map(|end| &rest[..end]);
    }
    None
}

/// Markup removed, common entities decoded, at most `max` characters.
fn clean_text(input: &str, max: usize) -> String {
    let mut output = String::new();
    let mut tag = false;
    for character in input.chars() {
        match character {
            '<' => tag = true,
            '>' => tag = false,
            _ if !tag && !character.is_control() => output.push(character),
            _ => {}
        }
    }
    output
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .trim()
        .chars()
        .take(max)
        .collect()
}

/// An http(s) address with a host and no credentials, no whitespace or control text.
fn safe_url(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let valid = !authority.is_empty()
        && !authority.contains('@')
        && url.len() <= 2048
        && !url.chars().any(|c| c.is_whitespace() || c.is_control());
    valid.then(|| url.to_owned())
}

fn url_host(url: &str) -> Option<&str> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let authority = rest.split(['/', '?', '#']).next()?;
    Some(authority.split(':').next().unwrap_or(authority))
}

/// One decoded query parameter of `url`.
fn query_value(url: &str, name: &str) -> Option<String> {
    let query = url.split_once('?')?.1.split('#').next()?;
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key == name).then(|| decode(value, true))
    })
}

/// The decoded title of an English Wikipedia article address.
fn wikipedia_title(url: &str) -> Option<String> {
    let rest = [
        "https://en.wikipedia.org/wiki/",
        "http://en.wikipedia.org/wiki/",
        "https://en.m.wikipedia.org/wiki/",
    ]
    .iter()
    .find_map(|prefix| url.strip_prefix(prefix))?;
    let title = decode(rest.split(['?', '#']).next()?, false).replace('_', " ");
    (!title.trim().is_empty()).then_some(title)
}

/// Percent-decoding; `plus` also turns `+` into a space (query values).
fn decode(text: &str, plus: bool) -> String {
    let bytes = text.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' if index + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap_or_default();
                match u8::from_str_radix(hex, 16) {
                    Ok(byte) => {
                        output.push(byte);
                        index += 3;
                        continue;
                    }
                    Err(_) => output.push(b'%'),
                }
            }
            b'+' if plus => output.push(b' '),
            byte => output.push(byte),
        }
        index += 1;
    }
    String::from_utf8_lossy(&output).into_owned()
}

/// Percent-encoding of one path segment.
fn encode(text: &str) -> String {
    text.bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

fn object<'a>(arguments: &'a Value, allowed: &[&str]) -> Result<&'a Map<String, Value>, Failure> {
    let args = arguments
        .as_object()
        .ok_or_else(|| invalid("arguments must be an object"))?;
    if let Some(key) = args.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(invalid(&format!("unexpected argument {key}")));
    }
    Ok(args)
}

/// An optional non-empty single-line string of at most `max` bytes.
fn text(args: &Map<String, Value>, key: &str, max: usize) -> Result<Option<String>, Failure> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value))
            if !value.trim().is_empty()
                && value.len() <= max
                && !value.chars().any(char::is_control) =>
        {
            Ok(Some(value.trim().to_owned()))
        }
        Some(_) => Err(invalid(&format!("{key} must be 1 to {max} bytes of text"))),
    }
}

/// Run a future to completion on this thread. Host requests answer synchronously in
/// the standalone process, so a waker that does nothing is enough.
pub fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    loop {
        if let std::task::Poll::Ready(output) = future.as_mut().poll(&mut context) {
            return output;
        }
        std::thread::yield_now();
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use std::sync::Mutex;

    /// Records requests; answers settings from `settings` and provider calls from `reply`.
    struct Fake {
        settings: Value,
        reply: fn(&str, &str, &Value) -> Result<Value, String>,
        seen: Mutex<Vec<(String, Value, Value)>>,
    }

    impl Host for Fake {
        fn request(
            &self,
            name: &str,
            params: Value,
            body: Value,
        ) -> impl Future<Output = Result<Value, String>> + Send {
            self.seen
                .lock()
                .unwrap()
                .push((name.to_owned(), params.clone(), body.clone()));
            let answer = if name == "ability.settings" {
                Ok(self.settings.clone())
            } else {
                (self.reply)(
                    params["provider"].as_str().unwrap(),
                    params["endpoint"].as_str().unwrap(),
                    &body,
                )
            };
            std::future::ready(answer)
        }
    }

    fn run(fake: &Fake, tool: &str, arguments: Value) -> Value {
        block_on(call(
            fake,
            Call {
                tool,
                arguments: &arguments,
                request_id: "request",
                now_ms: 0,
            },
        ))
        .unwrap()
    }

    fn ok(body: Value) -> Result<Value, String> {
        Ok(json!({"status": 200, "body": body.to_string()}))
    }

    #[test]
    fn settings_follow_the_manifest_defaults_and_saved_order() {
        let defaults = Settings::resolve(&Value::Null).unwrap();
        assert_eq!(defaults.order, ["tavily", "wikipedia"]);
        assert!(defaults.fallback);
        assert_eq!(
            (
                defaults.max_results,
                defaults.cap("tavily"),
                defaults.cap("wikipedia")
            ),
            (5, 1000, 0)
        );
        let saved = json!({"provider_order": " Brave, nonsense ,duckduckgo", "brave_enabled": true,
            "duckduckgo_enabled": true, "tavily_enabled": "yes", "max_results": 40});
        let settings = Settings::resolve(&saved).unwrap();
        // Listed first, then switched-on providers the list left out; a wrong-typed value keeps its default.
        assert_eq!(
            settings.order,
            ["brave", "duckduckgo", "tavily", "wikipedia"]
        );
        assert_eq!(settings.max_results, 10);
    }

    #[test]
    fn search_falls_back_and_reports_what_it_skipped() {
        let fake = Fake {
            settings: json!({}),
            reply: |provider, _, body| match provider {
                "tavily" => Err("credential_missing: no tavily_api_key in the vault".into()),
                "wikipedia" => {
                    assert_eq!(body["query"]["q"], "rust language");
                    assert_eq!(body["monthly_cap"], 0);
                    ok(
                        json!({"pages": [{"key": "Rust_(programming_language)", "title": "Rust (programming language)",
                        "description": "General-purpose <b>programming</b> language"}]}),
                    )
                }
                _ => unreachable!(),
            },
            seen: Mutex::new(Vec::new()),
        };
        let output = run(&fake, "web_search", json!({"query": "rust language"}));
        assert_eq!(output["provider"], "wikipedia");
        assert_eq!(
            output["results"][0]["url"],
            "https://en.wikipedia.org/wiki/Rust_%28programming_language%29"
        );
        assert_eq!(
            output["results"][0]["snippet"],
            "General-purpose programming language"
        );
        assert_eq!(output["skipped"][0]["error_code"], "credential_missing");
        let seen = fake.seen.lock().unwrap();
        assert_eq!(seen[1].2["monthly_cap"], 1000);
        assert_eq!(seen[1].2["json"]["query"], "rust language");
    }

    #[test]
    fn errors_carry_a_code_and_hint() {
        let fake = Fake {
            settings: json!({"fallback": false}),
            reply: |_, _, _| Ok(json!({"status": 429, "body": ""})),
            seen: Mutex::new(Vec::new()),
        };
        let output = run(&fake, "web_search", json!({"query": "x"}));
        assert_eq!(
            (output["status"].as_str(), output["error_code"].as_str()),
            (Some("error"), Some("rate_limited"))
        );
        assert!(output["hint"].as_str().unwrap().contains("Wait"));
        let denied = run(
            &fake,
            "web_search",
            json!({"query": "x", "provider": "brave"}),
        );
        assert_eq!(denied["error_code"], "denied");
        for arguments in [
            json!({"query": ""}),
            json!({"query": "x", "max_results": 11}),
            json!({"query": "x", "extra": 1}),
        ] {
            assert_eq!(
                run(&fake, "web_search", arguments)["error_code"],
                "invalid_arguments"
            );
        }
        let none = Fake {
            settings: json!({"tavily_enabled": false, "wikipedia_enabled": false}),
            reply: |_, _, _| unreachable!(),
            seen: Mutex::new(Vec::new()),
        };
        assert_eq!(
            run(&none, "web_search", json!({"query": "x"}))["error_code"],
            "unavailable"
        );
    }

    #[test]
    fn web_read_uses_wikipedia_for_articles_and_tavily_otherwise() {
        let fake = Fake {
            settings: json!({}),
            reply: |provider, endpoint, body| match (provider, endpoint) {
                ("wikipedia", "extract") => {
                    assert_eq!(body["query"]["titles"], "Ada Lovelace");
                    ok(
                        json!({"query": {"pages": [{"title": "Ada Lovelace", "extract": "Ada Lovelace was a mathematician."}]}}),
                    )
                }
                ("tavily", "extract") => {
                    assert_eq!(body["json"]["urls"][0], "https://example.com/a");
                    ok(
                        json!({"results": [], "failed_results": [{"url": "https://example.com/a", "error": "page not reachable"}]}),
                    )
                }
                _ => unreachable!(),
            },
            seen: Mutex::new(Vec::new()),
        };
        let article = run(
            &fake,
            "web_read",
            json!({"url": "https://en.wikipedia.org/wiki/Ada_Lovelace#Life", "max_chars": 500}),
        );
        assert_eq!(
            (article["provider"].as_str(), article["title"].as_str()),
            (Some("wikipedia"), Some("Ada Lovelace"))
        );
        assert_eq!(article["truncated"], false);
        let missing = run(&fake, "web_read", json!({"url": "https://example.com/a"}));
        assert_eq!(missing["error_code"], "not_found");
        for url in [
            "ftp://example.com",
            concat!("https://", "user", ":", "pw", "@example.com/"),
            "https://exa mple.com",
        ] {
            assert_eq!(
                run(&fake, "web_read", json!({"url": url}))["error_code"],
                "invalid_arguments"
            );
        }
    }

    #[test]
    fn duckduckgo_parsing_fails_honestly_and_filters_unsafe_links() {
        assert_eq!(
            parse_duckduckgo("<html>challenge</html>", 5)
                .unwrap_err()
                .code,
            "provider_error"
        );
        assert!(
            parse_duckduckgo("<a class=\"result__a\" href=\"javascript:evil\">Bad</a>", 5).is_err()
        );
        let html = "<a href=\"//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fa%2Bb&rut=x\" class=\"result__a\">An <b>example</b> &amp; test</a>";
        let rows = parse_duckduckgo(html, 1).unwrap();
        assert_eq!(rows[0]["url"], "https://example.com/a+b");
        assert_eq!(rows[0]["title"], "An example & test");
    }

    #[test]
    fn percent_coding_round_trips_titles() {
        assert_eq!(decode("Caf%C3%A9_%28Paris%29", false), "Café_(Paris)");
        assert_eq!(encode("Café_(Paris)"), "Caf%C3%A9_%28Paris%29");
        assert_eq!(decode("100%", false), "100%");
    }
}
