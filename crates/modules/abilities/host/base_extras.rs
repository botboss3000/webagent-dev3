//! Caller-owned identity presets, skill feedback and protected connection setup.
use super::core_agent_management::request;
use crate::*;
use serde::Deserialize;
pub fn owns(name: &str) -> bool {
    matches!(
        name,
        "register_user" | "manage_agent_profiles" | "rate_skill" | "request_agent_login"
    )
}
pub fn schemas() -> Vec<Value> {
    [
 ("register_user","Request a private account-registration form in the active browser. The user enters their own credentials; the existing registration policy and administrator approval still apply. No account is created until the user submits the form.",json!({}),vec![]),
 ("manage_agent_profiles","List/read/save/remove caller-private identity presets, or apply one to an agent the caller manages using its exact identity revision. Presets contain only name and description; they never carry tools, permissions, model assignments or credentials.",json!({"action":{"type":"string","enum":["list","read","save","remove","apply"]},"profile_id":{"type":"string","maxLength":64},"name":{"type":"string","maxLength":200},"description":{"type":"string","maxLength":2000},"expected_revision":{"type":"integer","minimum":0},"agent_id":{"type":"string","maxLength":128},"agent_revision":{"type":"integer","minimum":0}}),vec!["action"]),
 ("rate_skill","Record private feedback for one currently accessible bundled or own skill. Score is 1..5; optional comment is at most 1000 bytes. A rating does not edit guidance, change permissions or become a human instruction. Omit score to read the current rating and revision; when writing use that revision, initially zero.",json!({"ability_id":{"type":"string","maxLength":64},"kind":{"type":"string","enum":["ability","tool","selectable","own"]},"skill_id":{"type":"string","maxLength":64},"score":{"type":"integer","minimum":1,"maximum":5},"comment":{"type":"string","maxLength":1000},"expected_revision":{"type":"integer","minimum":0}}),vec!["ability_id","kind","skill_id"]),
 ("request_agent_login","Begin device authorization for a caller-owned subscription connection through Models. The user completes it on the protected model page; passwords, device codes and tokens never enter this result. API-key connections must be set up manually on that page.",json!({"connection_id":{"type":"string","maxLength":128}}),vec!["connection_id"])
 ].into_iter().map(|(name,description,properties,required)|json!({"type":"function","name":name,"description":description,"parameters":{"type":"object","properties":properties,"required":required,"additionalProperties":false}})).collect()
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
    let run = super::core_authority::check(app, sid, rid, generation, pending, name, args, &cancel)
        .await?;
    if shared_chat::actor(app) != app.store.current_user_id() {
        return Err(anyhow!(
            "Use an owner-private conversation for private account records"
        ));
    }
    let q: Args = serde_json::from_value(args.clone())?;
    let generation = run["generation"]
        .as_str()
        .ok_or_else(|| anyhow!("Run generation missing"))?;
    let output = match name {
        "register_user" => {
            return super::core_app_control::enqueue(
                app,
                sid,
                rid,
                generation,
                pending,
                name,
                args,
                "registration",
                cancel,
            )
            .await
        }
        "request_agent_login" => {
            let id = identifier(q.connection_id.as_deref(), 128)?;
            let started = request(
                app,
                "POST",
                format!("/models/connections/{id}/authorization"),
                json!({}),
            )
            .await?;
            if !started.is_object() {
                return Err(anyhow!("Authorization setup unavailable"));
            }
            json!({"authorization_started":true,"user_action_required":true,"setup_url":format!("/app?tab=agents&agent={}&agentView=model",session_agent_id(app,sid).await?)})
        }
        "rate_skill" => {
            let ability = identifier(q.ability_id.as_deref(), 64)?;
            let skill = identifier(q.skill_id.as_deref(), 64)?;
            let kind = q
                .kind
                .as_deref()
                .ok_or_else(|| anyhow!("Skill kind required"))?;
            let asset = tools::core_skills::rating_asset(
                app,
                sid,
                rid,
                &json!({"ability_id":ability,"kind":kind,"skill_id":skill}),
            )
            .await?;
            let id = digest(&json!([run["agent_id"], ability, kind, skill]));
            if q.score.is_none() {
                let rating = app
                    .store
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
            let result=app.store.base_write_artifact(sid,rid,pending,generation,"core_rating",&id,q.expected_revision.ok_or_else(||anyhow!("Rating revision required"))?,&json!({"ability_id":ability,"kind":kind,"skill_id":skill,"asset_sha256":asset["sha256"],"score":score,"comment":comment,"authority":"private_feedback"})).await?;
            json!({"saved":true,"rating_id":id,"revision":result["revision"]})
        }
        "manage_agent_profiles" => {
            if q.action.as_deref() == Some("list") {
                let items=app.store.list("core_profile:").await?.into_iter().filter(|v|v["artifact"]["deleted"]!=true).map(|v|json!({"profile_id":v["id"],"revision":v["revision"],"name":v["artifact"]["name"],"description":v["artifact"]["description"]})).take(100).collect::<Vec<_>>();
                return Ok(json!({"status":"success","output":{"profiles":items}}));
            }
            let id = identifier(q.profile_id.as_deref(), 64)?;
            let prior = app.store.get(&format!("core_profile:{id}")).await?;
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
                        if prior.is_none() && app.store.list("core_profile:").await?.len() >= 100 {
                            return Err(anyhow!("Profile quota reached"));
                        }
                        json!({"name":name,"description":description,"deleted":false})
                    };
                    app.store
                        .base_write_artifact(
                            sid,
                            rid,
                            pending,
                            generation,
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
                    request(app,"PUT",format!("/models/agents/{agent}/identity"),json!({"revision":q.agent_revision.ok_or_else(||anyhow!("Agent identity revision required"))?,"name":preset["artifact"]["name"],"description":preset["artifact"]["description"]})).await?
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
