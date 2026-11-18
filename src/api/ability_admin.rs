//! Tenant authority and the same live access calculation used by dispatch.
use super::*;
use crate::access::Permission;
use crate::capabilities::CapabilityLevel;
use axum::Extension;
fn denied() -> ApiError {
    ApiError(
        StatusCode::FORBIDDEN,
        "Agent ability access unavailable".into(),
    )
}
async fn authority(
    app: &App,
    identity: &Value,
    id: &str,
    write: bool,
) -> std::result::Result<Value, ApiError> {
    let user = identity["_user_id"].as_str().ok_or_else(denied)?;
    let rights = app
        .store
        .agent_authority(user, id)
        .await
        .map_err(|_| ApiError(StatusCode::NOT_FOUND, "Agent not found".into()))?;
    if (write && rights["can_manage"] != true)
        || (!write
            && rights["can_manage"] != true
            && !app
                .store
                .live_access(user, id)
                .await
                .is_ok_and(|a| a.admitted()))
    {
        return Err(denied());
    }
    Ok(rights)
}
fn update_error(error: anyhow::Error) -> ApiError {
    let message = error.to_string();
    if message.contains("revision") {
        conflict("Ability settings changed; reload before saving")
    } else if message.contains(" above it") {
        // A ladder-order refusal names the two levels and what clashes.
        bad(&message)
    } else {
        bad("Invalid ability policy update")
    }
}
fn map_permission(
    map: &Value,
    id: &str,
    default: Permission,
) -> std::result::Result<Permission, ApiError> {
    match map.get(id).or_else(|| map.get("*")) {
        Some(v) => Permission::from_policy(v.as_str().ok_or_else(|| bad("Invalid permission"))?)
            .map_err(Into::into),
        None => Ok(default),
    }
}
pub(super) async fn grant_ceiling(
    app: &App,
    identity: &Value,
    abilities: &Value,
    tools: &Value,
    previous_abilities: &Value,
    previous_tools: &Value,
    base: bool,
) -> std::result::Result<(), ApiError> {
    let account = app
        .store
        .account_by_id(identity["_user_id"].as_str().ok_or_else(denied)?)
        .await?
        .ok_or_else(denied)?;
    let principal = crate::access::Principal::from_account(&account)?;
    // The editor's own tier caps what they may grant, as it would on their own agent.
    let tier = principal.tier_id.as_str();
    let ability_ceiling = |id: &str| {
        if crate::access::ability_tier_allowed(id, tier, tier) {
            Permission::Auto
        } else {
            Permission::Deny
        }
    };
    let tool_ceiling = |name: &str| {
        if crate::access::tool_tier_allowed(name, tier) {
            Permission::Auto
        } else {
            Permission::Deny
        }
    };
    for (grants, previous, ability) in [
        (abilities, previous_abilities, true),
        (tools, previous_tools, false),
    ] {
        let map = grants
            .as_object()
            .ok_or_else(|| bad("Permission maps required"))?;
        let mut ids: std::collections::BTreeSet<String> = map.keys().cloned().collect();
        if let Some(old) = previous.as_object() {
            ids.extend(old.keys().cloned());
        }
        if ability {
            ids.extend(app.abilities.ids());
        } else {
            ids.extend(
                app.abilities
                    .all_schemas()
                    .iter()
                    .filter_map(|s| s["name"].as_str().map(str::to_owned)),
            );
        }
        ids.insert("*".into());
        let default = if ability && !base {
            Permission::Deny
        } else {
            Permission::Auto
        };
        for name in ids {
            let next = map_permission(grants, &name, default)?;
            let before = map_permission(previous, &name, default)?;
            // Compare resolved policies, including removed restrictions and wildcard
            // inheritance. An unchanged higher ceiling can be tightened by a delegate.
            if next.meet(before) == next {
                continue;
            }
            let ceiling = if ability {
                ability_ceiling(&name)
            } else {
                tool_ceiling(&name)
            };
            if next.meet(ceiling) != next {
                return Err(ApiError(
                    StatusCode::FORBIDDEN,
                    "Grant exceeds your account tier".into(),
                ));
            }
        }
    }
    // Ability and tool permissions interact: raising an ability can expose an
    // inherited Auto tool even when its own map did not change.
    for schema in app.abilities.all_schemas() {
        let name = schema["name"].as_str().unwrap_or("");
        let owners = app
            .abilities
            .tool_abilities(name, &Value::Null, &Value::Null, &Value::Null);
        if owners.is_empty() {
            continue;
        }
        let mut next = map_permission(tools, name, Permission::Auto)?;
        let mut before = map_permission(previous_tools, name, Permission::Auto)?;
        let mut ceiling = tool_ceiling(name);
        for id in owners {
            let default = if base {
                Permission::Auto
            } else {
                Permission::Deny
            };
            next = next.meet(map_permission(abilities, &id, default)?);
            before = before.meet(map_permission(previous_abilities, &id, default)?);
            ceiling = ceiling.meet(ability_ceiling(&id));
        }
        if next.meet(before) != next && next.meet(ceiling) != next {
            return Err(ApiError(
                StatusCode::FORBIDDEN,
                "Ability grant exceeds your account tool tier".into(),
            ));
        }
    }
    Ok(())
}
/// Chat controls the run loop owns, shown under Base as fixed. They are exempt from the
/// repeat guard and have no permission, execution or retry settings.
const RUNTIME_CONTROLS: &[(&str, &str)] = &[(
    "publish_main_message",
    "Publish the agent's reply to the main chat.",
)];

async fn catalog(app: &App, identity: &Value, id: &str, params: &HashMap<String, String>) -> Api {
    // The Abilities page is operator-only; members get abilities through their sessions.
    let rights = authority(app, identity, id, true).await?;
    let caller = identity["_user_id"].as_str().ok_or_else(denied)?;
    if params.contains_key("level_id") && rights["can_manage"] != true {
        return Err(denied());
    }
    let mut access = app
        .store
        .live_access(caller, id)
        .await
        .map_err(|_| denied())?;
    let levels = app.store.capability_levels(id).await?;
    if let Some(level) = params.get("level_id").filter(|v| !v.is_empty()) {
        access.capability_level = levels
            .effective(level)
            .map_err(|_| bad("Unknown capability level"))?;
    }
    let scoped = admin_agent_view(app, id).await?;
    let mut visibility = agent_abilities(&scoped).await?;
    access.capability_level.decorate_visibility(&mut visibility);
    let mut abilities = app.abilities.configuration_catalog(&visibility);
    let call_counts = app
        .store
        .for_user(caller)
        .await?
        .tool_call_counts(id)
        .await?;
    // Declared defaults are shown for the viewed level's rungs: it, then the levels below.
    let rungs = access
        .capability_level
        .chain
        .first()
        .map(|(rungs, _)| rungs.clone())
        .unwrap_or_else(|| vec![access.capability_level.id.clone()]);
    // The counts describe this caller's recorded model calls, including older sessions.
    for ability in &mut abilities {
        let ability_id = ability["id"].as_str().unwrap_or("").to_owned();
        ability["default"] = json!(webagent_abilities::ability_access::default_for(
            &ability_id,
            &rungs
        )
        .unwrap_or(Permission::Deny));
        let ready = ability["available"] == true;
        let permission = access.ability_permission(&ability_id);
        let reason = if !ready {
            "pending_port"
        } else if access.tier_ability_permission(&ability_id) == Permission::Deny {
            "tier_denied"
        } else if access.capability_level.ability_permission(&ability_id) == Permission::Deny {
            "capability_denied"
        } else if permission == Permission::Deny {
            "agent_denied"
        } else if permission == Permission::Ask {
            "approval_required"
        } else {
            "allowed"
        };
        ability["declared"] = json!(true);
        if !access.ability_role_allowed(&ability_id) {
            ability["role_blocked"] = json!(true);
        }
        ability["ready"] = json!(ready && ability["readiness"] == "ready");
        ability["installed"] = json!(ability["tools"]
            .as_array()
            .is_some_and(|ts| ts.iter().any(|t| t["supported"] == true)));
        // Current adapters have no per-agent credential settings. Unsupported workflows
        // cannot claim configured or ready from their declaration alone.
        ability["configured"] = json!(if ability_id == "mcp" {
            ready && crate::mcp::configured(&scoped, id).await
        } else {
            ready
        });
        ability["effective"] =
            json!({"permission":if ready {permission} else {Permission::Deny},"reason":reason});
        let active = json!({"active":[ability_id],"suppressed":[]});
        let loaded = json!([ability_id]);
        let mut usable = false;
        let mut effective = crate::access::ToolDecision {
            permission: Permission::Deny,
            reason: crate::access::Reason::Unavailable,
        };
        let mut first = true;
        if let Some(tools) = ability["tools"].as_array_mut() {
            for tool in tools {
                let name = tool["name"].as_str().unwrap_or("").to_owned();
                tool["call_count"] = json!(call_counts.get(&name).copied().unwrap_or(0));
                tool["default"] =
                    json!(crate::tools::tool_defaults(&name, &rungs).unwrap_or(Permission::Auto));
                tool["execution_override"] = if rights["can_manage"] == true {
                    visibility["tool_execution"][&name].clone()
                } else {
                    Value::Null
                };
                tool["execution"] =
                    serde_json::to_value(crate::tool_execution::resolve(&visibility, &name)?)
                        .map_err(anyhow::Error::from)?;
                tool["advanced_policies"] = crate::tool_execution::advanced_policies(&name);
                tool["result_deduplication"] =
                    json!(crate::tool_output::dedup_included(&visibility, &name));
                tool["duplicate_protection_capable"] = json!(crate::actions::supported(&name));
                tool["background_capable"] =
                    json!(crate::tool_execution::background_capable(&name));
                tool["max_supported_concurrency"] =
                    json!(crate::tool_execution::max_supported_concurrency(&name));
                let decision =
                    live_tool_decision(&scoped, &access, &name, &visibility, &active, &loaded);
                // The tool's or ability's role_access excludes the viewed level.
                let owners = app
                    .abilities
                    .tool_abilities(&name, &visibility, &active, &loaded);
                if access
                    .tool_decision_for_abilities(&name, &owners, true, true)
                    .reason
                    == crate::access::Reason::RoleDenied
                {
                    tool["role_blocked"] = json!(true);
                }
                if first
                    || (decision.permission != Permission::Deny
                        && (effective.permission == Permission::Deny
                            || decision.permission == Permission::Auto))
                {
                    effective = decision;
                }
                first = false;
                usable |= decision.permission != Permission::Deny;
                tool["effective"] = serde_json::to_value(decision).map_err(anyhow::Error::from)?;
            }
        }
        if ability_id == "base" {
            // The chat's own controls run inside the run loop, not the catalog. They are
            // listed here so the page shows them, locked: no permission or retry setting
            // may change them without breaking the conversation.
            if let Some(tools) = ability["tools"].as_array_mut() {
                for (name, description) in RUNTIME_CONTROLS {
                    tools.push(json!({"name":name,"description":description,"locked":true,
                        "runtime_control":true,"supported":true,
                        "call_count":call_counts.get(*name).copied().unwrap_or(0)}));
                }
            }
        }
        ability["available"] = json!(ready && permission != Permission::Deny && usable);
        if ready {
            ability["effective"] = serde_json::to_value(effective).map_err(anyhow::Error::from)?;
        }
    }
    // Administrator abilities are hidden unless both the owner and the viewer are
    // administrators; other abilities blocked by the owner's tier stay listed to upgrade.
    abilities.retain(|a| {
        let id = a["id"].as_str().unwrap_or("");
        !webagent_access::administrator_ability(id)
            || access.tier_ability_permission(id) != Permission::Deny
    });
    if rights["can_manage"] != true {
        abilities.retain(|a| a["available"] == true);
        for ability in &mut abilities {
            if let Some(tools) = ability["tools"].as_array_mut() {
                tools.retain(|t| t["effective"]["permission"] != "deny");
            }
        }
        // Members see effective entries, never raw private configuration maps.
        visibility["visibility"] = json!({});
        visibility["skill_visibility"] = json!({});
        visibility["tool_modes"] = json!({});
    }
    let mut permissions = app.store.agent_ability_permissions(id).await?;
    permissions["tool_permissions"] =
        app.store.agent_tool_permissions(id).await?["tool_permissions"].clone();
    Ok(Json(
        json!({"agent_id":id,"catalog_version":app.abilities.version(),"revision":visibility["revision"],
        "visibility":visibility["visibility"],"skill_visibility":visibility["skill_visibility"],"tool_modes":visibility["tool_modes"],"settings":if rights["can_manage"]==true {visibility["settings"].clone()} else {json!({})},
        "authority":{"can_manage":rights["can_manage"],"owner_id":rights["owner_id"]},
        "capability_levels":if rights["can_manage"]==true {serde_json::to_value(levels).map_err(anyhow::Error::from)?} else {json!({})},
        "capability_level_id":access.capability_level.id,"permissions":if rights["can_manage"]==true {permissions} else {json!({})},"abilities":abilities}),
    ))
}
pub(crate) async fn read(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path(id): Path<String>,
) -> Api {
    catalog(&app, &identity, &id, &HashMap::new()).await
}
pub(crate) async fn level_view(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path(id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Api {
    catalog(&app, &identity, &id, &params).await
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LevelBody {
    expected_revision: u64,
    level: CapabilityLevel,
}
pub(crate) async fn level_set(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path((id, level)): Path<(String, String)>,
    Json(body): Json<LevelBody>,
) -> Api {
    let _guard = app.gate.lock().await;
    authority(&app, &identity, &id, true).await?;
    if body.level.id != level {
        return Err(bad("Capability level identity mismatch"));
    }
    let levels = app.store.capability_levels(&id).await?;
    let previous = levels
        .levels
        .get(&level)
        .map(serde_json::to_value)
        .transpose()
        .map_err(anyhow::Error::from)?
        .unwrap_or(json!({}));
    grant_ceiling(
        &app,
        &identity,
        &serde_json::to_value(&body.level.ability_permissions).map_err(anyhow::Error::from)?,
        &serde_json::to_value(&body.level.tool_permissions).map_err(anyhow::Error::from)?,
        &previous["ability_permissions"],
        &previous["tool_permissions"],
        false,
    )
    .await?;
    let actor_store = app
        .store
        .for_user(identity["_user_id"].as_str().ok_or_else(denied)?)
        .await?;
    let saved = actor_store
        .save_capability_level(&id, body.level, body.expected_revision)
        .await
        .map_err(update_error)?;
    // Configuration applies at the next step: policy and tool decisions are re-read per
    // model call and before every tool, so running work is not stopped; only approvals
    // waiting under the old settings are revoked.
    phase3_api::revoke_waiting_approvals(
        &app,
        Some(&id),
        "this agent's capability level settings changed",
    )
    .await?;
    Ok(Json(json!({"capability_levels":saved})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MemberBody {
    expected_revision: u64,
    capability_level_id: String,
}
pub(crate) async fn member_set(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path((id, user)): Path<(String, String)>,
    Json(body): Json<MemberBody>,
) -> Api {
    let _guard = app.gate.lock().await;
    authority(&app, &identity, &id, true).await?;
    let old = app.store.live_access(&user, &id).await?.capability_level;
    let levels = app.store.capability_levels(&id).await?;
    let next = levels
        .levels
        .get(&body.capability_level_id)
        .ok_or_else(|| bad("Capability level not found"))?;
    grant_ceiling(
        &app,
        &identity,
        &serde_json::to_value(&next.ability_permissions).map_err(anyhow::Error::from)?,
        &serde_json::to_value(&next.tool_permissions).map_err(anyhow::Error::from)?,
        &serde_json::to_value(&old.ability_permissions).map_err(anyhow::Error::from)?,
        &serde_json::to_value(&old.tool_permissions).map_err(anyhow::Error::from)?,
        false,
    )
    .await?;
    let member = app
        .store
        .assign_capability_level(
            identity["_user_id"].as_str().ok_or_else(denied)?,
            &user,
            &id,
            &body.capability_level_id,
            body.expected_revision,
        )
        .await
        .map_err(update_error)?;
    // Configuration applies at the next step: each tool call is re-decided against live
    // permissions, so a revoked tool is refused like a disabled one and running work is not
    // stopped; only approvals waiting under the old settings are revoked.
    phase3_api::revoke_waiting_approvals(
        &app,
        Some(&id),
        "a member's capability level in this agent changed",
    )
    .await?;
    Ok(Json(json!({"membership":member})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PermissionsBody {
    expected_revision: u64,
    ability_permissions: Value,
    tool_permissions: Value,
}
pub(crate) async fn permissions_set(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path(id): Path<String>,
    Json(body): Json<PermissionsBody>,
) -> Api {
    let _guard = app.gate.lock().await;
    authority(&app, &identity, &id, true).await?;
    let previous_abilities = app.store.agent_ability_permissions(&id).await?;
    let previous_tools = app.store.agent_tool_permissions(&id).await?;
    grant_ceiling(
        &app,
        &identity,
        &body.ability_permissions,
        &body.tool_permissions,
        &previous_abilities["ability_permissions"],
        &previous_tools["tool_permissions"],
        true,
    )
    .await?;
    let actor_store = app
        .store
        .for_user(identity["_user_id"].as_str().ok_or_else(denied)?)
        .await?;
    let value = actor_store
        .set_agent_permissions(
            &id,
            body.expected_revision,
            body.ability_permissions,
            body.tool_permissions,
        )
        .await
        .map_err(update_error)?;
    // Configuration applies at the next step: policy and tool decisions are re-read per
    // model call and before every tool, so running work is not stopped; only approvals
    // waiting under the old settings are revoked.
    phase3_api::revoke_waiting_approvals(
        &app,
        Some(&id),
        "this agent's ability permissions changed",
    )
    .await?;
    Ok(Json(value))
}
async fn visibility(
    app: App,
    identity: Value,
    id: String,
    ability: String,
    tool: Option<String>,
    skill: Option<String>,
    body: AbilityVisibilityBody,
) -> Api {
    let _guard = app.gate.lock().await;
    authority(&app, &identity, &id, true).await?;
    if !app.abilities.known(&ability)
        || tool
            .as_ref()
            .is_some_and(|t| !app.abilities.has_tool(&ability, t))
        || skill.as_ref().is_some_and(|name| {
            name != "core"
                && app
                    .abilities
                    .configuration_skill_asset(&ability, "skill", name)
                    .is_none()
        })
        || (skill.as_deref() == Some("core")
            && app.abilities.configuration_skill(&ability).is_none())
    {
        return Err(bad("Unknown ability, skill or tool"));
    }
    if !matches!(body.visibility.as_str(), "off" | "loaded" | "on_request") {
        return Err(bad("Invalid visibility mode"));
    }
    // Core comes only from tool.json and skill.json; its mode cannot be changed.
    if tool
        .as_deref()
        .is_some_and(webagent_abilities::tool_definitions::is_core)
        || (skill.as_deref() == Some("core") && app.abilities.skill_is_core(&ability))
    {
        return Err(bad("Core tools and skills are always loaded"));
    }
    let mut scoped = admin_agent_view(&app, &id).await?;
    scoped.store = scoped
        .store
        .for_user(identity["_user_id"].as_str().ok_or_else(denied)?)
        .await?;
    let mut value = agent_abilities(&scoped).await?;
    if value["revision"] != body.expected_revision {
        return Err(conflict("Agent ability revision changed"));
    }
    let core_skill = skill.as_deref() == Some("core");
    let (key, name) = match tool {
        Some(t) => ("tool_modes", t),
        None => match skill {
            Some(t) if t != "core" => ("skill_visibility", format!("{ability}:{t}")),
            Some(_) => ("skill_visibility", ability),
            None => ("visibility", ability),
        },
    };
    // Match the server defaults so an unchanged write stays a no-op; a selectable
    // skill's default lives in its front matter, so only a saved value counts there.
    let fallback = if core_skill {
        Some(app.abilities.skill_default_mode(&name).to_owned())
    } else if key == "skill_visibility" {
        None
    } else {
        Some("on_request".to_owned())
    };
    if value[key][&name].as_str().or(fallback.as_deref()) == Some(body.visibility.as_str()) {
        return Ok(Json(value));
    }
    value[key][name] = json!(body.visibility);
    value["revision"] = json!(body.expected_revision + 1);
    scoped
        .store
        .save_agent_abilities(&value, body.expected_revision)
        .await?;
    // Configuration applies at the next step: policy and tool decisions are re-read per
    // model call and before every tool, so running work is not stopped; only approvals
    // waiting under the old settings are revoked.
    phase3_api::revoke_waiting_approvals(
        &app,
        Some(&id),
        "this agent's ability visibility changed",
    )
    .await?;
    Ok(Json(value))
}
pub(crate) async fn visibility_set(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path((id, ability)): Path<(String, String)>,
    Json(body): Json<AbilityVisibilityBody>,
) -> Api {
    if ability == webagent_abilities::base::ID && body.visibility == "off" {
        return Err(bad("Base is always on"));
    }
    visibility(app, identity, id, ability, None, None, body).await
}
pub(crate) async fn skill_set(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path((id, ability)): Path<(String, String)>,
    Json(body): Json<AbilityVisibilityBody>,
) -> Api {
    visibility(app, identity, id, ability, None, Some("core".into()), body).await
}
pub(crate) async fn skill_named_set(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path((id, ability, skill)): Path<(String, String, String)>,
    Json(body): Json<AbilityVisibilityBody>,
) -> Api {
    visibility(app, identity, id, ability, None, Some(skill), body).await
}
pub(crate) async fn tool_set(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path((id, ability, tool)): Path<(String, String, String)>,
    Json(body): Json<AbilityVisibilityBody>,
) -> Api {
    visibility(app, identity, id, ability, Some(tool), None, body).await
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ResultDeduplicationBody {
    expected_revision: u64,
    included: bool,
}
pub(crate) async fn result_deduplication_set(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path((id, ability, tool)): Path<(String, String, String)>,
    Json(body): Json<ResultDeduplicationBody>,
) -> Api {
    let _guard = app.gate.lock().await;
    authority(&app, &identity, &id, true).await?;
    if !app.abilities.known(&ability) || !app.abilities.has_tool(&ability, &tool) {
        return Err(bad("Unknown ability or tool"));
    }
    let mut scoped = admin_agent_view(&app, &id).await?;
    scoped.store = scoped
        .store
        .for_user(identity["_user_id"].as_str().ok_or_else(denied)?)
        .await?;
    let mut value = agent_abilities(&scoped).await?;
    if value["revision"] != body.expected_revision {
        return Err(conflict("Agent ability revision changed"));
    }
    if !value["tool_result_deduplication"].is_object() {
        value["tool_result_deduplication"] = json!({});
    }
    value["tool_result_deduplication"][&tool] = json!(body.included);
    value["revision"] = json!(body
        .expected_revision
        .checked_add(1)
        .ok_or_else(|| bad("Agent ability revision overflow"))?);
    scoped
        .store
        .save_agent_abilities(&value, body.expected_revision)
        .await
        .map_err(update_error)?;
    crate::wake::notify(&app).await;
    Ok(Json(value))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExecutionBody {
    expected_revision: u64,
    settings: Value,
}
pub(crate) async fn execution_set(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path((id, ability, tool)): Path<(String, String, String)>,
    Json(body): Json<ExecutionBody>,
) -> Api {
    let _guard = app.gate.lock().await;
    authority(&app, &identity, &id, true).await?;
    if !app.abilities.known(&ability) || !app.abilities.has_tool(&ability, &tool) {
        return Err(bad("Unknown ability or tool"));
    }
    if !body.settings.is_null() {
        let fields = body
            .settings
            .as_object()
            .ok_or_else(|| bad("Execution settings required"))?;
        // A retired on_run_end key from an older page is accepted and dropped.
        if !(2..=4).contains(&fields.len())
            || fields.keys().any(|key| {
                !matches!(
                    key.as_str(),
                    "mode" | "max_concurrency" | "on_run_end" | "duplicate_protection"
                )
            })
            || !fields.contains_key("mode")
            || !fields.contains_key("max_concurrency")
        {
            return Err(bad(
                "Expected execution settings with optional duplicate_protection",
            ));
        }
        if !matches!(
            body.settings["mode"].as_str(),
            Some("foreground" | "background" | "model")
        ) || !body.settings["max_concurrency"]
            .as_u64()
            .is_some_and(|concurrency| (1..=4).contains(&concurrency))
            || (body.settings["mode"] != "foreground"
                && !crate::tool_execution::background_capable(&tool))
        {
            return Err(bad("Invalid or unsupported tool execution settings"));
        }
    }
    let mut scoped = admin_agent_view(&app, &id).await?;
    scoped.store = scoped
        .store
        .for_user(identity["_user_id"].as_str().ok_or_else(denied)?)
        .await?;
    let mut value = agent_abilities(&scoped).await?;
    if value["revision"] != body.expected_revision {
        return Err(conflict("Agent ability revision changed"));
    }
    let revision = body
        .expected_revision
        .checked_add(1)
        .ok_or_else(|| bad("Agent ability revision overflow"))?;
    let before = value.clone();
    if !value["tool_execution"].is_object() {
        value["tool_execution"] = json!({});
    }
    if body.settings.is_null() {
        value["tool_execution"]
            .as_object_mut()
            .unwrap()
            .remove(&tool);
    } else {
        value["tool_execution"][&tool] = body.settings;
    }
    let settings = crate::tool_execution::resolve(&value, &tool)
        .map_err(|_| bad("Invalid or unsupported tool execution settings"))?;
    let settings = serde_json::to_value(settings).map_err(anyhow::Error::from)?;
    if !value["tool_execution"][&tool].is_null() {
        value["tool_execution"][&tool] = settings;
    }
    if before["tool_execution"] == value["tool_execution"] {
        return Ok(Json(before));
    }
    value["revision"] = json!(revision);
    scoped
        .store
        .save_agent_abilities(&value, body.expected_revision)
        .await
        .map_err(update_error)?;
    // Scheduling preferences apply at fresh admission; current authority fences still revoke stale grants.
    crate::wake::notify(&app).await;
    Ok(Json(value))
}
pub(crate) async fn skill_named_content(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path((id, ability, skill)): Path<(String, String, String)>,
) -> Api {
    authority(&app, &identity, &id, true).await?;
    let content = app
        .abilities
        .configuration_skill_asset(&ability, "skill", &skill)
        .ok_or_else(|| {
            ApiError(
                StatusCode::NOT_FOUND,
                "Skill content unavailable; reload the validated catalog".into(),
            )
        })?;
    Ok(Json(
        json!({"content":content,"catalog_version":app.abilities.version()}),
    ))
}
pub(crate) async fn tool_skill_content(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path((id, ability, tool)): Path<(String, String, String)>,
) -> Api {
    authority(&app, &identity, &id, true).await?;
    let content = app
        .abilities
        .configuration_skill_asset(&ability, "tool", &tool)
        .ok_or_else(|| {
            ApiError(
                StatusCode::NOT_FOUND,
                "Tool skill unavailable; reload the validated catalog".into(),
            )
        })?;
    Ok(Json(
        json!({"content":content,"catalog_version":app.abilities.version()}),
    ))
}

// Prompt configuration belongs to the same selected tenant, not the installation.
pub(crate) async fn composition(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path(id): Path<String>,
) -> Api {
    authority(&app, &identity, &id, true).await?;
    agent_context_composition(State(app), Path(id)).await
}
pub(crate) async fn composition_revision(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path(id): Path<String>,
) -> Api {
    authority(&app, &identity, &id, true).await?;
    agent_context_composition_revision(State(app), Path(id)).await
}
pub(crate) async fn skill_content(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path((id, ability)): Path<(String, String)>,
) -> Api {
    authority(&app, &identity, &id, true).await?;
    let content = app.abilities.configuration_skill(&ability).ok_or_else(|| {
        ApiError(
            StatusCode::NOT_FOUND,
            "Skill content unavailable; reload the validated catalog".into(),
        )
    })?;
    Ok(Json(
        json!({"content":content,"catalog_version":app.abilities.version()}),
    ))
}
pub(crate) async fn slots(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path(id): Path<String>,
) -> Api {
    authority(&app, &identity, &id, true).await?;
    prompt_slots_api(State(app), Path(id)).await
}
pub(crate) async fn slot(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path((id, name)): Path<(String, String)>,
) -> Api {
    authority(&app, &identity, &id, true).await?;
    prompt_slot_api(State(app), Path((id, name))).await
}
pub(crate) async fn slot_put(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path((id, name)): Path<(String, String)>,
    Json(body): Json<PromptSlotPutBody>,
) -> Api {
    let _guard = app.gate.lock().await;
    authority(&app, &identity, &id, true).await?;
    prompt_slot_put_api(State(app.clone()), Path((id.clone(), name)), Json(body)).await
}
pub(crate) async fn slot_delete(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path((id, name)): Path<(String, String)>,
    Json(body): Json<PromptSlotDeleteBody>,
) -> Api {
    let _guard = app.gate.lock().await;
    authority(&app, &identity, &id, true).await?;
    prompt_slot_delete_api(State(app.clone()), Path((id.clone(), name)), Json(body)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn delegated_ceiling_follows_the_editors_tier_access() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let user = app
            .store
            .create_user_account("limited_editor", "fixture-only-password-hash")
            .await
            .unwrap();
        let id = user["user_id"].as_str().unwrap();
        app.store
            .update_account_access(id, 1, crate::access::AccountStatus::Active, "free")
            .await
            .unwrap();
        let identity = json!({"_user_id":id});
        let unchanged = json!({"codebase_admin":"auto","web_access":"auto"});
        // A free editor may lift a restriction on an ability their tier has.
        assert!(grant_ceiling(
            &app,
            &identity,
            &json!({"web_access":"auto"}),
            &json!({}),
            &json!({"web_access":"deny"}),
            &json!({}),
            false
        )
        .await
        .is_ok());
        // Administrator abilities are never theirs to grant, directly or through the base default.
        assert!(grant_ceiling(
            &app,
            &identity,
            &json!({"codebase_admin":"auto"}),
            &json!({}),
            &json!({}),
            &json!({}),
            false
        )
        .await
        .is_err());
        assert!(grant_ceiling(
            &app,
            &identity,
            &json!({}),
            &json!({}),
            &json!({"codebase_admin":"deny"}),
            &json!({}),
            true
        )
        .await
        .is_err());
        assert!(grant_ceiling(
            &app,
            &identity,
            &json!({"codebase_admin":"deny"}),
            &json!({}),
            &unchanged,
            &json!({}),
            false
        )
        .await
        .is_ok());
    }
    #[tokio::test]
    async fn duplicate_result_tool_inclusion_is_independent_and_revision_fenced() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let identity = json!({"_user_id":app.store.current_user_id()});
        let before = app.store.agent_abilities().await.unwrap();
        let revision = before["revision"].as_u64().unwrap();
        let path = (
            "shared_default".into(),
            "codebase_admin".into(),
            "file_read".into(),
        );
        result_deduplication_set(
            State(app.clone()),
            Extension(identity.clone()),
            Path(path.clone()),
            Json(ResultDeduplicationBody {
                expected_revision: revision,
                included: false,
            }),
        )
        .await
        .unwrap();
        let saved = app.store.agent_abilities().await.unwrap();
        assert_eq!(saved["tool_result_deduplication"]["file_read"], false);
        for key in ["tool_execution", "tool_modes", "visibility"] {
            assert_eq!(saved[key], before[key]);
        }
        assert_eq!(
            result_deduplication_set(
                State(app.clone()),
                Extension(identity.clone()),
                Path(path),
                Json(ResultDeduplicationBody {
                    expected_revision: revision,
                    included: true
                })
            )
            .await
            .unwrap_err()
            .0,
            StatusCode::CONFLICT
        );
        assert!(result_deduplication_set(
            State(app),
            Extension(identity),
            Path((
                "shared_default".into(),
                "codebase_admin".into(),
                "unknown-tool".into()
            )),
            Json(ResultDeduplicationBody {
                expected_revision: revision + 1,
                included: true
            })
        )
        .await
        .is_err());
    }
    #[tokio::test]
    async fn tool_execution_is_authorized_validated_persisted_and_revision_fenced() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let identity = json!({"_user_id":app.store.current_user_id()});
        let before = app.store.agent_abilities().await.unwrap();
        let revision = before["revision"].as_u64().unwrap();
        let path = (
            "shared_default".into(),
            "codebase_admin".into(),
            "file_read".into(),
        );
        let settings = json!({"mode":"foreground","max_concurrency":2});
        execution_set(
            State(app.clone()),
            Extension(identity.clone()),
            Path(path.clone()),
            Json(ExecutionBody {
                expected_revision: revision,
                settings: settings.clone(),
            }),
        )
        .await
        .unwrap();
        let saved = app.store.agent_abilities().await.unwrap();
        assert_eq!(saved["tool_execution"]["file_read"], settings);
        assert_eq!(saved["revision"], revision + 1);
        assert_eq!(saved["visibility"], before["visibility"]);
        let response = read(
            State(app.clone()),
            Extension(identity.clone()),
            Path("shared_default".into()),
        )
        .await
        .unwrap();
        assert!(response.0.get("tool_execution").is_none());
        let tool = response.0["abilities"]
            .as_array()
            .unwrap()
            .iter()
            .find(|ability| ability["id"] == "codebase_admin")
            .unwrap()["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["name"] == "file_read")
            .unwrap();
        assert_eq!(tool["execution"], settings);
        assert!(tool["advanced_policies"].is_null());
        for ability in response.0["abilities"].as_array().unwrap() {
            for tool in ability["tools"].as_array().unwrap() {
                let name = tool["name"].as_str().unwrap();
                assert_eq!(
                    tool["advanced_policies"],
                    crate::tool_execution::advanced_policies(name)
                );
            }
        }
        assert_eq!(
            tool["background_capable"],
            crate::tool_execution::background_capable("file_read")
        );
        assert_eq!(
            tool["max_supported_concurrency"],
            crate::tool_execution::max_supported_concurrency("file_read")
        );
        let member = app
            .store
            .create_user_account("execution_reader", "fixture-only-password-hash")
            .await
            .unwrap();
        let member_id = member["user_id"].as_str().unwrap();
        app.store
            .update_account_access(member_id, 1, crate::access::AccountStatus::Active, "free")
            .await
            .unwrap();
        app.store
            .set_agent_membership(
                member_id,
                "shared_default",
                crate::access::Relationship::Member,
                0,
            )
            .await
            .unwrap();
        let member_identity = json!({"_user_id":member_id});
        // The Abilities page is operator-only.
        assert!(read(
            State(app.clone()),
            Extension(member_identity.clone()),
            Path("shared_default".into()),
        )
        .await
        .is_err());
        assert!(execution_set(
            State(app.clone()),
            Extension(member_identity),
            Path(path.clone()),
            Json(ExecutionBody {
                expected_revision: revision + 1,
                settings: settings.clone()
            })
        )
        .await
        .is_err());
        let stale = execution_set(
            State(app.clone()),
            Extension(identity.clone()),
            Path(path.clone()),
            Json(ExecutionBody {
                expected_revision: revision,
                settings: settings.clone(),
            }),
        )
        .await
        .err()
        .unwrap();
        assert_eq!(stale.0, StatusCode::CONFLICT);
        execution_set(
            State(app.clone()),
            Extension(identity.clone()),
            Path(path.clone()),
            Json(ExecutionBody {
                expected_revision: revision + 1,
                settings: settings.clone(),
            }),
        )
        .await
        .unwrap();
        assert_eq!(
            app.store.agent_abilities().await.unwrap()["revision"],
            revision + 1
        );
        for invalid in [
            json!({"mode":"invalid","max_concurrency":1}),
            json!({"mode":"foreground","max_concurrency":0}),
            json!({"mode":"foreground","max_concurrency":5}),
            json!({"mode":"foreground","max_concurrency":1.5}),
            json!({"mode":"foreground","max_concurrency":1,"extra":true}),
        ] {
            let error = execution_set(
                State(app.clone()),
                Extension(identity.clone()),
                Path(path.clone()),
                Json(ExecutionBody {
                    expected_revision: revision + 1,
                    settings: invalid,
                }),
            )
            .await
            .err()
            .unwrap();
            assert_eq!(error.0, StatusCode::BAD_REQUEST);
        }
        assert!(execution_set(
            State(app.clone()),
            Extension(json!({"_user_id":"outsider"})),
            Path(path.clone()),
            Json(ExecutionBody {
                expected_revision: revision + 1,
                settings: settings.clone()
            })
        )
        .await
        .is_err());
        for unsupported in [
            json!({"mode":"background","max_concurrency":1}),
            json!({"mode":"model","max_concurrency":1}),
            json!({"mode":"foreground","max_concurrency":2}),
        ] {
            let unsupported_path = (
                "shared_default".into(),
                "codebase_admin".into(),
                "file_write".into(),
            );
            let error = execution_set(
                State(app.clone()),
                Extension(identity.clone()),
                Path(unsupported_path),
                Json(ExecutionBody {
                    expected_revision: revision + 1,
                    settings: unsupported,
                }),
            )
            .await
            .err()
            .unwrap();
            assert_eq!(error.0, StatusCode::BAD_REQUEST);
        }
        let unknown = (
            "shared_default".into(),
            "codebase_admin".into(),
            "not_a_tool".into(),
        );
        assert!(execution_set(
            State(app.clone()),
            Extension(identity.clone()),
            Path(unknown),
            Json(ExecutionBody {
                expected_revision: revision + 1,
                settings: settings.clone()
            })
        )
        .await
        .is_err());
        let next = visibility_set(
            State(app.clone()),
            Extension(identity.clone()),
            Path(("shared_default".into(), "codebase_admin".into())),
            Json(AbilityVisibilityBody {
                expected_revision: revision + 1,
                visibility: "off".into(),
            }),
        )
        .await
        .unwrap();
        assert_eq!(next.0["revision"], revision + 2);
        assert_eq!(next.0["tool_execution"]["file_read"], settings);
        assert!(execution_set(
            State(app.clone()),
            Extension(identity),
            Path(path),
            Json(ExecutionBody {
                expected_revision: revision + 1,
                settings
            })
        )
        .await
        .is_err());
        assert_eq!(
            app.store.agent_abilities().await.unwrap()["revision"],
            revision + 2
        );
    }
    /// Preference changes preserve admitted work and exact inherited defaults can be restored.
    #[tokio::test]
    async fn execution_preference_preserves_runs_and_clears_override() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let sid = crate::runtime_tests::conversation(&app).await;
        let rid = crate::runtime_tests::queued(&app, &sid).await;
        let before_run = app.run(&rid).await.unwrap();
        let before = app.store.agent_abilities().await.unwrap();
        let revision = before["revision"].as_u64().unwrap();
        let path = (
            "shared_default".into(),
            "codebase_admin".into(),
            "file_list".into(),
        );
        let identity = json!({"_user_id":app.store.current_user_id()});
        execution_set(
            State(app.clone()),
            Extension(identity.clone()),
            Path(path.clone()),
            Json(ExecutionBody {
                expected_revision: revision,
                settings: json!({"mode":"background","max_concurrency":1}),
            }),
        )
        .await
        .unwrap();
        assert_eq!(app.run(&rid).await.unwrap(), before_run);
        execution_set(
            State(app.clone()),
            Extension(identity),
            Path(path),
            Json(ExecutionBody {
                expected_revision: revision + 1,
                settings: Value::Null,
            }),
        )
        .await
        .unwrap();
        let after = app.store.agent_abilities().await.unwrap();
        assert!(after["tool_execution"]["file_list"].is_null());
        assert_eq!(after["visibility"], before["visibility"]);
        assert_eq!(app.run(&rid).await.unwrap(), before_run);
    }
    #[tokio::test]
    async fn named_skill_save_is_independent_and_authorized() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let identity = json!({"_user_id":app.store.current_user_id()});
        let before = app.store.agent_abilities().await.unwrap();
        let revision = before["revision"].as_u64().unwrap();
        let path = (
            "shared_default".into(),
            "codebase_admin".into(),
            "ui_design".into(),
        );
        skill_named_set(
            State(app.clone()),
            Extension(identity.clone()),
            Path(path.clone()),
            Json(AbilityVisibilityBody {
                visibility: "off".into(),
                expected_revision: revision,
            }),
        )
        .await
        .unwrap();
        let saved = app.store.agent_abilities().await.unwrap();
        assert_eq!(saved["skill_visibility"]["codebase_admin:ui_design"], "off");
        assert_eq!(
            saved["skill_visibility"]["codebase_admin"],
            before["skill_visibility"]["codebase_admin"]
        );
        assert!(skill_named_set(
            State(app.clone()),
            Extension(identity.clone()),
            Path(path.clone()),
            Json(AbilityVisibilityBody {
                visibility: "loaded".into(),
                expected_revision: revision
            })
        )
        .await
        .is_err());
        assert!(
            skill_named_content(State(app.clone()), Extension(identity), Path(path.clone()))
                .await
                .is_ok()
        );
        assert!(skill_named_content(
            State(app),
            Extension(json!({"_user_id":"outsider"})),
            Path(path)
        )
        .await
        .is_err());
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SettingsBody {
    expected_revision: u64,
    settings: Value,
}
pub(crate) async fn settings_set(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path((id, ability)): Path<(String, String)>,
    Json(body): Json<SettingsBody>,
) -> Api {
    let _guard = app.gate.lock().await;
    authority(&app, &identity, &id, true).await?;
    let valid = match ability.as_str() {
        "git_control" => body.settings.as_object().is_some_and(|m| {
            m.len() == 1 && m.get("notify_repo_changes").is_some_and(Value::is_boolean)
        }),
        "optimization" => serde_json::from_value::<
            crate::optimization_settings::OptimizationSettings,
        >(body.settings.clone())
        .is_ok_and(|settings| settings.validate().is_ok()),
        _ => app
            .abilities
            .validate_optional_settings(&ability, &body.settings)
            .unwrap_or(false),
    };
    if !valid {
        return Err(bad("Unsupported or invalid ability settings"));
    }
    let mut scoped = admin_agent_view(&app, &id).await?;
    scoped.store = scoped
        .store
        .for_user(identity["_user_id"].as_str().ok_or_else(denied)?)
        .await?;
    let mut value = agent_abilities(&scoped).await?;
    if value["revision"] != body.expected_revision {
        return Err(conflict("Agent ability revision changed"));
    }
    if !value["settings"].is_object() {
        value["settings"] = json!({});
    }
    value["settings"][ability] = body.settings;
    value["revision"] = json!(body.expected_revision + 1);
    scoped
        .store
        .save_agent_abilities(&value, body.expected_revision)
        .await?;
    // Configuration applies at the next step: policy and tool decisions are re-read per
    // model call and before every tool, so running work is not stopped; only approvals
    // waiting under the old settings are revoked.
    phase3_api::revoke_waiting_approvals(&app, Some(&id), "this agent's ability settings changed")
        .await?;
    Ok(Json(value))
}
#[cfg(test)]
mod optimization_settings_authority_tests {
    use super::*;

    #[tokio::test]
    async fn optimization_settings_persist_and_fence_invalid_stale_and_denied_writes() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let identity = json!({"_user_id": app.store.current_user_id()});
        let revision = app.store.agent_abilities().await.unwrap()["revision"]
            .as_u64()
            .unwrap();
        let settings = serde_json::to_value(crate::optimization_settings::OptimizationSettings {
            attempts: 2,
            cpu_cores: 2,
            ram_mb: 4096,
            ..Default::default()
        })
        .unwrap();
        let path = ("shared_default".into(), "optimization".into());
        let saved = settings_set(
            State(app.clone()),
            Extension(identity.clone()),
            Path(path.clone()),
            Json(SettingsBody {
                expected_revision: revision,
                settings: settings.clone(),
            }),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(saved["settings"]["optimization"], settings);
        assert_eq!(
            app.store.agent_abilities().await.unwrap()["settings"]["optimization"],
            settings
        );
        let stale = settings_set(
            State(app.clone()),
            Extension(identity.clone()),
            Path(path.clone()),
            Json(SettingsBody {
                expected_revision: revision,
                settings: settings.clone(),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(stale.0, StatusCode::CONFLICT);
        for invalid in [
            json!({"cpu_cores":3}),
            json!({"ram_mb":0}),
            json!({"unknown":true}),
        ] {
            let result = settings_set(
                State(app.clone()),
                Extension(identity.clone()),
                Path(path.clone()),
                Json(SettingsBody {
                    expected_revision: revision + 1,
                    settings: invalid,
                }),
            )
            .await
            .unwrap_err();
            assert_eq!(result.0, StatusCode::BAD_REQUEST);
        }
        assert!(settings_set(
            State(app.clone()),
            Extension(json!({"_user_id":"outsider"})),
            Path(path),
            Json(SettingsBody {
                expected_revision: revision + 1,
                settings
            })
        )
        .await
        .is_err());
        assert_eq!(
            app.store.agent_abilities().await.unwrap()["revision"],
            revision + 1
        );
    }
}

#[cfg(test)]
mod duplicate_settings_tests {
    use super::*;
    #[tokio::test]
    async fn retired_duplicate_protection_is_not_offered() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let identity = json!({"_user_id":app.store.current_user_id()});
        let path = (
            "shared_default".into(),
            "codebase_admin".into(),
            "prompt_slot_write".into(),
        );
        let revision = app.store.agent_abilities().await.unwrap()["revision"]
            .as_u64()
            .unwrap();
        let old = json!({"mode":"foreground","max_concurrency":1});
        let saved = execution_set(
            State(app.clone()),
            Extension(identity.clone()),
            Path(path),
            Json(ExecutionBody {
                expected_revision: revision,
                settings: old,
            }),
        )
        .await
        .unwrap()
        .0;
        assert!(saved["tool_execution"]["prompt_slot_write"]["duplicate_protection"].is_null());
        let catalog = read(
            State(app.clone()),
            Extension(identity),
            Path("shared_default".into()),
        )
        .await
        .unwrap()
        .0;
        let tool = catalog["abilities"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|a| a["tools"].as_array().into_iter().flatten())
            .find(|t| t["name"] == "prompt_slot_write")
            .unwrap();
        assert_eq!(tool["duplicate_protection_capable"], false);
    }
}

#[cfg(test)]
mod optional_package_settings_authority_tests {
    use super::*;
    fn defaults() -> Value {
        let manifest: Value = serde_json::from_slice(
            &std::fs::read(
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("abilities/Core/automation/automation.json"),
            )
            .unwrap(),
        )
        .unwrap();
        Value::Object(
            manifest["config"]["settings"]
                .as_array()
                .unwrap()
                .iter()
                .map(|field| {
                    (
                        field["key"].as_str().unwrap().to_owned(),
                        field["default"].clone(),
                    )
                })
                .collect(),
        )
    }
    #[tokio::test]
    async fn omitted_package_settings_preserve_authority_revision_and_no_execution_grants() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let original = app.store.agent_abilities().await.unwrap();
        let revision = original["revision"].as_u64().unwrap();
        let original_access = app
            .store
            .live_access("admin", "shared_default")
            .await
            .unwrap()
            .ability_permission("automation");
        let identity = json!({"_user_id":"admin"});
        let path = ("shared_default".into(), "automation".into());
        let configured = defaults();
        let saved = settings_set(
            State(app.clone()),
            Extension(identity.clone()),
            Path(path.clone()),
            Json(SettingsBody {
                expected_revision: revision,
                settings: configured.clone(),
            }),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(saved["settings"]["automation"], configured);
        assert_eq!(saved["revision"], revision + 1);
        assert_eq!(saved["visibility"], original["visibility"]);
        assert_eq!(saved["tool_modes"], original["tool_modes"]);
        assert_eq!(
            app.store.agent_abilities().await.unwrap()["settings"]["automation"],
            configured
        );
        assert_eq!(
            app.store
                .live_access("admin", "shared_default")
                .await
                .unwrap()
                .ability_permission("automation"),
            original_access
        );
        let stale = settings_set(
            State(app.clone()),
            Extension(identity.clone()),
            Path(path.clone()),
            Json(SettingsBody {
                expected_revision: revision,
                settings: configured.clone(),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(stale.0, StatusCode::CONFLICT);
        let mut invalid = configured.clone();
        invalid.as_object_mut().unwrap().remove("require_approval");
        let malformed = settings_set(
            State(app.clone()),
            Extension(identity),
            Path(path.clone()),
            Json(SettingsBody {
                expected_revision: revision + 1,
                settings: invalid,
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(malformed.0, StatusCode::BAD_REQUEST);
        let outsider = settings_set(
            State(app.clone()),
            Extension(json!({"_user_id":"outsider"})),
            Path(path),
            Json(SettingsBody {
                expected_revision: revision + 1,
                settings: configured,
            }),
        )
        .await
        .unwrap_err();
        assert!(matches!(
            outsider.0,
            StatusCode::FORBIDDEN | StatusCode::NOT_FOUND
        ));
        assert_eq!(
            app.store.agent_abilities().await.unwrap()["revision"],
            revision + 1
        );
    }
}
