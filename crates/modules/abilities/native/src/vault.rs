//! Human-entered generic credentials remain encrypted and are never returned to mini apps or models.
use crate::{Call, NativeHost};
use anyhow::{anyhow, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
use webagent_host_core::{digest, now};

pub use webagent_abilities_host::valid_credential_name as valid_name;

#[must_use]
pub fn owns(name: &str) -> bool {
    matches!(
        name,
        "check_credential" | "request_credential" | "list_vault_keys"
    )
}
/// Tools this module executes; their schemas live in the ability's `tools/<tool>/tool.json`.
pub const TOOLS: &[&str] = &["check_credential", "request_credential", "list_vault_keys"];
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    name: Option<String>,
    origin: Option<String>,
    label: Option<String>,
    create_url: Option<String>,
}
/// Optional display link for the credential card: HTTPS, no embedded credentials.
pub fn create_url(raw: &str) -> Result<String> {
    let url = reqwest::Url::parse(raw)?;
    if raw.len() > 500
        || url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(anyhow!(
            "Use an HTTPS create_url without embedded credentials"
        ));
    }
    Ok(url.to_string())
}
pub fn origin(raw: &str) -> Result<String> {
    let url = reqwest::Url::parse(raw)?;
    if raw.len() > 500
        || url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(anyhow!(
            "Use a plain HTTPS service origin without credentials, path, query or fragment"
        ));
    }
    Ok(url.origin().ascii_serialization())
}
fn public(name: &str, record: &Value) -> Value {
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
    let (sid, rid, pending) = (call.sid, call.run_id, call.pending_id);
    let run = host.authorize(call, tool, args, &cancel).await?;
    let store = host.store();
    if host.actor() != store.current_user_id() {
        return Err(anyhow!(
            "Use an owner-private conversation for private credentials"
        ));
    }
    let q: Args = serde_json::from_value(args.clone())?;
    if tool == "list_vault_keys" {
        let keys = store
            .list("core_vault:")
            .await?
            .iter()
            .filter_map(|v| v["name"].as_str().map(|name| public(name, v)))
            .take(100)
            .collect::<Vec<_>>();
        return Ok(json!({"status":"success","output":{"keys":keys}}));
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
        return Ok(json!({"status":"success","output":public(name,&prior)}));
    }
    if tool != "request_credential" {
        return Err(anyhow!("Unknown credential tool"));
    }
    let origin = origin(
        q.origin
            .as_deref()
            .ok_or_else(|| anyhow!("HTTPS service origin required"))?,
    )?;
    let label = q.label.as_deref().map(str::trim).unwrap_or_default();
    if label.chars().count() > 80 || label.chars().any(char::is_control) {
        return Err(anyhow!("Use a short plain credential label"));
    }
    let link = q.create_url.as_deref().map(create_url).transpose()?;
    let _guard = host.gate().lock().await;
    host.authorize(call, tool, args, &cancel).await?;
    let mut saved = store.module_state(&format!("core_ui:{sid}")).await?;
    let request_id = digest(&json!([sid, rid, pending]));
    if saved["state"]["request_id"] != request_id {
        saved["state"] = json!({"request_id":request_id,"destination":"credential","actor_id":host.actor(),"agent_id":run["agent_id"],"session_id":sid,"created_at":now(),"credential_name":name,"credential_origin":origin,"credential_label":label,"credential_url":link,"expected_revision":prior["revision"].as_u64().unwrap_or(0),"run_id":rid});
        store
            .module_cas(
                &format!("core_ui:{sid}"),
                saved["revision"].as_u64().unwrap_or(0),
                &saved,
            )
            .await?;
    }
    Ok(
        json!({"status":"success","output":{"requested":true,"name":name,"origin":origin,"delivery":"private_human_form","secret_returned":false}}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn create_links_are_https_without_credentials() {
        assert_eq!(
            create_url("https://github.com/settings/personal-access-tokens/new").unwrap(),
            "https://github.com/settings/personal-access-tokens/new"
        );
        for value in [
            "javascript:alert(1)",
            "http://example.test/new",
            concat!("https://", "user", ":", "pass", "@example.test/new"),
        ] {
            assert!(create_url(value).is_err());
        }
    }
    #[test]
    fn service_origins_do_not_smuggle_paths_credentials_or_javascript() {
        assert_eq!(
            origin("https://example.test").unwrap(),
            "https://example.test"
        );
        for value in [
            "javascript:alert(1)",
            "http://example.test",
            concat!("https://", "user", ":", "pass", "@example.test"),
            "https://example.test/path",
            "https://example.test?x=1",
        ] {
            assert!(origin(value).is_err());
        }
    }
}
