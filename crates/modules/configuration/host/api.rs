//! Revision-fenced administrator updates to existing application configuration.
use super::{
    anyhow, bad, conflict, context_compaction, gate_registry, json, loop_runtime, required,
    tool_output, Api, App, Json, Path, PathBuf, Result, State, Value,
};
use anyhow::Context;
use webagent_configuration::{digest, read};
const IDS: [&str; 15] = [
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
    "voice",
    "browser_limits",
    "admin_alerts",
    "find_ranking",
    "models_runtime",
];
/// Live files (checkout-relative, listed in `live-files.json`) for configuration edited in place.
pub(crate) fn live(id: &str) -> Option<&'static str> {
    Some(match id {
        "chat" => "config/ui/chat/chat.json",
        "app_ui" => webagent_webapp::configuration::LIVE_SETTINGS,
        "context_compaction" => "config/context/context-compaction.json",
        "tool_output_retention" => crate::tool_output::LIVE_POLICY,
        "message_visibility" => "config/engine/gates/message_visibility_v1.json",
        "closing_status" => "config/engine/gates/closing_status.json",
        "voice" => webagent_media::settings::LIVE_VOICE,
        "browser_limits" => webagent_browser::limits::LIVE_LIMITS,
        "admin_alerts" => webagent_notifications::alerts::LIVE_CONFIG,
        "models_runtime" => webagent_models::settings::LIVE,
        #[cfg(feature = "administrator")]
        "find_ranking" => webagent_administration::find_ranking::LIVE_RANKING,
        _ => return None,
    })
}
/// Map a known data-root configuration owner to its runtime path; never accept a caller filesystem path.
fn path(app: &App, id: &str) -> Result<PathBuf> {
    Ok(match id {
        "loop_runtime" => app.runtime_path.clone(),
        "app_settings" => crate::store::settings_path(&app.store.root),
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
    if id == "app_settings" {
        return follows(crate::store::settings_path(&app.store.root));
    }
    live(id).map(str::to_owned)
}
/// The configuration id whose live file is `relative`, for saves made on its behalf.
pub(crate) fn id_for_live(app: &App, relative: &str) -> Option<&'static str> {
    IDS.into_iter()
        .find(|id| live_path(app, id).as_deref() == Some(relative))
}
/// Reuse each owner's domain validator before an administrator save.
pub(crate) fn validate(id: &str, value: &Value) -> Result<()> {
    if !value.is_object() {
        anyhow::bail!("Configuration must be an object");
    }
    match id {
        "app_settings" => {
            crate::store::admin_encryption_enabled(value)?;
            // Only the encryption switch lives here; UI settings belong to `app_ui` and
            // proxy trust to the accounts module.
            let known = |key: &String| {
                matches!(key.as_str(), "_instructions" | "admin_database_encryption")
            };
            if !value
                .as_object()
                .is_some_and(|object| object.keys().all(known))
            {
                anyhow::bail!("Unknown storage setting");
            }
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
        "voice" => {
            webagent_media::settings::VoiceSettings::parse(&serde_json::to_vec(value)?)?;
        }
        "browser_limits" => {
            webagent_browser::limits::Limits::parse(&serde_json::to_vec(value)?)?;
        }
        "admin_alerts" => {
            webagent_notifications::alerts::Config::parse(&serde_json::to_vec(value)?)?;
        }
        "models_runtime" => {
            webagent_models::settings::Settings::parse(&serde_json::to_vec(value)?)?;
        }
        #[cfg(feature = "administrator")]
        "find_ranking" => {
            webagent_administration::find_ranking::Ranking::parse(&serde_json::to_vec(value)?)?;
        }
        "tool_output_retention" => {
            serde_json::from_value::<tool_output::Policy>(value.clone())?.validate()?
        }
        "access_policy" => {
            crate::registration::validate_configuration(value)?;
            // Fixture policies may omit storage; the live file always has it.
            if let Some(storage) = value.get("storage") {
                let storage = serde_json::to_vec(storage)?;
                webagent_files::storage_policy::StoragePolicy::parse(&storage)?;
                webagent_storage::compression::Compression::from_policy(&storage)?;
            }
        }
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
        return Err(bad("Prompts are live Markdown files under config/configuration/prompts; edit them in place"));
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
        "access_policy" => app.store.access_repository().policy_lock_path(),
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
/// Every live file under `config/` (each file a `live-files.json` pattern there names), for
/// All Settings' raw editor. The broker's writer re-checks the list on every save.
fn config_files() -> Result<Vec<String>> {
    let root = webagent_configuration::paths::checkout();
    let index: Value = serde_json::from_slice(&std::fs::read(root.join("live-files.json"))?)?;
    let patterns: Vec<&str> = index["files"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|pattern| pattern.starts_with("config/"))
        .collect();
    let mut files = Vec::new();
    let mut pending = vec![root.join("config")];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.is_dir() {
                pending.push(path);
            } else if let Ok(relative) = path.strip_prefix(&root) {
                let relative = relative.to_string_lossy().replace('\\', "/");
                if patterns.iter().any(|pattern| glob(pattern, &relative)) {
                    files.push(relative);
                }
            }
        }
    }
    files.sort();
    Ok(files)
}

/// `pattern` matches `path` segment by segment; `*` matches within one segment.
fn glob(pattern: &str, path: &str) -> bool {
    fn wild(p: &[u8], s: &[u8]) -> bool {
        match p.split_first() {
            None => s.is_empty(),
            Some((b'*', rest)) => (0..=s.len()).any(|i| wild(rest, &s[i..])),
            Some((c, rest)) => s.first() == Some(c) && wild(rest, &s[1..]),
        }
    }
    let (p, s): (Vec<&str>, Vec<&str>) = (pattern.split('/').collect(), path.split('/').collect());
    p.len() == s.len()
        && p.iter()
            .zip(&s)
            .all(|(p, s)| wild(p.as_bytes(), s.as_bytes()))
}

/// One listed config file, or an error that names no other path.
fn listed(path: &str) -> std::result::Result<String, crate::ApiError> {
    config_files()?
        .into_iter()
        .find(|file| file == path)
        .ok_or_else(|| bad("Unknown config file"))
}

/// Fence over a file's exact bytes.
fn text_digest(bytes: &[u8]) -> String {
    digest(&json!(String::from_utf8_lossy(bytes)))
}

/// Check a JSON config file with its owner's validator where one exists: the administrator
/// configuration validators, the browser pages rules and the provider schema; any other file
/// must hold a JSON object.
fn check_file(app: &App, path: &str, value: &Value) -> Result<()> {
    if let Some(id) = id_for_live(app, path) {
        return validate(id, value);
    }
    let bytes = serde_json::to_vec(value)?;
    if path == webagent_browser::pages::LIVE_PAGES {
        webagent_browser::pages::Pages::parse(&bytes)?;
    } else if let Some(name) = path.strip_prefix("config/models/providers/") {
        // The provider loader validates a folder; check this file alone in a scratch one.
        let scratch = tempfile::tempdir()?;
        std::fs::write(scratch.path().join(name), &bytes)?;
        webagent_models::providers::validate_folder(scratch.path())?;
    } else {
        anyhow::ensure!(value.is_object(), "A config file holds a JSON object");
    }
    Ok(())
}

/// `GET /admin/config-files` → every editable live config file.
pub(crate) async fn files_list(State(_app): State<App>) -> Api {
    Ok(Json(json!({"files": config_files()?})))
}

/// `GET /admin/config-files/{path}` → the file's text and the digest a save sends back.
pub(crate) async fn file_get(State(_app): State<App>, Path(path): Path<String>) -> Api {
    let path = listed(&path)?;
    let bytes = crate::live_files::current(&path, LIVE_LIMIT)?;
    let text = String::from_utf8(bytes.clone()).map_err(|_| bad("Not a text file"))?;
    Ok(Json(
        json!({"path": path, "text": text, "digest": text_digest(&bytes)}),
    ))
}

/// `PUT /admin/config-files/{path}` `{"text", "expected_digest"}` → replace the file with the
/// text as sent (key order and layout kept), after JSON is parsed and checked by its owner's
/// validator. The broker replaces the file only while it still holds the bytes read.
pub(crate) async fn file_put(
    State(app): State<App>,
    Path(path): Path<String>,
    Json(body): Json<Value>,
) -> Api {
    let path = listed(&path)?;
    let expected = required(&body, "expected_digest", 64)?.to_owned();
    let text = body["text"].as_str().ok_or_else(|| bad("Text required"))?;
    if text.len() as u64 > LIVE_LIMIT {
        return Err(bad("File too large"));
    }
    if path.ends_with(".json") {
        let value: Value = serde_json::from_str(text).map_err(|_| bad("Invalid JSON"))?;
        check_file(&app, &path, &value)
            .map_err(|error| bad(&format!("Invalid configuration: {error:#}")))?;
    }
    let mut bytes = text.as_bytes().to_vec();
    if !bytes.ends_with(b"\n") {
        bytes.push(b'\n');
    }
    let _guard = app.gate.lock().await;
    let current = crate::live_files::current(&path, LIVE_LIMIT)?;
    if text_digest(&current) != expected {
        return Err(conflict("File changed; reload before saving"));
    }
    crate::live_files::save(&app, &path, &bytes, &current)
        .await
        .map_err(|_| conflict("File changed; reload before saving"))?;
    Ok(Json(json!({"path": path, "digest": text_digest(&bytes)})))
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
    #[test]
    fn globs_match_within_one_segment() {
        assert!(glob("config/ui/*.json", "config/ui/vibrate.json"));
        assert!(glob(
            "config/configuration/prompts/*.md",
            "config/configuration/prompts/a.md"
        ));
        assert!(!glob("config/ui/*.json", "config/ui/chat/chat.json"));
        assert!(!glob(
            "config/media/voice.json",
            "config/media/voice.json.bak"
        ));
    }
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
