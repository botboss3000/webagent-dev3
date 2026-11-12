//! The configuration composer's API. `POST /config/propose` asks the local inference service
//! (`super::inference`) which setting a request means on the page it was made on,
//! and answers with a card or a pick list, each setting with its description and current value.
//! `POST /config/apply` writes one value the user confirmed, through the owner's existing save:
//! live configuration files through the administrator configuration save (validator, digest
//! fence, broker write), agent settings through the agent identity update (its revision and
//! management check), and the display name through the profile update. Nothing else writes.
use std::path::Path as FilePath;

use axum::extract::{Extension, Path};
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
    let root = FilePath::new(webagent_configuration::live::CHECKOUT);
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
    /// The administrator configuration id (app layer) or identity field (agent layer).
    target: String,
}

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
            Ok(Current {
                value: document.pointer(pointer).cloned().ok_or_else(unsupported)?,
                revision: json!({"digest": webagent_configuration::digest(&document)}),
                document,
                target: id.to_owned(),
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
            })
        }
    }
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
    let describe = |id: &Value| {
        all.iter()
            .find(|(s, ..)| *id == s.id.as_str())
            .map(|(s, ..)| json!({"id": s.id, "description": s.description, "layers": s.layers}))
    };
    let mut ids: Vec<Value> = result["candidates"].as_array().cloned().unwrap_or_default();
    ids.extend(
        [
            &result["setting"],
            &result["first"]["setting"],
            &result["other"],
        ]
        .into_iter()
        .filter(|id| id.is_string())
        .cloned(),
    );
    result["settings"] = ids.iter().filter_map(describe).collect();
    // The card to confirm: the request's single change, or the first of two.
    let card = if result["outcome"] == "card" {
        Some(result.clone())
    } else if result["first"]["outcome"] == "card" {
        Some(result["first"].clone())
    } else {
        None
    };
    if let Some(card) = card {
        let (setting, descriptor, owner) = all
            .iter()
            .find(|(s, ..)| card["setting"] == s.id.as_str())
            .ok_or_else(|| bad("Unknown setting"))?;
        let params = card["params"].as_object().cloned().unwrap_or_default();
        let pointer = pointer(descriptor, layer, &params)?;
        let found = current(&app, &identity, page, layer, (descriptor, owner), &pointer).await;
        let review = match found {
            Ok(now) => {
                let proposed = match card.get("value") {
                    Some(value) => Some(merged(&now.value, value)),
                    None => resolved(&now.value, &card["change"]),
                };
                let limit = limits.get(&setting.id).and_then(Value::as_f64);
                let range = proposed
                    .as_ref()
                    .and_then(|p| values::out_of_range(setting, p, limit));
                json!({"current": now.value, "proposed": proposed, "revision": now.revision, "out_of_range": range})
            }
            Err(ApiError(_, reason)) => json!({"unavailable": reason}),
        };
        result["review"] = review;
    }
    Ok(Json(result))
}

/// `{"page", "setting", "value", "params"?, "revision"}` → the owner's save of that one value.
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
    let value = body.get("value").ok_or_else(|| bad("Value required"))?;
    let params = body["params"].as_object().cloned().unwrap_or_default();
    let pointer = pointer(descriptor, layer, &params)?;
    let now = current(&app, &identity, page, layer, (descriptor, owner), &pointer).await?;
    let value = merged(&now.value, value);
    check_kind(setting, &value)?;
    if let Some(limit) = values::out_of_range(setting, &value, None) {
        return Err(bad(&format!("Out of range: {limit}")));
    }
    if body["revision"] != now.revision {
        return Err(conflict("Setting changed; review it again"));
    }
    let mut document = now.document;
    match layer {
        "app" => {
            *document
                .pointer_mut(&pointer)
                .ok_or_else(|| bad("Unknown setting"))? = value.clone();
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
}
