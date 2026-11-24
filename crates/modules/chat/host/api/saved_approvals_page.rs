//! The agent's Permissions & Actions page: the caller's own tool-use counters and saved approvals for one agent
//! (`webagent_access::saved_approvals`), each marked lapsed when the tool's permission changed,
//! and the owner's revoke, change and approve-again edits. Only the caller's own entries are
//! read or changed, administrators included. Writes hold the app gate, as the approval
//! endpoint that adds entries does, and check the document revision the page was drawn from.
use crate::*;
use axum::Extension;
use serde::Deserialize;
use webagent_access::saved_approvals::{self as saved, Choice, Edit};

/// The caller's id, store and live access for `agent`, once they may use it.
async fn owner_context(
    app: &App,
    identity: &Value,
    agent: &str,
) -> std::result::Result<(String, Store, access::authority::LiveAccess), ApiError> {
    let store = crate::tools::core_scripts::page_store(app, identity, agent).await?;
    let actor = identity["_user_id"].as_str().unwrap_or("").to_owned();
    let access = app
        .store
        .live_access(&actor, agent)
        .await
        .map_err(|_| conflict("Access unavailable"))?;
    Ok((actor, store, access))
}

/// The tool's permission fingerprint now, or `None` when it can no longer be remembered.
fn fingerprint(app: &App, access: &access::authority::LiveAccess, tool: &str) -> Option<String> {
    let critical = webagent_abilities::tool_definitions::get(tool).is_some_and(|t| t.critical);
    if !saved::rememberable_tool(tool, critical) {
        return None;
    }
    let abilities = app
        .abilities
        .tool_abilities(tool, &Value::Null, &Value::Null, &Value::Null);
    Some(access.tool_permission_fingerprint(tool, &abilities))
}

/// GET: the caller's saved approvals for one agent, lapsed ones flagged.
pub async fn read(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path(agent): Path<String>,
) -> Api {
    let (actor, store, access) = owner_context(&app, &identity, &agent).await?;
    let doc = store.get(&saved::key(&agent)).await?.unwrap_or(Value::Null);
    let current = |tool: &str| fingerprint(&app, &access, tool);
    let settings = access.saved_approvals;
    Ok(Json(
        json!({"agent_id":agent,"revision":doc["revision"].as_u64().unwrap_or(0),
        "enabled":settings.enabled,"agent_wide":settings.agent_wide,
        "entries":saved::view(&doc, &actor, &current)}),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Change {
    expected_revision: u64,
    /// "revoke", "change" (with `match` and `reach`) or "reapprove".
    action: String,
    #[serde(rename = "match")]
    match_: Option<String>,
    reach: Option<String>,
}

/// POST: revoke, change the match or reach of, or approve again one of the caller's entries.
pub async fn change(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path((agent, id)): Path<(String, String)>,
    Json(body): Json<Change>,
) -> Api {
    let (actor, store, access) = owner_context(&app, &identity, &agent).await?;
    let edit = match body.action.as_str() {
        "revoke" => Edit::Revoke,
        "change" => Edit::Change(
            Choice::parse(&json!({"match":body.match_,"reach":body.reach}))
                .map_err(|error| bad(&error.to_string()))?,
        ),
        "reapprove" => Edit::Reapprove(String::new()),
        _ => return Err(bad("Unknown saved approval action")),
    };
    let _guard = app.gate.lock().await;
    let key = saved::key(&agent);
    let doc = store.get(&key).await?.unwrap_or(Value::Null);
    if doc["revision"].as_u64().unwrap_or(0) != body.expected_revision {
        return Err(conflict("Saved approvals changed; reload the page"));
    }
    let edit = match edit {
        Edit::Reapprove(_) => {
            let tool = doc["entries"]
                .as_array()
                .and_then(|entries| entries.iter().find(|entry| entry["id"] == id.as_str()))
                .and_then(|entry| entry["tool"].as_str())
                .unwrap_or("");
            let Some(now) = fingerprint(&app, &access, tool) else {
                return Err(conflict("This tool can no longer be remembered"));
            };
            Edit::Reapprove(now)
        }
        other => other,
    };
    let next = saved::edit(&doc, &actor, &id, edit, access.saved_approvals)
        .map_err(|error| conflict(&error.to_string()))?;
    store.put(&key, &next).await?;
    Ok(Json(json!({"revision":next["revision"]})))
}

/// GET: the caller's tool-use counters for one agent (calls, approvals asked, saved approvals
/// used, last used), written once as each run ends.
pub async fn tool_use(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path(agent): Path<String>,
) -> Api {
    let store = crate::tools::core_scripts::page_store(&app, &identity, &agent).await?;
    Ok(Json(
        json!({"agent_id":agent,"tools":store.tool_use(&agent).await?}),
    ))
}
