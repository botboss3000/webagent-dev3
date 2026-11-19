//! Base credential vault tools: human-entered generic credentials remain encrypted and are
//! never returned to models.
use crate::{Call, NativeHost};
use anyhow::{anyhow, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

#[path = "../check_credential/check_credential.rs"]
mod check_credential;
#[path = "../list_vault_keys/list_vault_keys.rs"]
mod list_vault_keys;
#[path = "../request_credential/request_credential.rs"]
mod request_credential;

pub use request_credential::{create_url, origin};
pub use webagent_abilities_host::valid_credential_name as valid_name;

#[must_use]
pub fn owns(name: &str) -> bool {
    matches!(
        name,
        "check_credential" | "request_credential" | "list_vault_keys"
    )
}
/// Built-in Base tools this module executes; each one's schema and effect live in its
/// `abilities/Core/base/tools/<tool>/<tool>.tool.json`.
pub const TOOLS: &[&str] = &["check_credential", "request_credential", "list_vault_keys"];
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Args {
    pub(super) name: Option<String>,
    pub(super) origin: Option<String>,
    pub(super) label: Option<String>,
    pub(super) create_url: Option<String>,
}
/// Argument types of this family's tools, checked against their tool.json by the crate test.
pub const ARGUMENT_TYPES: &[webagent_abilities::tool_arguments::ArgumentType] = &[
    webagent_abilities::argument_type!("check_credential", Args),
    webagent_abilities::argument_type!("list_vault_keys", Args),
    webagent_abilities::argument_type!("request_credential", Args),
];
pub(super) fn public(name: &str, record: &Value) -> Value {
    json!({"name":name,"configured":record["value"].as_str().is_some_and(|s|!s.is_empty()),"revision":record["revision"].as_u64().unwrap_or(0),"origin":record["origin"]})
}
/// Runs one credential tool in an owner-private conversation; secrets are never returned.
pub async fn execute(
    host: &dyn NativeHost,
    call: Call<'_>,
    name: &str,
    args: &Value,
    cancel: CancellationToken,
) -> Result<Value> {
    let tool = name;
    let run = host.authorize(call, tool, args, &cancel).await?;
    let store = host.store();
    if host.actor() != store.current_user_id() {
        return Err(anyhow!(
            "Use an owner-private conversation for private credentials"
        ));
    }
    let q: Args = serde_json::from_value(args.clone())?;
    if tool == "list_vault_keys" {
        return list_vault_keys::list(host).await;
    }
    let name = q
        .name
        .as_deref()
        .filter(|s| valid_name(s))
        .ok_or_else(|| anyhow!("Plain credential name required"))?;
    let prior = store
        .get(&format!("core_vault:{name}"))
        .await?
        .unwrap_or(Value::Null);
    if tool == "check_credential" {
        return Ok(check_credential::run(name, &prior));
    }
    if tool != "request_credential" {
        return Err(anyhow!("Unknown credential tool"));
    }
    request_credential::request(host, call, &run, args, &cancel, name, &prior, &q).await
}
