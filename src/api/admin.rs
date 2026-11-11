//! HTTP handlers for admin operations.
use super::*;
pub(crate) async fn overview(State(app): State<App>) -> Api {
    // Installation status must not reveal another owner\'s device authorization code.
    let operator = app
        .store
        .agent_authority(app.store.current_user_id(), app.store.current_agent_id())
        .await
        .is_ok_and(|r| r["can_manage"] == true);
    let login=app.login.read().await.clone().filter(|_|operator).filter(|v|v["connection_id"].is_null()).map(|v|json!({"verification_url":v["verification_url"],"user_code":v["user_code"],"status":v["status"]}));
    let mut runs = app.store.active_runs().await?;
    runs.extend(app.store.recent_runs(None).await?);
    let runs = runs.iter().map(public_run).collect::<Vec<_>>();
    Ok(Json(
        json!({"shutoff":app.shutoff().await?,"provider":{"connected":if operator {Some(app.store.get("provider_tokens").await?.is_some())}else{None},"login":login},
        "model":if operator {app.store.agent_model().await?["model"].clone()}else{Value::Null},"policy":app.store.get("policy").await?,"runs":runs}),
    ))
}
pub(crate) fn requested_selection(
    body: &Value,
) -> std::result::Result<provider::ProviderSelection, ApiError> {
    required(body, "model", 128)?;
    provider::ProviderSelection::parse(body, DEFAULT_AGENT_MODEL)
        .map_err(|_| bad("Invalid model or unsupported provider"))
}
pub(crate) async fn settings(State(app): State<App>, Json(body): Json<Value>) -> Api {
    require_agent_operator(&app, app.store.current_agent_id()).await?;
    let selection = requested_selection(&body)?;
    let _guard = app.gate.lock().await;
    let value = selection.value();
    app.store.save_agent_model(&value).await?;
    Ok(Json(value))
}
pub(crate) async fn admin_agent_view(app: &App, id: &str) -> std::result::Result<App, ApiError> {
    let mut scoped = app.clone();
    scoped.store = app
        .store
        .for_agent(id)
        .await
        .map_err(|_| ApiError(StatusCode::NOT_FOUND, "Agent not found".into()))?;
    let exists: Option<String> = sqlx::query_scalar("SELECT id FROM agents WHERE id=?")
        .bind(id)
        .fetch_optional(&scoped.store.agent_pool)
        .await
        .map_err(anyhow::Error::from)?;
    if exists.is_none() {
        return Err(ApiError(StatusCode::NOT_FOUND, "Agent not found".into()));
    }
    Ok(scoped)
}
// Model-only persisted settings retain the original subscription default.
pub(crate) async fn agent_model(State(app): State<App>, Path(id): Path<String>) -> Api {
    require_agent_operator(&app, &id).await?;
    let app = admin_agent_view(&app, &id).await?;
    let mut value = app.selection().await?.value();
    value["id"] = json!(id);
    Ok(Json(value))
}
pub(crate) async fn agent_model_set(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Api {
    require_agent_operator(&app, &id).await?;
    let app = admin_agent_view(&app, &id).await?;
    let selection = requested_selection(&body)?;
    let _guard = app.gate.lock().await;
    let value = selection.value();
    app.store.save_agent_model(&value).await?;
    let mut result = value;
    result["id"] = json!(id);
    Ok(Json(result))
}
// The original v3 seed used gpt-5.4. Move that exact old default once,
// while leaving any other explicitly selected model untouched.
pub(crate) async fn migrate_default_agent_model(store: &Store) -> Result<()> {
    const MARKER: &str = "migration:default_model_gpt_6_luna";
    if store.get(MARKER).await?.is_some() {
        return Ok(());
    }
    let mut value = store.agent_model().await?;
    if value["model"] == "gpt-5.4" {
        value["model"] = json!(DEFAULT_AGENT_MODEL);
        store.save_agent_model(&value).await?;
    }
    store.put(MARKER, &json!({"complete":true})).await?;
    Ok(())
}
pub(crate) async fn migrate_provider_selection(store: &Store) -> Result<()> {
    let value = store.agent_model().await?;
    let selection = provider::ProviderSelection::parse(&value, DEFAULT_AGENT_MODEL)?;
    let normalized = selection.value();
    if value != normalized {
        store.save_agent_model(&normalized).await?;
    }
    Ok(())
}

pub(crate) async fn migrate_tool_policy_defaults(store: &Store) -> Result<()> {
    let mut policy = store.get("policy").await?.unwrap_or_else(policy_default);
    let defaults = policy_default();
    let mut changed = false;
    if policy["rules"]["build_work"].is_null() {
        policy["rules"]["build_work"] = json!("require_approval");
        changed = true;
    }
    // Rules for the retired v3 build and source tools no longer gate anything.
    for retired in [
        "v3_build_work",
        "v3_worker_submit",
        "v3_source_list",
        "v3_source_read",
        "v3_source_apply",
    ] {
        if let Some(rules) = policy["rules"].as_object_mut() {
            changed |= rules.remove(retired).is_some();
        }
    }
    // Renamed Base background controls keep the rule their old names had.
    for (new, old) in [
        ("background_status", "task_status"),
        ("background_cancel", "task_cancel"),
    ] {
        if policy["rules"][new].is_null() || policy["rules"].get(old).is_some() {
            if let Some(rule) = policy["rules"][old].as_str().map(str::to_owned) {
                policy["rules"][new] = json!(rule);
            }
        }
    }
    for old in ["task_status", "task_wait", "task_cancel"] {
        if policy["rules"]
            .as_object_mut()
            .is_some_and(|rules| rules.remove(old).is_some())
        {
            changed = true;
        }
    }
    // A tool folder's `policy_ceiling` is its installation rule, set nowhere else.
    for (name, definition) in webagent_abilities::tool_definitions::definitions() {
        let rule = json!(match definition.policy_ceiling {
            webagent_access::Permission::Auto => "allow",
            webagent_access::Permission::Ask => "require_approval",
            webagent_access::Permission::Deny => "deny",
        });
        if policy["rules"][name] != rule {
            policy["rules"][name] = rule;
            changed = true;
        }
    }
    for (name, _) in defaults["rules"].as_object().unwrap() {
        if policy["rules"][name].is_null() {
            policy["rules"][name] = json!("deny");
            changed = true;
        }
    }
    if changed {
        policy["version"] = json!(policy["version"].as_u64().unwrap_or(0) + 1);
        store.put("policy", &policy).await?;
    }
    Ok(())
}
pub(crate) async fn ability_catalog(State(app): State<App>) -> Api {
    let policy = app
        .store
        .get("policy")
        .await?
        .unwrap_or_else(policy_default);
    let visibility = Value::Null;
    Ok(Json(json!({"catalog_version":app.abilities.version(),
        "visibility_revision":visibility["revision"],
        "abilities":app.abilities.catalog_with(&policy, &visibility, &Value::Null)})))
}
pub(crate) async fn agent_ability_catalog(State(app): State<App>, Path(id): Path<String>) -> Api {
    let app = admin_agent_view(&app, &id).await?;
    let visibility = agent_abilities(&app).await?;
    let policy = app
        .store
        .get("policy")
        .await?
        .unwrap_or_else(policy_default);
    Ok(Json(json!({"catalog_version":app.abilities.version(),
        "revision":visibility["revision"],"visibility":visibility["visibility"],
        "skill_visibility":visibility["skill_visibility"],"tool_modes":visibility["tool_modes"],
        "abilities":app.abilities.configuration_catalog(&policy, &visibility)})))
}
// Same fresh-agent Prompt / Skills / Tools composition as the v2 Abilities
// overview. V3 has no provider tokenizer yet, so the numbers are explicit
// character-based estimates over the actual persisted slots and sent schemas.
pub(crate) fn estimated_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4)
}
pub(crate) async fn agent_context_composition(
    State(app): State<App>,
    Path(id): Path<String>,
) -> Api {
    let app = admin_agent_view(&app, &id).await?;
    let slots = app.store.list_prompt_slots().await?;
    let visibility = agent_abilities(&app).await?;
    let policy = app
        .store
        .get("policy")
        .await?
        .unwrap_or_else(policy_default);
    let mut prompt_sources = vec![json!({"label":"Base instructions",
        "tokens":estimated_tokens(provider::DEFAULT_INSTRUCTIONS)})];
    for slot in slots {
        let content = slot["content"].as_str().unwrap_or("").trim();
        if !content.is_empty() {
            prompt_sources.push(json!({"label":slot["slot_name"],
                "tokens":estimated_tokens(content)}));
        }
    }
    let guidance = app
        .abilities
        .visible_guidance(&policy, &visibility, &Value::Null);
    let skill_sources = if guidance.is_empty() {
        Vec::new()
    } else {
        vec![json!({"label":"Visible ability guidance","tokens":estimated_tokens(&guidance)})]
    };
    let tool_sources = app
        .abilities
        .schemas_with(&policy, &json!([]), &visibility, &Value::Null)
        .into_iter()
        .map(|schema| {
            let name = schema["name"].as_str().unwrap_or("Tool");
            json!({"label":name,"tokens":estimated_tokens(&schema.to_string())})
        })
        .collect::<Vec<_>>();
    let sum = |sources: &Vec<Value>| -> usize {
        sources
            .iter()
            .map(|source| source["tokens"].as_u64().unwrap_or(0) as usize)
            .sum()
    };
    let prompt_tokens = sum(&prompt_sources);
    let skill_tokens = sum(&skill_sources);
    let tool_tokens = sum(&tool_sources);
    Ok(Json(json!({
        "tokens":prompt_tokens + skill_tokens + tool_tokens,
        "estimated":true,
        "breakdown":{"system_prompt":{"tokens":prompt_tokens},
            "skills":{"tokens":skill_tokens},"tools":{"tokens":tool_tokens}},
        "sources":{"system_prompt":prompt_sources,"skills":skill_sources,
            "tools":tool_sources}
    })))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AbilityVisibilityBody {
    pub(crate) visibility: String,
    pub(crate) expected_revision: u64,
}
pub(crate) async fn agent_ability_visibility_set(
    State(app): State<App>,
    Path((id, ability_id)): Path<(String, String)>,
    Json(body): Json<AbilityVisibilityBody>,
) -> Api {
    let app = admin_agent_view(&app, &id).await?;
    if !app.abilities.known(&ability_id) {
        return Err(bad("Unknown ability ID"));
    }
    if !matches!(body.visibility.as_str(), "off" | "visible" | "discoverable") {
        return Err(bad("Invalid ability visibility"));
    }
    let _guard = app.gate.lock().await;
    let mut value = agent_abilities(&app).await?;
    if value["revision"] != body.expected_revision {
        return Err(conflict("Agent ability revision changed"));
    }
    let old = value["visibility"][&ability_id]
        .as_str()
        .unwrap_or("discoverable");
    if old == body.visibility {
        return Ok(Json(value));
    }
    value["revision"] = json!(body.expected_revision + 1);
    value["visibility"][&ability_id] = json!(body.visibility);
    app.store
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
pub(crate) async fn agent_ability_skill_visibility_set(
    State(app): State<App>,
    Path((id, ability_id)): Path<(String, String)>,
    Json(body): Json<AbilityVisibilityBody>,
) -> Api {
    let app = admin_agent_view(&app, &id).await?;
    if !app.abilities.known(&ability_id) {
        return Err(bad("Unknown ability ID"));
    }
    if !matches!(body.visibility.as_str(), "off" | "visible" | "discoverable") {
        return Err(bad("Invalid skill visibility"));
    }
    let _guard = app.gate.lock().await;
    let mut value = agent_abilities(&app).await?;
    if value["revision"] != body.expected_revision {
        return Err(conflict("Agent ability revision changed"));
    }
    let old = value["skill_visibility"][&ability_id]
        .as_str()
        .unwrap_or("visible");
    if old == body.visibility {
        return Ok(Json(value));
    }
    value["revision"] = json!(body.expected_revision + 1);
    value["skill_visibility"][&ability_id] = json!(body.visibility);
    app.store
        .save_agent_abilities(&value, body.expected_revision)
        .await?;
    // Configuration applies at the next step: policy and tool decisions are re-read per
    // model call and before every tool, so running work is not stopped; only approvals
    // waiting under the old settings are revoked.
    phase3_api::revoke_waiting_approvals(&app, Some(&id), "this agent's skill visibility changed")
        .await?;
    Ok(Json(value))
}
pub(crate) async fn agent_tool_mode_set(
    State(app): State<App>,
    Path((id, ability_id, tool_name)): Path<(String, String, String)>,
    Json(body): Json<AbilityVisibilityBody>,
) -> Api {
    let app = admin_agent_view(&app, &id).await?;
    if !app.abilities.has_tool(&ability_id, &tool_name) {
        return Err(bad("Unknown ability tool"));
    }
    if !matches!(body.visibility.as_str(), "off" | "visible" | "discoverable") {
        return Err(bad("Invalid tool mode"));
    }
    let _guard = app.gate.lock().await;
    let mut value = agent_abilities(&app).await?;
    if value["revision"] != body.expected_revision {
        return Err(conflict("Agent ability revision changed"));
    }
    if value["tool_modes"][&tool_name] == body.visibility {
        return Ok(Json(value));
    }
    value["revision"] = json!(body.expected_revision + 1);
    value["tool_modes"][&tool_name] = json!(body.visibility);
    app.store
        .save_agent_abilities(&value, body.expected_revision)
        .await?;
    // Configuration applies at the next step: policy and tool decisions are re-read per
    // model call and before every tool, so running work is not stopped; only approvals
    // waiting under the old settings are revoked.
    phase3_api::revoke_waiting_approvals(&app, Some(&id), "this agent's tool modes changed")
        .await?;
    Ok(Json(value))
}
pub(crate) async fn policy_get(State(app): State<App>) -> Api {
    let mut policy = app
        .store
        .get("policy")
        .await?
        .unwrap_or_else(policy_default);
    let names: std::collections::BTreeSet<String> = app
        .abilities
        .all_schemas()
        .iter()
        .chain(crate::mcp::all_schemas().iter())
        .filter_map(|s| s["name"].as_str().map(str::to_owned))
        .chain(
            ["list_abilities", "load_ability"]
                .into_iter()
                .map(str::to_owned),
        )
        .collect();
    // Show unnamed tools with the template rule the runtime applies to them.
    let template = policy_default();
    for name in &names {
        if policy["rules"].get(name).is_none() {
            policy["rules"][name] = template["rules"].get(name).cloned().unwrap_or_else(|| {
                json!(if crate::tools::is_read(name) {
                    "allow"
                } else {
                    "require_approval"
                })
            });
        }
    }
    Ok(Json(
        json!({"version":policy["version"],"rules":policy["rules"],"catalog_names":names}),
    ))
}
// Tenant connections are the sole credential authority after the catalog migration.
// Retire the global route so installation oversight cannot bypass connection ownership.
pub(crate) async fn provider_start(State(app): State<App>) -> Api {
    require_agent_operator(&app, "shared_default").await?;
    Err(conflict(
        "Use the selected agent's Models page to authorize a connection",
    ))
}
/// A device authorization belongs to one connection. Never copy subscription tokens.
pub(crate) async fn provider_start_for(
    app: App,
    credential_ref: String,
    connection_id: Option<String>,
) -> Api {
    app.models_runtime()
        .provider_start_for(credential_ref, connection_id)
        .await
        .map_err(Into::into)
}
pub(crate) async fn provider_logout(State(app): State<App>) -> Api {
    require_agent_operator(&app, "shared_default").await?;
    // Revocation belongs to the owner-scoped catalog, never a global token deletion.
    Err(conflict(
        "Use the selected agent's Models page to revoke a connection",
    ))
}

#[cfg(test)]
mod legacy_credential_tests {
    use super::*;
    // Installation-only legacy routes cannot change credentials behind an explicit role plan.
    #[tokio::test]
    async fn legacy_provider_routes_cannot_bypass_connection_ownership() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let before = app.store.get("provider_tokens").await.unwrap();
        *app.login.write().await =
            Some(json!({"connection_id":"tenant-connection","status":"pending"}));
        assert!(matches!(
            provider_start(State(app.clone())).await,
            Err(ApiError(StatusCode::CONFLICT, _))
        ));
        assert!(matches!(
            provider_logout(State(app.clone())).await,
            Err(ApiError(StatusCode::CONFLICT, _))
        ));
        assert_eq!(app.store.get("provider_tokens").await.unwrap(), before);
        assert_eq!(
            app.login.read().await.as_ref().unwrap()["connection_id"],
            "tenant-connection"
        );
    }
}

async fn require_agent_operator(app: &App, agent: &str) -> Result<(), ApiError> {
    if !app
        .store
        .agent_authority(app.store.current_user_id(), agent)
        .await
        .is_ok_and(|r| r["can_manage"] == true)
    {
        return Err(ApiError(StatusCode::NOT_FOUND, "Agent not found".into()));
    }
    Ok(())
}
