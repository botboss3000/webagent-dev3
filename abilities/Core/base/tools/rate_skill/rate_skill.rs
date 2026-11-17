//! rate_skill: reads or saves the caller's private skill rating.
use super::{identifier, Args, Call, NativeHost};
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use webagent_host_core::digest;

pub(super) async fn run(
    host: &dyn NativeHost,
    call: Call<'_>,
    sid: &str,
    rid: &str,
    run: &Value,
    q: Args,
) -> Result<Value> {
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
    let output = json!({"saved":true,"rating_id":id,"revision":result["revision"]});
    Ok(json!({"status":"success","output":output}))
}
