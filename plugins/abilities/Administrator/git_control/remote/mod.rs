//! Source Control owns remote site lifecycle. GitHub stores source; Cloudflare builds and serves it.
use crate::*;
use axum::Extension;
mod model;
mod operations;
mod provider;
mod provision;
#[cfg(test)]
mod tests;
use model::*;
use operations::Operation;
use provider::*;
pub fn field<'a>(value: &'a Value, key: &str, max: usize) -> Result<&'a str> {
    value[key]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= max && !s.chars().any(char::is_control))
        .ok_or_else(|| anyhow!("Missing or invalid {key}"))
}
pub const TOOLS: [&str; 8] = [
    "source_remote_read",
    "source_remote_commit",
    "source_site_create",
    "source_site_status",
    "source_preview_start",
    "source_preview_stop",
    "source_publish",
    "source_rollback",
];
pub fn owns(name: &str) -> bool {
    TOOLS.contains(&name)
}
pub fn is_read(name: &str) -> bool {
    matches!(name, "source_remote_read" | "source_site_status")
}
pub fn needs_approval(name: &str) -> bool {
    matches!(name, "source_publish" | "source_rollback")
}
async fn authority(app: &App, identity: &Value, agent: &str) -> Result<Store> {
    let uid = identity["_user_id"]
        .as_str()
        .ok_or_else(|| anyhow!("Sign in required"))?;
    anyhow::ensure!(identifier(agent), "Invalid agent");
    let account = app
        .store
        .account_by_id(uid)
        .await?
        .ok_or_else(|| anyhow!("Account unavailable"))?;
    anyhow::ensure!(
        access::Principal::from_account(&account)?.active()
            && app.store.agent_authority(uid, agent).await?["can_manage"] == true,
        "Agent management authority required"
    );
    app.store.for_user(uid).await?.for_agent(agent).await
}
pub async fn settings_get(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path(agent): Path<String>,
) -> Api {
    let store = authority(&app, &identity, &agent).await.map_err(|_| {
        ApiError(
            StatusCode::FORBIDDEN,
            "Remote site settings unavailable".into(),
        )
    })?;
    Ok(Json(list(&store, &agent).await?))
}
pub async fn settings_put(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path((agent, id)): Path<(String, String)>,
    Json(body): Json<Save>,
) -> Api {
    let store = authority(&app, &identity, &agent).await.map_err(|_| {
        ApiError(
            StatusCode::FORBIDDEN,
            "Remote site settings unavailable".into(),
        )
    })?;
    if !identifier(&id) {
        return Err(bad("Invalid site identity"));
    }
    body.configuration.validate().map_err(|_| {
        bad("Use a valid static-site configuration and the fixed remote Wrangler recipe")
    })?;
    let result = save(&store, &agent, &id, body).await.map_err(|error| {
        if error.to_string().contains("revision") || error.to_string().contains("busy") {
            conflict("Site connection changed or is busy; reload before saving")
        } else {
            ApiError::from(error)
        }
    })?;
    // In-flight work rechecks the new connection revision before every external effect.
    crate::phase3_api::quiesce_agent(&app, &agent).await?;
    Ok(Json(result))
}
/// Existing Engine credential-binding flow forces human approval even in Auto mode.
pub async fn approval_binding(
    store: &Store,
    run: &Value,
    name: &str,
    args: &Value,
) -> Result<Value> {
    anyhow::ensure!(needs_approval(name), "Unsupported Source Control approval");
    let owner = run["requester_user_id"]
        .as_str()
        .unwrap_or(store.current_user_id());
    let agent = run["agent_id"].as_str().unwrap_or("shared_default");
    let store = store.for_user(owner).await?.for_agent(agent).await?;
    let site = load(&store, agent, field(args, "site_id", 80)?).await?;
    anyhow::ensure!(
        args["connection_revision"].as_u64() == Some(site.revision),
        "Connection changed before approval"
    );
    let secret = credentials(&store, &site).await?;
    let state = if name == "source_publish" {
        json!([
            site.runtime["preview"]["commit_sha"],
            site.runtime["preview"]["branch"]
        ])
    } else {
        site.runtime["rollback_target"].clone()
    };
    Ok(
        json!({"kind":"source_control_owner","owner_user_id":owner,"agent_id":agent,"site_id":site.id,"connection_revision":site.revision,"credential_digest":digest(&serde_json::to_value(secret)?),"candidate_digest":digest(&state),"tool":name,"arguments_digest":digest(args),"run_id":run["id"],"session_id":run["session_id"],"participation_generation":run["participation_generation"]}),
    )
}
pub async fn execute(
    app: &App,
    sid: &str,
    run: &Value,
    name: &str,
    args: &Value,
    pending_id: &str,
    cancel: CancellationToken,
) -> Result<Value> {
    let agent = session_agent_id(app, sid).await?;
    let owner = crate::shared_chat::actor(app);
    let live = crate::shared_chat::live_access(&app.store, owner, sid, &agent).await?;
    anyhow::ensure!(live.admitted(), "Source Control access revoked");
    let store = app.store.for_user(owner).await?.for_agent(&agent).await?;
    if name == "source_site_status" && args["site_id"].is_null() {
        return Ok(json!({"status":"success","output":list(&store,&agent).await?}));
    }
    if name == "source_site_create" && args["operation"] == "configure" {
        // Configure has no external effect, so invalid input is a readable result, not an uncertain failure.
        return Ok(match configure(&store, &agent, args).await {
            Ok(output) => json!({"status":"success","output":output}),
            Err(error) => json!({"status":"failed","output":{"error":error.to_string(),"example":{"site_id":"my-site","operation":"configure","request_id":"my-site-configure-1","connection_revision":0,"configuration":{"github_owner":"octocat","github_repo":"my-site","worker_name":"my-site","custom_domain":"my-site.example.com"}}}}),
        });
    }
    let id = field(args, "site_id", 80)?;
    let args = &resolve_attachments(app, sid, args).await?;
    anyhow::ensure!(
        args["configuration"].is_null(),
        "Configuration is only accepted by the configure operation"
    );
    let _lock = lock(&store, &agent, id)?;
    let site = load(&store, &agent, id).await?;
    anyhow::ensure!(
        args["connection_revision"].as_u64() == Some(site.revision),
        "Connection changed; inspect site status before retrying"
    );
    if needs_approval(name) {
        let pending = if run["pending"]["id"] == pending_id
            || run["pending"]["action_pending_id"] == pending_id
        {
            Some(&run["pending"])
        } else {
            run["batch"].as_array().and_then(|a| {
                a.iter()
                    .map(|v| &v["pending"])
                    .find(|p| p["id"] == pending_id || p["action_pending_id"] == pending_id)
            })
        };
        anyhow::ensure!(
            pending.is_some_and(|p| p["human_approved"] == true && p["name"] == name),
            "Publish and rollback require fresh human approval through the tool approval card"
        );
        let pending = pending.unwrap();
        let current = approval_binding(&store, run, name, &pending["arguments"]).await?;
        anyhow::ensure!(
            pending["authorization"] == current,
            "Source or connection changed after approval"
        );
    }
    let secret = credentials(&store, &site).await?;
    if name == "source_remote_commit" {
        for content in args["changes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v["content"].as_str())
        {
            anyhow::ensure!(
                [&secret.github, &secret.cloudflare]
                    .into_iter()
                    .all(|s| s.is_empty() || !content.contains(s)),
                "Provider credential cannot be committed as source"
            );
        }
    }
    if secret.github.is_empty() || secret.cloudflare.is_empty() {
        return Ok(
            json!({"status":"failed","output":{"error":"credentials_required","credential_requests":vault_requests(&site),"setup_links":setup_links(),"instruction":CREDENTIAL_INSTRUCTION}}),
        );
    }
    let http = Http::new(secret.clone(), cancel)?;
    let mut result = run_operation(&store, site, &http, name, args).await?;
    provider::scrub(&mut result, &secret);
    Ok(result)
}
pub async fn run_operation<T: Transport>(
    store: &Store,
    mut site: Site,
    transport: &T,
    name: &str,
    args: &Value,
) -> Result<Value> {
    let current_credentials = digest(&serde_json::to_value(credentials(store, &site).await?)?);
    let credential_digest = transport
        .credential_digest()
        .unwrap_or(current_credentials.clone());
    anyhow::ensure!(
        credential_digest == current_credentials,
        "Vault credential changed before operation admission"
    );
    let write = !is_read(name);
    let request = if write {
        field(args, "request_id", 80)?
    } else {
        "read"
    };
    anyhow::ensure!(
        !write || identifier(request),
        "Canonical request ID required"
    );
    let key = receipt_key(&site.agent_id, &site.id, request);
    let signature = digest(&json!([name, args]));
    if write {
        if let Some(old) = store.get(&key).await? {
            anyhow::ensure!(
                old["signature"] == signature,
                "ProviderRequest ID already belongs to a different operation"
            );
            return Ok(json!({"status":old["status"],"output":old,"replayed":true}));
        }
        anyhow::ensure!(
            site.runtime["uncertain_receipt"].is_null(),
            "Reconcile the previous uncertain operation before new writes"
        );
        anyhow::ensure!(
            site.runtime["active_receipt"].is_null(),
            "Previous operation was interrupted; inspect and reconcile its receipt"
        );
        if site.runtime.is_null() {
            site.runtime = json!({});
        }
        site.runtime["active_receipt"] = json!(request);
        store
            .put(
                &site_key(&site.agent_id, &site.id),
                &serde_json::to_value(&site)?,
            )
            .await?;
    }
    let receipt = json!({"request_id":request,"site_id":site.id,"owner_id":store.current_user_id(),"tool":name,"signature":signature,"connection_revision":site.revision,"status":"running","steps":[],"started_at":now(),"started_at_ms":chrono::Utc::now().timestamp_millis()});
    let mut op = Operation {
        credential_digest,
        store,
        site,
        transport,
        receipt,
        key,
    };
    if write {
        op.persist().await?;
    }
    let result = match name {
        "source_remote_read" => op.read(args).await,
        "source_remote_commit" => op.commit(args).await,
        "source_site_create" => op.provision(args["operation"] == "connect").await,
        "source_preview_start" => op.preview(args).await,
        "source_preview_stop" => op.stop().await,
        "source_publish" => op.publish(args).await,
        "source_rollback" => op.rollback(args).await,
        "source_site_status" => {
            op.reconcile_previous().await?;
            let mut builds = json!({});
            for kind in ["preview", "production"] {
                if op.site.runtime[kind]["build_id"].is_string() {
                    match op.poll(kind).await {
                        Ok(v) => builds[kind] = v,
                        Err(_) => {
                            builds[kind] = json!({"status":"unavailable","reason":"Provider status could not be verified"})
                        }
                    }
                }
            }
            let receipt_id = op.site.runtime["uncertain_receipt"]
                .as_str()
                .or_else(|| op.site.runtime["active_receipt"].as_str());
            let receipt = if let Some(request) = receipt_id {
                store
                    .get(&receipt_key(&op.site.agent_id, &op.site.id, request))
                    .await?
            } else {
                None
            };
            Ok(
                json!({"site_id":op.site.id,"connection_revision":op.site.revision,"runtime":op.site.runtime,"builds":builds,"unresolved_operation":receipt,"last_reconciled_receipt":op.site.runtime["last_reconciled_receipt"],"live_provider_acceptance":"not_certified"}),
            )
        }
        _ => Err(anyhow!("Unknown Source Control tool")),
    };
    if !write {
        return result.map(|output| json!({"status":"success","output":output}));
    }
    op.receipt["finished_at"] = json!(now());
    match result {
        Ok(output) => {
            op.receipt["status"] = json!("success");
            op.receipt["output"] = output;
        }
        Err(error) => {
            let possibly_sent = op.receipt["steps"]
                .as_array()
                .is_some_and(|a| !a.is_empty());
            op.receipt["status"] = json!(if possibly_sent { "uncertain" } else { "failed" });
            op.receipt["output"] = json!({"error":error.to_string(),"instruction":"Inspect this receipt and remote state before retrying"});
            if possibly_sent {
                op.site.runtime["uncertain_receipt"] = json!(request);
            }
        }
    };
    op.persist().await?;
    op.site.runtime["active_receipt"] = Value::Null;
    store
        .put(
            &site_key(&op.site.agent_id, &op.site.id),
            &serde_json::to_value(&op.site)?,
        )
        .await?;
    Ok(json!({"status":op.receipt["status"],"output":op.receipt}))
}

const CREDENTIAL_INSTRUCTION: &str = "For each missing credential, first show the user its label, create_url as a clickable link and the exact permissions, then activate Core Visualizer and call request_credential with only its name and origin so the private panel opens. Wait for check_credential to report it configured before the next one. Also show every setup_links entry as a clickable link. Never ask for token values in chat or tool arguments.";
/// A sent chat attachment in this conversation becomes exact file bytes for source_remote_commit.
async fn resolve_attachments(app: &App, sid: &str, args: &Value) -> Result<Value> {
    let mut args = args.clone();
    for change in args["changes"].as_array_mut().into_iter().flatten() {
        let Some(id) = change["attachment_id"].as_str().map(str::to_owned) else { continue };
        anyhow::ensure!(
            change.get("content").is_none() && change.get("content_base64").is_none(),
            "Use one of content, content_base64 or attachment_id per file"
        );
        let record = app
            .store
            .get(&webagent_files::attachment_key(&id))
            .await?
            .filter(|record| webagent_files::uploads::sent_view(record, sid).is_some())
            .ok_or_else(|| anyhow!("Attachment {id} is not a file sent in this conversation"))?;
        let bytes = webagent_files::uploads::content_bytes(&record)?;
        let object = change.as_object_mut().ok_or_else(|| anyhow!("Invalid change"))?;
        object.remove("attachment_id");
        object.insert("content_base64".into(), json!(base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes)));
    }
    Ok(args)
}
/// Model-callable setup accepts public site metadata; only the human vault form accepts secrets.
pub async fn configure(store: &Store, agent: &str, args: &Value) -> Result<Value> {
    let id = field(args, "site_id", 80)?;
    anyhow::ensure!(
        identifier(field(args, "request_id", 80)?),
        "Canonical request ID required"
    );
    if let Some(old) = store.get(&site_key(agent, id)).await? {
        anyhow::ensure!(
            old["revoked"] != true,
            "Owner revoked this connection; only human settings may re-enable it"
        );
    }
    let configuration: Configuration = serde_json::from_value(args["configuration"].clone())
        .map_err(|error| anyhow!("configuration needs github_owner, github_repo and worker_name; Cloudflare IDs are optional and discovered from the saved token ({error})"))?;
    configuration.validate()?;
    let expected_revision = args["connection_revision"]
        .as_u64()
        .ok_or_else(|| anyhow!("Connection revision required"))?;
    let mut output = save(
        store,
        agent,
        id,
        Save {
            configuration,
            expected_revision,
            revoked: false,
        },
    )
    .await?;
    output["next_step"]=json!(format!("{CREDENTIAL_INSTRUCTION} When both are configured, call source_site_create with operation=create (or connect) and the returned connection_revision."));
    Ok(output)
}
