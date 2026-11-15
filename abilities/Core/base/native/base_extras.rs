//! Caller-owned identity presets, skill feedback and protected connection setup.
use crate::{app_control, Call, NativeHost};
use anyhow::{anyhow, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
use webagent_host_core::digest;
/// Whether this family executes tool `name`.
#[must_use]
pub fn owns(name: &str) -> bool {
    matches!(
        name,
        "register_user" | "manage_agent_profiles" | "rate_skill" | "request_agent_login"
    )
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    action: Option<String>,
    profile_id: Option<String>,
    name: Option<String>,
    description: Option<String>,
    expected_revision: Option<u64>,
    agent_id: Option<String>,
    agent_revision: Option<u64>,
    ability_id: Option<String>,
    kind: Option<String>,
    skill_id: Option<String>,
    score: Option<u8>,
    comment: Option<String>,
    connection_id: Option<String>,
}
fn identifier(raw: Option<&str>, max: usize) -> Result<&str> {
    let raw = raw.ok_or_else(|| anyhow!("Identifier required"))?;
    if raw.is_empty()
        || raw.len() > max
        || !raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
    {
        return Err(anyhow!("Invalid identifier"));
    }
    Ok(raw)
}
/// Run one owner-private account tool at `call`'s position under fresh run authority.
///
/// # Errors
/// When authority, the caller, the arguments or the private record checks fail.
pub async fn execute(
    host: &dyn NativeHost,
    call: Call<'_>,
    name: &str,
    args: &Value,
    cancel: CancellationToken,
) -> Result<Value> {
    let (sid, rid) = (call.sid, call.run_id);
    let run = host.authorize(call, name, args, &cancel).await?;
    if host.actor() != host.store().current_user_id() {
        return Err(anyhow!(
            "Use an owner-private conversation for private account records"
        ));
    }
    let q: Args = serde_json::from_value(args.clone())?;
    let generation = run["generation"]
        .as_str()
        .ok_or_else(|| anyhow!("Run generation missing"))?;
    let call = Call { generation, ..call };
    let output = match name {
        "register_user" => {
            return app_control::enqueue(host, call, name, args, "registration", cancel).await
        }
        "request_agent_login" => {
            let id = identifier(q.connection_id.as_deref(), 128)?;
            let started = host
                .agent_request(
                    "POST",
                    format!("/models/connections/{id}/authorization"),
                    json!({}),
                )
                .await?;
            if !started.is_object() {
                return Err(anyhow!("Authorization setup unavailable"));
            }
            json!({"authorization_started":true,"user_action_required":true,"setup_url":format!("/app?tab=agents&agent={}&agentView=model",host.session_agent_id(sid).await?)})
        }
        "rate_skill" => {
            let ability = identifier(q.ability_id.as_deref(), 64)?;
            let skill = identifier(q.skill_id.as_deref(), 64)?;
            let kind = q
                .kind
                .as_deref()
                .ok_or_else(|| anyhow!("Skill kind required"))?;
            let asset = host
                .rating_asset(
                    sid,
                    rid,
                    &json!({"ability_id":ability,"kind":kind,"skill_id":skill}),
                )
                .await?;
            let id = digest(&json!([run["agent_id"], ability, kind, skill]));
            if q.score.is_none() {
                let rating = host
                    .store()
                    .get(&format!("core_rating:{id}"))
                    .await?
                    .unwrap_or(json!({"revision":0,"artifact":null}));
                return Ok(
                    json!({"status":"success","output":{"rating_id":id,"revision":rating["revision"],"rating":rating["artifact"]}}),
                );
            }
            let score = q
                .score
                .filter(|s| (1..=5).contains(s))
                .ok_or_else(|| anyhow!("Score must be 1..5"))?;
            let comment = q.comment.unwrap_or_default();
            if comment.len() > 1000 {
                return Err(anyhow!("Rating comment exceeds bounds"));
            }
            let result=host.write_artifact(call,"core_rating",&id,q.expected_revision.ok_or_else(||anyhow!("Rating revision required"))?,&json!({"ability_id":ability,"kind":kind,"skill_id":skill,"asset_sha256":asset["sha256"],"score":score,"comment":comment,"authority":"private_feedback"})).await?;
            json!({"saved":true,"rating_id":id,"revision":result["revision"]})
        }
        "manage_agent_profiles" => {
            if q.action.as_deref() == Some("list") {
                let items=host.store().list("core_profile:").await?.into_iter().filter(|v|v["artifact"]["deleted"]!=true).map(|v|json!({"profile_id":v["id"],"revision":v["revision"],"name":v["artifact"]["name"],"description":v["artifact"]["description"]})).take(100).collect::<Vec<_>>();
                return Ok(json!({"status":"success","output":{"profiles":items}}));
            }
            let id = identifier(q.profile_id.as_deref(), 64)?;
            let prior = host.store().get(&format!("core_profile:{id}")).await?;
            match q.action.as_deref() {
                Some("read") => prior
                    .filter(|v| v["artifact"]["deleted"] != true)
                    .ok_or_else(|| anyhow!("Profile not found"))?,
                Some("save" | "remove") => {
                    let remove = q.action.as_deref() == Some("remove");
                    let artifact = if remove {
                        if prior
                            .as_ref()
                            .is_none_or(|v| v["artifact"]["deleted"] == true)
                        {
                            return Err(anyhow!("Profile not found"));
                        }
                        json!({"deleted":true})
                    } else {
                        let name = q
                            .name
                            .filter(|v| {
                                !v.trim().is_empty()
                                    && v.len() <= 200
                                    && !v.chars().any(char::is_control)
                            })
                            .ok_or_else(|| anyhow!("Bounded profile name required"))?;
                        let description = q.description.unwrap_or_default();
                        if description.len() > 2000 {
                            return Err(anyhow!("Profile description exceeds bounds"));
                        }
                        if prior.is_none() && host.store().list("core_profile:").await?.len() >= 100
                        {
                            return Err(anyhow!("Profile quota reached"));
                        }
                        json!({"name":name,"description":description,"deleted":false})
                    };
                    host.write_artifact(
                        call,
                        "core_profile",
                        id,
                        q.expected_revision
                            .ok_or_else(|| anyhow!("Profile revision required"))?,
                        &artifact,
                    )
                    .await?
                }
                Some("apply") => {
                    let preset = prior
                        .filter(|v| v["artifact"]["deleted"] != true)
                        .ok_or_else(|| anyhow!("Profile not found"))?;
                    if preset["revision"]
                        != q.expected_revision
                            .ok_or_else(|| anyhow!("Profile revision required"))?
                    {
                        return Err(anyhow!("Profile revision changed"));
                    }
                    let agent = identifier(q.agent_id.as_deref(), 128)?;
                    host.agent_request("PUT",format!("/models/agents/{agent}/identity"),json!({"revision":q.agent_revision.ok_or_else(||anyhow!("Agent identity revision required"))?,"name":preset["artifact"]["name"],"description":preset["artifact"]["description"]})).await?
                }
                _ => return Err(anyhow!("Unknown profile action")),
            }
        }
        _ => return Err(anyhow!("Unknown Base account tool")),
    };
    Ok(json!({"status":"success","output":output}))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn private_record_identifiers_cannot_be_paths_or_query_strings() {
        for value in ["../x", "x/y", "x?user=other", ""] {
            assert!(identifier(Some(value), 64).is_err());
        }
        assert!(identifier(Some("profile-1"), 64).is_ok());
    }
}
