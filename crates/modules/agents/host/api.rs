//! Agent API composition: neighbors validate their settings; Agents owns identity/revisions/SQL.
use crate::access::Principal;
use crate::{Api, ApiError, App, State};
use axum::{
    extract::{Path, Query},
    http::StatusCode,
    routing::{get, post},
    Extension, Json, Router,
};
use serde_json::{json, Value};
#[path = "bridge.rs"]
mod super_bridge;
async fn principal(app: &App, identity: &Value) -> Result<Principal, ApiError> {
    let account = app
        .store
        .account_by_id(identity["_user_id"].as_str().unwrap_or(""))
        .await?
        .ok_or_else(denied)?;
    let caller = Principal::from_account(&account)?;
    if !caller.active() {
        return Err(denied());
    }
    Ok(caller)
}
fn denied() -> ApiError {
    ApiError(
        StatusCode::FORBIDDEN,
        "Agent or model administration unavailable".into(),
    )
}
async fn authority(
    app: &App,
    identity: &Value,
    agent: &str,
    write: bool,
) -> Result<Value, ApiError> {
    let caller = principal(app, identity).await?;
    let rights = app
        .store
        .agent_authority(&caller.user_id, agent)
        .await
        .map_err(|_| denied())?;
    if if write {
        rights["can_manage"] != true
    } else {
        rights["can_manage"] != true
            && !app
                .store
                .live_access(&caller.user_id, agent)
                .await
                .is_ok_and(|a| a.admitted())
    } {
        return Err(denied());
    }
    Ok(rights)
}
async fn agent_metadata(store: &crate::store::Store, agent: &str) -> anyhow::Result<Value> {
    let blob: Vec<u8> = sqlx::query_scalar("SELECT metadata FROM agents WHERE id=?")
        .bind(agent)
        .fetch_one(&store.agent_pool)
        .await?;
    store.unseal_agent(&blob)
}
pub(crate) async fn status_value(
    store: &crate::store::Store,
    agent: &str,
) -> anyhow::Result<Value> {
    webagent_agents::repository::Repository::new(&store.agent_pool, agent, store)?
        .status()
        .await
}
/// Existing mode readers in Chat/Engine use the same encrypted metadata and defaults.
pub(crate) async fn modes_config(
    store: &crate::store::Store,
    agent: &str,
) -> anyhow::Result<Value> {
    let view = store.for_agent(agent).await?;
    webagent_agents::modes::config(&agent_metadata(&view, agent).await?)
}
#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct RosterQuery {
    /// The recycling bin: deleted agents the caller manages, instead of the usable roster.
    deleted: bool,
}
// AGENT-DISCOVERY: list only agents the caller can use or explicitly administer.
async fn agents(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Query(query): Query<RosterQuery>,
) -> Api {
    use sqlx::Row;
    let caller = principal(&app, &identity).await?;
    let mut values = Vec::new();
    for id in app.store.agent_ids().await? {
        let Ok(rights) = authority(&app, &identity, &id, false).await else {
            continue;
        };
        let view = app.store.for_agent(&id).await?;
        let mut value = webagent_agents::repository::Repository::new(&view.agent_pool, &id, &view)?
            .summary()
            .await?;
        if (value["status"] == "deleted") != query.deleted
            || (query.deleted && rights["can_manage"] != true)
        {
            continue;
        }
        value["can_manage"] = rights["can_manage"].clone();
        values.push(value);
    }
    // Even a caller with no memberships can create their first owned agent.
    Ok(Json(
        json!({"agents":values,"caller_id":caller.user_id,"can_create_agent":caller.role != crate::access::Role::Guest}),
    ))
}

// IDENTITY-WRITE: identity and named execution modes; tenant authority and revision fence edits.
async fn identity_get(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path(id): Path<String>,
) -> Api {
    use sqlx::Row;
    let rights = authority(&app, &identity, &id, true).await?;
    let store = app.store.for_agent(&id).await?;
    let owned = webagent_agents::repository::Repository::new(&store.agent_pool, &id, &store)?
        .identity()
        .await?;
    let config = webagent_agents::modes::config(&agent_metadata(&store, &id).await?)?;
    let defaults = crate::context_compaction::Config::read(&app.compaction_config_path)?;
    let override_settings = crate::context_compaction::agent_thresholds(&store, &id).await?;
    // Keep Identity readable if a later app-default edit invalidates an override.
    let effective =
        override_settings
            .clone()
            .unwrap_or(crate::context_compaction::AgentThresholds {
                trigger_tokens: defaults.trigger_tokens,
                tail_tokens: defaults.tail_tokens,
            });
    let compaction_valid = effective.apply(&defaults).is_ok();
    let tool_output_defaults = crate::tool_output::Policy::live()?;
    let tool_output_override = crate::tool_output::agent_override(&store, &id).await?;
    let tool_output_overridden = tool_output_override.is_some();
    let tool_output_policy = match tool_output_override {
        Some(value) => serde_json::from_value::<crate::tool_output::Policy>(value)
            .map_err(anyhow::Error::from)?,
        None => tool_output_defaults.clone(),
    };
    tool_output_policy.validate()?;
    let tool_output_eligible_tools = crate::tool_output::eligible_tools();
    let settings = crate::gate_controls::agent_visibility(&store, &id).await?;
    let closing = crate::gate_controls::agent_closing(&store, &id).await?;
    let manager = crate::manager::config::incoming(&store, &id).await?;
    let closing_protocol = crate::gate_registry::closing_config()?;
    let fixed = crate::gate_registry::visibility_config()?.guidance;
    Ok(Json(
        json!({"member_access":store.access_repository().member_access(&id).await?,"capability_levels":store.capability_levels(&id).await?,"shared_chat":crate::shared_chat::policy(&store,&id).await?,"manager_incoming_message":manager,"closing_message":closing,"closing_protocol":{"flags":closing_protocol.flags,"notification_max_words":closing_protocol.notification_max_words},"message_visibility":{"mode":settings.mode,"guidance":settings.guidance,"fixed_protocol":fixed},"execution_modes":config["execution_modes"],"default_execution_mode":config["default_execution_mode"],"id":id,"name":owned["name"],
        "compaction_thresholds":{"trigger_tokens":effective.trigger_tokens,"tail_tokens":effective.tail_tokens},
        "compaction_defaults":{"trigger_tokens":defaults.trigger_tokens,"tail_tokens":defaults.tail_tokens,"summary_tokens":defaults.summary_tokens},
        "tool_result_deduplication":crate::tool_output::dedup_policy(&store,&id).await?,"tool_output_policy":tool_output_policy,"tool_output_defaults":tool_output_defaults,
        "tool_output_overridden":tool_output_overridden,"tool_output_eligible_tools":tool_output_eligible_tools,
        "compaction_overridden":override_settings.is_some(),"compaction_valid":compaction_valid,
        "description":owned["description"],"revision":owned["revision"],
        "can_manage":rights["can_manage"]}),
    ))
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct IdentityUpdate {
    member_access: Option<crate::access::membership::MemberAccess>,
    tool_result_deduplication: Option<crate::tool_output::DedupPolicy>,
    revision: i64,
    shared_chat: Option<crate::shared_chat::SharedChatPolicy>,
    name: Option<String>,
    description: Option<String>,
    execution_modes: Option<Value>,
    default_execution_mode: Option<String>,
    message_visibility: Option<crate::gate_controls::AgentVisibility>,
    closing_message: Option<crate::gate_controls::AgentClosing>,
    compaction_thresholds: Option<crate::context_compaction::AgentThresholds>,
    manager_incoming_message: Option<crate::manager::config::IncomingMessage>,
    tool_output_policy: Option<crate::tool_output::Policy>,
    reset_tool_output_policy: Option<bool>,
}
async fn identity_put(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path(id): Path<String>,
    Json(body): Json<IdentityUpdate>,
) -> Api {
    if body.tool_output_policy.is_some() && body.reset_tool_output_policy.is_some() {
        return Err(crate::bad("Set a tool output policy or reset it, not both"));
    }
    if let Some(policy) = body.tool_output_policy.as_ref() {
        policy
            .validate()
            .map_err(|_| crate::bad("Invalid tool output retention policy"))?;
    }
    if body.revision < 0
        || (body.name.is_none()
            && body.description.is_none()
            && body.execution_modes.is_none()
            && body.default_execution_mode.is_none()
            && body.message_visibility.is_none()
            && body.closing_message.is_none()
            && body.compaction_thresholds.is_none()
            && body.manager_incoming_message.is_none()
            && body.tool_output_policy.is_none()
            && body.reset_tool_output_policy.is_none()
            && body.shared_chat.is_none()
            && body.member_access.is_none()
            && body.tool_result_deduplication.is_none())
        || body
            .name
            .as_ref()
            .is_some_and(|s| s.trim().is_empty() || s.len() > 200)
        || body.description.as_ref().is_some_and(|s| s.len() > 2000)
    {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "Name must contain 1–200 bytes; description at most 2000 bytes".into(),
        ));
    }
    let _guard = app.gate.lock().await;
    authority(&app, &identity, &id, true).await?;
    let store = app.store.for_agent(&id).await?;
    let blob: Vec<u8> = sqlx::query_scalar("SELECT metadata FROM agents WHERE id=?")
        .bind(&id)
        .fetch_one(&store.agent_pool)
        .await
        .map_err(anyhow::Error::from)?;
    let mut metadata = store.unseal(&blob)?;
    if let Some(settings) = body.tool_result_deduplication {
        // Preserve legacy tool overrides before replacing only the independently fenced agent policy.
        let tools =
            crate::store::Store::tool_result_overrides(&metadata).map_err(anyhow::Error::from)?;
        metadata["tool_result_deduplication_tools"] = tools;
        metadata["tool_result_deduplication"] =
            serde_json::to_value(settings).map_err(anyhow::Error::from)?;
    }
    if let Some(settings) = body.shared_chat {
        if settings.admin_view || settings.admin_join {
            return Err(crate::bad(
                "Use conversation privacy for new chats; legacy administrative access is disabled",
            ));
        }
        metadata["shared_chat"] = serde_json::to_value(settings).map_err(anyhow::Error::from)?;
    }
    if let Some(settings) = body.member_access {
        metadata["member_access"] = serde_json::to_value(settings).map_err(anyhow::Error::from)?;
    }
    let own_update = webagent_agents::IdentityUpdate {
        revision: body.revision,
        name: body.name.clone(),
        description: body.description.clone(),
        execution_modes: body.execution_modes.clone(),
        default_execution_mode: body.default_execution_mode.clone(),
    };
    own_update
        .apply(&mut metadata)
        .map_err(|e| crate::bad(&e.to_string()))?;
    if let Some(thresholds) = body.compaction_thresholds {
        let defaults = crate::context_compaction::Config::read(&app.compaction_config_path)?;
        thresholds.apply(&defaults).map_err(|_| {
            crate::bad("Invalid compaction thresholds: tail plus summary must be below trigger")
        })?;
        metadata["compaction_thresholds"] =
            serde_json::to_value(thresholds).map_err(anyhow::Error::from)?;
    }
    if let Some(policy) = body.tool_output_policy {
        metadata["tool_output_policy"] =
            serde_json::to_value(policy).map_err(anyhow::Error::from)?;
    } else if body.reset_tool_output_policy == Some(true) {
        if let Some(fields) = metadata.as_object_mut() {
            fields.remove("tool_output_policy");
        }
    }
    if let Some(settings) = body.message_visibility {
        settings
            .validate()
            .map_err(|e| crate::bad(&e.to_string()))?;
        metadata["message_visibility"] =
            serde_json::to_value(settings).map_err(anyhow::Error::from)?;
    }
    if let Some(settings) = body.closing_message {
        settings
            .validate()
            .map_err(|e| crate::bad(&e.to_string()))?;
        metadata["closing_message"] =
            serde_json::to_value(settings).map_err(anyhow::Error::from)?;
    }
    let manager_prompt = body
        .manager_incoming_message
        .as_ref()
        .map(|settings| settings.criteria.clone());
    if let Some(settings) = body.manager_incoming_message {
        settings
            .validate()
            .map_err(|e| crate::bad(&e.to_string()))?;
        if settings.enabled {
            let mut scoped = app.clone();
            scoped.store = store.clone();
            // Validate the actual judge appointment before accepting an enabled gate.
            let mut plan = store
                .model_catalog()
                .await?
                .resolve_role(&id, crate::model_selection::ModelRole::System)
                .map_err(|_| {
                    crate::bad(
                        "Assign an authorized System model before enabling incoming-message review",
                    )
                })?;
            scoped
                .pin_model_plan(&mut plan)
                .await
                .map_err(|_| crate::bad("System model is unavailable"))?;
        }
        // Criteria are per agent and stay in its encrypted record.
        let next = json!({"enabled":settings.enabled,"criteria":settings.criteria});
        // Prompt edits also withdraw any review pinned to the earlier Identity revision.
        if manager_prompt.is_some() {
            metadata["manager_incoming_message_revision"] = json!(metadata
                ["manager_incoming_message_revision"]
                .as_u64()
                .unwrap_or(0)
                .checked_add(1)
                .ok_or_else(|| crate::bad("Manager settings revision exhausted"))?);
        }
        metadata["manager_incoming_message"] = next;
    }
    let mut transaction = store
        .agent_pool
        .begin()
        .await
        .map_err(anyhow::Error::from)?;
    webagent_agents::repository::Repository::new(&store.agent_pool, &id, &store)?
        .save_in(
            &mut transaction,
            &own_update,
            &metadata,
            crate::now() as i64,
        )
        .await
        .map_err(|_| crate::conflict("Identity changed; reload before saving"))?;
    transaction.commit().await.map_err(anyhow::Error::from)?;
    identity_get(State(app.clone()), Extension(identity), Path(id)).await
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentStatusChange {
    expected_revision: u64,
    status: String,
}
async fn operator(app: &App, identity: &Value, agent: &str) -> Result<(), ApiError> {
    if !app
        .store
        .agent_authority(identity["_user_id"].as_str().unwrap_or(""), agent)
        .await
        .is_ok_and(|rights| rights["can_manage"] == true)
    {
        return Err(ApiError(StatusCode::NOT_FOUND, "Agent not found".into()));
    }
    Ok(())
}
async fn agent_status(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path(id): Path<String>,
) -> Api {
    crate::phase3_api::administrator(&app, &identity).await?;
    operator(&app, &identity, &id).await?;
    let store = app.store.for_agent(&id).await?;
    Ok(Json(status_value(&store, &id).await?))
}
async fn change_agent_status(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path(id): Path<String>,
    Json(body): Json<AgentStatusChange>,
) -> Api {
    crate::phase3_api::administrator(&app, &identity).await?;
    operator(&app, &identity, &id).await?;
    if !matches!(body.status.as_str(), "active" | "disabled") {
        return Err(crate::bad("Agent status must be active or disabled"));
    }
    let bridge = super_bridge::Bridge {
        app: &app,
        identity: &identity,
    };
    let service = webagent_agents::service::Service {
        storage: &app.store,
        access: &bridge,
        effects: &bridge,
        gate: app.gate.as_ref(),
    };
    service
        .set_status(&id, body.expected_revision, &body.status)
        .await
        .map(Json)
        .map_err(|error| {
            let message = error.to_string();
            if message == "Invalid agent status" {
                crate::bad(&message)
            } else if message == "Agent revision changed" {
                crate::conflict("Agent status changed; reload before saving")
            } else if message == "Agent administration unavailable"
                || message == "Administrator access required"
            {
                denied()
            } else if message == "Agent not found" {
                ApiError(StatusCode::NOT_FOUND, message)
            } else {
                error.into()
            }
        })
}
pub(crate) use webagent_agents::NewAgent;
pub(crate) async fn create_agent(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Json(body): Json<NewAgent>,
) -> Api {
    if principal(&app, &identity).await?.role == crate::access::Role::Guest {
        return Err(denied());
    }
    body.validate()
        .map_err(|_| crate::bad("Invalid agent fields"))?;
    let bridge = super_bridge::Bridge {
        app: &app,
        identity: &identity,
    };
    let service = webagent_agents::service::Service {
        storage: &app.store,
        access: &bridge,
        effects: &bridge,
        gate: app.gate.as_ref(),
    };
    service
        .create(&body, crate::now() as i64)
        .await
        .map(Json)
        .map_err(|error| {
            let message = error.to_string();
            if matches!(
                message.as_str(),
                "Request identifier reused with different input" | "Agent ownership changed"
            ) {
                crate::conflict(&message)
            } else if message == "Agent administration unavailable" {
                denied()
            } else {
                error.into()
            }
        })
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct BinChange {
    expected_revision: u64,
}
async fn set_deleted(app: &App, identity: &Value, id: &str, expected: u64, deleted: bool) -> Api {
    let bridge = super_bridge::Bridge { app, identity };
    let service = webagent_agents::service::Service {
        storage: &app.store,
        access: &bridge,
        effects: &bridge,
        gate: app.gate.as_ref(),
    };
    service
        .set_deleted(id, expected, deleted)
        .await
        .map(Json)
        .map_err(|error| {
            let message = error.to_string();
            match message.as_str() {
                "Agent revision changed" => crate::conflict("Agent changed; reload and try again"),
                "Only active agents can be deleted" | "Agent is not in the recycling bin" => {
                    crate::conflict(&message)
                }
                "The default agent cannot be deleted" | "Invalid agent status" => {
                    crate::bad(&message)
                }
                "Agent not found" => ApiError(StatusCode::NOT_FOUND, message),
                _ if message.ends_with("administration unavailable") => denied(),
                _ => error.into(),
            }
        })
}
// AGENT-BIN: managers move agents to the recycling bin and back; chat admits only active agents.
async fn delete_agent(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path(id): Path<String>,
    Json(body): Json<BinChange>,
) -> Api {
    set_deleted(&app, &identity, &id, body.expected_revision, true).await
}
async fn restore_agent(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path(id): Path<String>,
    Json(body): Json<BinChange>,
) -> Api {
    set_deleted(&app, &identity, &id, body.expected_revision, false).await
}

pub fn router() -> Router<App> {
    Router::new()
        .route("/agents", get(agents).post(create_agent))
        .route("/agents/{id}", axum::routing::delete(delete_agent))
        .route("/agents/{id}/restore", post(restore_agent))
        .route("/agents/{id}/identity", get(identity_get).put(identity_put))
}

/// Nested under the authenticated administrator router at its existing URL.
pub fn admin_router() -> Router<App> {
    Router::new().route(
        "/agents/{id}/status",
        get(agent_status).put(change_agent_status),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime_tests::{fixture, selection_fixture};
    async fn tenant_user(app: &App, name: &str) -> String {
        let account = app
            .store
            .create_user_account(name, "long-hash-placeholder")
            .await
            .unwrap();
        let id = account["user_id"].as_str().unwrap();
        app.store
            .update_account_access(
                id,
                1,
                crate::access::AccountStatus::Active,
                crate::access::Role::User,
                "operator",
            )
            .await
            .unwrap();
        id.to_owned()
    }
    #[tokio::test]
    async fn agent_status_api_disables_calls_and_rejects_stale_revision() {
        let (_root, app) = fixture().await;
        let admin = json!({"_user_id":"admin"});
        let Json(before) = agent_status(
            State(app.clone()),
            Extension(admin.clone()),
            Path("shared_default".into()),
        )
        .await
        .unwrap();
        let revision = before["revision"].as_u64().unwrap();
        let Json(disabled) = change_agent_status(
            State(app.clone()),
            Extension(admin.clone()),
            Path("shared_default".into()),
            Json(AgentStatusChange {
                expected_revision: revision,
                status: "disabled".into(),
            }),
        )
        .await
        .unwrap();
        assert_eq!(disabled["status"], "disabled");
        assert!(!app
            .store
            .live_access("admin", "shared_default")
            .await
            .unwrap()
            .admitted());
        assert!(change_agent_status(
            State(app.clone()),
            Extension(admin),
            Path("shared_default".into()),
            Json(AgentStatusChange {
                expected_revision: revision,
                status: "active".into()
            })
        )
        .await
        .is_err());
        let outsider = tenant_user(&app, "status-outsider").await;
        assert_eq!(
            agent_status(
                State(app),
                Extension(json!({"_user_id":outsider})),
                Path("shared_default".into())
            )
            .await
            .unwrap_err()
            .0,
            StatusCode::FORBIDDEN
        );
    }
    #[tokio::test]
    async fn recycling_bin_api_hides_blocks_and_restores_owned_agents() {
        let (_root, app) = fixture().await;
        let owner = json!({"_user_id":tenant_user(&app, "bin-owner").await});
        let Json(created) = create_agent(
            State(app.clone()),
            Extension(owner.clone()),
            Json(NewAgent {
                client_request_id: "bin-agent".into(),
                name: "Bin agent".into(),
                description: None,
            }),
        )
        .await
        .unwrap();
        let id = created["id"].as_str().unwrap().to_owned();
        let roster = |deleted: bool| {
            let (app, owner) = (app.clone(), owner.clone());
            async move {
                let Json(value) =
                    agents(State(app), Extension(owner), Query(RosterQuery { deleted }))
                        .await
                        .unwrap();
                value["agents"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|agent| agent["id"].as_str().unwrap().to_owned())
                    .collect::<Vec<_>>()
            }
        };
        assert!(roster(false).await.contains(&id));
        assert!(!roster(true).await.contains(&id));
        let view = app.store.for_agent(&id).await.unwrap();
        let revision = status_value(&view, &id).await.unwrap()["revision"]
            .as_u64()
            .unwrap();
        let outsider = json!({"_user_id":tenant_user(&app, "bin-outsider").await});
        assert_eq!(
            delete_agent(
                State(app.clone()),
                Extension(outsider),
                Path(id.clone()),
                Json(BinChange {
                    expected_revision: revision
                }),
            )
            .await
            .unwrap_err()
            .0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            delete_agent(
                State(app.clone()),
                Extension(owner.clone()),
                Path("shared_default".into()),
                Json(BinChange {
                    expected_revision: 1
                }),
            )
            .await
            .unwrap_err()
            .0,
            StatusCode::BAD_REQUEST
        );
        let Json(deleted) = delete_agent(
            State(app.clone()),
            Extension(owner.clone()),
            Path(id.clone()),
            Json(BinChange {
                expected_revision: revision,
            }),
        )
        .await
        .unwrap();
        assert_eq!(deleted["status"], "deleted");
        assert!(!roster(false).await.contains(&id));
        assert_eq!(roster(true).await, vec![id.clone()]);
        let owner_id = owner["_user_id"].as_str().unwrap();
        assert!(!app
            .store
            .live_access(owner_id, &id)
            .await
            .unwrap()
            .admitted());
        assert_eq!(
            restore_agent(
                State(app.clone()),
                Extension(owner.clone()),
                Path(id.clone()),
                Json(BinChange {
                    expected_revision: revision
                }),
            )
            .await
            .unwrap_err()
            .0,
            StatusCode::CONFLICT
        );
        let Json(restored) = restore_agent(
            State(app.clone()),
            Extension(owner.clone()),
            Path(id.clone()),
            Json(BinChange {
                expected_revision: revision + 1,
            }),
        )
        .await
        .unwrap();
        assert_eq!(restored["status"], "active");
        assert!(roster(false).await.contains(&id));
        assert!(roster(true).await.is_empty());
    }
    #[tokio::test]
    async fn visibility_preferences_preserve_fixed_protocol_and_revision_fence() {
        let (_root, app) = fixture().await;
        let identity = json!({"_user_id":"admin"});
        let Json(before) = identity_get(
            State(app.clone()),
            Extension(identity.clone()),
            Path("shared_default".into()),
        )
        .await
        .unwrap();
        assert_eq!(before["message_visibility"]["mode"], "manual");
        let patch = |revision, guidance: &str| IdentityUpdate {
            member_access: None,
            tool_result_deduplication: None,
            shared_chat: None,
            revision,
            name: None,
            description: None,
            execution_modes: None,
            default_execution_mode: None,
            message_visibility: Some(crate::gate_controls::AgentVisibility {
                mode: crate::gate_controls::PromotionMode::None,
                guidance: guidance.into(),
            }),
            closing_message: None,
            manager_incoming_message: None,
            compaction_thresholds: None,
            tool_output_policy: None,
            reset_tool_output_policy: None,
        };
        let revision = before["revision"].as_i64().unwrap();
        let Json(saved) = identity_put(
            State(app.clone()),
            Extension(identity.clone()),
            Path("shared_default".into()),
            Json(patch(revision, "Keep progress concise")),
        )
        .await
        .unwrap();
        assert_eq!(
            saved["message_visibility"]["fixed_protocol"],
            before["message_visibility"]["fixed_protocol"]
        );
        assert_eq!(saved["message_visibility"]["mode"], "none");
        assert!(saved["message_visibility"].get("format").is_none());
        let settings = crate::gate_controls::agent_visibility(&app.store, "shared_default")
            .await
            .unwrap();
        assert_eq!(settings.mode, crate::gate_controls::PromotionMode::None);
        assert_eq!(settings.guidance, "Keep progress concise");
        let Json(reloaded) = identity_get(
            State(app.clone()),
            Extension(identity.clone()),
            Path("shared_default".into()),
        )
        .await
        .unwrap();
        assert_eq!(reloaded, saved);
        assert_eq!(
            identity_put(
                State(app.clone()),
                Extension(identity.clone()),
                Path("shared_default".into()),
                Json(patch(revision, "stale"))
            )
            .await
            .unwrap_err()
            .0,
            StatusCode::CONFLICT
        );
        let outsider = tenant_user(&app, "visibility-outsider").await;
        assert!(identity_put(
            State(app.clone()),
            Extension(json!({"_user_id":outsider})),
            Path("shared_default".into()),
            Json(patch(revision + 1, "forbidden"))
        )
        .await
        .is_err());
        assert!(identity_put(
            State(app.clone()),
            Extension(identity),
            Path("shared_default".into()),
            Json(patch(revision + 1, &"x".repeat(4097)))
        )
        .await
        .is_err());
    }
    /// Agent toggle and tool inclusion remain independent, including legacy shared-key metadata.
    #[tokio::test]
    async fn dedup_policy_and_tool_overrides_are_independent_and_migrate_atomically() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let id = "shared_default".to_owned();
        let identity = json!({"_user_id":app.store.current_user_id()});
        let blob: Vec<u8> = sqlx::query_scalar("SELECT metadata FROM agents WHERE id=?")
            .bind(&id)
            .fetch_one(&app.store.agent_pool)
            .await
            .unwrap();
        let mut metadata = app.store.unseal(&blob).unwrap();
        metadata["tool_result_deduplication"] = json!({"enabled":false,"file_read":false});
        metadata
            .as_object_mut()
            .unwrap()
            .remove("tool_result_deduplication_tools");
        sqlx::query("UPDATE agents SET metadata=? WHERE id=?")
            .bind(app.store.seal_agent(&metadata).unwrap())
            .bind(&id)
            .execute(&app.store.agent_pool)
            .await
            .unwrap();
        let legacy = identity_get(
            State(app.clone()),
            Extension(identity.clone()),
            Path(id.clone()),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(
            legacy["tool_result_deduplication"],
            json!({"enabled":false})
        );
        let before = app.store.agent_abilities().await.unwrap();
        assert_eq!(
            before["tool_result_deduplication"],
            json!({"file_read":false})
        );
        let update = |revision: i64, enabled: bool| {
            serde_json::from_value::<IdentityUpdate>(json!({
            "revision":revision,"tool_result_deduplication":{"enabled":enabled}}))
            .unwrap()
        };
        let saved = identity_put(
            State(app.clone()),
            Extension(identity.clone()),
            Path(id.clone()),
            Json(update(legacy["revision"].as_i64().unwrap(), true)),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(saved["tool_result_deduplication"], json!({"enabled":true}));
        let controls = app.store.agent_abilities().await.unwrap();
        assert_eq!(controls["tool_result_deduplication"]["file_read"], false);
        // Exercise the actual authenticated ability route without exposing its private module.
        use tower::ServiceExt;
        let token = crate::uuid();
        app.store.put(&format!("auth:{}",crate::digest(&json!(token))),
            &json!({"csrf":"fixture","expires":crate::now()+3600,"generation":"fixture-generation"})).await.unwrap();
        let request = axum::http::Request::builder()
            .method("PUT")
            .uri(format!(
                "/api/v3/agents/{id}/abilities/codebase_admin/tools/file_read/result-deduplication"
            ))
            .header("content-type", "application/json")
            .header("cookie", format!("v3_session={token}"))
            .header("x-csrf-token", "fixture")
            .body(axum::body::Body::from(
                json!({"expected_revision":controls["revision"],"included":true}).to_string(),
            ))
            .unwrap();
        assert_eq!(
            crate::api::router(app.clone())
                .oneshot(request)
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        let after_tool = identity_get(
            State(app.clone()),
            Extension(identity.clone()),
            Path(id.clone()),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(
            after_tool["tool_result_deduplication"],
            json!({"enabled":true})
        );
        assert_eq!(
            identity_put(
                State(app.clone()),
                Extension(identity.clone()),
                Path(id.clone()),
                Json(update(saved["revision"].as_i64().unwrap(), false))
            )
            .await
            .unwrap_err()
            .0,
            StatusCode::CONFLICT
        );
        identity_put(
            State(app.clone()),
            Extension(identity.clone()),
            Path(id.clone()),
            Json(update(after_tool["revision"].as_i64().unwrap(), false)),
        )
        .await
        .unwrap();
        let final_policy = crate::tool_output::dedup_policy(&app.store, &id)
            .await
            .unwrap();
        assert!(!final_policy.enabled);
        let final_tools = app.store.agent_abilities().await.unwrap();
        assert_eq!(final_tools["tool_result_deduplication"]["file_read"], true);
        for key in ["visibility", "tool_modes", "tool_execution"] {
            assert_eq!(final_tools[key], before[key]);
        }
        let blob: Vec<u8> = sqlx::query_scalar("SELECT metadata FROM agents WHERE id=?")
            .bind(&id)
            .fetch_one(&app.store.agent_pool)
            .await
            .unwrap();
        let migrated = app.store.unseal(&blob).unwrap();
        assert_eq!(
            migrated["tool_result_deduplication"],
            json!({"enabled":false})
        );
        assert_eq!(
            migrated["tool_result_deduplication_tools"],
            json!({"file_read":true})
        );
        assert!(
            serde_json::from_value::<IdentityUpdate>(json!({"revision":1,
            "tool_result_deduplication":{"enabled":true,"file_read":false}}))
            .is_err()
        );
    }

    #[tokio::test]
    async fn identity_compaction_thresholds_are_agent_scoped_and_validated() {
        let (_root, app) = fixture().await;
        let identity = json!({"_user_id":"admin"});
        let Json(before) = identity_get(
            State(app.clone()),
            Extension(identity.clone()),
            Path("shared_default".into()),
        )
        .await
        .unwrap();
        assert_eq!(before["compaction_overridden"], false);
        let defaults = &before["compaction_defaults"];
        let trigger = defaults["trigger_tokens"].as_u64().unwrap() as usize + 1000;
        let tail = defaults["tail_tokens"].as_u64().unwrap() as usize;
        let update = |revision, trigger, tail| IdentityUpdate {
            member_access: None,
            tool_result_deduplication: None,
            shared_chat: None,
            revision,
            name: None,
            description: None,
            execution_modes: None,
            default_execution_mode: None,
            message_visibility: None,
            closing_message: None,
            manager_incoming_message: None,
            compaction_thresholds: Some(crate::context_compaction::AgentThresholds {
                trigger_tokens: trigger,
                tail_tokens: tail,
            }),
            tool_output_policy: None,
            reset_tool_output_policy: None,
        };
        let revision = before["revision"].as_i64().unwrap();
        let Json(saved) = identity_put(
            State(app.clone()),
            Extension(identity.clone()),
            Path("shared_default".into()),
            Json(update(revision, trigger, tail)),
        )
        .await
        .unwrap();
        assert_eq!(saved["compaction_overridden"], true);
        assert_eq!(
            saved["compaction_thresholds"]["trigger_tokens"].as_u64(),
            Some(trigger as u64)
        );
        let store = app.store.for_agent("shared_default").await.unwrap();
        let override_settings =
            crate::context_compaction::agent_thresholds(&store, "shared_default")
                .await
                .unwrap()
                .unwrap();
        assert_eq!(override_settings.trigger_tokens, trigger);
        assert_eq!(
            identity_put(
                State(app.clone()),
                Extension(identity.clone()),
                Path("shared_default".into()),
                Json(update(revision, trigger, tail))
            )
            .await
            .unwrap_err()
            .0,
            StatusCode::CONFLICT
        );
        assert_eq!(
            identity_put(
                State(app.clone()),
                Extension(identity.clone()),
                Path("shared_default".into()),
                Json(update(
                    saved["revision"].as_i64().unwrap(),
                    trigger,
                    trigger
                ))
            )
            .await
            .unwrap_err()
            .0,
            StatusCode::BAD_REQUEST
        );
        let outsider = tenant_user(&app, "compaction-outsider").await;
        assert_eq!(
            identity_put(
                State(app.clone()),
                Extension(json!({"_user_id":outsider})),
                Path("shared_default".into()),
                Json(update(
                    saved["revision"].as_i64().unwrap(),
                    trigger + 1000,
                    tail
                ))
            )
            .await
            .unwrap_err()
            .0,
            StatusCode::FORBIDDEN
        );
    }
    #[tokio::test]
    async fn duplicate_result_identity_toggle_is_revision_fenced_and_manager_only() {
        let (_root, app) = fixture().await;
        let identity = json!({"_user_id":"admin"});
        let Json(before) = identity_get(
            State(app.clone()),
            Extension(identity.clone()),
            Path("shared_default".into()),
        )
        .await
        .unwrap();
        assert_eq!(before["tool_result_deduplication"]["enabled"], true);
        let update = |revision| IdentityUpdate {
            member_access: None,
            tool_result_deduplication: Some(crate::tool_output::DedupPolicy { enabled: false }),
            shared_chat: None,
            revision,
            name: None,
            description: None,
            execution_modes: None,
            default_execution_mode: None,
            message_visibility: None,
            closing_message: None,
            compaction_thresholds: None,
            manager_incoming_message: None,
            tool_output_policy: None,
            reset_tool_output_policy: None,
        };
        let revision = before["revision"].as_i64().unwrap();
        let Json(saved) = identity_put(
            State(app.clone()),
            Extension(identity.clone()),
            Path("shared_default".into()),
            Json(update(revision)),
        )
        .await
        .unwrap();
        assert_eq!(saved["tool_result_deduplication"]["enabled"], false);
        assert_eq!(
            identity_put(
                State(app.clone()),
                Extension(identity.clone()),
                Path("shared_default".into()),
                Json(update(revision))
            )
            .await
            .unwrap_err()
            .0,
            StatusCode::CONFLICT
        );
        let outsider = tenant_user(&app, "duplicate-result-outsider").await;
        assert_eq!(
            identity_put(
                State(app.clone()),
                Extension(json!({"_user_id":outsider})),
                Path("shared_default".into()),
                Json(update(saved["revision"].as_i64().unwrap()))
            )
            .await
            .unwrap_err()
            .0,
            StatusCode::FORBIDDEN
        );
    }
    #[tokio::test]
    async fn tool_output_policy_roundtrips_validates_and_resets_with_identity_revision() {
        let (_root, app) = fixture().await;
        let identity = json!({"_user_id":"admin"});
        let Json(before) = identity_get(
            State(app.clone()),
            Extension(identity.clone()),
            Path("shared_default".into()),
        )
        .await
        .unwrap();
        let defaults = crate::tool_output::Policy::live().unwrap();
        assert_eq!(before["tool_output_overridden"], false);
        assert_eq!(before["tool_output_defaults"], json!(defaults));
        assert_eq!(before["tool_output_policy"], json!(defaults));
        let edited = crate::tool_output::Policy {
            enabled: false,
            scope: crate::tool_output::Scope::Selected,
            tools: vec!["file_read".into()],
            lifetime_calls: 9,
            ordinary_chars: 32_000,
            promoted_chars: 18_000,
            refresh_on_recall: false,
            promote_on_edit: false,
        };
        let update = |revision, tool_output_policy, reset_tool_output_policy| IdentityUpdate {
            member_access: None,
            tool_result_deduplication: None,
            shared_chat: None,
            revision,
            name: None,
            description: None,
            execution_modes: None,
            default_execution_mode: None,
            message_visibility: None,
            closing_message: None,
            compaction_thresholds: None,
            manager_incoming_message: None,
            tool_output_policy,
            reset_tool_output_policy,
        };
        let revision = before["revision"].as_i64().unwrap();
        let Json(saved) = identity_put(
            State(app.clone()),
            Extension(identity.clone()),
            Path("shared_default".into()),
            Json(update(revision, Some(edited.clone()), None)),
        )
        .await
        .unwrap();
        assert_eq!(saved["tool_output_overridden"], true);
        assert_eq!(saved["tool_output_policy"], json!(edited));
        let Json(reloaded) = identity_get(
            State(app.clone()),
            Extension(identity.clone()),
            Path("shared_default".into()),
        )
        .await
        .unwrap();
        assert_eq!(reloaded, saved);
        assert_eq!(
            identity_put(
                State(app.clone()),
                Extension(identity.clone()),
                Path("shared_default".into()),
                Json(update(revision, Some(edited.clone()), None))
            )
            .await
            .unwrap_err()
            .0,
            StatusCode::CONFLICT
        );

        let mut invalid = edited.clone();
        invalid.lifetime_calls = 0;
        assert_eq!(
            identity_put(
                State(app.clone()),
                Extension(identity.clone()),
                Path("shared_default".into()),
                Json(update(
                    saved["revision"].as_i64().unwrap(),
                    Some(invalid),
                    None
                ))
            )
            .await
            .unwrap_err()
            .0,
            StatusCode::BAD_REQUEST
        );
        let mut invalid = edited.clone();
        invalid.tools = vec!["file_write".into()];
        assert_eq!(
            identity_put(
                State(app.clone()),
                Extension(identity.clone()),
                Path("shared_default".into()),
                Json(update(
                    saved["revision"].as_i64().unwrap(),
                    Some(invalid),
                    None
                ))
            )
            .await
            .unwrap_err()
            .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            identity_put(
                State(app.clone()),
                Extension(identity.clone()),
                Path("shared_default".into()),
                Json(update(
                    saved["revision"].as_i64().unwrap(),
                    Some(edited.clone()),
                    Some(true)
                ))
            )
            .await
            .unwrap_err()
            .0,
            StatusCode::BAD_REQUEST
        );

        let outsider = tenant_user(&app, "tool-output-outsider").await;
        assert_eq!(
            identity_put(
                State(app.clone()),
                Extension(json!({"_user_id":outsider})),
                Path("shared_default".into()),
                Json(update(
                    saved["revision"].as_i64().unwrap(),
                    Some(edited.clone()),
                    None
                ))
            )
            .await
            .unwrap_err()
            .0,
            StatusCode::FORBIDDEN
        );

        let Json(reset) = identity_put(
            State(app.clone()),
            Extension(identity.clone()),
            Path("shared_default".into()),
            Json(update(
                saved["revision"].as_i64().unwrap(),
                None,
                Some(true),
            )),
        )
        .await
        .unwrap();
        assert_eq!(reset["tool_output_overridden"], false);
        assert_eq!(reset["tool_output_policy"], json!(defaults));
        let Json(reloaded) = identity_get(
            State(app.clone()),
            Extension(identity),
            Path("shared_default".into()),
        )
        .await
        .unwrap();
        assert_eq!(reloaded, reset);
        let store = app.store.for_agent("shared_default").await.unwrap();
        assert_eq!(
            crate::tool_output::effective(&store, "shared_default")
                .await
                .unwrap(),
            defaults
        );
    }

    #[tokio::test]
    async fn notification_guidance_roundtrips_with_identity_revision() {
        let (_root, app) = fixture().await;
        let identity = json!({"_user_id":"admin"});
        let Json(before) = identity_get(
            State(app.clone()),
            Extension(identity.clone()),
            Path("shared_default".into()),
        )
        .await
        .unwrap();
        assert_eq!(before["closing_message"]["notification_guidance"], "");
        let revision = before["revision"].as_i64().unwrap();
        let update = |revision, guidance: &str| IdentityUpdate {
            member_access: None,
            tool_result_deduplication: None,
            shared_chat: None,
            revision,
            name: None,
            description: None,
            execution_modes: None,
            default_execution_mode: None,
            message_visibility: None,
            closing_message: Some(crate::gate_controls::AgentClosing {
                notification_guidance: guidance.into(),
            }),
            manager_incoming_message: None,
            compaction_thresholds: None,
            tool_output_policy: None,
            reset_tool_output_policy: None,
        };
        let Json(saved) = identity_put(
            State(app.clone()),
            Extension(identity.clone()),
            Path("shared_default".into()),
            Json(update(revision, "Name the next step")),
        )
        .await
        .unwrap();
        assert_eq!(
            saved["closing_message"]["notification_guidance"],
            "Name the next step"
        );
        let Json(reloaded) = identity_get(
            State(app.clone()),
            Extension(identity.clone()),
            Path("shared_default".into()),
        )
        .await
        .unwrap();
        assert_eq!(reloaded, saved);
        assert_eq!(
            identity_put(
                State(app.clone()),
                Extension(identity.clone()),
                Path("shared_default".into()),
                Json(update(revision, "stale"))
            )
            .await
            .unwrap_err()
            .0,
            StatusCode::CONFLICT
        );
        assert!(identity_put(
            State(app.clone()),
            Extension(identity),
            Path("shared_default".into()),
            Json(update(
                saved["revision"].as_i64().unwrap(),
                &"x".repeat(4097)
            ))
        )
        .await
        .is_err());
    }
    #[tokio::test]
    async fn identity_modes_persist_default_and_enforce_live_boundary() {
        let (_root, app) = fixture().await;
        let identity = json!({"_user_id":"admin"});
        let Json(before) = identity_get(
            State(app.clone()),
            Extension(identity.clone()),
            Path("shared_default".into()),
        )
        .await
        .unwrap();
        let Json(saved)=identity_put(State(app.clone()),Extension(identity.clone()),Path("shared_default".into()),Json(IdentityUpdate { member_access:None, tool_result_deduplication:None, shared_chat:None,
            revision:before["revision"].as_i64().unwrap(),name:None,description:None,
            execution_modes:Some(json!([{"id":"review","label":"Review","permission_policy":"read_only","prompt":"Inspect only"}])),
            default_execution_mode:Some("review".into()),message_visibility:None,closing_message:None,manager_incoming_message:None,compaction_thresholds:None, tool_output_policy:None,reset_tool_output_policy:None})).await.unwrap();
        let Json(reloaded) = identity_get(
            State(app.clone()),
            Extension(identity.clone()),
            Path("shared_default".into()),
        )
        .await
        .unwrap();
        assert_eq!(saved, reloaded);
        let Json(session) = crate::api::create_session(
            State(app.clone()),
            Json(json!({"client_request_id":crate::uuid()})),
        )
        .await
        .unwrap();
        let sid = session["id"].as_str().unwrap();
        let mode = crate::session_mode(&app, sid).await.unwrap();
        assert_eq!(mode["execution_mode"], "review");
        assert_eq!(mode["mode"], "read_only");
        assert_eq!(mode["definition"]["prompt"], "Inspect only");
        let mut modes = saved["execution_modes"].clone();
        modes.as_array_mut().unwrap().last_mut().unwrap()["permission_policy"] = json!("write");
        identity_put(
            State(app.clone()),
            Extension(identity.clone()),
            Path("shared_default".into()),
            Json(IdentityUpdate {
                member_access: None,
                tool_result_deduplication: None,
                shared_chat: None,
                revision: saved["revision"].as_i64().unwrap(),
                name: None,
                description: None,
                execution_modes: Some(modes),
                default_execution_mode: None,
                message_visibility: None,
                closing_message: None,
                manager_incoming_message: None,
                compaction_thresholds: None,
                tool_output_policy: None,
                reset_tool_output_policy: None,
            }),
        )
        .await
        .unwrap();
        assert_eq!(
            crate::session_mode(&app, sid).await.unwrap()["mode"],
            "read_only"
        );
        let Json(next) = crate::api::change_session_mode(&app, sid, "review", Some(0), None)
            .await
            .unwrap();
        assert_eq!(next["mode"], "write");
        assert!(
            crate::api::change_session_mode(&app, sid, "unknown", None, None)
                .await
                .is_err()
        );
    }
    #[tokio::test]
    async fn identity_is_scoped_validated_and_revision_fenced() {
        let (_root, app) = fixture().await;
        let identity = json!({"_user_id":"admin"});
        let Json(before) = identity_get(
            State(app.clone()),
            Extension(identity.clone()),
            Path("shared_default".into()),
        )
        .await
        .unwrap();
        let revision = before["revision"].as_i64().unwrap();
        let Json(saved) = identity_put(
            State(app.clone()),
            Extension(identity.clone()),
            Path("shared_default".into()),
            Json(IdentityUpdate {
                member_access: None,
                tool_result_deduplication: None,
                shared_chat: None,
                revision,
                name: Some("  Renamed  ".into()),
                description: Some("Description".into()),
                execution_modes: None,
                default_execution_mode: None,
                message_visibility: None,
                closing_message: None,
                manager_incoming_message: None,
                compaction_thresholds: None,
                tool_output_policy: None,
                reset_tool_output_policy: None,
            }),
        )
        .await
        .unwrap();
        assert_eq!(saved["name"], "Renamed");
        assert_eq!(saved["description"], "Description");
        assert!(identity_put(
            State(app.clone()),
            Extension(identity.clone()),
            Path("shared_default".into()),
            Json(IdentityUpdate {
                member_access: None,
                tool_result_deduplication: None,
                shared_chat: None,
                revision,
                name: Some("Stale".into()),
                description: None,
                execution_modes: None,
                default_execution_mode: None,
                message_visibility: None,
                closing_message: None,
                manager_incoming_message: None,
                compaction_thresholds: None,
                tool_output_policy: None,
                reset_tool_output_policy: None
            })
        )
        .await
        .is_err());
        assert!(identity_put(
            State(app.clone()),
            Extension(identity),
            Path("shared_default".into()),
            Json(IdentityUpdate {
                member_access: None,
                tool_result_deduplication: None,
                shared_chat: None,
                revision: revision + 1,
                name: Some(" ".into()),
                description: None,
                execution_modes: None,
                default_execution_mode: None,
                message_visibility: None,
                closing_message: None,
                manager_incoming_message: None,
                compaction_thresholds: None,
                tool_output_policy: None,
                reset_tool_output_policy: None
            })
        )
        .await
        .is_err());
        let outsider = tenant_user(&app, "identity-outsider").await;
        assert!(identity_put(
            State(app.clone()),
            Extension(json!({"_user_id":outsider})),
            Path("shared_default".into()),
            Json(IdentityUpdate {
                member_access: None,
                tool_result_deduplication: None,
                shared_chat: None,
                revision: revision + 1,
                name: Some("Forbidden".into()),
                description: None,
                execution_modes: None,
                default_execution_mode: None,
                message_visibility: None,
                closing_message: None,
                manager_incoming_message: None,
                compaction_thresholds: None,
                tool_output_policy: None,
                reset_tool_output_policy: None
            })
        )
        .await
        .is_err());
    }
}
#[cfg(test)]
mod manager_identity_tests {
    use super::*;
    #[tokio::test]
    async fn manager_identity_is_off_by_default_scoped_and_revision_fenced() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let identity = json!({"_user_id":"admin"});
        let before = identity_get(
            State(app.clone()),
            Extension(identity.clone()),
            Path("shared_default".into()),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(
            before["manager_incoming_message"],
            json!({"enabled":false,"criteria":""})
        );
        let revision = before["revision"].as_i64().unwrap();
        let body = |revision, criteria: &str| {
            serde_json::from_value::<IdentityUpdate>(json!({
            "revision":revision,"manager_incoming_message":{"enabled":true,"criteria":criteria}}))
            .unwrap()
        };
        assert!(identity_put(
            State(app.clone()),
            Extension(identity.clone()),
            Path("shared_default".into()),
            Json(body(revision, " "))
        )
        .await
        .is_err());
        let saved = identity_put(
            State(app.clone()),
            Extension(identity.clone()),
            Path("shared_default".into()),
            Json(body(revision, "Clothing questions only")),
        )
        .await
        .unwrap()
        .0;
        let reloaded = identity_get(
            State(app.clone()),
            Extension(identity.clone()),
            Path("shared_default".into()),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(saved, reloaded);
        // Criteria are stored in the agent's own record, not a shared prompt file.
        assert!(!app.store.root.join("config/prompts.json").exists());
        assert_eq!(reloaded["manager_incoming_message"]["criteria"], "Clothing questions only");
        assert_eq!(
            identity_put(
                State(app.clone()),
                Extension(identity.clone()),
                Path("shared_default".into()),
                Json(body(revision, "Stale"))
            )
            .await
            .unwrap_err()
            .0,
            StatusCode::CONFLICT
        );
        let user = app
            .store
            .create_user_account("manager-setting-outsider", "fixture-only-password")
            .await
            .unwrap();
        assert_eq!(
            identity_put(
                State(app.clone()),
                Extension(json!({"_user_id":user["user_id"]})),
                Path("shared_default".into()),
                Json(body(revision + 1, "Forbidden"))
            )
            .await
            .unwrap_err()
            .0,
            StatusCode::FORBIDDEN
        );
        assert!(identity_put(
            State(app.clone()),
            Extension(identity),
            Path("shared_default".into()),
            Json(body(revision + 1, &"x".repeat(8193)))
        )
        .await
        .is_err());
    }
}

#[cfg(test)]
mod manager_disabled_prompt_tests {
    use super::*;
    #[tokio::test]
    async fn save_criteria_while_off_without_system_model_and_reload_from_the_agent_record() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let identity = json!({"_user_id":"admin"});
        let mut catalog = app.store.model_catalog().await.unwrap();
        catalog
            .agent_models
            .get_mut("shared_default")
            .unwrap()
            .roles
            .remove(&crate::model_selection::ModelRole::System);
        app.store
            .put("model_catalog", &serde_json::to_value(catalog).unwrap())
            .await
            .unwrap();
        let before = identity_get(
            State(app.clone()),
            Extension(identity.clone()),
            Path("shared_default".into()),
        )
        .await
        .unwrap()
        .0;
        let body=serde_json::from_value::<IdentityUpdate>(json!({
            "revision":before["revision"],"manager_incoming_message":{"enabled":false,"criteria":"Clothing store questions"}})).unwrap();
        let saved = identity_put(
            State(app.clone()),
            Extension(identity.clone()),
            Path("shared_default".into()),
            Json(body),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(
            saved["manager_incoming_message"],
            json!({"enabled":false,"criteria":"Clothing store questions"})
        );
        let reload = identity_get(
            State(app.clone()),
            Extension(identity),
            Path("shared_default".into()),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(reload["manager_incoming_message"]["criteria"], "Clothing store questions");
        assert_eq!(reload["manager_incoming_message"]["enabled"], false);
        let store = app.store.for_agent("shared_default").await.unwrap();
        let raw: Vec<u8> =
            sqlx::query_scalar("SELECT metadata FROM agents WHERE id='shared_default'")
                .fetch_one(&store.agent_pool)
                .await
                .unwrap();
        assert_eq!(store.unseal(&raw).unwrap()["manager_incoming_message"]["criteria"],
            "Clothing store questions");
    }
}
