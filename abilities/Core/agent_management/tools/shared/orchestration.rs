//! Agent Management's session and sub agent tools over owner-private conversations and
//! normal owning API admission. Starting and messaging sessions stay in session
//! messaging, which calls [`in_scope`], [`allow_delegation`] and [`spawn_limits`] here.
use std::collections::HashMap;

use crate::{Call, NativeHost};
use anyhow::{anyhow, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use webagent_access::authority::LiveAccess;
use webagent_access::storage::Repository;
use webagent_host_core::{digest, key, StoreCore};

/// Settings key of the ability that owns these tools.
const ABILITY: &str = "agent_management";

#[path = "../list_delegatable_agents/list_delegatable_agents.rs"]
mod list_delegatable_agents;
#[path = "../list_sessions/list_sessions.rs"]
mod list_sessions;
#[path = "../promote_spawn/promote_spawn.rs"]
mod promote_spawn;
#[path = "../read_session/read_session.rs"]
mod read_session;
#[path = "../run_optimizer/run_optimizer.rs"]
mod run_optimizer;
#[path = "../search_sessions/search_sessions.rs"]
mod search_sessions;
#[path = "../start_spawn/start_spawn.rs"]
mod start_spawn;
#[path = "../stop_session/stop_session.rs"]
mod stop_session;

pub use promote_spawn::require_child;
pub use start_spawn::spawn_limits;

/// Whether this family executes `name`; the start and messaging tools in [`TOOLS`] run
/// through session messaging.
#[must_use]
pub fn owns(name: &str) -> bool {
    matches!(
        name,
        "list_sessions"
            | "read_session"
            | "search_sessions"
            | "stop_session"
            | "list_delegatable_agents"
            | "run_optimizer"
            | "promote_spawn"
    )
}

/// Tools this adapter executes; their schemas live in the ability's `tools/<tool>/<tool>.tool.json`.
pub const TOOLS: &[&str] = &[
    "start_spawn",
    "start_session",
    "delegate_to_agent",
    "message_session",
    "list_sessions",
    "read_session",
    "search_sessions",
    "stop_session",
    "list_delegatable_agents",
    "promote_spawn",
    "run_optimizer",
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    session_id: Option<String>,
    from: Option<usize>,
    to: Option<usize>,
    all: Option<bool>,
    detail: Option<String>,
    cursor: Option<usize>,
    limit: Option<usize>,
    query: Option<String>,
    prompt: Option<String>,
    purpose: Option<String>,
    name: Option<String>,
    description: Option<String>,
    instructions: Option<String>,
}
/// Argument types of this family's tools, checked against their tool.json by the crate test.
pub const ARGUMENT_TYPES: &[webagent_abilities::tool_arguments::ArgumentType] = &[
    webagent_abilities::argument_type!("list_delegatable_agents", Args),
    webagent_abilities::argument_type!("list_sessions", Args),
    webagent_abilities::argument_type!("promote_spawn", Args),
    webagent_abilities::argument_type!("read_session", Args),
    webagent_abilities::argument_type!("run_optimizer", Args),
    webagent_abilities::argument_type!("search_sessions", Args),
    webagent_abilities::argument_type!("stop_session", Args),
];

/// The access repository bound to `store`'s agent plane.
fn access(store: &StoreCore) -> Repository<'_, StoreCore> {
    Repository::new(
        &store.root,
        &store.app_pool,
        &store.agent_pool,
        &store.agent_id,
        &store.user_id,
        store,
    )
}

/// `user`'s live access to `agent`, as the root store's `live_access` resolves it.
async fn live_access(host: &dyn NativeHost, user: &str, agent: &str) -> Result<LiveAccess> {
    webagent_access::authority::validate_live_access(user, agent)?;
    let store = host.agent_store(agent).await?;
    access(&store).live_access(user, agent).await
}

/// `user`'s authority over `agent`, as the root store's `agent_authority` resolves it.
async fn agent_authority(host: &dyn NativeHost, user: &str, agent: &str) -> Result<Value> {
    let store = host.agent_store(agent).await?;
    access(&store).agent_authority(user, agent).await
}

/// Whether `agent` is owner-private (not shareable, viewable or joinable by
/// administrators) and admitted for the storage owner.
async fn private_admitted(host: &dyn NativeHost, agent: &str, admit: bool) -> Result<bool> {
    let policy = host.shared_chat_policy(agent).await?;
    Ok(admit
        && !policy.shareable
        && !policy.admin_view
        && !policy.admin_join
        && live_access(host, host.store().current_user_id(), agent)
            .await?
            .admitted())
}

fn owner_required(host: &dyn NativeHost) -> Result<()> {
    if host.actor() != host.store().current_user_id() {
        return Err(anyhow!("Owner-private orchestration required"));
    }
    Ok(())
}

/// Session `sid`'s Agent Management settings and agent.
async fn settings(host: &dyn NativeHost, sid: &str) -> Result<(Value, String)> {
    let (_, visibility, _) = host.ability_controls(sid).await?;
    let agent = host
        .store()
        .get(&key("session", sid))
        .await?
        .and_then(|s| s["agent_id"].as_str().map(str::to_owned))
        .unwrap_or_else(|| "shared_default".into());
    Ok((visibility["settings"][ABILITY].clone(), agent))
}

/// Whether session `sid` may start sessions of other agents. The Default Agent may unless
/// its settings say otherwise.
pub async fn allow_delegation(host: &dyn NativeHost, sid: &str) -> Result<bool> {
    let (settings, agent) = settings(host, sid).await?;
    Ok(webagent_abilities::controls::delegation_allowed(
        &settings, &agent,
    ))
}

/// Whether session `sid` reaches every owner-private session, not only those it started.
async fn owned_scope(host: &dyn NativeHost, sid: &str) -> Result<bool> {
    let (settings, agent) = settings(host, sid).await?;
    Ok(settings["session_scope"]
        .as_str()
        .map_or(agent == "shared_default", |scope| scope == "owned"))
}

/// How `session` relates to session `sid`: its sub agent or a session it started.
fn relation(sid: &str, session: &Value) -> Option<&'static str> {
    if session["parent_session_id"] == sid {
        Some("spawn")
    } else if session["started_by_session_id"] == sid {
        Some("started_here")
    } else {
        None
    }
}

/// Session `target` when session `sid` may reach it: one it started, or with the owned
/// scope any owner-private session. Archived sessions are included for unarchiving.
pub async fn in_scope(host: &dyn NativeHost, sid: &str, target: &str) -> Result<Value> {
    owner_required(host)?;
    Uuid::parse_str(target)?;
    if target == sid {
        return Err(anyhow!("Use the current conversation directly"));
    }
    let session = host
        .store()
        .get(&key("session", target))
        .await?
        .filter(|s| matches!(s["status"].as_str(), Some("active" | "archived")))
        .ok_or_else(|| anyhow!("Session unavailable"))?;
    let agent = session["agent_id"].as_str().unwrap_or("shared_default");
    if !private_admitted(host, agent, true).await? {
        return Err(anyhow!("Session unavailable"));
    }
    if relation(sid, &session).is_none() && !owned_scope(host, sid).await? {
        return Err(anyhow!(
            "Session is outside this agent's scope: it reaches only sessions this conversation started"
        ));
    }
    Ok(session)
}

/// Active owner-private sub agents of session `sid`.
pub async fn children(host: &dyn NativeHost, sid: &str) -> Result<Vec<Value>> {
    owner_required(host)?;
    let mut items = Vec::new();
    for session in host.store().list("session:").await? {
        if session["parent_session_id"] == sid && session["status"] == "active" {
            let agent = session["agent_id"].as_str().unwrap_or("shared_default");
            if private_admitted(host, agent, true).await? {
                items.push(session);
            }
        }
    }
    Ok(items)
}

/// The run status of `sid`'s unfinished run, or `idle`.
fn run_status(host: &dyn NativeHost, active: &[Value], sid: &str) -> String {
    active
        .iter()
        .find(|r| r["session_id"] == sid && !host.terminal(r["status"].as_str().unwrap_or("")))
        .and_then(|r| r["status"].as_str())
        .unwrap_or("idle")
        .to_owned()
}

/// The active sessions other than `sid` that `sid` may reach, newest first, as list rows
/// without run status, and whether the owned scope applies.
async fn reachable(host: &dyn NativeHost, sid: &str) -> Result<(Vec<Value>, bool)> {
    owner_required(host)?;
    let owned = owned_scope(host, sid).await?;
    let mut admitted: HashMap<String, bool> = HashMap::new();
    let mut rows = Vec::new();
    for session in host.store().list("session:").await? {
        let id = session["id"].as_str().unwrap_or("");
        if id == sid || session["status"] != "active" || session["module_silent"] == true {
            continue;
        }
        let relation = relation(sid, &session);
        if relation.is_none() && !owned {
            continue;
        }
        let agent = session["agent_id"]
            .as_str()
            .unwrap_or("shared_default")
            .to_owned();
        let allowed = match admitted.get(&agent) {
            Some(allowed) => *allowed,
            None => {
                let allowed = private_admitted(host, &agent, true).await?;
                admitted.insert(agent.clone(), allowed);
                allowed
            }
        };
        if allowed {
            rows.push(json!({"session_id":id,"title":session["title"],"agent_id":agent,
                "last_active_at":session["last_active_at"].as_i64().or(session["created_at"].as_i64()),
                "relation":relation.unwrap_or("owned")}));
        }
    }
    rows.sort_by_key(|r| std::cmp::Reverse(r["last_active_at"].as_i64().unwrap_or(0)));
    Ok((rows, owned))
}
/// `value` as text cut to `limit` characters, marking a cut.
fn clip(value: &Value, limit: usize) -> Value {
    let text = value
        .as_str()
        .map_or_else(|| value.to_string(), str::to_owned);
    if text.chars().count() <= limit {
        return json!(text);
    }
    json!(format!(
        "{}… [cut]",
        text.chars().take(limit).collect::<String>()
    ))
}

/// Text of a message interaction item.
fn message_text(item: &Value) -> String {
    item["content"].as_str().map_or_else(
        || {
            item["content"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|p| p["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n")
        },
        str::to_owned,
    )
}

/// Number interactions into turns: a user message, an agent reply, or a tool call with
/// its result.
fn turns(items: &[(i64, String, Value)], limit: usize) -> Vec<Value> {
    let mut turns: Vec<Value> = Vec::new();
    let mut calls: HashMap<String, usize> = HashMap::new();
    for (_, run, item) in items {
        match item["type"].as_str().unwrap_or("") {
            "message" => {
                let kind = if item["role"] == "user" {
                    "user"
                } else {
                    "agent"
                };
                turns.push(json!({"turn":turns.len()+1,"run_id":run,"kind":kind,
                    "text":clip(&json!(message_text(item)),limit)}));
            }
            "function_call" => {
                if let Some(id) = item["call_id"].as_str() {
                    calls.insert(id.to_owned(), turns.len());
                }
                turns.push(json!({"turn":turns.len()+1,"run_id":run,"kind":"tool",
                    "tool":item["name"],"arguments":clip(&item["arguments"],limit),"result":null}));
            }
            "function_call_output" => {
                if let Some(at) = item["call_id"].as_str().and_then(|id| calls.get(id)) {
                    turns[*at]["result"] = clip(&item["output"], limit);
                }
            }
            _ => {}
        }
    }
    turns
}

/// Run tool `name` with `args` at `call`'s position.
pub async fn execute(
    host: &dyn NativeHost,
    call: Call<'_>,
    name: &str,
    args: &Value,
    cancel: CancellationToken,
) -> Result<Value> {
    let Call {
        sid,
        run_id: rid,
        pending_id: pending,
        ..
    } = call;
    let run = host.authorize(call, name, args, &cancel).await?;
    owner_required(host)?;
    let q: Args = serde_json::from_value(args.clone())?;
    let request_id = digest(&json!([sid, rid, pending]));
    let output = match name {
        "list_sessions" => list_sessions::run(host, sid, &q).await?,
        "search_sessions" => search_sessions::run(host, sid, &q).await?,
        "list_delegatable_agents" => list_delegatable_agents::run(host, sid).await?,
        "promote_spawn" => {
            promote_spawn::run(host, call, args, &cancel, &run, &request_id, q).await?
        }
        "run_optimizer" => run_optimizer::run(host, call, sid, name, args, &cancel, q).await?,
        "read_session" | "stop_session" => {
            let target = q
                .session_id
                .as_deref()
                .ok_or_else(|| anyhow!("Session ID required"))?;
            if name == "read_session" {
                read_session::run(host, sid, target, &q).await?
            } else {
                in_scope(host, sid, target).await?;
                let snapshot = host
                    .agent_request("GET", format!("/sessions/{target}"), json!({}))
                    .await?;
                stop_session::stop(host, target, &request_id, &snapshot).await?
            }
        }
        _ => return Err(anyhow!("Unknown Agent Management tool")),
    };
    Ok(json!({"status":"success","output":output}))
}

#[cfg(test)]
#[path = "../../tests/orchestration.rs"]
mod tests;
