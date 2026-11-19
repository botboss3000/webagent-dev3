//! Configuration Controller ability (`abilities/Core/config_controller`): the
//! configuration composer from an agent conversation. `find_setting` asks the composer which
//! setting a request means and what it would change. Saving is split by layer so each has its
//! own permission: `change_user_setting` (the user's own), `change_agent_setting` (an agent the
//! user manages) and `change_app_setting` (everyone; administrators). All go through the same
//! apply path as the API and act as the chat's signed-in user, so the composer's own page checks
//! decide what they may see and change.
use anyhow::{anyhow, Result};
use axum::{Extension, Json};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use tokio_util::sync::CancellationToken;
use webagent_abilities::tool_arguments::read;
use webagent_accounts_host::AccountsStore;

use crate::{api, shared_chat, App, State};

/// Tools this adapter executes; their schemas live in the ability's `tools/<tool>/<tool>.tool.json`.
pub const TOOLS: &[&str] = &[
    "find_setting",
    "change_user_setting",
    "change_agent_setting",
    "change_app_setting",
];

/// True when this adapter executes tool `name`.
pub fn owns(name: &str) -> bool {
    TOOLS.contains(&name)
}

/// `find_setting`'s arguments.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FindArgs {
    request: String,
    layer: String,
    #[serde(default)]
    agent: Option<String>,
}

/// The `change_*_setting` arguments; `agent` is declared by `change_agent_setting` only.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChangeArgs {
    #[serde(default)]
    agent: Option<String>,
    setting: String,
    value: Value,
    #[serde(default)]
    params: Option<Map<String, Value>>,
    revision: Map<String, Value>,
}

/// Argument types of this adapter's tools, checked against their tool.json by the root test.
#[cfg(test)]
pub(crate) const ARGUMENT_TYPES: &[webagent_abilities::tool_arguments::ArgumentType] = &[
    webagent_abilities::argument_type!("find_setting", FindArgs),
    webagent_abilities::argument_type!("change_user_setting", ChangeArgs),
    webagent_abilities::argument_type!("change_agent_setting", ChangeArgs),
    webagent_abilities::argument_type!("change_app_setting", ChangeArgs),
];

/// Model-facing schemas for [`TOOLS`].
pub fn schemas() -> Vec<Value> {
    crate::tools::folder_schemas(TOOLS)
}

/// Run one call after the live policy and approval checks.
#[allow(clippy::too_many_arguments)]
pub async fn execute(
    app: &App,
    sid: &str,
    rid: &str,
    generation: &str,
    pending: &str,
    name: &str,
    args: &Value,
    cancel: CancellationToken,
) -> Result<Value> {
    super::core_authority::check(app, sid, rid, generation, pending, name, args, &cancel).await?;
    let actor = shared_chat::actor(app).to_owned();
    let account = app
        .store
        .account_by_id(&actor)
        .await?
        .ok_or_else(|| anyhow!("Account unavailable"))?;
    let principal = crate::access::Principal::from_account(&account)?;
    let identity = json!({"_user_id": actor, "_role": principal.role});
    let mut caller = app.clone();
    caller.store = app.store.for_user(&actor).await?;
    // The composer reads only these fields; the page is added below.
    let (layer, agent, mut body) = if name == "find_setting" {
        let find: FindArgs = read(name, args)?;
        (find.layer, find.agent, json!({"text": find.request}))
    } else {
        let change: ChangeArgs = read(name, args)?;
        let layer = match name {
            "change_user_setting" => "user",
            "change_agent_setting" => "agent",
            "change_app_setting" => "app",
            _ => "",
        };
        let mut body = json!({"setting": change.setting, "value": change.value,
            "revision": change.revision});
        if let Some(params) = change.params {
            body["params"] = json!(params);
        }
        (layer.to_owned(), change.agent, body)
    };
    // The agent layer is this conversation's agent unless the call names another; the page
    // check still requires the user to manage it.
    let page = match layer.as_str() {
        "agent" => match agent {
            Some(agent) => format!("agent:{agent}"),
            None => format!("agent:{}", crate::session_agent_id(app, sid).await?),
        },
        "app" | "user" => layer.clone(),
        _ => return Ok(json!({"status": "error", "output": "layer must be app, agent or user"})),
    };
    body["page"] = json!(page);
    let answer = match name {
        "find_setting" => {
            api::composer::propose(State(caller), Extension(identity), Json(body)).await
        }
        _ => api::composer::apply(State(caller), Extension(identity), Json(body)).await,
    };
    Ok(match answer {
        Ok(Json(output)) => json!({"status": "success", "output": output}),
        Err(error) => json!({"status": "error", "output": error.1}),
    })
}
