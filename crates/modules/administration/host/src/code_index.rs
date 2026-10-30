//! Code Index adapter calls routed through Administrator run authority: shared index
//! maintenance under broker authority (never caller-selected paths), and fresh bounded
//! configuration excerpts attached to a feature-bound source read.
use crate::{trusted, AdministrationHost};
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use webagent_access::Principal;
use webagent_accounts_host::AccountsStore;
use webagent_administration::code_index::{
    bounded, maintenance_arguments, pointers, read_json, runtime_state, snippet_bytes,
};

/// Run `code_index_maintenance` through native authority, renewing the broker lease
/// during long scans. Snapshot reads go to the tool worker; live work to the broker.
pub async fn maintenance(
    host: &dyn AdministrationHost,
    sid: &str,
    run_id: &str,
    args: &Value,
    cancel: CancellationToken,
) -> Result<Value> {
    let args = maintenance_arguments(args)?;
    let name = "code_index_maintenance";
    let fingerprint = host.fingerprint(sid, run_id, name).await?;
    if args["root"] != "live" {
        let allowed = [
            "action", "root", "scope", "status", "feature", "offset", "limit", "entry_id",
        ];
        let worker_args = Value::Object(
            args.as_object()
                .into_iter()
                .flatten()
                .filter(|(k, _)| allowed.contains(&k.as_str()))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        );
        let result = host
            .worker(name, &worker_args, sid, None, cancel.clone())
            .await?;
        if cancel.is_cancelled() || host.fingerprint(sid, run_id, name).await? != fingerprint {
            bail!("Maintenance authority changed during read");
        }
        return Ok(result);
    }
    let fingerprint_value = json!(fingerprint);
    let trusted = || trusted(host.actor(), sid, &fingerprint_value);
    let future = host.broker(name, &args, sid, Some(trusted()), cancel.clone());
    tokio::pin!(future);
    loop {
        tokio::select! {
            result=&mut future=>{
                let result=result?;
                if cancel.is_cancelled()||host.fingerprint(sid,run_id,name).await?!=fingerprint{bail!("Maintenance authority changed; inspect the existing request");}
                return Ok(result)
            },
            _=cancel.cancelled()=>{
                let _=host.broker("administrator_host_revoke",&json!({}),sid,Some(trusted()),CancellationToken::new()).await;
                bail!("Maintenance interrupted; inspect the original request before retrying")
            },
            _=tokio::time::sleep(Duration::from_secs(5))=>{
                let current=host.fingerprint(sid,run_id,name).await;
                if current.as_ref().ok()!=Some(&fingerprint){
                    let _=host.broker("administrator_host_revoke",&json!({}),sid,Some(trusted()),CancellationToken::new()).await;
                    bail!("Maintenance authority revoked")
                }
                host.broker("administrator_host_lease",&json!({}),sid,Some(trusted()),cancel.clone()).await?;
            }
        }
    }
}

/// The caller's fresh principal, if their account exists.
async fn principal(host: &dyn AdministrationHost) -> Result<Option<Principal>> {
    let store = host.store();
    let account = store.account_by_id(store.current_user_id()).await?;
    account.as_ref().map(Principal::from_account).transpose()
}

/// Attach configuration excerpts to a successful `code_index_lookup` snippet. Called
/// only inside an already-authorized Code Index read; fresh account status and
/// administrator scope additionally constrain configuration access.
pub async fn attach_configuration(
    host: &dyn AdministrationHost,
    args: &Value,
    result: &mut Value,
    cancel: &CancellationToken,
) -> Result<()> {
    if args["action"] != "snippet" || result["status"] != "success" {
        return Ok(());
    }
    if cancel.is_cancelled() {
        bail!("Configuration read cancelled");
    }
    let Some(source) = result["results"]
        .as_array_mut()
        .and_then(|rows| rows.first_mut())
    else {
        return Ok(());
    };
    if source["stale"] == true {
        return Ok(());
    }
    let Some(refs) = source["configuration"]["references"].as_array().cloned() else {
        return Ok(());
    };
    if refs.is_empty() {
        return Ok(());
    }
    let principal_now = principal(host).await?;
    if !principal_now.as_ref().is_some_and(Principal::active) {
        bail!("Configuration caller unavailable");
    }
    let administrator = principal_now.as_ref().is_some_and(Principal::administrator);
    let code_bytes = source["text"].as_str().map(str::len).unwrap_or(0);
    let budget = args["max_chars"].as_u64().unwrap_or(12000).min(12000) as usize;
    let mut remaining = budget.saturating_sub(code_bytes + snippet_bytes(source));
    let published = args["root"] == "published";
    let mut snippets = source["configuration_snippets"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    for reference in refs.iter().take(4) {
        let Some(path) = reference["path"].as_str() else {
            continue;
        };
        if !matches!(
            path,
            "ui/chat/chat.json" | "crates/modules/context/context-compaction.json"
        ) {
            // The worker resolves other indexed source JSON; these two live files are read
            // by native readers that apply the caller's administrator scope.
            if !snippets.iter().any(|entry| entry["path"] == path) {
                snippets.push(json!({"path":path,"status":"unavailable","reason":"not_indexed_or_supported_runtime_alias"}));
            }
            continue;
        }
        let state = runtime_state(path, published, administrator);
        if state != "read" {
            snippets.push(json!({"path":path,"status":state}));
            continue;
        }
        let actual = if path.ends_with("context-compaction.json") {
            host.compaction_config_path()
        } else {
            host.chat_ui_config_path()
        };
        let load = async { read_json(actual) };
        let loaded = tokio::select! {
            _=cancel.cancelled()=>bail!("Configuration read cancelled"),
            loaded=tokio::time::timeout(std::time::Duration::from_secs(2),load)=>loaded,
        };
        let Ok(Ok((value, sha256))) = loaded else {
            snippets
                .push(json!({"path":path,"status":"unavailable","reason":"bounded_read_failed"}));
            continue;
        };
        let requested = pointers(reference);
        let count = requested.len().max(1);
        let per_pointer = remaining / count;
        for pointer in requested {
            let mut item = json!({"path":path,"runtime_path":actual.to_string_lossy(),"pointer":pointer,"sha256":sha256,"read_at":webagent_host_core::now(),"source_kind":"untrusted_configuration"});
            if let Some(selected) = value.pointer(&pointer) {
                let rendered = serde_json::to_string_pretty(selected)?;
                let (text, truncated) = bounded(&rendered, per_pointer.min(remaining));
                item["status"] = json!(if text.is_empty() {
                    "budget_exhausted"
                } else {
                    "set"
                });
                item["text"] = json!(text);
                item["truncated"] = json!(truncated);
                remaining = remaining.saturating_sub(text.len());
            } else {
                item["status"] = json!("unset");
            }
            snippets.push(item);
        }
    }
    // Re-read principal before exposing data, including administrator downgrade.
    let fresh = principal(host).await?;
    if cancel.is_cancelled()
        || !fresh.as_ref().is_some_and(Principal::active)
        || administrator != fresh.as_ref().is_some_and(Principal::administrator)
    {
        bail!("Configuration caller changed during read");
    }
    source["configuration_snippets"] = json!(snippets);
    Ok(())
}
