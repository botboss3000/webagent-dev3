//! The configuration composer's API. `POST /config/propose` asks the local inference service
//! (`super::inference`) which setting a request means on the page it was made on,
//! and answers with a card or a pick list, each setting with its description and current value.
//! `POST /config/apply` writes one value the user confirmed, through the owner's existing save:
//! live configuration files through the administrator configuration save (validator, digest
//! fence, broker write), agent settings through the agent identity update (its revision and
//! management check), an agent's `chat.json` values through that agent's chat overlay (the
//! chat module's per-agent overrides, which `reset` removes so the app value applies again),
//! and the display name through the profile update. Nothing else writes.
//! `GET /config/settings` lists every setting a page can change with its current value and
//! revision, for the settings pages' rows; each row saves through `apply`.
use std::path::Path as FilePath;

use axum::extract::{Extension, Path, Query};
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Map, Value};
use webagent_configuration::catalog;
use webagent_inference::matcher::Setting;
use webagent_inference::values;

use super::inference::ask;
use crate::{bad, conflict, Api, ApiError, App, State};

const LIVE_LIMIT: u64 = 4 * 1024 * 1024;

/// The catalog's descriptors with the owner file each came from.
fn descriptors() -> Result<Vec<(Setting, Value, String)>, ApiError> {
    let root = &webagent_configuration::paths::checkout();
    Ok(catalog::load(root)?
        .into_iter()
        .map(|s| {
            let owner = s.config.unwrap_or_default();
            (Setting::from_descriptor(&s.descriptor), s.descriptor, owner)
        })
        .collect())
}

fn forbidden() -> ApiError {
    ApiError(
        StatusCode::FORBIDDEN,
        "You can't change settings on this page".into(),
    )
}

/// The page's layer, after checking the caller may change settings there: the app dashboard
/// needs an administrator, an agent's dashboard someone who manages that agent, and the user's
/// own page a registered account.
async fn page_layer<'a>(app: &App, identity: &Value, page: &'a str) -> Result<&'a str, ApiError> {
    if page == "app" {
        crate::phase3_api::administrator(app, identity).await?;
        return Ok("app");
    }
    if page == "user" {
        return if identity["_role"] == "guest" {
            Err(forbidden())
        } else {
            Ok("user")
        };
    }
    let agent = page
        .strip_prefix("agent:")
        .ok_or_else(|| bad("Unknown page"))?;
    let user = identity["_user_id"].as_str().unwrap_or_default();
    let rights = app
        .store
        .agent_authority(user, agent)
        .await
        .map_err(|_| forbidden())?;
    if rights["can_manage"] != true {
        return Err(forbidden());
    }
    Ok("agent")
}

/// The setting's pointer on `layer`, with its parameters filled in.
fn pointer(
    descriptor: &Value,
    layer: &str,
    params: &Map<String, Value>,
) -> Result<String, ApiError> {
    let key = if layer == "agent" && descriptor.get("agent_pointer").is_some() {
        "agent_pointer"
    } else {
        "pointer"
    };
    let mut pointer = descriptor[key].as_str().unwrap_or_default().to_owned();
    for (name, allowed) in descriptor["parameters"].as_object().into_iter().flatten() {
        let value = params
            .get(name)
            .and_then(Value::as_str)
            .ok_or_else(|| bad(&format!("Choose a {name}")))?;
        if !allowed
            .as_array()
            .is_some_and(|a| a.contains(&json!(value)))
        {
            return Err(bad(&format!("Unknown {name}")));
        }
        pointer = pointer.replace(&format!("{{{name}}}"), value);
    }
    Ok(pointer)
}

/// Where a setting's current value lives on a layer, read with the fence its save checks.
struct Current {
    value: Value,
    /// The whole document the value sits in, as its save takes it.
    document: Value,
    revision: Value,
    /// The administrator configuration id (app layer), identity field or `chat_overlay`
    /// (agent layer).
    target: String,
    /// Where a chat value comes from: `agent` (the agent's override), `app` (the app file's own
    /// value on that surface) or `common` (a surface without its own value shows Common's).
    source: Option<&'static str>,
}

/// Target of agent-layer values kept in the agent's `chat.json` overlay.
const CHAT_OVERLAY: &str = "chat_overlay";

async fn current(
    app: &App,
    identity: &Value,
    page: &str,
    layer: &str,
    (descriptor, owner): (&Value, &str),
    pointer: &str,
) -> Result<Current, ApiError> {
    let unsupported = || bad("This setting can't be changed here yet");
    match layer {
        "app" => {
            let id = super::live_configuration::id_for_live(app, owner).ok_or_else(unsupported)?;
            let bytes = crate::live_files::current(owner, LIVE_LIMIT)?;
            let document: Value = serde_json::from_slice(&bytes).map_err(anyhow::Error::from)?;
            let own = document.pointer(pointer).cloned();
            let source =
                inherit(descriptor, pointer).map(|_| if own.is_some() { "app" } else { "common" });
            let value = own
                .or_else(|| inherit(descriptor, pointer).and_then(|p| document.pointer(p).cloned()))
                .ok_or_else(unsupported)?;
            Ok(Current {
                value,
                revision: json!({"digest": webagent_configuration::digest(&document)}),
                document,
                target: id.to_owned(),
                source,
            })
        }
        // chat.json values without an identity field live in the agent's chat overlay.
        "agent"
            if descriptor.get("agent_pointer").is_none()
                && super::live_configuration::live("chat") == Some(owner) =>
        {
            let agent = page.trim_start_matches("agent:").to_owned();
            let Json(record) =
                crate::chat_config::get(State(app.clone()), Path((agent, "chat".to_owned())))
                    .await?;
            let effective = &record["effective"];
            let own = effective.pointer(pointer).cloned();
            let source = if record["overlay"].pointer(pointer).is_some() {
                "agent"
            } else if own.is_some() {
                "app"
            } else {
                "common"
            };
            let value = own
                .or_else(|| {
                    inherit(descriptor, pointer).and_then(|p| effective.pointer(p).cloned())
                })
                .ok_or_else(unsupported)?;
            Ok(Current {
                value,
                revision: json!({"overlay_revision": record["revision"]}),
                source: Some(source),
                document: Value::Null,
                target: CHAT_OVERLAY.into(),
            })
        }
        "agent" => {
            let agent = page.trim_start_matches("agent:").to_owned();
            let Json(identity_doc) =
                crate::agents_api::identity(app.clone(), identity.clone(), agent).await?;
            let field = pointer.split('/').nth(1).unwrap_or_default().to_owned();
            if !crate::agents_api::IDENTITY_SETTINGS.contains(&field.as_str()) {
                return Err(unsupported());
            }
            Ok(Current {
                value: identity_doc
                    .pointer(pointer)
                    .cloned()
                    .ok_or_else(unsupported)?,
                revision: json!({"revision": identity_doc["revision"]}),
                document: identity_doc,
                target: field,
                source: None,
            })
        }
        _ => {
            if descriptor["id"] != "user.display_name" {
                return Err(bad("Personal settings aren't stored yet"));
            }
            let Json(me) = super::me(State(app.clone()), Extension(identity.clone())).await?;
            Ok(Current {
                value: me["display_name"].clone(),
                revision: json!({}),
                document: json!({"username": me["user"]}),
                target: "profile".into(),
                source: None,
            })
        }
    }
}

/// A surface pointer's Common fallback (`inherit_pointer`), when this pointer isn't Common's.
fn inherit<'a>(descriptor: &'a Value, pointer: &str) -> Option<&'a str> {
    descriptor["inherit_pointer"]
        .as_str()
        .filter(|common| *common != pointer)
}

/// Set `pointer` in `document`, creating missing objects on the way; `None` removes the key.
fn put_at(document: &mut Value, pointer: &str, value: Option<Value>) -> Result<(), ApiError> {
    let keys: Vec<String> = pointer
        .split('/')
        .skip(1)
        .map(|key| key.replace("~1", "/").replace("~0", "~"))
        .collect();
    let (last, parents) = keys.split_last().ok_or_else(|| bad("Unknown setting"))?;
    let mut node = document;
    for key in parents {
        node = node
            .as_object_mut()
            .ok_or_else(|| bad("Unknown setting"))?
            .entry(key.clone())
            .or_insert_with(|| json!({}));
    }
    let object = node.as_object_mut().ok_or_else(|| bad("Unknown setting"))?;
    match value {
        Some(value) => {
            object.insert(last.clone(), value);
        }
        None => {
            object.remove(last);
        }
    }
    Ok(())
}

/// `{width, unit}` and `{enabled}` values replace only the keys they name.
fn merged(old: &Value, new: &Value) -> Value {
    match (old.as_object(), new.as_object()) {
        (Some(old), Some(new)) => {
            let mut out = old.clone();
            out.extend(new.clone());
            Value::Object(out)
        }
        _ => new.clone(),
    }
}

/// A relative change applied to the current value, or `None` when the user must pick a value
/// ("bigger" with no amount).
fn resolved(current: &Value, change: &Value) -> Option<Value> {
    let by = change["by"].as_f64()?;
    let (number, wrap) = match current.get("width") {
        Some(width) => (width.as_f64()?, true),
        None => (current.as_f64()?, false),
    };
    let next = match change["op"].as_str()? {
        "add" => number + by,
        "multiply" => number * by,
        _ => return None,
    };
    let next = values::clean(next);
    Some(if wrap {
        merged(current, &json!({"width": next}))
    } else {
        next
    })
}

/// `{"text", "page"}` → the matcher's answer, with descriptions, and for a card the current value,
/// the proposed value and the revision `apply` must send back.
pub(crate) async fn propose(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Json(body): Json<Value>,
) -> Api {
    let text = body["text"]
        .as_str()
        .ok_or_else(|| bad("Request text required"))?;
    let page = body["page"].as_str().ok_or_else(|| bad("Page required"))?;
    let layer = page_layer(&app, &identity, page).await?;
    let all = descriptors()?;
    let read = |id: &str, pointer: &str| -> Option<Value> {
        let (_, _, owner) = all.iter().find(|(s, ..)| s.id == id)?;
        let bytes = crate::live_files::current(owner, LIVE_LIMIT).ok()?;
        serde_json::from_slice::<Value>(&bytes)
            .ok()?
            .pointer(pointer)
            .cloned()
    };
    // Bounds that depend on related live values.
    let mut limits = Map::new();
    if let Some(max) = read(
        "access.login_protection.address.block_seconds",
        "/login_protection/address/max_block_seconds",
    ) {
        limits.insert("access.login_protection.address.block_seconds".into(), max);
    }
    if let (Some(trigger), Some(summary)) = (
        read("context.compaction.tail_tokens", "/trigger_tokens").and_then(|v| v.as_f64()),
        read("context.compaction.tail_tokens", "/summary_tokens").and_then(|v| v.as_f64()),
    ) {
        limits.insert(
            "context.compaction.tail_tokens".into(),
            json!(trigger - summary - 1.0),
        );
    }
    let agents = app.store.agent_ids().await.unwrap_or_default();
    let mut result =
        ask(json!({"text": text, "page": page, "agents": agents, "limits": limits})).await?;
    // Every setting the answer names: a card's, a pick list's, and each part's of a request
    // with several changes.
    let describe = |id: &Value| {
        all.iter()
            .find(|(s, ..)| *id == s.id.as_str())
            .map(|(s, ..)| json!({"id": s.id, "description": s.description, "layers": s.layers}))
    };
    let parts = result["parts"].as_array().cloned().unwrap_or_default();
    let mut ids: Vec<Value> = Vec::new();
    for answer in std::iter::once(&result).chain(&parts) {
        ids.extend(
            answer["candidates"]
                .as_array()
                .into_iter()
                .flatten()
                .cloned(),
        );
        ids.push(answer["setting"].clone());
    }
    ids.retain(Value::is_string);
    ids.dedup();
    result["settings"] = ids.iter().filter_map(describe).collect();
    // Each card to confirm gets its review: the request's single change, or every card part.
    if result["outcome"] == "card" {
        result["review"] = review(&app, &identity, page, layer, &all, &limits, &result).await?;
    }
    for (index, part) in parts.iter().enumerate() {
        if part["outcome"] == "card" {
            result["parts"][index]["review"] =
                review(&app, &identity, page, layer, &all, &limits, part).await?;
        }
    }
    Ok(Json(result))
}

/// A card's current value, proposed value, revision and range check, or why the setting
/// can't be changed on this page.
async fn review(
    app: &App,
    identity: &Value,
    page: &str,
    layer: &str,
    all: &[(Setting, Value, String)],
    limits: &Map<String, Value>,
    card: &Value,
) -> Result<Value, ApiError> {
    let (setting, descriptor, owner) = all
        .iter()
        .find(|(s, ..)| card["setting"] == s.id.as_str())
        .ok_or_else(|| bad("Unknown setting"))?;
    let params = card["params"].as_object().cloned().unwrap_or_default();
    let pointer = pointer(descriptor, layer, &params)?;
    Ok(
        match current(app, identity, page, layer, (descriptor, owner), &pointer).await {
            Ok(now) => {
                let proposed = match card.get("value") {
                    Some(value) => Some(merged(&now.value, value)),
                    None => resolved(&now.value, &card["change"]),
                };
                let limit = limits.get(&setting.id).and_then(Value::as_f64);
                let range = proposed
                    .as_ref()
                    .and_then(|p| values::out_of_range(setting, p, limit));
                json!({"current": now.value, "proposed": proposed, "revision": now.revision,
                    "out_of_range": range, "source": now.source})
            }
            Err(ApiError(_, reason)) => json!({"unavailable": reason}),
        },
    )
}

/// Every combination of a descriptor's parameter values (`{"tier": [...]}`); one empty map
/// for a setting without parameters.
fn combinations(parameters: &Value) -> Vec<Map<String, Value>> {
    let mut all = vec![Map::new()];
    for (name, values) in parameters.as_object().into_iter().flatten() {
        let values = values.as_array().cloned().unwrap_or_default();
        all = all
            .into_iter()
            .flat_map(|base| {
                values.iter().map(move |value| {
                    let mut next = base.clone();
                    next.insert(name.clone(), value.clone());
                    next
                })
            })
            .collect();
    }
    all
}

/// `?page=` → every setting the page's layer holds, one entry per parameter value, each with
/// its descriptor, `params`, owner file and either `value` plus the `revision` `apply` must
/// send back, or the reason it can't be changed here (`unavailable`).
pub(crate) async fn settings(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Query(query): Query<std::collections::HashMap<String, String>>,
) -> Api {
    let page = query
        .get("page")
        .map(String::as_str)
        .ok_or_else(|| bad("Page required"))?;
    let layer = page_layer(&app, &identity, page).await?;
    let mut out = Vec::new();
    for (setting, descriptor, owner) in descriptors()? {
        if !setting.layers.iter().any(|l| l == layer) {
            continue;
        }
        for params in combinations(&descriptor["parameters"]) {
            let mut entry = descriptor.clone();
            if let Some(fields) = entry.as_object_mut() {
                // Matcher wording and storage paths are not for the page.
                for key in ["examples", "pointer", "agent_pointer"] {
                    fields.remove(key);
                }
            }
            entry["owner"] = json!(owner);
            entry["params"] = Value::Object(params.clone());
            let found = match pointer(&descriptor, layer, &params) {
                Ok(pointer) => {
                    current(
                        &app,
                        &identity,
                        page,
                        layer,
                        (&descriptor, owner.as_str()),
                        &pointer,
                    )
                    .await
                }
                Err(error) => Err(error),
            };
            match found {
                Ok(now) => {
                    entry["value"] = now.value;
                    entry["revision"] = now.revision;
                    if let Some(source) = now.source {
                        entry["source"] = json!(source);
                    }
                }
                Err(ApiError(_, reason)) => entry["unavailable"] = json!(reason),
            }
            out.push(entry);
        }
    }
    Ok(Json(json!({"page": page, "layer": layer, "settings": out})))
}

/// `{"page", "setting", "value", "params"?, "revision"}` → the owner's save of that one value.
/// `"reset": true` instead of a value removes an agent's chat override.
pub(crate) async fn apply(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Json(body): Json<Value>,
) -> Api {
    let page = body["page"].as_str().ok_or_else(|| bad("Page required"))?;
    let layer = page_layer(&app, &identity, page).await?;
    let all = descriptors()?;
    let (setting, descriptor, owner) = all
        .iter()
        .find(|(s, ..)| body["setting"] == s.id.as_str())
        .ok_or_else(|| bad("Unknown setting"))?;
    if !setting.layers.iter().any(|l| l == layer) {
        return Err(bad("This setting isn't changed on this page"));
    }
    let reset = body["reset"] == true;
    let params = body["params"].as_object().cloned().unwrap_or_default();
    let pointer = pointer(descriptor, layer, &params)?;
    let now = current(&app, &identity, page, layer, (descriptor, owner), &pointer).await?;
    // Reset drops an agent's chat override, or an app surface value so Common's applies.
    let resettable =
        now.target == CHAT_OVERLAY || (layer == "app" && inherit(descriptor, &pointer).is_some());
    if reset && !resettable {
        return Err(bad("Only a chat setting can be reset"));
    }
    let value = if reset {
        Value::Null
    } else {
        let value = body.get("value").ok_or_else(|| bad("Value required"))?;
        let value = merged(&now.value, value);
        check_kind(setting, &value)?;
        if let Some(limit) = values::out_of_range(setting, &value, None) {
            return Err(bad(&format!("Out of range: {limit}")));
        }
        value
    };
    if body["revision"] != now.revision {
        return Err(conflict("Setting changed; review it again"));
    }
    let mut document = now.document;
    match layer {
        "agent" if now.target == CHAT_OVERLAY => {
            // A merge patch nested along the pointer; null drops the override.
            let mut patch = value.clone();
            for key in pointer
                .split('/')
                .skip(1)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
            {
                patch = json!({ key.replace("~1", "/").replace("~0", "~"): patch });
            }
            let body = serde_json::from_value(
                json!({"patch": patch, "expected_revision": now.revision["overlay_revision"]}),
            )
            .map_err(anyhow::Error::from)?;
            let agent = page.trim_start_matches("agent:").to_owned();
            let _saved =
                crate::chat_config::patch(State(app), Path((agent, "chat".to_owned())), Json(body))
                    .await?;
        }
        "app" => {
            put_at(&mut document, &pointer, (!reset).then(|| value.clone()))?;
            let save = json!({"expected_digest": now.revision["digest"], "config": document});
            let _saved =
                super::live_configuration::put_config(State(app), Path(now.target), Json(save))
                    .await?;
        }
        "agent" => {
            *document
                .pointer_mut(&pointer)
                .ok_or_else(|| bad("Unknown setting"))? = value.clone();
            let mut field = document[&now.target].take();
            if let Some(fields) = field.as_object_mut() {
                // Read-only parts of the identity view are not part of the update.
                fields.remove("fixed_protocol");
            }
            let update = json!({"revision": now.revision["revision"], now.target.clone(): field});
            let agent = page.trim_start_matches("agent:").to_owned();
            let _saved = crate::agents_api::update_identity(app, identity, agent, update).await?;
        }
        _ => {
            let update = json!({"username": document["username"], "display_name": value});
            let _saved =
                super::update_profile(State(app), Extension(identity), Json(update)).await?;
        }
    }
    Ok(Json(
        json!({"setting": setting.id, "layer": layer, "value": value, "previous": now.value}),
    ))
}

/// The value has the setting's type: a number for numbers, one of its choices, and so on.
fn check_kind(setting: &Setting, value: &Value) -> Result<(), ApiError> {
    let ok = match setting.kind.as_str() {
        "integer" => value.as_i64().is_some(),
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        "enum" => value
            .as_str()
            .is_some_and(|v| setting.choices.iter().any(|c| c == v)),
        "string" => value
            .as_str()
            .is_some_and(|v| !v.trim().is_empty() && v.chars().count() <= 200),
        "length" => {
            value["width"].is_number() && matches!(value["unit"].as_str(), Some("px" | "%"))
        }
        "object" => value.is_object(),
        "agent_id" => value
            .as_str()
            .is_some_and(|v| !v.is_empty() && v.len() <= 128),
        _ => false,
    };
    if ok {
        Ok(())
    } else {
        Err(bad("Value doesn't fit this setting"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changes_resolve_against_current_values() {
        let add = json!({"op": "add", "by": 3});
        assert_eq!(resolved(&json!(14), &add), Some(json!(17)));
        let width = json!({"width": 45, "unit": "%"});
        let half = json!({"op": "multiply", "by": 0.5});
        assert_eq!(
            resolved(&width, &half),
            Some(json!({"width": 22.5, "unit": "%"}))
        );
        assert_eq!(resolved(&json!(14), &json!({"op": "increase"})), None);
        assert_eq!(
            merged(
                &json!({"enabled": false, "voice": "a"}),
                &json!({"enabled": true})
            ),
            json!({"enabled": true, "voice": "a"})
        );
    }

    #[test]
    fn surface_values_are_created_and_removed() {
        let mut doc = json!({"chat_common": {"fade": {"top": 200}}});
        put_at(&mut doc, "/chat_desktop/fade/top", Some(json!(120))).unwrap();
        assert_eq!(doc["chat_desktop"]["fade"]["top"], 120);
        put_at(&mut doc, "/chat_desktop/fade/top", None).unwrap();
        assert_eq!(doc["chat_desktop"]["fade"], json!({}));
        let descriptor = json!({"inherit_pointer": "/chat_common/fade/top"});
        assert_eq!(
            inherit(&descriptor, "/chat_mobile/fade/top"),
            Some("/chat_common/fade/top")
        );
        assert_eq!(inherit(&descriptor, "/chat_common/fade/top"), None);
    }

    #[test]
    fn parameters_expand_to_every_combination() {
        assert_eq!(combinations(&Value::Null), vec![Map::new()]);
        let tiers = combinations(&json!({"tier": ["operator", "basic"]}));
        assert_eq!(
            tiers.iter().map(|m| m["tier"].clone()).collect::<Vec<_>>(),
            [json!("operator"), json!("basic")]
        );
        assert_eq!(combinations(&json!({"a": [1, 2], "b": [3, 4]})).len(), 4);
    }
}
