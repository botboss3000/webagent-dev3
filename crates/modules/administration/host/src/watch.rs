//! Background passes for one caller: renew or revoke admitted broker jobs, and record
//! content-free repository-change notices. Neither executes model instructions or
//! creates assistant replies. The root runs them for every user on a timer.
use crate::{trusted, AdministrationHost};
use anyhow::Result;
use serde_json::json;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use webagent_access::Permission;
use webagent_host_core::now;

/// Renew each admitted job whose authority fingerprint is unchanged; revoke the rest and
/// drop their renewal records.
pub async fn renew_jobs(host: &dyn AdministrationHost) -> Result<()> {
    let store = host.store();
    let empty = json!({});
    for record in store
        .list("administrator_host:")
        .await?
        .into_iter()
        .take(32)
    {
        let (Some(sid), Some(run_id), Some(name)) = (
            record["session_id"].as_str(),
            record["run_id"].as_str(),
            record["tool_name"].as_str(),
        ) else {
            continue;
        };
        let fresh = host.fingerprint(sid, run_id, name).await;
        let valid = fresh.is_ok_and(|value| record["access_fingerprint"] == value.as_str());
        let identity = trusted(store.current_user_id(), sid, &record["access_fingerprint"]);
        let command = if valid {
            "administrator_host_lease"
        } else {
            "administrator_host_revoke"
        };
        let request = host.broker(
            command,
            &empty,
            sid,
            Some(identity),
            CancellationToken::new(),
        );
        let _ = tokio::time::timeout(Duration::from_secs(2), request).await;
        if !valid {
            store.delete(&format!("administrator_host:{sid}")).await?;
        }
    }
    Ok(())
}

/// Count broker jobs still leased to `sid`, renewing them as `renew_jobs` does. Changed
/// authority counts none; the next renewal pass revokes those jobs.
pub async fn session_jobs(host: &dyn AdministrationHost, sid: &str) -> Result<u64> {
    let store = host.store();
    let Some(record) = store.get(&format!("administrator_host:{sid}")).await? else {
        return Ok(0);
    };
    let (Some(run_id), Some(name)) = (record["run_id"].as_str(), record["tool_name"].as_str())
    else {
        return Ok(0);
    };
    let fresh = host.fingerprint(sid, run_id, name).await;
    if !fresh.is_ok_and(|value| record["access_fingerprint"] == value.as_str()) {
        return Ok(0);
    }
    let identity = trusted(store.current_user_id(), sid, &record["access_fingerprint"]);
    let empty = json!({});
    let request = host.broker(
        "administrator_host_lease",
        &empty,
        sid,
        Some(identity),
        CancellationToken::new(),
    );
    let reply = tokio::time::timeout(Duration::from_secs(2), request).await??;
    Ok(reply["output"]["updated"]
        .as_u64()
        .or_else(|| reply["updated"].as_u64())
        .unwrap_or(0))
}

/// Whether the caller opted into repository notices for this session and may run Git
/// without approval.
async fn watching(host: &dyn AdministrationHost, sid: &str) -> Result<Option<String>> {
    let Some(access) = host.access(sid, &[("git_tool", "git_control")]).await? else {
        return Ok(None);
    };
    // Notices are an opt-in observation, never an approval bypass for a caller whose
    // Git tool remains gated.
    let watching = access.admitted
        && access.settings["git_control"]["notify_repo_changes"] == true
        && access.permissions.first() == Some(&Permission::Auto);
    Ok(watching.then_some(access.agent))
}

/// Observe the 32 newest active sessions that opted in, and record a notice when the
/// repository snapshot changed.
pub async fn repository_notices(host: &dyn AdministrationHost) -> Result<()> {
    let store = host.store();
    let mut sessions = store.list("session:").await?;
    sessions.sort_by_key(|session| std::cmp::Reverse(session["created_at"].as_u64().unwrap_or(0)));
    let empty = json!({});
    for session in sessions
        .into_iter()
        .filter(|s| s["status"] == "active")
        .take(32)
    {
        let Some(sid) = session["id"]
            .as_str()
            .filter(|sid| uuid::Uuid::parse_str(sid).is_ok())
        else {
            continue;
        };
        if watching(host, sid).await?.is_none() {
            continue;
        }
        let watch = host.worker(
            "administrator_git_watch",
            &empty,
            sid,
            None,
            CancellationToken::new(),
        );
        let Ok(Ok(value)) = tokio::time::timeout(Duration::from_secs(5), watch).await else {
            continue;
        };
        if value["status"] != "success" {
            continue;
        }
        let snapshot = &value["output"];
        let key = format!("administrator_notice:{sid}");
        let old = store.get(&key).await?.unwrap_or_default();
        if old["snapshot_sha256"] == snapshot["snapshot_sha256"] {
            continue;
        }
        // Recheck after I/O: a revocation withdraws the observation.
        let Some(agent) = watching(host, sid).await? else {
            continue;
        };
        let notification = if snapshot["notify"] == true {
            "Repository has new uncommitted changes or unpublished commits. No action was taken."
        } else {
            ""
        };
        let notice = json!({"session_id":sid,"agent_id":agent,
            "snapshot_sha256":snapshot["snapshot_sha256"],"dirty":snapshot["dirty"],
            "ahead":snapshot["ahead"],"observed_at":now(),"notification":notification});
        host.record(
            Some(&agent),
            vec![(key, notice.clone())],
            sid,
            "repository_notice",
            &notice,
        )
        .await?;
    }
    Ok(())
}
