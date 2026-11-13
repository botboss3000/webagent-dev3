//! Ability plugins that call back into the app through declared host requests
//! (`requests_v1`, see `webagent_extensions::requests`). Dev builds run each call in the
//! plugin's own process from the published release, under the engine sandbox; stable
//! builds (`stable-abilities`) link the plugin. Either way a host request must be declared
//! in the plugin's `plugin.json`, and it runs as the chatting user with that user's
//! permissions.
use crate::{ability_controls, digest, modules, now, shared_chat, tools, App};
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Map, Value};
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;
use webagent_abilities::package::RequestsPlugin;
use webagent_extensions::plugin::Request;
use webagent_extensions::providers::{self, Method, ProviderCall, Providers};
use webagent_extensions::requests::{self, Declaration, Kind, ReviewAction, SkillAction};

/// Longest a plugin call may run, host requests included.
const CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
const MAX_WATCH_PROMPT: usize = 16_000;
const MAX_WATCH_SECONDS: u64 = 30 * 86_400;
/// Largest provider reply returned to a plugin.
const MAX_PROVIDER_BODY: usize = 1024 * 1024;
const PROVIDER_AGENT: &str = "WebAgent-v3/1.0 (+https://webagent.live)";

/// One tool call routed to a plugin. `ability` is its `Group/ability_id` folder.
pub(crate) struct Call<'a> {
    pub app: &'a App,
    pub sid: &'a str,
    pub run_id: &'a str,
    pub generation: &'a str,
    pub pending: &'a str,
    pub name: &'a str,
    pub args: &'a Value,
    pub cancel: CancellationToken,
    pub ability: &'static str,
}

/// The abilities folder of the release this process runs.
fn abilities_root() -> PathBuf {
    crate::live_assets::published_root()
        .map(|root| root.join("abilities"))
        .unwrap_or_else(|| {
            std::env::var_os("V3_ABILITIES_ROOT").map_or_else(
                || webagent_configuration::paths::app().join("abilities"),
                PathBuf::from,
            )
        })
}

/// Authorize and run one plugin tool call.
pub(crate) async fn execute(call: Call<'_>) -> Result<Value> {
    let Call {
        app,
        sid,
        run_id,
        generation,
        pending,
        name,
        args,
        ..
    } = call;
    tools::core_authority::check(
        app,
        sid,
        run_id,
        generation,
        pending,
        name,
        args,
        &call.cancel,
    )
    .await?;
    let id = call.ability.rsplit('/').next().unwrap_or(call.ability);
    let root = abilities_root().join(call.ability);
    let folders = webagent_abilities::tool_definitions::folder_names(&root).unwrap_or_default();
    let plugin = RequestsPlugin::load(&root, id, &folders)?;
    if !plugin.tools.iter().any(|tool| tool == name) {
        return Err(anyhow!("Tool is not served by its plugin"));
    }
    let actor = shared_chat::actor(app).to_owned();
    let request_id = digest(&json!([actor, sid, run_id, pending]));
    let context = json!({"request_id": request_id, "now_ms": now().saturating_mul(1000)});
    let host = Host {
        call: &call,
        ability: id,
        actor: &actor,
        request_id: &request_id,
        declared: &plugin.requests,
        providers: &plugin.providers,
    };
    let output = tokio::time::timeout(CALL_TIMEOUT, run(&plugin, id, &host, &context))
        .await
        .map_err(|_| anyhow!("Plugin call timed out"))??;
    // A plugin reports a tool failure as an error output carrying its code and hint.
    if output["status"] == "error" {
        return Ok(output);
    }
    // A result the app built for the plugin (a `Review` request) passes through whole, so
    // its denied or rejected status reaches the engine unchanged.
    if let Some(result) = output
        .get("tool_result")
        .filter(|result| result["status"].is_string())
    {
        return Ok(result.clone());
    }
    Ok(json!({"status": "success", "output": output}))
}

/// Dev: the plugin's own process from the published release, in the engine sandbox.
#[cfg(not(feature = "stable-abilities"))]
async fn run(
    plugin: &RequestsPlugin,
    _id: &str,
    host: &Host<'_>,
    context: &Value,
) -> Result<Value> {
    spawn(plugin, host, context).await
}

/// One call in the plugin's own process from the published release, in the engine sandbox.
async fn spawn(plugin: &RequestsPlugin, host: &Host<'_>, context: &Value) -> Result<Value> {
    use std::os::unix::process::CommandExt as _;
    let executable =
        plugin.executable(&webagent_configuration::paths::app().join("assets/artifacts"))?;
    let sandbox = crate::engine_sandbox::prepare(&executable)?;
    let mut command = std::process::Command::new(&executable);
    // SAFETY: `prepare` built every rule in the parent; the closure only applies that
    // Landlock/seccomp/descriptor fence and allocates nothing between fork and exec.
    unsafe {
        command.pre_exec(move || sandbox.apply());
    }
    let mut child = tokio::process::Command::from(command)
        .current_dir("/")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let input = child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("Plugin input unavailable"))?;
    let output = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("Plugin output unavailable"))?;
    let tool = host.call.name;
    let result = webagent_extensions::plugin::drive(
        tokio::io::BufReader::new(output),
        input,
        tool,
        host.call.args,
        context,
        |request| host.answer(request),
    )
    .await;
    let _ = child.kill().await;
    let _ = child.wait().await;
    result.map_err(|error| anyhow!("{error}"))
}

/// Stable: linked plugins run in this binary; the rest (the optional Administrator pack)
/// run as in dev.
#[cfg(feature = "stable-abilities")]
async fn run(plugin: &RequestsPlugin, id: &str, host: &Host<'_>, context: &Value) -> Result<Value> {
    let now_ms = context["now_ms"].as_u64().unwrap_or_default();
    match id {
        "agent_management" => {
            let call = webagent_agent_management::Call {
                tool: host.call.name,
                arguments: host.call.args,
                request_id: host.request_id,
                now_ms,
            };
            webagent_agent_management::call(&Linked(host), call)
                .await
                .map_err(|error| anyhow!(error))
        }
        "web_search" => {
            let call = webagent_web_search::Call {
                tool: host.call.name,
                arguments: host.call.args,
                request_id: host.request_id,
                now_ms,
            };
            webagent_web_search::call(&Linked(host), call)
                .await
                .map_err(|error| anyhow!(error))
        }
        _ => spawn(plugin, host, context).await,
    }
}

#[cfg(feature = "stable-abilities")]
struct Linked<'a, 'b>(&'a Host<'b>);

#[cfg(feature = "stable-abilities")]
impl webagent_agent_management::Host for Linked<'_, '_> {
    fn request(
        &self,
        name: &str,
        params: Value,
        body: Value,
    ) -> impl std::future::Future<Output = Result<Value, String>> + Send {
        self.0.answer(Request {
            name: name.to_owned(),
            params,
            body,
        })
    }
}

#[cfg(feature = "stable-abilities")]
impl webagent_web_search::Host for Linked<'_, '_> {
    fn request(
        &self,
        name: &str,
        params: Value,
        body: Value,
    ) -> impl std::future::Future<Output = Result<Value, String>> + Send {
        self.0.answer(Request {
            name: name.to_owned(),
            params,
            body,
        })
    }
}

/// HTTPS only, no redirects, bounded time: a declared origin is the only host reached.
fn provider_client() -> Result<&'static reqwest::Client> {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    if let Some(client) = CLIENT.get() {
        return Ok(client);
    }
    let client = reqwest::Client::builder()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(std::time::Duration::from_secs(5))
        .timeout(std::time::Duration::from_secs(20))
        .user_agent(PROVIDER_AGENT)
        .build()?;
    Ok(CLIENT.get_or_init(|| client))
}

/// A provider reply as text, refused past `MAX_PROVIDER_BODY`.
async fn bounded_text(mut response: reqwest::Response) -> Result<String> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len() + chunk.len() > MAX_PROVIDER_BODY {
            bail!("provider_error: the reply is larger than 1 MiB");
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Answers one call's host requests.
struct Host<'a> {
    call: &'a Call<'a>,
    ability: &'a str,
    actor: &'a str,
    request_id: &'a str,
    declared: &'a Declaration,
    providers: &'a Providers,
}

impl Host<'_> {
    async fn answer(&self, request: Request) -> Result<Value, String> {
        let kind = self
            .declared
            .kind(&request.name)
            .map_err(|error| error.to_string())?;
        let result = match kind {
            Kind::Route(method, template) => {
                // manage_session reaches only sessions in the agent's session scope.
                if request.name.starts_with("sessions.")
                    && request.name != "sessions.read"
                    && request.name != "sessions.watch"
                {
                    tools::native::orchestration::in_scope(
                        &tools::native_host::Host(self.call.app),
                        self.call.sid,
                        request.params["session_id"].as_str().unwrap_or(""),
                    )
                    .await
                    .map_err(|error| format!("{error:#}"))?;
                }
                let path =
                    requests::path(template, &request.params).map_err(|error| error.to_string())?;
                let body = object(request.body)?;
                tools::core_agent_management::request(self.call.app, method, path, body).await
            }
            Kind::Skills(action) => self.skills(action, &request.params, &request.body).await,
            Kind::Settings => ability_controls(self.call.app, self.call.sid)
                .await
                .map(|(_, _, visibility, _)| visibility["settings"][self.ability].clone()),
            Kind::Watch => self.watch(&request.params, &request.body).await,
            Kind::ProviderRequest => self.provider_request(&request.params, &request.body).await,
            Kind::Review(action) => Ok(self.review(action, &request.params, &request.body).await),
        };
        result.map_err(|error| format!("{error:#}"))
    }

    /// Whether the caller holds administrator authority in this session.
    async fn administrator(&self) -> Result<bool> {
        let app = self.call.app;
        let agent = crate::session_agent_id(app, self.call.sid).await?;
        let access =
            shared_chat::live_access(&app.store, self.actor, self.call.sid, &agent).await?;
        Ok(access.admitted() && access.principal.administrator())
    }

    /// A diagnostics or review-log request for this call's own session and run. Every
    /// outcome is a finished tool result; a store failure reads as unavailable.
    async fn review(&self, action: ReviewAction, params: &Value, body: &Value) -> Value {
        let (app, sid, run_id) = (self.call.app, self.call.sid, self.call.run_id);
        let unavailable = |output: &str| json!({"status":"error","error_kind":"diagnostics_unavailable","output":output});
        let denied =
            |output: &str| json!({"status":"denied","error_kind":"denied","output":output});
        match action {
            ReviewAction::Progress => {
                let run = match requests::diagnostics_target(&params["run_id"], run_id) {
                    Some(target) => app.run(target).await.ok(),
                    None => None,
                };
                match run.filter(|run| run["session_id"].as_str() == Some(sid)) {
                    Some(run) => {
                        json!({"status":"success","output":crate::task_progress::project(&run)})
                    }
                    None => unavailable(
                        "Diagnostics unavailable in this session; omit run_id or use current",
                    ),
                }
            }
            ReviewAction::Issues => {
                let whole = body["scope"].as_str() == Some("all") || body["user_id"].is_string();
                let view = if !whole {
                    app.store.issues(Some(sid), 0).await
                } else {
                    match self.administrator().await {
                        Ok(true) => {
                            app.store
                                .issues_all(
                                    body["user_id"].as_str(),
                                    body["since"].as_i64().unwrap_or(0),
                                    body["include_resolved"] == true,
                                )
                                .await
                        }
                        Ok(false) => {
                            return denied("Only an administrator reads the whole review log")
                        }
                        Err(error) => Err(error),
                    }
                };
                match view {
                    Ok(issues) => json!({"status":"success","output":issues}),
                    Err(_) => unavailable("Issue groups are unavailable for this session"),
                }
            }
            ReviewAction::Note | ReviewAction::Open | ReviewAction::Resolve => {
                if !self.administrator().await.unwrap_or(false) {
                    return denied("agent_review is administrator-only");
                }
                let (result, failed) = match action {
                    ReviewAction::Resolve => (
                        app.store.resolve_issue(sid, body).await,
                        "The resolution could not be recorded",
                    ),
                    ReviewAction::Open => (
                        app.store.open_issue(sid, run_id, body).await,
                        "The review issue could not be recorded",
                    ),
                    _ => (
                        app.store.note_issue(sid, run_id, body).await,
                        "The review note could not be recorded",
                    ),
                };
                result.unwrap_or_else(|_| unavailable(failed))
            }
        }
    }

    /// One call to a provider the plugin declared, made as the caller. The caller's vault
    /// key goes into the declared header and is never returned to the plugin; a monthly
    /// cap the plugin passes is checked against the caller's own counter. Refusals read
    /// `code: message` so the plugin can map them to its error codes.
    async fn provider_request(&self, params: &Value, body: &Value) -> Result<Value> {
        let call = ProviderCall::parse(params, body)
            .map_err(|error| anyhow!("invalid_request: {error}"))?;
        let (provider, endpoint) = self
            .providers
            .endpoint(&call.provider, &call.endpoint)
            .ok_or_else(|| anyhow!("invalid_request: undeclared provider endpoint"))?;
        if call.json.is_some() && endpoint.method != Method::Post {
            bail!("invalid_request: a JSON body needs a POST endpoint");
        }
        let store = self.call.app.store.for_user(self.actor).await?;
        let credential = match &provider.credential {
            Some(credential) => {
                let record = store
                    .get(&format!("core_vault:{}", credential.name))
                    .await?
                    .unwrap_or(Value::Null);
                let key = record["value"]
                    .as_str()
                    .filter(|key| !key.is_empty())
                    .ok_or_else(|| {
                        anyhow!(
                            "credential_missing: no {} credential is saved",
                            credential.name
                        )
                    })?;
                Some((credential, format!("{}{key}", credential.prefix)))
            }
            None => None,
        };
        // Attempts count, not successes; concurrent calls may pass a cap by a few.
        let counter = providers::usage_key(self.ability, &call.provider, &providers::month(now()));
        let used = store
            .get(&counter)
            .await?
            .and_then(|value| value["count"].as_u64())
            .unwrap_or_default();
        if call.monthly_cap > 0 && used >= call.monthly_cap {
            bail!(
                "quota_exhausted: {} reached its monthly cap of {}",
                call.provider,
                call.monthly_cap
            );
        }
        store.put(&counter, &json!({"count": used + 1})).await?;
        let url = format!("{}{}", provider.origin, endpoint.path);
        let client = provider_client()?;
        let mut request = match endpoint.method {
            Method::Get => client.get(&url),
            Method::Post => client.post(&url),
        }
        .query(&call.query)
        .header("accept", "application/json, text/html;q=0.9");
        if let Some(json) = &call.json {
            request = request.json(json);
        }
        if let Some((credential, value)) = &credential {
            request = request.header(credential.header.as_str(), value.as_str());
        }
        let response = tokio::select! {
            _ = self.call.cancel.cancelled() => bail!("tool execution cancelled"),
            sent = request.send() => sent
                .map_err(|_| anyhow!("provider_unreachable: {} could not be reached", call.provider))?,
        };
        let status = response.status().as_u16();
        Ok(json!({"status": status, "body": bounded_text(response).await?}))
    }

    /// The caller's own skills on an agent they administer.
    async fn skills(&self, action: SkillAction, params: &Value, body: &Value) -> Result<Value> {
        let text = |name: &str| {
            params[name]
                .as_str()
                .filter(|value| requests::valid_id(value))
        };
        let agent = text("agent_id").ok_or_else(|| anyhow!("Invalid agent"))?;
        if params
            .as_object()
            .is_some_and(|params| params.keys().any(|key| key != "agent_id" && key != "name"))
        {
            return Err(anyhow!("Unexpected skill parameter"));
        }
        let app = self.call.app;
        if app.store.agent_authority(self.actor, agent).await?["can_manage"] != true {
            return Err(anyhow!("Agent administration required"));
        }
        let store = app
            .store
            .for_user(self.actor)
            .await?
            .for_agent(agent)
            .await?;
        let name = || text("name").ok_or_else(|| anyhow!("Invalid skill name"));
        match action {
            SkillAction::List => store.list_own_skills().await,
            SkillAction::Read => store
                .load_own_skill(name()?)
                .await?
                .ok_or_else(|| anyhow!("Skill not found")),
            SkillAction::Save | SkillAction::Remove => {
                let revision = body["expected_revision"]
                    .as_i64()
                    .ok_or_else(|| anyhow!("Skill revision required"))?;
                let attempt = digest(&json!([self.actor, agent, self.call.sid, self.call.run_id]));
                store
                    .mutate_own_skill(
                        name()?,
                        body["description"].as_str(),
                        body["instructions"].as_str(),
                        Some("selectable"),
                        revision,
                        action == SkillAction::Remove,
                        self.request_id,
                        &attempt,
                    )
                    .await
            }
        }
    }

    /// Subscribe this session to another of the caller's sessions, after re-authorizing the
    /// tool call and its Automation dependency.
    async fn watch(&self, params: &Value, body: &Value) -> Result<Value> {
        let Call {
            app,
            sid,
            run_id,
            generation,
            pending,
            name,
            args,
            ..
        } = *self.call;
        let target = params["session_id"]
            .as_str()
            .filter(|value| requests::valid_uuid(value))
            .ok_or_else(|| anyhow!("Invalid session"))?;
        let prompt = body["prompt"]
            .as_str()
            .filter(|prompt| !prompt.trim().is_empty() && prompt.len() <= MAX_WATCH_PROMPT)
            .ok_or_else(|| anyhow!("Bounded watch prompt required"))?;
        let expires = body["expires_ms"]
            .as_u64()
            .filter(|value| *value > now() * 1000 && *value <= (now() + MAX_WATCH_SECONDS) * 1000)
            .ok_or_else(|| anyhow!("Watch expiry must be within 30 days"))?;
        let run = tools::core_authority::check(
            app,
            sid,
            run_id,
            generation,
            pending,
            name,
            args,
            &self.call.cancel,
        )
        .await?;
        tools::core_authority::dependency(app, sid, &run, "event_subscribe").await?;
        let subscription = json!({"request_id": self.request_id, "source": "session_settled",
            "filter": {"session_id": target}, "prompt": prompt,
            "destination": {"kind": "existing_chat", "session_id": sid},
            "expires_ms": expires, "daily_cap": 100});
        modules::tool_call(
            app,
            sid,
            run_id,
            "event_subscribe",
            &subscription,
            pending,
            self.call.cancel.clone(),
        )
        .await
    }
}

/// A route body: an object, empty when omitted.
fn object(body: Value) -> Result<Value, String> {
    match body {
        Value::Null => Ok(Value::Object(Map::new())),
        Value::Object(_) => Ok(body),
        _ => Err("Request body must be an object".into()),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[tokio::test]
    async fn host_requests_must_be_declared_and_run_as_the_caller() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let sid = crate::runtime_tests::conversation(&app).await;
        let declared =
            Declaration::parse(&json!({"requests": ["sessions.read", "agents.list"]})).unwrap();
        let args = json!({});
        let call = Call {
            app: &app,
            sid: &sid,
            run_id: "run",
            generation: "generation",
            pending: "pending",
            name: "get_agent",
            args: &args,
            cancel: CancellationToken::new(),
            ability: "Core/agent_management",
        };
        let actor = shared_chat::actor(&app).to_owned();
        let host = Host {
            call: &call,
            ability: "agent_management",
            actor: &actor,
            request_id: "request",
            declared: &declared,
            providers: &Providers::default(),
        };
        let request = |name: &str, params: Value| Request {
            name: name.into(),
            params,
            body: Value::Null,
        };
        let read = host
            .answer(request("sessions.read", json!({"session_id": sid})))
            .await
            .unwrap();
        assert_eq!(read["session"]["id"], sid);
        let undeclared = host
            .answer(request("sessions.list", Value::Null))
            .await
            .unwrap_err();
        assert!(undeclared.contains("Undeclared"), "{undeclared}");
        assert!(host
            .answer(request(
                "sessions.read",
                json!({"session_id": "../accounts"})
            ))
            .await
            .is_err());
        assert!(host
            .answer(request("agents.list", json!({"extra": "x"})))
            .await
            .is_err());
        assert!(host
            .answer(request("accounts.delete", Value::Null))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn review_requests_stay_in_the_calls_session() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let sid = crate::runtime_tests::conversation(&app).await;
        let run_id = crate::runtime_tests::queued(&app, &sid).await;
        let other = crate::runtime_tests::conversation(&app).await;
        let foreign = crate::runtime_tests::queued(&app, &other).await;
        let declared = Declaration::parse(&json!({"requests": [
            "diagnostics.progress", "diagnostics.issues", "review.note"]}))
        .unwrap();
        let args = json!({});
        let call = Call {
            app: &app,
            sid: &sid,
            run_id: &run_id,
            generation: "generation",
            pending: "pending",
            name: "read_diagnostics",
            args: &args,
            cancel: CancellationToken::new(),
            ability: "Administrator/diagnostics",
        };
        let actor = shared_chat::actor(&app).to_owned();
        let host = Host {
            call: &call,
            ability: "diagnostics",
            actor: &actor,
            request_id: "request",
            declared: &declared,
            providers: &Providers::default(),
        };
        let ask = |name: &str, params: Value, body: Value| Request {
            name: name.into(),
            params,
            body,
        };
        let own = host
            .answer(ask(
                "diagnostics.progress",
                json!({"run_id": null}),
                Value::Null,
            ))
            .await
            .unwrap();
        assert_eq!(own["status"], "success", "{own}");
        let current = host
            .answer(ask(
                "diagnostics.progress",
                json!({"run_id": "current"}),
                Value::Null,
            ))
            .await
            .unwrap();
        assert_eq!(current, own);
        for run in [json!("nope"), json!(foreign)] {
            let refused = host
                .answer(ask(
                    "diagnostics.progress",
                    json!({"run_id": run}),
                    Value::Null,
                ))
                .await
                .unwrap();
            assert_eq!(
                refused["error_kind"], "diagnostics_unavailable",
                "{refused}"
            );
        }
        let issues = host
            .answer(ask(
                "diagnostics.issues",
                Value::Null,
                json!({"issues": true}),
            ))
            .await
            .unwrap();
        assert_eq!(issues["status"], "success", "{issues}");
        let administrator = host.administrator().await.unwrap();
        let whole = host
            .answer(ask(
                "diagnostics.issues",
                Value::Null,
                json!({"scope": "all"}),
            ))
            .await
            .unwrap();
        let note = host
            .answer(ask("review.note", Value::Null, json!({"note": ""})))
            .await
            .unwrap();
        if administrator {
            assert_eq!(whole["status"], "success", "{whole}");
            assert_eq!(note["status"], "rejected", "{note}");
        } else {
            assert_eq!(whole["status"], "denied", "{whole}");
            assert_eq!(note["status"], "denied", "{note}");
        }
        assert!(host
            .answer(ask("review.open", Value::Null, Value::Null))
            .await
            .unwrap_err()
            .contains("Undeclared"));
    }

    #[tokio::test]
    async fn provider_requests_add_the_callers_key_and_enforce_the_cap() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let sid = crate::runtime_tests::conversation(&app).await;
        let manifest = json!({"requests": ["web.provider_request"], "providers": {
            "fixture": {"origin": "https://provider.invalid",
                "credential": {"name": "fixture_key", "header": "X-Fixture", "prefix": ""},
                "endpoints": {"search": {"method": "GET", "path": "/search"}}}}});
        let declared = Declaration::parse(&manifest).unwrap();
        let providers = Providers::parse(&manifest, &declared).unwrap();
        let args = json!({});
        let call = Call {
            app: &app,
            sid: &sid,
            run_id: "run",
            generation: "generation",
            pending: "pending",
            name: "web_search",
            args: &args,
            cancel: CancellationToken::new(),
            ability: "Web/web_search",
        };
        let actor = shared_chat::actor(&app).to_owned();
        let host = Host {
            call: &call,
            ability: "web_search",
            actor: &actor,
            request_id: "request",
            declared: &declared,
            providers: &providers,
        };
        let ask = |endpoint: &str, body: Value| Request {
            name: "web.provider_request".into(),
            params: json!({"provider": "fixture", "endpoint": endpoint}),
            body,
        };
        // No saved key: refused before counting or any network call.
        let missing = host
            .answer(ask("search", json!({"monthly_cap": 5})))
            .await
            .unwrap_err();
        assert!(missing.starts_with("credential_missing:"), "{missing}");
        let store = app.store.for_user(&actor).await.unwrap();
        let key = ["fixture", "provider", "value"].join("-");
        store
            .put("core_vault:fixture_key", &json!({"value": key}))
            .await
            .unwrap();
        let counter = providers::usage_key("web_search", "fixture", &providers::month(now()));
        store.put(&counter, &json!({"count": 5})).await.unwrap();
        let capped = host
            .answer(ask("search", json!({"monthly_cap": 5})))
            .await
            .unwrap_err();
        assert!(capped.starts_with("quota_exhausted:"), "{capped}");
        // Under the cap the attempt is counted and made; its failure never echoes the key.
        let failed = host
            .answer(ask(
                "search",
                json!({"monthly_cap": 10, "query": {"q": "x"}}),
            ))
            .await
            .unwrap_err();
        assert!(
            failed.starts_with("provider_unreachable:") && !failed.contains(&key),
            "{failed}"
        );
        assert_eq!(store.get(&counter).await.unwrap().unwrap()["count"], 6);
        for (endpoint, body) in [
            ("extract", json!({})),
            ("search", json!({"json": {"q": "x"}})),
        ] {
            let refused = host.answer(ask(endpoint, body)).await.unwrap_err();
            assert!(refused.starts_with("invalid_request:"), "{refused}");
        }
    }
}
