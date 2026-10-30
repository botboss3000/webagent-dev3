//! Credential-owner authority binds trusted requester identity and exact pending revisions.
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
pub fn reference(run: &Value, current_user: &str, requirement: &Value) -> Result<String> {
    if requirement["kind"] != "credential_owner" {
        anyhow::bail!("Unsupported tool authorization declaration");
    }
    let kind = requirement["credential_kind"]
        .as_str()
        .ok_or_else(|| anyhow!("Credential kind missing"))?;
    if kind.is_empty() || !kind.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        anyhow::bail!("Invalid credential kind");
    }
    let owner = run["requester_user_id"].as_str().unwrap_or(current_user);
    // The authenticated run requester selects the credential namespace. Model arguments cannot select it.
    let reference = format!("tool_credential:{owner}:{kind}");
    Ok(reference)
}
pub fn binding(
    run: &Value,
    current_user: &str,
    name: &str,
    args: &Value,
    requirement: &Value,
    credential: &Value,
) -> Result<Value> {
    if requirement["kind"] != "credential_owner" {
        anyhow::bail!("Unsupported tool authorization declaration");
    }
    let kind = requirement["credential_kind"]
        .as_str()
        .ok_or_else(|| anyhow!("Credential kind missing"))?;
    if kind.is_empty() || !kind.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        anyhow::bail!("Invalid credential kind");
    }
    let owner = run["requester_user_id"].as_str().unwrap_or(current_user);
    // The authenticated run requester selects the credential namespace. Model arguments cannot select it.
    let reference = format!("tool_credential:{owner}:{kind}");
    if credential["owner_user_id"] != owner || credential["revoked"] == true {
        anyhow::bail!("Credential owner or status invalid");
    }
    let version = credential["version"]
        .as_u64()
        .ok_or_else(|| anyhow!("Credential version missing"))?;
    Ok(
        json!({"kind":"credential_owner","owner_user_id":owner,"credential_ref":reference,"credential_version":version,
        "session_id":run["session_id"],"run_id":run["id"],"tool":name,"arguments_digest":webagent_configuration::digest(args),
        "participation_generation":run["participation_generation"].as_u64().unwrap_or(0),
        "policy_version":run["pending"]["policy_version"],"mode_revision":run["pending"]["mode_revision"],
        "visibility_revision":run["pending"]["visibility_revision"],"ability_state_revision":run["pending"]["ability_state_revision"],
        "access_revision":run["pending"]["access_revision"]}),
    )
}
