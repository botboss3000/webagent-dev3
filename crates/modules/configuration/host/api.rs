//! Revision-fenced administrator updates to existing application configuration.
use super::{
    anyhow, bad, conflict, context_compaction, gate_registry, json, loop_runtime, required,
    tool_output, Api, App, Json, Path, PathBuf, Result, State, Value,
};
use anyhow::Context;
use webagent_configuration::{digest, read};
const IDS: [&str; 10] = [
    "app_settings",
    "app_ui",
    "chat",
    "prompts",
    "context_compaction",
    "loop_runtime",
    "tool_output_retention",
    "access_policy",
    "message_visibility",
    "closing_status",
];
/// Live files (checkout-relative, listed in `live-files.json`) for configuration edited in place.
pub(crate) fn live(id: &str) -> Option<&'static str> {
    Some(match id {
        "chat" => "ui/chat/chat.json",
        "app_ui" => webagent_webapp::configuration::LIVE_SETTINGS,
        "context_compaction" => "crates/modules/context/context-compaction.json",
        "tool_output_retention" => crate::tool_output::LIVE_POLICY,
        "message_visibility" => "crates/modules/engine/gates/message_visibility_v1.json",
        "closing_status" => "crates/modules/engine/gates/closing_status_v1.json",
        _ => return None,
    })
}
/// Map a known data-root configuration owner to its runtime path; never accept a caller filesystem path.
fn path(app: &App, id: &str) -> Result<PathBuf> {
    Ok(match id {
        "loop_runtime" => app.runtime_path.clone(),
        "app_settings" => app.store.root.join("config/app-settings.json"),
        "access_policy" => app.store.access_policy_path(),
        _ => anyhow::bail!("Unknown configuration"),
    })
}
const LIVE_LIMIT: u64 = 4 * 1024 * 1024;
/// The live file behind `id` for this app: fixed for most owners; the runtime file follows
/// `app.runtime_path`, which is live (relative) in installations and a file in fixtures.
fn live_path(app: &App, id: &str) -> Option<String> {
    let follows = |path: std::path::PathBuf| {
        path.is_relative()
            .then(|| path.to_string_lossy().into_owned())
    };
    if id == "loop_runtime" {
        return follows(app.runtime_path.clone());
    }
    if id == "access_policy" {
        return follows(app.store.access_policy_path());
    }
    live(id).map(str::to_owned)
}
/// Reuse each owner's domain validator before an administrator save.
pub(crate) fn validate(id: &str, value: &Value) -> Result<()> {
    if !value.is_object() {
        anyhow::bail!("Configuration must be an object");
    }
    match id {
        "app_settings" => {
            crate::store::admin_encryption_enabled(value)?;
            webagent_webapp::configuration::validate_app_settings(value)?;
        }
        "app_ui" => webagent_webapp::configuration::validate_app_settings(value)?,
        "chat" => webagent_chat::configuration::validate_chat_config(value)
            .map_err(|_| anyhow::anyhow!("Invalid chat configuration"))?,
        "loop_runtime" => {
            serde_json::from_value::<loop_runtime::Config>(value.clone())?.validate()?
        }
        "context_compaction" => {
            serde_json::from_value::<context_compaction::Config>(value.clone())?.validate()?
        }
        "tool_output_retention" => {
            serde_json::from_value::<tool_output::Policy>(value.clone())?.validate()?
        }
        "access_policy" => crate::registration::validate_configuration(value)?,
        "message_visibility" | "closing_status" => {
            let expected = if id == "message_visibility" {
                gate_registry::MESSAGE_VISIBILITY
            } else {
                gate_registry::CLOSING_STATUS
            };
            let manifest: gate_registry::Manifest = serde_json::from_value(value.clone())?;
            gate_registry::validate_manifest(expected, &manifest)?;
            // The integral gate validator checks both config and fixed tool binding.
        }
        "prompts" => validate_prompts(value)?,
        _ => anyhow::bail!("Unknown configuration"),
    }
    Ok(())
}
/// Validate base strings and optional naming, manager, and retention sections without exposing their values.
/// Published source sections cannot borrow missing naming fields from the current publication.
pub(crate) fn validate_source_prompts(value: &Value) -> Result<()> {
    if let Some(naming) = value.get("session_naming") {
        crate::session_naming::validate_source_settings(naming)?;
    }
    validate_prompts(value)
}
pub(crate) fn validate_prompts(value: &Value) -> Result<()> {
    let object = value
        .as_object()
        .ok_or_else(|| anyhow!("Invalid prompts"))?;
    let placeholder = value["tool_result_deduplication"]["placeholder"]
        .as_str()
        .context("Required duplicate-result placeholder missing")?;
    webagent_context_contracts::tool_results::validate_template(placeholder)?;
    for key in ["base_instructions", "closing_reporting"] {
        if value[key]
            .as_str()
            .is_none_or(|s| s.trim().is_empty() || s.len() > 65536)
        {
            anyhow::bail!("Invalid base prompt");
        }
    }
    for (key, v) in object {
        if let Some(text) = v.as_str() {
            if text.len() > 65536 {
                anyhow::bail!("Prompt exceeds limit");
            }
        } else if !v.is_object() {
            anyhow::bail!("Invalid prompt section");
        }
        match key.as_str() {
            "session_naming" => crate::session_naming::validate_settings(v)?,
            "manager_incoming_message" => {
                if v.as_object().is_none_or(|m| {
                    m.values()
                        .any(|v| v.as_str().is_none_or(|s| s.len() > 8192))
                }) {
                    anyhow::bail!("Invalid manager prompts");
                }
            }
            "tool_output_retention" => {
                for key in ["guidance", "placeholder"] {
                    if v[key]
                        .as_str()
                        .is_none_or(|s| s.trim().is_empty() || s.len() > 8192)
                    {
                        anyhow::bail!("Invalid retention prompt");
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}
/// Describe supported configuration owners and the boundaries at which their consumers adopt updates.
pub(crate) async fn list(State(_app): State<App>) -> Api {
    Ok(Json(
        json!({"configurations":IDS.iter().map(|id|json!({"id":id,"adoption":if *id=="loop_runtime"{"next_scheduler_tick"}else if *id=="prompts"{"next_model_call_or_new_job"}else{"next_request_or_checkpoint"}})).collect::<Vec<_>>(),
        "diagnostics":"Use the existing package-owned diagnostics configuration API",
        "infrastructure":"Use the protected operator reconfiguration workflow"}),
    ))
}
/// Return the saved configuration with a canonical digest for stale-write protection.
pub(crate) async fn get_config(State(app): State<App>, Path(id): Path<String>) -> Api {
    let value = match live_path(&app, &id) {
        Some(relative) => {
            webagent_configuration::live::json(&relative, LIVE_LIMIT, |value| validate(&id, value))?
        }
        None if id == "prompts" => crate::live_assets::resolved_prompts(&app.store.root)?,
        None => read(&path(&app, &id).map_err(|_| bad("Unknown configuration"))?)?,
    };
    Ok(Json(
        json!({"id":id,"config":value,"digest":digest(&value)}),
    ))
}
/// Validate and atomically replace configuration only when its digest still matches.
/// Live files are replaced through the broker, which also fences on the bytes read here.
pub(crate) async fn put_config(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Api {
    if id == "prompts" {
        return Err(bad("Prompts are live Markdown files under crates/modules/configuration/prompts; edit them in place"));
    }
    let expected = required(&body, "expected_digest", 64)?;
    let value = &body["config"];
    if live_path(&app, &id).is_none() {
        path(&app, &id).map_err(|_| bad("Unknown configuration"))?;
    }
    validate(&id, value).map_err(|_| bad("Invalid configuration"))?;
    let _guard = app.gate.lock().await;
    if let Some(relative) = live_path(&app, &id) {
        let relative = relative.as_str();
        let current = crate::live_files::current(relative, LIVE_LIMIT)?;
        let old: Value = serde_json::from_slice(&current).map_err(anyhow::Error::from)?;
        if digest(&old) != expected {
            return Err(conflict("Configuration changed; reload before saving"));
        }
        crate::live_files::save(&app, relative, &crate::live_files::pretty(value)?, &current)
            .await
            .map_err(|_| conflict("Configuration changed; reload before saving"))?;
        if id == "loop_runtime" {
            crate::wake::notify(&app).await;
        }
        return Ok(Json(
            json!({"id":id,"digest":digest(value),"restart_required":false}),
        ));
    }
    let path = path(&app, &id)?;
    let lock_path = match id.as_str() {
        "access_policy" => app.store.root.join("config/access-policy.lock"),
        _ => path.with_extension("json.publication.lock"),
    };
    let saved =
        webagent_configuration::update_if_current(&path, &lock_path, &expected, None, |_| {
            Ok(value.clone())
        })?;
    if saved.is_none() {
        return Err(conflict("Configuration changed; reload before saving"));
    }
    if id == "loop_runtime" {
        crate::wake::notify(&app).await;
    }
    Ok(Json(
        json!({"id":id,"digest":digest(value),"restart_required":false}),
    ))
}
/// Report only catalog identities and bounded API/loop adoption receipts.
pub(crate) async fn publication(State(app): State<App>) -> Api {
    let consumers=["api","loop"].iter().map(|role|{
        let receipt=std::fs::read(app.store.root.join("publication").join(format!("{role}.json"))).ok()
            .and_then(|b|serde_json::from_slice::<Value>(&b).ok()).unwrap_or(Value::Null);
        json!({"role":role,"revision":receipt["revision"],"catalog_version":receipt["catalog_version"]})
    }).collect::<Vec<_>>();
    Ok(Json(
        json!({"catalog_version":app.abilities.version(),"consumers":consumers,"plugin_protocol":1}),
    ))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn invalid_configuration_and_stale_digest_preserve_current() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let before = get_config(State(app.clone()), Path("loop_runtime".into()))
            .await
            .unwrap()
            .0;
        let mut value = before["config"].clone();
        value["scheduler"]["max_active_runs"] = json!(5);
        let result = put_config(
            State(app.clone()),
            Path("loop_runtime".into()),
            Json(json!({"expected_digest":before["digest"],"config":value})),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(result["restart_required"], false);
        assert!(put_config(
            State(app.clone()),
            Path("loop_runtime".into()),
            Json(json!({"expected_digest":before["digest"],"config":before["config"]}))
        )
        .await
        .is_err());
        value["scheduler"]["max_active_runs"] = json!(999);
        assert!(put_config(
            State(app.clone()),
            Path("loop_runtime".into()),
            Json(json!({"expected_digest":result["digest"],"config":value}))
        )
        .await
        .is_err());
        assert_eq!(
            get_config(State(app), Path("loop_runtime".into()))
                .await
                .unwrap()
                .0["digest"],
            result["digest"]
        );
    }
}

#[cfg(test)]
mod background_interval_tests {
    use super::*;
    /// The generic saved-file path emits the same adoption hint and rejects invalid timing.
    #[tokio::test]
    async fn generic_interval_save_notifies_and_preserves_invalid_input() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let before = get_config(State(app.clone()), Path("loop_runtime".into()))
            .await
            .unwrap()
            .0;
        let mut value = before["config"].clone();
        value["scheduler"]["background_rescan_interval_ms"] = json!(15000);
        let hints = crate::wake::subscribe();
        let saved = put_config(
            State(app.clone()),
            Path("loop_runtime".into()),
            Json(json!({
   "expected_digest":before["digest"],"config":value})),
        )
        .await
        .unwrap()
        .0;
        assert!(hints.has_changed().unwrap());
        assert_eq!(
            get_config(State(app.clone()), Path("loop_runtime".into()))
                .await
                .unwrap()
                .0["config"],
            value
        );
        value["scheduler"]["background_rescan_interval_ms"] = json!(0);
        assert!(put_config(
            State(app.clone()),
            Path("loop_runtime".into()),
            Json(json!({
   "expected_digest":saved["digest"],"config":value}))
        )
        .await
        .is_err());
        assert_eq!(
            get_config(State(app), Path("loop_runtime".into()))
                .await
                .unwrap()
                .0["digest"],
            saved["digest"]
        );
    }
}
