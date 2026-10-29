//! Ability plugins that call back into the app through declared host requests
//! (`requests_v1`, see `webagent_extensions::requests`). Dev builds run each call in the
//! plugin's own process from the published release, under the engine sandbox; stable
//! builds (`stable-abilities`) link the plugin. Either way a host request must be declared
//! in the plugin's `plugin.json`, and it runs as the chatting user with that user's
//! permissions.
use crate::{ability_controls, digest, modules, now, shared_chat, tools, App};
use anyhow::{anyhow, Result};
use serde_json::{json, Map, Value};
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;
use webagent_abilities::package::RequestsPlugin;
use webagent_extensions::plugin::Request;
use webagent_extensions::requests::{self, Declaration, Kind, SkillAction};

/// Longest a plugin call may run, host requests included.
const CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
const MAX_WATCH_PROMPT: usize = 16_000;
const MAX_WATCH_SECONDS: u64 = 30 * 86_400;

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
        .map(|root| root.join("plugins/abilities"))
        .unwrap_or_else(|| {
            PathBuf::from(
                std::env::var("V3_ABILITIES_ROOT")
                    .unwrap_or_else(|_| "/opt/v3/plugins/abilities".into()),
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
    };
    let output = tokio::time::timeout(CALL_TIMEOUT, run(&plugin, id, &host, &context))
        .await
        .map_err(|_| anyhow!("Plugin call timed out"))??;
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
    use std::os::unix::process::CommandExt as _;
    let executable = plugin.executable(std::path::Path::new("/opt/v3/assets/artifacts"))?;
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

/// Stable: the plugin is linked into this binary.
#[cfg(feature = "stable-abilities")]
async fn run(
    _plugin: &RequestsPlugin,
    id: &str,
    host: &Host<'_>,
    context: &Value,
) -> Result<Value> {
    let call = webagent_agent_management::Call {
        tool: host.call.name,
        arguments: host.call.args,
        request_id: host.request_id,
        now_ms: context["now_ms"].as_u64().unwrap_or_default(),
    };
    match id {
        "agent_management" => webagent_agent_management::call(&Linked(host), call)
            .await
            .map_err(|error| anyhow!(error)),
        _ => Err(anyhow!("Plugin {id} is not linked into this build")),
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

/// Answers one call's host requests.
struct Host<'a> {
    call: &'a Call<'a>,
    ability: &'a str,
    actor: &'a str,
    request_id: &'a str,
    declared: &'a Declaration,
}

impl Host<'_> {
    async fn answer(&self, request: Request) -> Result<Value, String> {
        let kind = self
            .declared
            .kind(&request.name)
            .map_err(|error| error.to_string())?;
        let result = match kind {
            Kind::Route(method, template) => {
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
        };
        result.map_err(|error| format!("{error:#}"))
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
}
