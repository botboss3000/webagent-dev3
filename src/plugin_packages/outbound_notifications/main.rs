//! Optional module: all persisted state crosses the host's sealed namespace CAS.
use anyhow::{bail, ensure, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, net::IpAddr, time::Duration};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

const EVENTS: &[&str] = &[
    "session.created",
    "message.received",
    "run.started",
    "run.completed",
    "run.failed",
    "run.cancelled",
    "run.stopped",
    "run.waiting_approval",
    "session.help_requested",
    "session.help_cancelled",
    "session.help_resolved",
];
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct State {
    generation: String,
    cursor: u64,
    subscriptions: BTreeMap<String, Subscription>,
    deliveries: BTreeMap<String, Delivery>,
}
#[derive(Clone, Serialize, Deserialize)]
struct Subscription {
    id: String,
    revision: u64,
    url: String,
    events: Vec<String>,
    enabled: bool,
    key_id: String,
    key: String,
    #[serde(default)]
    activation_seq: u64,
}
#[derive(Clone, Serialize, Deserialize)]
struct Delivery {
    id: String,
    subscription: String,
    url: String,
    body: String,
    signature: String,
    key_id: String,
    status: String,
    attempts: u32,
    due_ms: u64,
    lease_until_ms: u64,
    attempt_id: String,
    http_status: Option<u16>,
    #[serde(default)]
    finished_ms: Option<u64>,
}
#[derive(Deserialize)]
struct Request {
    protocol: u32,
    operation: String,
    #[serde(default)]
    args: Value,
    #[serde(default)]
    state: Value,
    now_ms: u64,
    context: Context,
}
#[derive(Deserialize)]
struct Context {
    generation: String,
    administrator: bool,
    #[serde(default)]
    dispatch_authorized: bool,
    #[serde(default)]
    latest_event_seq: Option<u64>,
    #[serde(default)]
    event_scan_cursor: Option<u64>,
}
fn string(v: &Value, key: &str) -> Result<String> {
    Ok(v[key]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 2048)
        .ok_or_else(|| anyhow::anyhow!("invalid {key}"))?
        .to_owned())
}
fn supplied_key(args: &Value) -> Result<String> {
    let secret = string(args, "signing_key")?;
    ensure!(
        secret.len() == 64 && secret.bytes().all(|b| b.is_ascii_hexdigit()),
        "signing key must be 32-byte hex"
    );
    Ok(secret.to_ascii_lowercase())
}
// RFC 2104 HMAC, using the already pinned sha2 dependency.
fn sign(secret: &[u8], body: &[u8]) -> String {
    let mut k = [0u8; 64];
    if secret.len() > 64 {
        k[..32].copy_from_slice(&Sha256::digest(secret));
    } else {
        k[..secret.len()].copy_from_slice(secret);
    }
    let mut inner = Sha256::new();
    inner.update(k.map(|b| b ^ 0x36));
    inner.update(body);
    let mut outer = Sha256::new();
    outer.update(k.map(|b| b ^ 0x5c));
    outer.update(inner.finalize());
    hex::encode(outer.finalize())
}
fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => {
            let [a, b, c, _] = v.octets();
            !(a == 0
                || a == 10
                || a == 127
                || a >= 224
                || (a == 100 && (64..=127).contains(&b))
                || (a == 169 && b == 254)
                || (a == 172 && (16..=31).contains(&b))
                || (a == 192 && (b == 0 || b == 168 || (b == 88 && c == 99)))
                || (a == 198 && (b == 18 || b == 19 || (b == 51 && c == 100)))
                || (a == 203 && b == 0 && c == 113))
        }
        IpAddr::V6(v) => {
            let s = v.segments();
            (s[0] & 0xe000) == 0x2000
                && !(s[0] == 0x2001 && (s[1] < 0x200 || s[1] == 0xdb8))
                && s[0] != 0x2002
        }
    }
}
fn url(input: &str) -> Result<reqwest::Url> {
    let u = reqwest::Url::parse(input)?;
    ensure!(
        u.scheme() == "https"
            && u.username().is_empty()
            && u.password().is_none()
            && u.fragment().is_none()
            && u.port_or_known_default() == Some(443),
        "unsafe destination"
    );
    let host = u
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("host required"))?;
    ensure!(
        host.contains('.')
            && !host.ends_with('.')
            && !host.ends_with(".localhost")
            && !host.ends_with(".local"),
        "unsafe host"
    );
    if let Ok(ip) = host.trim_matches(['[', ']']).parse::<IpAddr>() {
        ensure!(public_ip(ip), "unsafe address");
    }
    Ok(u)
}
async fn send(d: &Delivery) -> Result<u16> {
    let u = url(&d.url)?;
    let host = u.host_str().unwrap();
    let addresses: Vec<_> =
        tokio::time::timeout(Duration::from_secs(3), tokio::net::lookup_host((host, 443)))
            .await??
            .collect();
    ensure!(
        !addresses.is_empty() && addresses.iter().all(|a| public_ip(a.ip())),
        "unsafe DNS result"
    );
    // Resolve once, check every address, then pin exactly these addresses for TLS transport.
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .connect_timeout(Duration::from_secs(3))
        .resolve_to_addrs(host, &addresses)
        .build()?;
    let response = client
        .post(u)
        .header("content-type", "application/json")
        .header("x-webagent-delivery-id", &d.id)
        .header("x-webagent-key-id", &d.key_id)
        .header("x-webagent-signature", format!("sha256={}", d.signature))
        .body(d.body.clone())
        .send()
        .await?;
    Ok(response.status().as_u16())
}
fn safe_subscription(s: &Subscription) -> Value {
    json!({"id":s.id,"revision":s.revision,"url":s.url,"events":s.events,"enabled":s.enabled,"key_id":s.key_id})
}
async fn handle(req: Request) -> Result<Value> {
    ensure!(
        req.protocol == 1 && req.context.administrator,
        "administrator required"
    );
    ensure!(!req.context.generation.is_empty(), "generation required");
    let mut state: State = if req.state.is_null() {
        State::default()
    } else {
        serde_json::from_value(req.state)?
    };
    if state.generation != req.context.generation {
        for s in state.subscriptions.values_mut() {
            s.enabled = false;
            s.revision += 1;
        }
        for d in state
            .deliveries
            .values_mut()
            .filter(|d| matches!(d.status.as_str(), "pending" | "sending" | "uncertain"))
        {
            d.status = if matches!(d.status.as_str(), "sending" | "uncertain") {
                "uncertain_withdrawn"
            } else {
                "withdrawn"
            }
            .into();
            d.finished_ms = Some(req.now_ms);
        }
        state.generation = req.context.generation.clone();
    }
    let result = match req.operation.as_str() {
        "deactivate" => {
            for s in state.subscriptions.values_mut() {
                if s.enabled {
                    s.enabled = false;
                    s.revision += 1;
                }
            }
            for d in state
                .deliveries
                .values_mut()
                .filter(|d| matches!(d.status.as_str(), "pending" | "sending" | "uncertain"))
            {
                d.status = if matches!(d.status.as_str(), "sending" | "uncertain") {
                    "uncertain_withdrawn"
                } else {
                    "withdrawn"
                }
                .into();
                d.finished_ms = Some(req.now_ms);
            }
            json!({"inactive":true})
        }
        "probe" => json!({"status":"ready","protocol":1}),
        "list" => {
            json!({"subscriptions":state.subscriptions.values().map(safe_subscription).collect::<Vec<_>>(),"cursor":state.cursor})
        }
        "create" => {
            ensure!(state.subscriptions.len() < 64, "subscription capacity");
            let id = string(&req.args, "id")?;
            ensure!(
                id.len() <= 128
                    && id
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_')),
                "invalid subscription ID"
            );
            ensure!(
                !state.subscriptions.contains_key(&id),
                "subscription exists"
            );
            let destination = string(&req.args, "url")?;
            ensure!(destination.len() <= 512, "destination size");
            url(&destination)?;
            let events: Vec<String> = serde_json::from_value(req.args["events"].clone())?;
            ensure!(
                !events.is_empty()
                    && events.len() <= EVENTS.len()
                    && events.iter().all(|e| EVENTS.contains(&e.as_str())),
                "invalid events"
            );
            let s = Subscription {
                id: id.clone(),
                revision: 0,
                url: destination,
                events,
                enabled: false,
                key_id: uuid::Uuid::new_v4().to_string(),
                key: supplied_key(&req.args)?,
                activation_seq: state.cursor,
            };
            let out = safe_subscription(&s);
            state.subscriptions.insert(id, s);
            out
        }
        "update" | "rotate_key" | "delete" => {
            let id = string(&req.args, "id")?;
            let s = state
                .subscriptions
                .get_mut(&id)
                .ok_or_else(|| anyhow::anyhow!("subscription absent"))?;
            ensure!(
                req.args["expected_revision"].as_u64() == Some(s.revision),
                "revision conflict"
            );
            if req.operation == "delete" {
                state.subscriptions.remove(&id);
                for d in state
                    .deliveries
                    .values_mut()
                    .filter(|d| d.subscription == id && d.status != "delivered")
                {
                    d.status = if matches!(d.status.as_str(), "sending" | "uncertain") {
                        "uncertain_withdrawn"
                    } else {
                        "withdrawn"
                    }
                    .into();
                    d.finished_ms = Some(req.now_ms);
                }
                json!({"deleted":id})
            } else {
                if req.operation == "rotate_key" {
                    s.key = supplied_key(&req.args)?;
                    s.key_id = uuid::Uuid::new_v4().to_string();
                } else {
                    if let Some(enabled) = req.args.get("enabled") {
                        let enabled = enabled
                            .as_bool()
                            .ok_or_else(|| anyhow::anyhow!("invalid enabled"))?;
                        if enabled && !s.enabled {
                            s.activation_seq =
                                state.cursor.max(req.context.latest_event_seq.ok_or_else(
                                    || anyhow::anyhow!("activation cursor unavailable"),
                                )?);
                        }
                        s.enabled = enabled;
                    }
                    if req.args.get("url").is_some() {
                        let destination = string(&req.args, "url")?;
                        ensure!(destination.len() <= 512, "destination size");
                        url(&destination)?;
                        s.url = destination;
                    }
                    if let Some(events) = req.args.get("events") {
                        let events: Vec<String> = serde_json::from_value(events.clone())?;
                        ensure!(
                            !events.is_empty()
                                && events.len() <= EVENTS.len()
                                && events.iter().all(|e| EVENTS.contains(&e.as_str())),
                            "invalid events"
                        );
                        s.events = events;
                    }
                    ensure!(
                        req.args.get("enabled").is_some()
                            || req.args.get("url").is_some()
                            || req.args.get("events").is_some(),
                        "empty update"
                    );
                }
                s.revision += 1;
                safe_subscription(s)
            }
        }
        "tick" => {
            ensure!(req.context.dispatch_authorized, "trusted dispatch required");
            let events = req.args["events"]
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("events required"))?;
            ensure!(events.len() <= 200, "event batch bound");
            // Only the authenticated host supplies a fresh activation scan cursor.
            // An old active subscription keeps its old scan and still reports gaps.
            if let Some(scan) = req.context.event_scan_cursor {
                state.cursor = state.cursor.max(scan);
            }
            ensure!(
                req.args["pruned_through"].as_u64().unwrap_or(0) <= state.cursor,
                "cursor_expired"
            );
            for event in events {
                let seq = event["seq"]
                    .as_u64()
                    .ok_or_else(|| anyhow::anyhow!("event sequence required"))?;
                if seq <= state.cursor {
                    continue;
                }
                let kind = string(event, "type")?;
                ensure!(
                    kind.len() <= 128
                        && kind
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-')),
                    "invalid event type"
                );
                // Future lifecycle types are not ours to decode. Consume their cursor
                // without interpreting or forwarding their potentially new payload schema.
                if !EVENTS.contains(&kind.as_str()) {
                    state.cursor = seq;
                    continue;
                }
                let event_id = string(event, "event_id")?;
                ensure!(event_id.len() <= 128, "event ID size");
                for field in ["session_id", "run_id", "agent_id"] {
                    ensure!(
                        event[field].is_null()
                            || event[field]
                                .as_str()
                                .map(|s| s.len() <= 128)
                                .unwrap_or(false),
                        "invalid event metadata"
                    );
                }
                ensure!(
                    event["created_at"].as_u64().is_some(),
                    "event timestamp required"
                );
                // Construct only the fixed metadata envelope; never forward arbitrary event fields.
                let projection = json!({"event_id":event_id,"type":kind,"session_id":event["session_id"],"run_id":event["run_id"],"agent_id":event["agent_id"],"created_at":event["created_at"],"schema_version":1});
                for s in state
                    .subscriptions
                    .values()
                    .filter(|s| s.enabled && seq > s.activation_seq && s.events.contains(&kind))
                {
                    let id = hex::encode(Sha256::digest(format!("{}:{}", s.id, event_id)));
                    if state.deliveries.contains_key(&id) {
                        continue;
                    }
                    ensure!(state.deliveries.len() < 2048, "delivery capacity");
                    let body =
                        json!({"schema_version":1,"delivery_id":id,"event":projection}).to_string();
                    let signature = sign(&hex::decode(&s.key)?, body.as_bytes());
                    state.deliveries.insert(
                        id.clone(),
                        Delivery {
                            id,
                            subscription: s.id.clone(),
                            url: s.url.clone(),
                            body,
                            signature,
                            key_id: s.key_id.clone(),
                            status: "pending".into(),
                            attempts: 0,
                            due_ms: req.now_ms,
                            lease_until_ms: 0,
                            attempt_id: String::new(),
                            http_status: None,
                            finished_ms: None,
                        },
                    );
                }
                state.cursor = seq;
            }
            json!({"cursor":state.cursor})
        }
        "reserve_delivery" => {
            ensure!(req.context.dispatch_authorized, "trusted dispatch required");
            for d in state.deliveries.values_mut().filter(|d| {
                d.status == "sending" && d.lease_until_ms <= req.now_ms && d.attempts >= 5
            }) {
                d.status = "exhausted".into();
                d.finished_ms = Some(req.now_ms);
            }
            let candidate = state.deliveries.values_mut().find(|d| {
                d.due_ms <= req.now_ms
                    && (d.status == "pending"
                        || (d.status == "sending" && d.lease_until_ms <= req.now_ms))
                    && state
                        .subscriptions
                        .get(&d.subscription)
                        .map(|s| s.enabled)
                        .unwrap_or(false)
            });
            if let Some(d) = candidate {
                d.status = "sending".into();
                d.attempts += 1;
                d.attempt_id = uuid::Uuid::new_v4().to_string();
                d.lease_until_ms = req.now_ms.saturating_add(30_000);
                json!({"dispatch_id":d.id,"delivery_id":d.id,"attempt_id":d.attempt_id})
            } else {
                Value::Null
            }
        }
        "execute_delivery" => {
            ensure!(req.context.dispatch_authorized, "trusted dispatch required");
            let id = string(&req.args, "delivery_id")?;
            let attempt = string(&req.args, "attempt_id")?;
            let d = state
                .deliveries
                .get_mut(&id)
                .ok_or_else(|| anyhow::anyhow!("delivery absent"))?;
            ensure!(
                d.status == "sending" && d.attempt_id == attempt && d.lease_until_ms > req.now_ms,
                "lease conflict"
            );
            ensure!(
                state
                    .subscriptions
                    .get(&d.subscription)
                    .map(|s| s.enabled)
                    .unwrap_or(false),
                "delivery withdrawn"
            );
            match send(d).await {
                Ok(status) => {
                    d.http_status = Some(status);
                    if (200..300).contains(&status) {
                        d.status = "delivered".into();
                    } else if status == 429 || status >= 500 {
                        retry(d, req.now_ms);
                    } else {
                        d.status = "failed".into();
                    }
                }
                Err(_) => {
                    retry(d, req.now_ms);
                }
            }
            if matches!(d.status.as_str(), "delivered" | "failed" | "exhausted") {
                d.finished_ms = Some(req.now_ms);
            }
            json!({"id":id,"status":d.status,"attempts":d.attempts,"http_status":d.http_status})
        }
        "acknowledge_gap" => {
            ensure!(
                req.args["expected_cursor"].as_u64() == Some(state.cursor),
                "cursor conflict"
            );
            let resume = req.args["resume_since"]
                .as_u64()
                .ok_or_else(|| anyhow::anyhow!("resume cursor required"))?;
            ensure!(resume >= state.cursor, "cursor cannot regress");
            state.cursor = resume;
            json!({"cursor":resume,"gap_acknowledged":true})
        }
        "prune_receipts" => {
            let before = req.args["before_ms"]
                .as_u64()
                .ok_or_else(|| anyhow::anyhow!("retention boundary required"))?;
            ensure!(before <= req.now_ms, "future retention boundary");
            let count = state.deliveries.len();
            state.deliveries.retain(|_, d| {
                !matches!(
                    d.status.as_str(),
                    "delivered" | "failed" | "exhausted" | "withdrawn"
                ) || !d.finished_ms.map(|t| t < before).unwrap_or(false)
            });
            json!({"pruned":count-state.deliveries.len()})
        }
        "history" => {
            let limit = req.args["limit"].as_u64().unwrap_or(100);
            ensure!((1..=200).contains(&limit), "history limit");
            let after = req.args["after"].as_str().unwrap_or("");
            let entries: Vec<_> = state
                .deliveries
                .values()
                .filter(|d| d.id.as_str() > after)
                .take(limit as usize + 1)
                .collect();
            let more = entries.len() > limit as usize;
            let page = &entries[..entries.len().min(limit as usize)];
            json!({"deliveries":page.iter().map(|d|json!({"id":d.id,"subscription":d.subscription,"status":d.status,"attempts":d.attempts,"http_status":d.http_status,"due_ms":d.due_ms})).collect::<Vec<_>>(),"after":if more {page.last().map(|d|d.id.as_str())} else {None}})
        }
        _ => bail!("unsupported operation"),
    };
    Ok(json!({"ok":true,"result":result,"state":state,"admissions":[]}))
}
fn retry(d: &mut Delivery, now: u64) {
    if d.attempts >= 5 {
        d.status = "exhausted".into();
    } else {
        d.status = "pending".into();
        d.due_ms = now.saturating_add(1000u64.saturating_mul(2u64.pow(d.attempts.min(8))));
    }
}
#[tokio::main]
async fn main() -> Result<()> {
    let mut input = BufReader::new(tokio::io::stdin());
    let mut output = tokio::io::stdout();
    loop {
        let mut line = String::new();
        let read = (&mut input)
            .take(4 * 1024 * 1024 + 1)
            .read_line(&mut line)
            .await?;
        if read == 0 {
            break;
        }
        if line.len() > 4 * 1024 * 1024 {
            bail!("frame capacity");
        }
        let result = match serde_json::from_str::<Request>(&line) {
            Ok(req) => handle(req).await,
            Err(e) => Err(e.into()),
        };
        let reply = match result {
            Ok(v) => v,
            Err(e) => json!({"ok":false,"error":e.to_string()}),
        };
        output
            .write_all(serde_json::to_string(&reply)?.as_bytes())
            .await?;
        output.write_all(b"\n").await?;
        output.flush().await?;
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancelled_event_is_supported() {
        assert!(EVENTS.contains(&"run.cancelled"));
    }
    #[test]
    fn rfc4231() {
        assert_eq!(
            sign(&[0x0b; 20], b"Hi There"),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
    }
    #[test]
    fn destinations() {
        for s in [
            "http://example.com",
            "https://127.0.0.1",
            "https://192.168.1.2",
            "https://user@example.com",
            "https://example.com:8443",
            "https://example.com/#fragment",
        ] {
            assert!(url(s).is_err(), "{s}");
        }
        assert!(url("https://example.com/hook").is_ok());
    }
    #[test]
    fn address_ranges() {
        for s in [
            "0.1.2.3",
            "100.64.0.1",
            "169.254.1.1",
            "172.16.1.1",
            "198.18.0.1",
            "203.0.113.4",
            "::1",
            "::ffff:8.8.8.8",
            "2001:db8::1",
            "2002:808:808::1",
        ] {
            assert!(!public_ip(s.parse().unwrap()), "{s}");
        }
        assert!(public_ip("8.8.8.8".parse().unwrap()));
    }
    #[test]
    fn bounded_retry() {
        let mut d = Delivery {
            id: "id".into(),
            subscription: "s".into(),
            url: "https://example.com".into(),
            body: "{}".into(),
            signature: "sig".into(),
            key_id: "k".into(),
            status: "sending".into(),
            attempts: 1,
            due_ms: 0,
            lease_until_ms: 0,
            attempt_id: "a".into(),
            http_status: None,
            finished_ms: None,
        };
        retry(&mut d, 10);
        assert_eq!(d.due_ms, 2010);
        assert_eq!(d.id, "id");
        d.attempts = 5;
        retry(&mut d, 10);
        assert_eq!(d.status, "exhausted");
    }
    async fn call(op: &str, args: Value, state: Value, generation: &str) -> Value {
        handle(Request {
            protocol: 1,
            operation: op.into(),
            args,
            state,
            now_ms: 1000,
            context: Context {
                generation: generation.into(),
                administrator: true,
                dispatch_authorized: true,
                latest_event_seq: Some(0),
                event_scan_cursor: None,
            },
        })
        .await
        .unwrap()
    }
    #[tokio::test]
    async fn staged_delivery_rotation_and_reinstall() {
        let created=call("create",json!({"id":"s","url":"https://example.com/hook","events":["run.completed"],"signing_key":"1111111111111111111111111111111111111111111111111111111111111111"}),Value::Null,"g1").await;
        assert!(created["result"].get("key").is_none());
        assert_eq!(created["result"]["enabled"], false);
        let enabled = call(
            "update",
            json!({"id":"s","expected_revision":0,"enabled":true}),
            created["state"].clone(),
            "g1",
        )
        .await;
        let event = json!({"seq":1,"event_id":"e1","type":"run.completed","session_id":"session","run_id":"run","agent_id":"agent","created_at":1,"model_text":"must not leave"});
        let staged = call(
            "tick",
            json!({"events":[event.clone()],"pruned_through":0}),
            enabled["state"].clone(),
            "g1",
        )
        .await;
        assert_eq!(staged["state"]["deliveries"].as_object().unwrap().len(), 1);
        let id = staged["state"]["deliveries"]
            .as_object()
            .unwrap()
            .keys()
            .next()
            .unwrap()
            .clone();
        let signature = staged["state"]["deliveries"][&id]["signature"].clone();
        assert!(!staged["state"]["deliveries"][&id]["body"]
            .as_str()
            .unwrap()
            .contains("must not leave"));
        let duplicate = call(
            "tick",
            json!({"events":[event],"pruned_through":0}),
            staged["state"].clone(),
            "g1",
        )
        .await;
        assert_eq!(
            duplicate["state"]["deliveries"].as_object().unwrap().len(),
            1
        );
        let rotated=call("rotate_key",json!({"id":"s","expected_revision":1,"signing_key":"2222222222222222222222222222222222222222222222222222222222222222"}),duplicate["state"].clone(),"g1").await;
        assert_eq!(rotated["state"]["deliveries"][&id]["signature"], signature);
        let reserved = call(
            "reserve_delivery",
            json!({}),
            rotated["state"].clone(),
            "g1",
        )
        .await;
        assert_eq!(reserved["result"]["delivery_id"], id);
        assert_eq!(reserved["state"]["deliveries"][&id]["status"], "sending");
        let reinstalled = call("list", json!({}), reserved["state"].clone(), "g2").await;
        assert_eq!(reinstalled["state"]["subscriptions"]["s"]["enabled"], false);
        assert_eq!(
            reinstalled["state"]["deliveries"][&id]["status"],
            "uncertain_withdrawn"
        );
    }
    #[tokio::test]
    async fn authority_and_cursor_gap() {
        let denied = handle(Request {
            protocol: 1,
            operation: "tick".into(),
            args: json!({"events":[]}),
            state: Value::Null,
            now_ms: 0,
            context: Context {
                generation: "g".into(),
                administrator: true,
                dispatch_authorized: false,
                latest_event_seq: Some(0),
                event_scan_cursor: None,
            },
        })
        .await;
        assert!(denied.is_err());
        let gap = handle(Request {
            protocol: 1,
            operation: "tick".into(),
            args: json!({"events":[],"pruned_through":2}),
            state: Value::Null,
            now_ms: 0,
            context: Context {
                generation: "g".into(),
                administrator: true,
                dispatch_authorized: true,
                latest_event_seq: Some(0),
                event_scan_cursor: None,
            },
        })
        .await;
        assert!(gap.unwrap_err().to_string().contains("cursor_expired"));
    }
    #[tokio::test]
    async fn fresh_activation_scan_survives_retention_and_keeps_stable_envelope() {
        let created=call("create",json!({"id":"fresh","url":"https://example.com/hook","events":["run.completed"],"signing_key":"1111111111111111111111111111111111111111111111111111111111111111"}),Value::Null,"g1").await;
        let enabled=handle(serde_json::from_value(json!({"protocol":1,"operation":"update","args":{"id":"fresh","expected_revision":0,"enabled":true},"state":created["state"],"now_ms":1000,"context":{"generation":"g1","administrator":true,"latest_event_seq":600}})).unwrap()).await.unwrap();
        assert_eq!(enabled["state"]["cursor"], 0);
        assert_eq!(
            enabled["state"]["subscriptions"]["fresh"]["activation_seq"],
            600
        );
        let event = json!({"seq":601,"event_id":"e601","type":"run.completed","session_id":"s601","run_id":"r601","agent_id":"a601","created_at":1000,"model_text":"must not leave"});
        let tick_request = json!({"protocol":1,"operation":"tick","args":{"events":[event],"pruned_through":500},"state":enabled["state"],"now_ms":2000,"context":{"generation":"g1","administrator":true,"dispatch_authorized":true,"event_scan_cursor":600}});
        let tick = handle(serde_json::from_value(tick_request.clone()).unwrap())
            .await
            .unwrap();
        assert_eq!(tick["state"]["cursor"], 601);
        let deliveries = tick["state"]["deliveries"].as_object().unwrap();
        assert_eq!(deliveries.len(), 1);
        let (id, delivery) = deliveries.iter().next().unwrap();
        assert_eq!(delivery["status"], "pending");
        assert_eq!(delivery["attempts"], 0);
        assert_eq!(*id, hex::encode(Sha256::digest("fresh:e601")));
        let body = delivery["body"].as_str().unwrap();
        let envelope: Value = serde_json::from_str(body).unwrap();
        assert_eq!(envelope["event"]["event_id"], "e601");
        assert_eq!(envelope["event"]["session_id"], "s601");
        assert!(envelope["event"].get("model_text").is_none());
        assert_eq!(
            delivery["signature"],
            sign(&hex::decode("11".repeat(32)).unwrap(), body.as_bytes())
        );
        let mut repeated = tick_request;
        repeated["state"] = tick["state"].clone();
        let duplicate = handle(serde_json::from_value(repeated).unwrap())
            .await
            .unwrap();
        assert_eq!(
            duplicate["state"]["deliveries"],
            tick["state"]["deliveries"]
        );
        assert_eq!(duplicate["state"]["cursor"], 601);
    }
    #[tokio::test]
    async fn old_active_scan_still_fails_closed_on_retention_gap() {
        let created=call("create",json!({"id":"old","url":"https://example.com/hook","events":["run.completed"],"signing_key":"1111111111111111111111111111111111111111111111111111111111111111"}),Value::Null,"g1").await;
        let mut state = created["state"].clone();
        state["cursor"] = json!(100);
        state["subscriptions"]["old"]["enabled"] = json!(true);
        state["subscriptions"]["old"]["activation_seq"] = json!(100);
        let request = json!({"protocol":1,"operation":"tick","args":{"events":[],"pruned_through":500},"state":state,"now_ms":2000,"context":{"generation":"g1","administrator":true,"dispatch_authorized":true,"event_scan_cursor":100}});
        let result = handle(serde_json::from_value(request).unwrap()).await;
        assert!(result.unwrap_err().to_string().contains("cursor_expired"));
        assert_eq!(state["cursor"], 100);
        assert!(state["deliveries"].as_object().unwrap().is_empty());
    }
    #[tokio::test]
    async fn mixed_canonical_and_future_events_do_not_block_subscribed_terminal_delivery() {
        assert_eq!(EVENTS.len(), 11);
        for kind in [
            "run.stopped",
            "session.help_requested",
            "session.help_cancelled",
            "session.help_resolved",
        ] {
            assert!(EVENTS.contains(&kind));
        }
        let created=call("create",json!({"id":"terminal","url":"https://example.com/hook","events":["run.completed"],"signing_key":"1111111111111111111111111111111111111111111111111111111111111111"}),Value::Null,"g1").await;
        let enabled = call(
            "update",
            json!({"id":"terminal","expected_revision":0,"enabled":true}),
            created["state"].clone(),
            "g1",
        )
        .await;
        let mut events = Vec::new();
        for (index, kind) in [
            "run.stopped",
            "session.help_requested",
            "session.help_cancelled",
            "session.help_resolved",
        ]
        .into_iter()
        .enumerate()
        {
            events.push(json!({"seq":index+1,"event_id":format!("event-{}",index+1),"type":kind,"session_id":"s","run_id":"r","agent_id":"a","created_at":1000}));
        }
        // Intentionally lacks event_id and has a future payload schema: no known
        // envelope validation or payload forwarding is appropriate for this type.
        events.push(json!({"seq":5,"type":"future.changed","session_id":{"new_schema":true},"payload":{"private":"not forwarded"}}));
        events.push(json!({"seq":6,"event_id":"terminal-6","type":"run.completed","session_id":"s","run_id":"r","agent_id":"a","created_at":1000}));
        let tick = call(
            "tick",
            json!({"events":events,"pruned_through":0}),
            enabled["state"].clone(),
            "g1",
        )
        .await;
        assert_eq!(tick["state"]["cursor"], 6);
        let deliveries = tick["state"]["deliveries"].as_object().unwrap();
        assert_eq!(deliveries.len(), 1);
        let (id, delivery) = deliveries.iter().next().unwrap();
        assert_eq!(*id, hex::encode(Sha256::digest("terminal:terminal-6")));
        let body = delivery["body"].as_str().unwrap();
        let envelope: Value = serde_json::from_str(body).unwrap();
        assert_eq!(envelope["event"]["type"], "run.completed");
        assert!(!body.contains("not forwarded"));
        assert_eq!(
            delivery["signature"],
            sign(&hex::decode("11".repeat(32)).unwrap(), body.as_bytes())
        );
    }
}
