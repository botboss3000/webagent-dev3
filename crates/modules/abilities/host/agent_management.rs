//! Agent/session management reuses the owning authenticated API handlers through a fixed tool-route map.
use crate::*;
use axum::{body::Body, Extension};
use serde::Deserialize;
use tower::ServiceExt;
pub fn owns(name: &str) -> bool {
    matches!(
        name,
        "list_agent_templates"
            | "list_my_agents"
            | "get_agent"
            | "list_agent_tools"
            | "create_agent"
            | "update_agent"
            | "set_agent_tool"
            | "edit_agent_prompt"
            | "set_agent_ability"
            | "manage_agent_skills"
            | "manage_agent_chat_config"
            | "list_user_sessions"
            | "manage_user_session"
            | "create_user_session"
            | "watch_sessions"
    )
}
/// Tools this adapter executes; their schemas live in the ability's `tools/<tool>/tool.json`.
pub const TOOLS: &[&str] = &["list_agent_templates", "list_my_agents", "get_agent", "list_agent_tools", "create_agent", "update_agent", "set_agent_tool", "edit_agent_prompt", "set_agent_ability", "manage_agent_skills", "manage_agent_chat_config", "list_user_sessions", "manage_user_session", "kick_user_session", "watch_sessions", "create_user_session"];
pub fn schemas() -> Vec<Value> {
    crate::tools::folder_schemas(TOOLS)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    agent_id: Option<String>,
    ability_id: Option<String>,
    tool: Option<String>,
    slot: Option<String>,
    session_id: Option<String>,
    action: Option<String>,
    request: Option<Value>,
    name: Option<String>,
    description: Option<String>,
    instructions: Option<String>,
    expected_revision: Option<i64>,
    document: Option<String>,
    patch: Option<Value>,
    prompt: Option<String>,
    expires_ms: Option<u64>,
}
fn part(value: Option<&str>) -> Result<&str> {
    let value = value.ok_or_else(|| anyhow!("Required identifier missing"))?;
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
    {
        return Err(anyhow!("Invalid identifier"));
    }
    Ok(value)
}
/// Only fixed route callers use this bridge; no URL, method, identity or credential comes from model arguments.
pub async fn request(app: &App, method: &str, path: String, body: Value) -> Result<Value> {
    let actor = shared_chat::actor(app).to_owned();
    let mut caller = app.clone();
    caller.store = app.store.for_user(&actor).await?;
    caller.chat_access = None;
    caller.chat_actor = Some(actor.clone());
    let router = api::core_tools_router()
        .layer(Extension(json!({"_user_id":actor})))
        .layer(Extension(caller.clone()))
        .with_state(caller);
    let request = axum::http::Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body)?))?;
    let response = router.oneshot(request).await?;
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 2 * 1024 * 1024).await?;
    let result: Value = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow!("Management API returned an invalid response"))?;
    if !status.is_success() {
        return Err(anyhow!(
            "{}",
            result["error"]
                .as_str()
                .unwrap_or("Management request rejected")
        ));
    }
    Ok(result)
}
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
    let q: Args = serde_json::from_value(args.clone())?;
    let actor = shared_chat::actor(app);
    let mut body = q.request.clone().unwrap_or(json!({}));
    if !body.is_object() {
        return Err(anyhow!("Request must be an object"));
    }
    let request_id = digest(&json!([actor, sid, rid, pending]));
    let output = match name {
        "list_agent_templates" => {
            json!({"templates":[{"id":"blank","name":"Blank agent","description":"Caller-owned agent with platform defaults; no new access grants."}]})
        }
        "list_my_agents" => request(app, "GET", "/models/agents".into(), json!({})).await?,
        "get_agent" | "list_agent_tools" => {
            let agent = part(q.agent_id.as_deref())?;
            request(
                app,
                "GET",
                if name == "get_agent" {
                    format!("/models/agents/{agent}/identity")
                } else {
                    format!("/agents/{agent}/abilities")
                },
                json!({}),
            )
            .await?
        }
        "create_agent" => {
            body["client_request_id"] = json!(request_id);
            request(app, "POST", "/models/agents".into(), body).await?
        }
        "update_agent" => {
            let id = part(q.agent_id.as_deref())?;
            request(app, "PUT", format!("/models/agents/{id}/identity"), body).await?
        }
        "set_agent_tool" => {
            let id = part(q.agent_id.as_deref())?;
            let ability = part(q.ability_id.as_deref())?;
            let tool = part(q.tool.as_deref())?;
            request(
                app,
                "PUT",
                format!("/agents/{id}/abilities/{ability}/tools/{tool}/mode"),
                body,
            )
            .await?
        }
        "set_agent_ability" => {
            let id = part(q.agent_id.as_deref())?;
            let ability = part(q.ability_id.as_deref())?;
            request(
                app,
                "PUT",
                format!("/agents/{id}/abilities/{ability}/visibility"),
                body,
            )
            .await?
        }
        "edit_agent_prompt" => {
            let id = part(q.agent_id.as_deref())?;
            let slot = part(q.slot.as_deref())?;
            request(app, "PUT", format!("/agents/{id}/slots/{slot}"), body).await?
        }
        "manage_agent_skills" => {
            let agent = part(q.agent_id.as_deref())?;
            if app.store.agent_authority(actor, agent).await?["can_manage"] != true {
                return Err(anyhow!("Agent administration required"));
            }
            let store = app.store.for_user(actor).await?.for_agent(agent).await?;
            match q.action.as_deref() {
                Some("list") => store.list_own_skills().await?,
                Some("read") => store
                    .load_own_skill(part(q.name.as_deref())?)
                    .await?
                    .ok_or_else(|| anyhow!("Skill not found"))?,
                Some(action @ ("save" | "remove")) => {
                    store
                        .mutate_own_skill(
                            part(q.name.as_deref())?,
                            q.description.as_deref(),
                            q.instructions.as_deref(),
                            Some("selectable"),
                            q.expected_revision
                                .ok_or_else(|| anyhow!("Skill revision required"))?,
                            action == "remove",
                            &request_id,
                            &digest(&json!([actor, agent, sid, rid])),
                        )
                        .await?
                }
                _ => return Err(anyhow!("Unknown skill action")),
            }
        }
        "manage_agent_chat_config" => {
            let agent = part(q.agent_id.as_deref())?;
            let document = match q.document.as_deref() {
                Some(document @ ("chat" | "session_dropdown")) => document,
                _ => return Err(anyhow!("Unknown chat configuration document")),
            };
            let path = format!("/agents/{agent}/chat-config/{document}");
            let revision = || q.expected_revision.ok_or_else(|| anyhow!("Chat configuration revision required"));
            match q.action.as_deref() {
                Some("read") => request(app, "GET", path, json!({})).await?,
                Some("patch") => {
                    let patch = q.patch.clone().ok_or_else(|| anyhow!("Chat configuration patch required"))?;
                    request(app, "PATCH", path, json!({"patch":patch,"expected_revision":revision()?})).await?
                }
                Some("reset") => request(app, "DELETE", path, json!({"expected_revision":revision()?})).await?,
                _ => return Err(anyhow!("Unknown chat configuration action")),
            }
        }
        "watch_sessions" => {
            let (_, _, visibility, _) = ability_controls(app, sid).await?;
            if visibility["settings"]["agent_management"]["allow_session_watch"] == false {
                return Err(anyhow!("Session watching is disabled"));
            }
            let target = part(q.session_id.as_deref())?;
            Uuid::parse_str(target)?;
            request(app, "GET", format!("/sessions/{target}"), json!({})).await?;
            let prompt = q
                .prompt
                .as_deref()
                .filter(|v| !v.trim().is_empty() && v.len() <= 16000)
                .ok_or_else(|| anyhow!("Bounded watch prompt required"))?;
            let expires = q
                .expires_ms
                .filter(|v| *v > now() * 1000 && *v <= (now() + 30 * 86400) * 1000)
                .ok_or_else(|| anyhow!("Watch expiry must be within 30 days"))?;
            let run = super::core_authority::check(
                app, sid, rid, generation, pending, name, args, &cancel,
            )
            .await?;
            super::core_authority::dependency(app, sid, &run, "event_subscribe").await?;
            crate::modules::tool_call(app,sid,rid,"event_subscribe",&json!({"request_id":request_id,"source":"session_settled","filter":{"session_id":target},"prompt":prompt,"destination":{"kind":"existing_chat","session_id":sid},"expires_ms":expires,"daily_cap":100}),pending,cancel).await?
        }
        "list_user_sessions" => request(app, "GET", "/sessions".into(), json!({})).await?,
        "create_user_session" => {
            body["client_request_id"] = json!(request_id);
            request(app, "POST", "/sessions".into(), body).await?
        }
        "manage_user_session" => {
            let target = part(q.session_id.as_deref())?;
            Uuid::parse_str(target)?;
            body["client_request_id"] = json!(request_id);
            match q.action.as_deref() {
                Some("update") => {
                    request(app, "PATCH", format!("/sessions/{target}"), body).await?
                }
                Some("recycle") => {
                    request(app, "DELETE", format!("/sessions/{target}"), body).await?
                }
                Some("restore") => {
                    request(app, "POST", format!("/sessions/{target}/restore"), body).await?
                }
                _ => return Err(anyhow!("Unknown session action")),
            }
        }
        _ => return Err(anyhow!("Unknown Agent Management tool")),
    };
    Ok(json!({"status":"success","output":output}))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identifiers_cannot_escape_a_fixed_management_route() {
        for value in ["../accounts", "x?admin=1", "x/y", "x%2fy", ""] {
            assert!(part(Some(value)).is_err());
        }
        assert!(part(Some("agent-a_1")).is_ok());
    }
    #[tokio::test]
    async fn owning_api_bridge_preserves_session_lifecycle_and_tenant_checks() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let sid = crate::runtime_tests::conversation(&app).await;
        let snapshot = request(&app, "GET", format!("/sessions/{sid}"), json!({}))
            .await
            .unwrap();
        assert_eq!(snapshot["session"]["id"], sid);
        let mut foreign = app.clone();
        foreign.chat_actor = Some("absent-user".into());
        assert!(
            request(&foreign, "GET", format!("/sessions/{sid}"), json!({}))
                .await
                .is_err()
        );
        let body = json!({"client_request_id":"core-management-recycle","expected_revision":snapshot["session"]["metadata"]["revision"].as_u64().or_else(||snapshot["session"]["metadata_revision"].as_u64()).unwrap_or(0)});
        let recycled = request(&app, "DELETE", format!("/sessions/{sid}"), body)
            .await
            .unwrap();
        assert!(recycled.is_object());
        assert_eq!(
            app.store.get(&key("session", &sid)).await.unwrap().unwrap()["status"],
            "recycled"
        );
    }
}
