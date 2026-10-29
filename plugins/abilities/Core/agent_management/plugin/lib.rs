//! Agent Management as an ability plugin. It reaches the app only through the host
//! requests its `plugin.json` declares, made as the chatting user; the app checks each
//! request against that declaration and against the caller's own permissions. The same
//! code runs as a standalone process in dev builds (`main.rs`) and linked into stable
//! builds (root feature `stable-abilities`).
use serde_json::{json, Map, Value};
use std::future::Future;

/// Tools this plugin runs. `kick_user_session` keeps its folder here but is served by Chat.
pub const TOOLS: [&str; 15] = [
    "list_agent_templates",
    "list_my_agents",
    "get_agent",
    "list_agent_tools",
    "create_agent",
    "update_agent",
    "set_agent_tool",
    "edit_agent_prompt",
    "set_agent_ability",
    "manage_agent_skills",
    "manage_agent_chat_config",
    "list_user_sessions",
    "manage_user_session",
    "watch_sessions",
    "create_user_session",
];

/// Host requests this plugin makes; `plugin.json` declares exactly these.
pub const REQUESTS: [&str; 23] = [
    "agents.list",
    "agents.identity.read",
    "agents.abilities.read",
    "agents.create",
    "agents.identity.update",
    "agents.tool_mode.set",
    "agents.ability_visibility.set",
    "agents.slot.write",
    "agents.chat_config.read",
    "agents.chat_config.patch",
    "agents.chat_config.reset",
    "skills.list",
    "skills.read",
    "skills.save",
    "skills.remove",
    "sessions.list",
    "sessions.create",
    "sessions.read",
    "sessions.update",
    "sessions.recycle",
    "sessions.restore",
    "sessions.watch",
    "ability.settings",
];

const ARGUMENTS: [&str; 15] = [
    "agent_id",
    "ability_id",
    "tool",
    "slot",
    "session_id",
    "action",
    "request",
    "name",
    "description",
    "instructions",
    "expected_revision",
    "document",
    "patch",
    "prompt",
    "expires_ms",
];

const MAX_PROMPT: usize = 16_000;
const MAX_WATCH_MS: u64 = 30 * 86_400 * 1000;

/// The app, as one plugin call sees it.
pub trait Host {
    /// Make the declared request `name`. `params` fill its route; `body` is its JSON body.
    fn request(
        &self,
        name: &str,
        params: Value,
        body: Value,
    ) -> impl Future<Output = Result<Value, String>> + Send;
}

/// One tool call, with the facts the host supplies.
pub struct Call<'a> {
    pub tool: &'a str,
    pub arguments: &'a Value,
    /// Stable for this call and its retries; used as the client request id.
    pub request_id: &'a str,
    pub now_ms: u64,
}

/// Run one tool and return its output.
pub async fn call<H: Host + Sync>(host: &H, call: Call<'_>) -> Result<Value, String> {
    let args = Arguments::parse(call.arguments)?;
    let mut body = args.object("request")?;
    let tool = call.tool;
    match tool {
        "list_agent_templates" => Ok(json!({"templates":[{"id":"blank","name":"Blank agent",
            "description":"Caller-owned agent with platform defaults; no new access grants."}]})),
        "list_my_agents" => host.request("agents.list", json!({}), json!({})).await,
        "get_agent" => {
            let params = json!({"agent_id": args.id("agent_id")?});
            host.request("agents.identity.read", params, json!({})).await
        }
        "list_agent_tools" => {
            let params = json!({"agent_id": args.id("agent_id")?});
            host.request("agents.abilities.read", params, json!({})).await
        }
        "create_agent" => {
            body.insert("client_request_id".into(), json!(call.request_id));
            host.request("agents.create", json!({}), Value::Object(body)).await
        }
        "update_agent" => {
            let params = json!({"agent_id": args.id("agent_id")?});
            host.request("agents.identity.update", params, Value::Object(body)).await
        }
        "set_agent_tool" => {
            let params = json!({"agent_id": args.id("agent_id")?,
                "ability_id": args.id("ability_id")?, "tool": args.id("tool")?});
            host.request("agents.tool_mode.set", params, Value::Object(body)).await
        }
        "set_agent_ability" => {
            let params = json!({"agent_id": args.id("agent_id")?, "ability_id": args.id("ability_id")?});
            host.request("agents.ability_visibility.set", params, Value::Object(body)).await
        }
        "edit_agent_prompt" => {
            let params = json!({"agent_id": args.id("agent_id")?, "slot": args.id("slot")?});
            host.request("agents.slot.write", params, Value::Object(body)).await
        }
        "manage_agent_skills" => skills(host, &args).await,
        "manage_agent_chat_config" => chat_config(host, &args).await,
        "watch_sessions" => watch(host, &args, call.now_ms).await,
        "list_user_sessions" => host.request("sessions.list", json!({}), json!({})).await,
        "create_user_session" => {
            body.insert("client_request_id".into(), json!(call.request_id));
            host.request("sessions.create", json!({}), Value::Object(body)).await
        }
        "manage_user_session" => {
            let params = json!({"session_id": args.session("session_id")?});
            body.insert("client_request_id".into(), json!(call.request_id));
            let request = match args.text("action") {
                Some("update") => "sessions.update",
                Some("recycle") => "sessions.recycle",
                Some("restore") => "sessions.restore",
                _ => return Err("Unknown session action".into()),
            };
            host.request(request, params, Value::Object(body)).await
        }
        _ => Err("Unknown Agent Management tool".into()),
    }
}

async fn skills<H: Host + Sync>(host: &H, args: &Arguments<'_>) -> Result<Value, String> {
    let agent = args.id("agent_id")?;
    match args.text("action") {
        Some("list") => host.request("skills.list", json!({"agent_id": agent}), json!({})).await,
        Some("read") => {
            let params = json!({"agent_id": agent, "name": args.id("name")?});
            host.request("skills.read", params, json!({})).await
        }
        Some(action @ ("save" | "remove")) => {
            let params = json!({"agent_id": agent, "name": args.id("name")?});
            let revision = args
                .integer("expected_revision")
                .ok_or("Skill revision required")?;
            let body = json!({"description": args.text("description"),
                "instructions": args.text("instructions"), "expected_revision": revision});
            let request = if action == "save" { "skills.save" } else { "skills.remove" };
            host.request(request, params, body).await
        }
        _ => Err("Unknown skill action".into()),
    }
}

async fn chat_config<H: Host + Sync>(host: &H, args: &Arguments<'_>) -> Result<Value, String> {
    let document = match args.text("document") {
        Some(document @ ("chat" | "session_dropdown")) => document,
        _ => return Err("Unknown chat configuration document".into()),
    };
    let params = json!({"agent_id": args.id("agent_id")?, "document": document});
    let revision = || {
        args.integer("expected_revision")
            .ok_or("Chat configuration revision required")
    };
    match args.text("action") {
        Some("read") => host.request("agents.chat_config.read", params, json!({})).await,
        Some("patch") => {
            let patch = args.value("patch").ok_or("Chat configuration patch required")?;
            let body = json!({"patch": patch, "expected_revision": revision()?});
            host.request("agents.chat_config.patch", params, body).await
        }
        Some("reset") => {
            let body = json!({"expected_revision": revision()?});
            host.request("agents.chat_config.reset", params, body).await
        }
        _ => Err("Unknown chat configuration action".into()),
    }
}

/// Watch another of the caller's sessions: the host re-authorizes the call and its
/// Automation dependency before subscribing this session to it.
async fn watch<H: Host + Sync>(host: &H, args: &Arguments<'_>, now_ms: u64) -> Result<Value, String> {
    let settings = host.request("ability.settings", json!({}), json!({})).await?;
    if settings["allow_session_watch"] == false {
        return Err("Session watching is disabled".into());
    }
    let target = args.session("session_id")?;
    host.request("sessions.read", json!({"session_id": target}), json!({}))
        .await?;
    let prompt = args
        .text("prompt")
        .filter(|prompt| !prompt.trim().is_empty() && prompt.len() <= MAX_PROMPT)
        .ok_or("Bounded watch prompt required")?;
    let expires = args
        .integer("expires_ms")
        .and_then(|value| u64::try_from(value).ok())
        .filter(|value| *value > now_ms && *value <= now_ms.saturating_add(MAX_WATCH_MS))
        .ok_or("Watch expiry must be within 30 days")?;
    let body = json!({"prompt": prompt, "expires_ms": expires});
    host.request("sessions.watch", json!({"session_id": target}), body)
        .await
}

/// A tool's arguments; unknown names are refused like the schemas do.
struct Arguments<'a>(&'a Map<String, Value>);

impl<'a> Arguments<'a> {
    fn parse(value: &'a Value) -> Result<Self, String> {
        let object = value.as_object().ok_or("Arguments must be an object")?;
        if let Some(unknown) = object.keys().find(|key| !ARGUMENTS.contains(&key.as_str())) {
            return Err(format!("Unknown argument: {unknown}"));
        }
        Ok(Self(object))
    }

    fn value(&self, name: &str) -> Option<&'a Value> {
        self.0.get(name).filter(|value| !value.is_null())
    }

    fn text(&self, name: &str) -> Option<&'a str> {
        self.value(name).and_then(Value::as_str)
    }

    fn integer(&self, name: &str) -> Option<i64> {
        self.value(name).and_then(Value::as_i64)
    }

    /// The `request` body: an object, empty when omitted.
    fn object(&self, name: &str) -> Result<Map<String, Value>, String> {
        match self.value(name) {
            None => Ok(Map::new()),
            Some(Value::Object(object)) => Ok(object.clone()),
            Some(_) => Err("Request must be an object".into()),
        }
    }

    /// An identifier that stays inside one route segment.
    fn id(&self, name: &str) -> Result<&'a str, String> {
        let value = self.text(name).ok_or("Required identifier missing")?;
        if valid_id(value) {
            Ok(value)
        } else {
            Err("Invalid identifier".into())
        }
    }

    fn session(&self, name: &str) -> Result<&'a str, String> {
        let value = self.id(name)?;
        if valid_uuid(value) {
            Ok(value)
        } else {
            Err("Invalid session identifier".into())
        }
    }
}

/// 1–128 ASCII letters, digits, `_` or `-`.
pub fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

/// A hyphenated UUID.
pub fn valid_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
}

/// Run a future to completion on this thread. Plugin host requests answer synchronously
/// in the standalone process, so a waker that does nothing is enough.
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
    use super::*;
    use std::sync::Mutex;

    /// Records requests and answers each from a fixed table.
    struct Fake {
        seen: Mutex<Vec<(String, Value, Value)>>,
        settings: Value,
    }

    impl Fake {
        fn new() -> Self {
            Self { seen: Mutex::new(Vec::new()), settings: json!({}) }
        }
        fn seen(&self) -> Vec<(String, Value, Value)> {
            self.seen.lock().map(|seen| seen.clone()).unwrap_or_default()
        }
    }

    impl Host for Fake {
        fn request(&self, name: &str, params: Value, body: Value) -> impl Future<Output = Result<Value, String>> + Send {
            if let Ok(mut seen) = self.seen.lock() {
                seen.push((name.to_owned(), params, body));
            }
            let reply = if name == "ability.settings" { self.settings.clone() } else { json!({"ok": name}) };
            std::future::ready(Ok(reply))
        }
    }

    fn run(host: &Fake, tool: &str, arguments: Value) -> Result<Value, String> {
        let call = Call { tool, arguments: &arguments, request_id: "req-1", now_ms: 1_000 };
        crate::block_on(super::call(host, call))
    }

    const SESSION: &str = "0b7c1d2e-3f40-4a5b-8c6d-7e8f90a1b2c3";

    #[test]
    fn tools_make_only_declared_requests_with_fixed_names() {
        let host = Fake::new();
        run(&host, "get_agent", json!({"agent_id":"agent-a"})).unwrap();
        run(&host, "set_agent_tool", json!({"agent_id":"a","ability_id":"b","tool":"c","request":{"mode":"off"}})).unwrap();
        run(&host, "create_agent", json!({"request":{"name":"New"}})).unwrap();
        run(&host, "manage_user_session", json!({"session_id":SESSION,"action":"recycle","request":{"expected_revision":3}})).unwrap();
        let seen = host.seen();
        assert!(seen.iter().all(|(name, _, _)| REQUESTS.contains(&name.as_str())));
        assert_eq!(seen[0], ("agents.identity.read".into(), json!({"agent_id":"agent-a"}), json!({})));
        assert_eq!(seen[1].1, json!({"agent_id":"a","ability_id":"b","tool":"c"}));
        assert_eq!(seen[2].2, json!({"name":"New","client_request_id":"req-1"}));
        assert_eq!(seen[3].0, "sessions.recycle");
        assert_eq!(seen[3].2, json!({"expected_revision":3,"client_request_id":"req-1"}));
    }

    #[test]
    fn identifiers_cannot_escape_a_route_segment() {
        let host = Fake::new();
        for value in ["../accounts", "x?admin=1", "x/y", "x%2fy", ""] {
            assert!(run(&host, "get_agent", json!({"agent_id": value})).is_err(), "{value}");
        }
        assert!(run(&host, "manage_user_session", json!({"session_id":"agent-a","action":"update"})).is_err());
        assert!(run(&host, "get_agent", json!({"agent_id":"a","url":"/admin"})).is_err());
        assert!(host.seen().is_empty());
    }

    #[test]
    fn session_watch_checks_the_setting_bounds_and_target_first() {
        let host = Fake::new();
        let watch = |expires: u64| json!({"session_id":SESSION,"prompt":"Tell me","expires_ms":expires});
        run(&host, "watch_sessions", watch(2_000)).unwrap();
        let names: Vec<String> = host.seen().into_iter().map(|(name, _, _)| name).collect();
        assert_eq!(names, ["ability.settings", "sessions.read", "sessions.watch"]);
        assert!(run(&host, "watch_sessions", watch(1_000 + MAX_WATCH_MS + 1)).is_err());
        let off = Fake { settings: json!({"allow_session_watch": false}), ..Fake::new() };
        assert_eq!(run(&off, "watch_sessions", watch(2_000)), Err("Session watching is disabled".into()));
        assert_eq!(off.seen().len(), 1);
    }

    #[test]
    fn skills_and_chat_config_need_revisions_for_writes() {
        let host = Fake::new();
        assert!(run(&host, "manage_agent_skills", json!({"agent_id":"a","action":"save","name":"s"})).is_err());
        run(&host, "manage_agent_skills", json!({"agent_id":"a","action":"save","name":"s","instructions":"Do","expected_revision":0})).unwrap();
        assert!(run(&host, "manage_agent_chat_config", json!({"agent_id":"a","document":"chat","action":"reset"})).is_err());
        assert!(run(&host, "manage_agent_chat_config", json!({"agent_id":"a","document":"other","action":"read"})).is_err());
        run(&host, "manage_agent_chat_config", json!({"agent_id":"a","document":"chat","action":"patch","patch":{"x":1},"expected_revision":4})).unwrap();
        let seen = host.seen();
        assert_eq!(seen[0], ("skills.save".into(), json!({"agent_id":"a","name":"s"}),
            json!({"description":null,"instructions":"Do","expected_revision":0})));
        assert_eq!(seen[1].2, json!({"patch":{"x":1},"expected_revision":4}));
    }
}
