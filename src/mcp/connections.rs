//! Revisioned agent connection metadata and separate owner-bound credentials.
use crate::{json, Api, ApiError, App, CancellationToken, Value};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Connection {
    id: String,
    name: String,
    endpoint: String,
    enabled: bool,
    allowed_tools: Vec<String>,
    tools: Vec<Value>,
    owner_id: String,
    credential_ref: String,
    revision: u64,
}
#[derive(Default, Serialize, Deserialize)]
struct Catalog {
    revision: u64,
    connections: BTreeMap<String, Connection>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Configuration {
    pub name: String,
    pub endpoint: String,
    pub enabled: bool,
    pub allowed_tools: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Body {
    pub expected_revision: u64,
    pub connection: Configuration,
    pub bearer: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RevisionBody {
    pub expected_revision: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct QueryBody {
    pub query: Option<String>,
    pub endpoint: Option<String>,
    pub bearer: Option<String>,
}
fn denied() -> ApiError {
    ApiError(
        StatusCode::FORBIDDEN,
        "MCP connection access unavailable".into(),
    )
}
fn doc(agent: &str) -> String {
    format!("agent:{agent}:mcp_connections")
}
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 80
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
}
fn endpoint(raw: &str) -> Result<String, ApiError> {
    if raw.len() > 2048 || raw.trim() != raw || raw.chars().any(char::is_control) {
        return Err(crate::bad("Invalid MCP endpoint"));
    }
    let url = reqwest::Url::parse(raw).map_err(|_| crate::bad("Invalid MCP endpoint"))?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(crate::bad(
            "Use a public HTTPS MCP endpoint without embedded credentials",
        ));
    }
    Ok(url.to_string())
}
async fn authority(
    app: &App,
    identity: &Value,
    agent: &str,
    write: bool,
) -> Result<String, ApiError> {
    let caller = identity["_user_id"].as_str().ok_or_else(denied)?;
    let account = app.store.account_by_id(caller).await?.ok_or_else(denied)?;
    if !crate::access::Principal::from_account(&account)?.active() {
        return Err(denied());
    }
    let rights = app
        .store
        .agent_authority(caller, agent)
        .await
        .map_err(|_| denied())?;
    if (write && rights["can_manage"] != true)
        || (!write
            && rights["can_manage"] != true
            && !app
                .store
                .live_access(caller, agent)
                .await
                .is_ok_and(|a| a.admitted()))
    {
        return Err(denied());
    }
    Ok(caller.to_owned())
}
async fn load(app: &App, agent: &str) -> Result<Catalog, ApiError> {
    let store = app.store.for_agent(agent).await?;
    store
        .get(&doc(agent))
        .await?
        .map(serde_json::from_value)
        .transpose()
        .map_err(anyhow::Error::from)
        .map_err(Into::into)
        .map(|v| v.unwrap_or_default())
}
fn projection(agent: &str, catalog: &Catalog, caller: &str, manage: bool) -> Value {
    json!({"agent_id":agent,"available":super::bridge::available(),"revision":catalog.revision,"connections":catalog.connections.values().map(|c|json!({"id":c.id,"name":c.name,"endpoint":c.endpoint,"enabled":c.enabled,"revision":c.revision,"allowed_tools":c.allowed_tools,"tools":c.tools,"authenticated":!c.credential_ref.is_empty(),"can_edit":manage&&c.owner_id==caller})).collect::<Vec<_>>()})
}
pub(crate) async fn read(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path(agent): Path<String>,
) -> Api {
    let caller = authority(&app, &identity, &agent, false).await?;
    let rights = app.store.agent_authority(&caller, &agent).await?;
    let mut catalog = load(&app, &agent).await?;
    if rights["can_manage"] != true {
        catalog.connections.retain(|_, c| c.enabled);
        for c in catalog.connections.values_mut() {
            c.tools
                .retain(|t| c.allowed_tools.iter().any(|n| t["name"] == n.as_str()));
        }
    }
    Ok(Json(projection(
        &agent,
        &catalog,
        &caller,
        rights["can_manage"] == true,
    )))
}
async fn discover(
    endpoint: &str,
    bearer: Option<&str>,
    cancel: CancellationToken,
) -> Result<Vec<Value>, ApiError> {
    let result = super::bridge::call(
        json!({"version":1,"operation":"inspect","endpoint":endpoint,"bearer":bearer}),
        cancel,
    )
    .await
    .map_err(|_| crate::bad("MCP inspection failed"))?;
    if result["status"] != "success" {
        return Err(crate::bad(
            "Could not inspect MCP server; verify its endpoint and credentials",
        ));
    }
    let tools = result["output"]["tools"]
        .as_array()
        .filter(|ts| ts.len() <= 128)
        .ok_or_else(|| crate::bad("MCP tool catalog is invalid or too large"))?;
    let mut seen = std::collections::BTreeSet::new();
    for tool in tools {
        let name = tool["name"]
            .as_str()
            .filter(|n| !n.is_empty() && n.len() <= 128 && !n.chars().any(char::is_control))
            .ok_or_else(|| crate::bad("Invalid MCP tool name"))?;
        if !seen.insert(name)
            || !tool["inputSchema"].is_object()
            || tool["inputSchema"]["type"] != "object"
        {
            return Err(crate::bad("Invalid MCP tool schema"));
        }
    }
    Ok(tools.clone())
}
pub(crate) async fn search(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path(agent): Path<String>,
    Json(body): Json<QueryBody>,
) -> Api {
    authority(&app, &identity, &agent, true).await?;
    let query = body
        .query
        .filter(|q| q.len() <= 200)
        .ok_or_else(|| crate::bad("Search query required"))?;
    let result = super::bridge::call(
        json!({"version":1,"operation":"search","query":query}),
        CancellationToken::new(),
    )
    .await
    .map_err(|_| crate::bad("MCP search unavailable"))?;
    authority(&app, &identity, &agent, true).await?;
    Ok(Json(result))
}
pub(crate) async fn inspect(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path(agent): Path<String>,
    Json(body): Json<QueryBody>,
) -> Api {
    authority(&app, &identity, &agent, true).await?;
    let url = endpoint(
        body.endpoint
            .as_deref()
            .ok_or_else(|| crate::bad("Endpoint required"))?,
    )?;
    validate_bearer(body.bearer.as_deref())?;
    let tools = discover(&url, body.bearer.as_deref(), CancellationToken::new()).await?;
    authority(&app, &identity, &agent, true).await?;
    Ok(Json(json!({"status":"success","output":{"tools":tools}})))
}
fn validate_bearer(bearer: Option<&str>) -> Result<(), ApiError> {
    if bearer
        .is_some_and(|v| v.len() < 8 || v.len() > 4096 || !v.bytes().all(|b| b.is_ascii_graphic()))
    {
        return Err(crate::bad("Use an 8–4096 character bearer credential"));
    }
    Ok(())
}
// Shared file leases fence API configuration against loop dispatch across processes.
// A revoke waits for admitted calls to stop/finish before it commits its new generation.
async fn resource_lease(
    app: &App,
    agent: &str,
    write: bool,
    cancel: &CancellationToken,
) -> crate::Result<std::fs::File> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let directory = app.store.root.join("mcp/leases");
    std::fs::create_dir_all(&directory)?;
    anyhow::ensure!(
        std::fs::symlink_metadata(&directory)?.file_type().is_dir(),
        "MCP lease directory invalid"
    );
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(0x20000)
        .open(directory.join(crate::digest(&json!(agent))))?;
    let metadata = file.metadata()?;
    anyhow::ensure!(
        metadata.is_file() && metadata.permissions().mode() & 0o077 == 0,
        "MCP lease file invalid"
    );
    tokio::time::timeout(std::time::Duration::from_secs(35),async {
        loop {
            if cancel.is_cancelled(){anyhow::bail!("MCP resource wait cancelled");}
            let locked=if write {fs2::FileExt::try_lock_exclusive(&file)}else{fs2::FileExt::try_lock_shared(&file)};
            match locked {
                Ok(())=>return Ok::<_,anyhow::Error>(()),
                Err(error) if error.kind()==std::io::ErrorKind::WouldBlock=>{},
                Err(error)=>return Err(error.into()),
            }
            tokio::select!{_ = cancel.cancelled()=>anyhow::bail!("MCP resource wait cancelled"),_ = tokio::time::sleep(std::time::Duration::from_millis(25))=>{}}
        }
    }).await.map_err(|_|anyhow::anyhow!("MCP connection is busy; reload before retrying"))??;
    Ok(file)
}
async fn save(
    app: &App,
    identity: &Value,
    agent: &str,
    id: &str,
    body: Body,
    cancel: CancellationToken,
) -> Api {
    let caller = authority(app, identity, agent, true).await?;
    if !valid_id(id)
        || body.connection.name.is_empty()
        || body.connection.name.len() > 100
        || body.connection.name.chars().any(char::is_control)
        || body.connection.allowed_tools.len() > 128
    {
        return Err(crate::bad("Invalid MCP connection"));
    }
    validate_bearer(body.bearer.as_deref())?;
    let url = endpoint(&body.connection.endpoint)?;
    // The credential is scoped to this exact saved endpoint, never model arguments.
    let before = load(app, agent).await?;
    if before.revision != body.expected_revision {
        return Err(crate::conflict(
            "MCP connections changed; reload before saving",
        ));
    }
    let previous = before.connections.get(id);
    if previous.is_some_and(|c| c.owner_id != caller) {
        return Err(denied());
    }
    if previous.is_none() && before.connections.len() >= 32 {
        return Err(crate::bad("MCP connection limit reached"));
    }
    let mut reference = previous
        .filter(|c| c.endpoint == url)
        .map(|c| c.credential_ref.clone())
        .unwrap_or_default();
    let bearer = if let Some(value) = body.bearer.as_ref() {
        Some(value.clone())
    } else {
        credential(app, agent, id, &caller, &reference).await?
    };
    // Disabling preserves the reviewed catalog and works even when the server/plugin is down.
    let tools = if body.connection.enabled
        || (!body.connection.allowed_tools.is_empty() && previous.is_none_or(|c| c.endpoint != url))
    {
        discover(&url, bearer.as_deref(), cancel.clone()).await?
    } else {
        previous
            .filter(|c| c.endpoint == url)
            .map(|c| c.tools.clone())
            .unwrap_or_default()
    };
    let unique: std::collections::BTreeSet<_> = body.connection.allowed_tools.iter().collect();
    if unique.len() != body.connection.allowed_tools.len()
        || body
            .connection
            .allowed_tools
            .iter()
            .any(|name| !tools.iter().any(|t| t["name"] == name.as_str()))
    {
        return Err(crate::bad(
            "Select only tools discovered from this MCP server",
        ));
    }
    if cancel.is_cancelled() {
        return Err(crate::bad("MCP connection update cancelled"));
    }
    let _lease = resource_lease(app, agent, true, &cancel).await?;
    let _guard = app.gate.lock().await;
    authority(app, identity, agent, true).await?;
    let store = app.store.for_agent(agent).await?;
    let old: Option<Vec<u8>> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
        .bind(doc(agent))
        .fetch_optional(&store.agent_pool)
        .await
        .map_err(anyhow::Error::from)?;
    let mut next: Catalog = old
        .as_ref()
        .map(|b| store.unseal(b))
        .transpose()?
        .map(serde_json::from_value)
        .transpose()
        .map_err(anyhow::Error::from)?
        .unwrap_or_default();
    if next.revision != body.expected_revision {
        return Err(crate::conflict(
            "MCP connections changed; reload before saving",
        ));
    }
    let old_reference = next
        .connections
        .get(id)
        .map(|c| c.credential_ref.clone())
        .unwrap_or_default();
    if body.bearer.is_some() {
        reference = format!("mcp_credential:{}", crate::uuid());
        app.store.put(&reference,&json!({"bearer":bearer,"agent_id":agent,"connection_id":id,"owner_id":caller,"endpoint":url})).await?;
    }
    // Never reuse a generation after deletion/recreation of the same ID.
    let revision = next
        .revision
        .checked_add(1)
        .ok_or_else(|| crate::bad("MCP revision exhausted"))?;
    next.connections.insert(
        id.into(),
        Connection {
            id: id.into(),
            name: body.connection.name,
            endpoint: url,
            enabled: body.connection.enabled,
            allowed_tools: body.connection.allowed_tools,
            tools,
            owner_id: caller.clone(),
            credential_ref: reference.clone(),
            revision,
        },
    );
    if cancel.is_cancelled() {
        if body.bearer.is_some() {
            let _ = app.store.delete(&reference).await;
        }
        return Err(crate::bad("MCP connection update cancelled"));
    }
    if let Err(error) = persist(&store, agent, id, &caller, &mut next, old).await {
        if body.bearer.is_some() {
            let _ = app.store.delete(&reference).await;
        }
        return Err(error);
    }
    if previous.is_some() {
        crate::phase3_api::quiesce_agent(app, agent).await?;
    }
    if !old_reference.is_empty() && old_reference != reference {
        app.store.delete(&old_reference).await?;
    }
    Ok(Json(projection(agent, &next, &caller, true)))
}
async fn persist(
    store: &crate::Store,
    agent: &str,
    id: &str,
    caller: &str,
    next: &mut Catalog,
    old: Option<Vec<u8>>,
) -> Result<(), ApiError> {
    next.revision = next
        .revision
        .checked_add(1)
        .ok_or_else(|| crate::bad("MCP revision exhausted"))?;
    let sealed = store.seal(&serde_json::to_value(&*next).map_err(anyhow::Error::from)?)?;
    let mut tx = store
        .agent_pool
        .begin()
        .await
        .map_err(anyhow::Error::from)?;
    let affected = if let Some(old) = old {
        sqlx::query("UPDATE docs SET value=? WHERE key=? AND value=?")
            .bind(sealed)
            .bind(doc(agent))
            .bind(old)
            .execute(&mut *tx)
            .await
            .map_err(anyhow::Error::from)?
            .rows_affected()
    } else {
        sqlx::query("INSERT INTO docs(key,value) VALUES(?,?) ON CONFLICT(key) DO NOTHING")
            .bind(doc(agent))
            .bind(sealed)
            .execute(&mut *tx)
            .await
            .map_err(anyhow::Error::from)?
            .rows_affected()
    };
    if affected != 1 {
        return Err(crate::conflict(
            "MCP connections changed; reload before saving",
        ));
    }
    sqlx::query("INSERT INTO docs(key,value) VALUES(?,?)")
        .bind(format!("agent:{agent}:mcp_audit:{}", next.revision))
        .bind(store.seal(&json!({"actor_id":caller,"connection_id":id,"revision":next.revision}))?)
        .execute(&mut *tx)
        .await
        .map_err(anyhow::Error::from)?;
    tx.commit().await.map_err(anyhow::Error::from)?;
    Ok(())
}
async fn credential(
    app: &App,
    agent: &str,
    id: &str,
    owner: &str,
    reference: &str,
) -> Result<Option<String>, ApiError> {
    if reference.is_empty() {
        return Ok(None);
    }
    let account = app.store.account_by_id(owner).await?.ok_or_else(denied)?;
    if !crate::access::Principal::from_account(&account)?.active()
        || app.store.agent_authority(owner, agent).await?["can_manage"] != true
    {
        return Err(denied());
    }

    let secret = app.store.get(reference).await?.ok_or_else(denied)?;
    if secret["agent_id"] != agent || secret["connection_id"] != id || secret["owner_id"] != owner {
        return Err(denied());
    }
    Ok(Some(
        secret["bearer"].as_str().ok_or_else(denied)?.to_owned(),
    ))
}
pub(crate) async fn put(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path((agent, id)): Path<(String, String)>,
    Json(body): Json<Body>,
) -> Api {
    save(&app, &identity, &agent, &id, body, CancellationToken::new()).await
}
pub(crate) async fn remove(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path((agent, id)): Path<(String, String)>,
    Json(body): Json<RevisionBody>,
) -> Api {
    authority(&app, &identity, &agent, true).await?;
    let _lease = resource_lease(&app, &agent, true, &CancellationToken::new()).await?;
    let _guard = app.gate.lock().await;
    let caller = authority(&app, &identity, &agent, true).await?;
    let store = app.store.for_agent(&agent).await?;
    let old: Option<Vec<u8>> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
        .bind(doc(&agent))
        .fetch_optional(&store.agent_pool)
        .await
        .map_err(anyhow::Error::from)?;
    let mut next: Catalog = old
        .as_ref()
        .map(|b| store.unseal(b))
        .transpose()?
        .map(serde_json::from_value)
        .transpose()
        .map_err(anyhow::Error::from)?
        .unwrap_or_default();
    if next.revision != body.expected_revision {
        return Err(crate::conflict(
            "MCP connections changed; reload before saving",
        ));
    }
    let record = next
        .connections
        .get(&id)
        .ok_or_else(|| crate::bad("MCP connection not found"))?;
    if record.owner_id != caller {
        return Err(denied());
    }
    let reference = record.credential_ref.clone();
    next.connections.remove(&id);
    persist(&store, &agent, &id, &caller, &mut next, old).await?;
    crate::phase3_api::quiesce_agent(&app, &agent).await?;
    if !reference.is_empty() {
        app.store.delete(&reference).await?;
    }
    Ok(Json(projection(&agent, &next, &caller, true)))
}
pub(super) async fn model_connections(app: &App, agent: &str) -> crate::Result<Value> {
    let catalog = load(app, agent)
        .await
        .map_err(|_| anyhow::anyhow!("MCP catalog unavailable"))?;
    Ok(
        json!({"status":"success","output":{"revision":catalog.revision,"connections":catalog.connections.values().filter(|c|c.enabled).map(|c|json!({"id":c.id,"name":c.name,"endpoint":c.endpoint,"revision":c.revision,"tools":c.allowed_tools})).collect::<Vec<_>>()}}),
    )
}
fn selected<'a>(catalog: &'a Catalog, args: &Value) -> crate::Result<&'a Connection> {
    catalog
        .connections
        .get(args["connection_id"].as_str().unwrap_or(""))
        .filter(|c| c.enabled)
        .ok_or_else(|| anyhow::anyhow!("MCP connection unavailable"))
}
pub(super) async fn model_tools(app: &App, agent: &str, args: &Value) -> crate::Result<Value> {
    let catalog = load(app, agent)
        .await
        .map_err(|_| anyhow::anyhow!("MCP catalog unavailable"))?;
    let c = selected(&catalog, args)?;
    Ok(
        json!({"status":"success","output":{"connection_id":c.id,"revision":c.revision,"tools":c.tools.iter().filter(|t|c.allowed_tools.iter().any(|n|t["name"]==n.as_str())).collect::<Vec<_>>()}}),
    )
}
async fn call_request(
    app: &App,
    agent: &str,
    args: &Value,
    cancel: CancellationToken,
) -> crate::Result<(std::fs::File, Value)> {
    let lease = resource_lease(app, agent, false, &cancel).await?;
    let catalog = load(app, agent)
        .await
        .map_err(|_| anyhow::anyhow!("MCP catalog unavailable"))?;
    let c = selected(&catalog, args)?;
    anyhow::ensure!(
        args["expected_revision"] == c.revision,
        "MCP connection changed; list its tools again"
    );
    let schema = c
        .tools
        .iter()
        .find(|t| {
            t["name"] == args["tool"] && c.allowed_tools.iter().any(|n| t["name"] == n.as_str())
        })
        .ok_or_else(|| anyhow::anyhow!("MCP tool not permitted"))?;
    anyhow::ensure!(
        args["arguments"].is_object(),
        "MCP arguments must be an object"
    );
    let bearer = credential(app, agent, &c.id, &c.owner_id, &c.credential_ref)
        .await
        .map_err(|_| anyhow::anyhow!("MCP credential unavailable"))?;
    let live = load(app, agent)
        .await
        .map_err(|_| anyhow::anyhow!("MCP catalog unavailable"))?;
    anyhow::ensure!(
        live.connections
            .get(&c.id)
            .is_some_and(|l| l.enabled && l.revision == c.revision),
        "MCP connection revoked"
    );
    Ok((
        lease,
        json!({"version":1,"operation":"call","endpoint":c.endpoint,"bearer":bearer,"tool":args["tool"],"arguments":args["arguments"],"expected_schema":schema}),
    ))
}
pub(super) async fn model_call(
    app: &App,
    agent: &str,
    args: &Value,
    cancel: CancellationToken,
) -> crate::Result<Value> {
    let (_lease, request) = match call_request(app, agent, args, cancel.clone()).await {
        Ok(prepared) => prepared,
        // These checks finish before a package request can be sent. Do not turn
        // an unavailable, revoked or stale connection into an uncertain write.
        Err(_) => {
            return Ok(
                json!({"status":"denied","output":"MCP connection is unavailable, changed, or the tool is not permitted; list its tools before retrying"}),
            )
        }
    };
    match super::bridge::call(request, cancel).await {
        Ok(value) => Ok(value),
        Err(_) => Ok(
            json!({"status":"uncertain","output":"MCP call interrupted; reconcile external effects before retrying"}),
        ),
    }
}

pub(super) async fn model_add(
    app: &App,
    agent: &str,
    args: &Value,
    cancel: CancellationToken,
) -> crate::Result<Value> {
    let denied = || json!({"status":"denied","output":"MCP connection update denied or stale; list saved connections before retrying"});
    let before = match load(app, agent).await {
        Ok(value) => value,
        Err(_) => return Ok(denied()),
    };
    if args["expected_revision"] != before.revision
        || before
            .connections
            .contains_key(args["connection_id"].as_str().unwrap_or(""))
    {
        return Ok(denied());
    }
    let Some(expected_revision) = args["expected_revision"].as_u64() else {
        return Ok(denied());
    };
    let Ok(allowed_tools) = serde_json::from_value(args["allowed_tools"].clone()) else {
        return Ok(denied());
    };
    let body = Body {
        expected_revision,
        connection: Configuration {
            name: args["name"].as_str().unwrap_or("").into(),
            endpoint: args["endpoint"].as_str().unwrap_or("").into(),
            enabled: true,
            allowed_tools,
        },
        bearer: None,
    };
    let identity = json!({"_user_id":app.store.current_user_id()});
    match save(
        app,
        &identity,
        agent,
        args["connection_id"].as_str().unwrap_or(""),
        body,
        cancel,
    )
    .await
    {
        Ok(result) => Ok(json!({"status":"success","output":result.0})),
        Err(error) if error.0.is_client_error() => Ok(denied()),
        Err(_) => Ok(
            json!({"status":"uncertain","output":"MCP connection update interrupted; inspect saved connections before retrying"}),
        ),
    }
}
pub(crate) async fn configured(app: &App, agent: &str) -> bool {
    load(app, agent).await.is_ok_and(|c| {
        c.connections
            .values()
            .any(|c| c.enabled && !c.allowed_tools.is_empty())
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn endpoints_never_embed_credentials() {
        for url in [
            "http://example.com/mcp",
            "https://u:p@example.test/mcp",
            "https://example.com/mcp?token=x",
            "https://example.com/mcp#x",
        ] {
            assert!(endpoint(url).is_err());
        }
        assert!(endpoint("https://example.com/mcp").is_ok());
    }
    #[test]
    fn connection_projection_never_exposes_secret_reference() {
        let mut catalog = Catalog::default();
        catalog.connections.insert(
            "one".into(),
            Connection {
                id: "one".into(),
                name: "Fixture".into(),
                endpoint: "https://example.com/mcp".into(),
                enabled: true,
                allowed_tools: vec!["read".into()],
                tools: vec![],
                owner_id: "admin".into(),
                credential_ref: "private-secret-reference".into(),
                revision: 1,
            },
        );
        let view = projection("shared_default", &catalog, "admin", true);
        assert!(!view.to_string().contains("private-secret-reference"));
        assert_eq!(view["connections"][0]["authenticated"], true);
    }
    #[tokio::test]
    async fn stale_updates_and_other_callers_cannot_remove_connections() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let agent = "shared_default";
        let mut catalog = Catalog {
            revision: 2,
            ..Default::default()
        };
        catalog.connections.insert(
            "one".into(),
            Connection {
                id: "one".into(),
                name: "Fixture".into(),
                endpoint: "https://example.com/mcp".into(),
                enabled: true,
                allowed_tools: vec!["read".into()],
                tools: vec![
                    json!({"name":"read","description":"","inputSchema":{"type":"object"}}),
                ],
                owner_id: "admin".into(),
                credential_ref: String::new(),
                revision: 1,
            },
        );
        app.store
            .for_agent(agent)
            .await
            .unwrap()
            .put(&doc(agent), &serde_json::to_value(&catalog).unwrap())
            .await
            .unwrap();
        assert!(matches!(
            remove(
                State(app.clone()),
                Extension(json!({"_user_id":"admin"})),
                Path((agent.into(), "one".into())),
                Json(RevisionBody {
                    expected_revision: 1
                })
            )
            .await,
            Err(ApiError(StatusCode::CONFLICT, _))
        ));
        assert!(remove(
            State(app.clone()),
            Extension(json!({"_user_id":"missing"})),
            Path((agent.into(), "one".into())),
            Json(RevisionBody {
                expected_revision: 2
            })
        )
        .await
        .is_err());
        let view = model_tools(&app, agent, &json!({"connection_id":"one"}))
            .await
            .unwrap();
        assert_eq!(view["output"]["tools"].as_array().unwrap().len(), 1);
        assert!(call_request(
            &app,
            agent,
            &json!({"connection_id":"one","expected_revision":1,"tool":"write","arguments":{}}),
            CancellationToken::new()
        )
        .await
        .is_err());
        let removed = remove(
            State(app),
            Extension(json!({"_user_id":"admin"})),
            Path((agent.into(), "one".into())),
            Json(RevisionBody {
                expected_revision: 2,
            }),
        )
        .await
        .unwrap();
        assert_eq!(removed.0["revision"], 3);
        assert_eq!(removed.0["connections"], json!([]));
    }
    #[tokio::test]
    async fn credentials_are_private_and_connection_generations_survive_recreation() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let identity = json!({"_user_id":"admin"});
        let body = |rev, bearer: Option<&str>| Body {
            expected_revision: rev,
            connection: Configuration {
                name: "Fixture".into(),
                endpoint: "https://example.com/mcp".into(),
                enabled: false,
                allowed_tools: vec![],
            },
            bearer: bearer.map(str::to_owned),
        };
        let view = save(
            &app,
            &identity,
            "shared_default",
            "fixture",
            body(0, Some("fixture-private-token")),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert!(!view.0.to_string().contains("fixture-private-token"));
        assert!(!view.0.to_string().contains("credential_ref"));
        let first = load(&app, "shared_default").await.unwrap();
        let reference = first.connections["fixture"].credential_ref.clone();
        assert_eq!(
            app.store.get(&reference).await.unwrap().unwrap()["bearer"],
            "fixture-private-token"
        );
        assert!(model_add(&app,"shared_default",&json!({"connection_id":"fixture","expected_revision":1,"name":"Replace","endpoint":"https://other.example.com/mcp","allowed_tools":[]}),CancellationToken::new()).await.unwrap()["status"] == "denied");
        assert!(model_add(&app,"shared_default",&json!({"connection_id":"new","expected_revision":99,"name":"Future","endpoint":"https://example.com/mcp","allowed_tools":[]}),CancellationToken::new()).await.unwrap()["status"] == "denied");
        let _ = remove(
            State(app.clone()),
            Extension(identity.clone()),
            Path(("shared_default".into(), "fixture".into())),
            Json(RevisionBody {
                expected_revision: 1,
            }),
        )
        .await
        .unwrap();
        assert!(app.store.get(&reference).await.unwrap().is_none());
        let _ = save(
            &app,
            &identity,
            "shared_default",
            "fixture",
            body(2, None),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let mut recreated = load(&app, "shared_default").await.unwrap();
        assert_eq!(recreated.connections["fixture"].revision, 3);
        let c = recreated.connections.get_mut("fixture").unwrap();
        c.enabled = true;
        c.allowed_tools = vec!["read".into()];
        c.tools =
            vec![json!({"name":"read","description":"Fixture","inputSchema":{"type":"object"}})];
        app.store
            .for_agent("shared_default")
            .await
            .unwrap()
            .put(
                &doc("shared_default"),
                &serde_json::to_value(&recreated).unwrap(),
            )
            .await
            .unwrap();
        let error = call_request(
            &app,
            "shared_default",
            &json!({"connection_id":"fixture","expected_revision":1,"tool":"read","arguments":{}}),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("connection changed"));
    }
    #[tokio::test]
    async fn removal_waits_for_transport_lease_before_revoking_connection() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let identity = json!({"_user_id":"admin"});
        let body = Body {
            expected_revision: 0,
            connection: Configuration {
                name: "Fixture".into(),
                endpoint: "https://example.com/mcp".into(),
                enabled: false,
                allowed_tools: vec![],
            },
            bearer: None,
        };
        let _ = save(
            &app,
            &identity,
            "shared_default",
            "fixture",
            body,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let lease = resource_lease(&app, "shared_default", false, &CancellationToken::new())
            .await
            .unwrap();
        let task = tokio::spawn(remove(
            State(app.clone()),
            Extension(identity),
            Path(("shared_default".into(), "fixture".into())),
            Json(RevisionBody {
                expected_revision: 1,
            }),
        ));
        tokio::time::sleep(std::time::Duration::from_millis(75)).await;
        assert!(!task.is_finished());
        assert!(load(&app, "shared_default")
            .await
            .unwrap()
            .connections
            .contains_key("fixture"));
        drop(lease);
        task.await.unwrap().unwrap();
        assert!(load(&app, "shared_default")
            .await
            .unwrap()
            .connections
            .is_empty());
    }

    #[tokio::test]
    async fn revoked_credential_owner_cannot_supply_an_agent_token() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let account = app
            .store
            .create_user_account("mcp_delegate", "fixture-password-hash")
            .await
            .unwrap();
        let uid = account["user_id"].as_str().unwrap();
        app.store
            .update_account_access(
                uid,
                1,
                crate::access::AccountStatus::Active,
                crate::access::Role::User,
                "basic",
            )
            .await
            .unwrap();
        app.store
            .assign_agent_membership(
                "admin",
                uid,
                "shared_default",
                crate::access::Relationship::Member,
                true,
                0,
            )
            .await
            .unwrap();
        let body = Body {
            expected_revision: 0,
            connection: Configuration {
                name: "Private fixture".into(),
                endpoint: "https://example.com/mcp".into(),
                enabled: false,
                allowed_tools: vec![],
            },
            bearer: Some("fixture-owner-token".into()),
        };
        let _ = save(
            &app,
            &json!({"_user_id":uid}),
            "shared_default",
            "fixture",
            body,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let catalog = load(&app, "shared_default").await.unwrap();
        let reference = &catalog.connections["fixture"].credential_ref;
        assert!(
            credential(&app, "shared_default", "fixture", uid, reference)
                .await
                .is_ok()
        );
        let member = app
            .store
            .agent_membership(uid, "shared_default")
            .await
            .unwrap()
            .unwrap();
        app.store
            .assign_agent_membership(
                "admin",
                uid,
                "shared_default",
                crate::access::Relationship::Member,
                false,
                member["revision"].as_u64().unwrap(),
            )
            .await
            .unwrap();
        assert!(
            credential(&app, "shared_default", "fixture", uid, reference)
                .await
                .is_err()
        );
        let member = app
            .store
            .agent_membership(uid, "shared_default")
            .await
            .unwrap()
            .unwrap();
        app.store
            .assign_agent_membership(
                "admin",
                uid,
                "shared_default",
                crate::access::Relationship::Member,
                true,
                member["revision"].as_u64().unwrap(),
            )
            .await
            .unwrap();
        app.store
            .update_account_access(
                uid,
                app.store.account_by_id(uid).await.unwrap().unwrap()["access_revision"]
                    .as_u64()
                    .unwrap(),
                crate::access::AccountStatus::Suspended,
                crate::access::Role::User,
                "basic",
            )
            .await
            .unwrap();
        assert!(
            credential(&app, "shared_default", "fixture", uid, reference)
                .await
                .is_err()
        );
    }
    #[tokio::test]
    async fn failed_catalog_commit_does_not_leave_rotated_credentials() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let identity = json!({"_user_id":"admin"});
        let body = |rev, token: &str| Body {
            expected_revision: rev,
            connection: Configuration {
                name: "Fixture".into(),
                endpoint: "https://example.com/mcp".into(),
                enabled: false,
                allowed_tools: vec![],
            },
            bearer: Some(token.into()),
        };
        let _ = save(
            &app,
            &identity,
            "shared_default",
            "fixture",
            body(0, "fixture-original-token"),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let store = app.store.for_agent("shared_default").await.unwrap();
        sqlx::query("CREATE TRIGGER reject_mcp_update BEFORE UPDATE ON docs WHEN OLD.key='agent:shared_default:mcp_connections' BEGIN SELECT RAISE(ABORT,'fixture-only'); END").execute(&store.agent_pool).await.unwrap();
        assert!(save(
            &app,
            &identity,
            "shared_default",
            "fixture",
            body(1, "fixture-rotated-token"),
            CancellationToken::new()
        )
        .await
        .is_err());
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM docs WHERE key LIKE 'mcp_credential:%'")
                .fetch_one(&app.store.app_pool)
                .await
                .unwrap();
        assert_eq!(count, 1);
        let catalog = load(&app, "shared_default").await.unwrap();
        assert_eq!(catalog.connections["fixture"].revision, 1);
        assert_eq!(
            credential(
                &app,
                "shared_default",
                "fixture",
                "admin",
                &catalog.connections["fixture"].credential_ref
            )
            .await
            .unwrap()
            .as_deref(),
            Some("fixture-original-token")
        );
        let denied = model_call(
            &app,
            "shared_default",
            &json!({"connection_id":"missing","expected_revision":0,"tool":"write","arguments":{}}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(denied["status"], "denied");
        assert_ne!(denied["uncertain"], true);
    }
}
