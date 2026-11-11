//! Session scripts (`script_save`, `script_list`, `script_run`): storage per conversation
//! tree and step dispatch. Pure shape and substitution rules live in
//! `webagent_abilities::scripts`.
//!
//! A tree is the top-level conversation and every sub-agent session spawned from it; the
//! root comes from the spawn record (`core_spawn_origin:{sid}`). Scripts are one sealed
//! user-plane document per root and agent, owned by the root session, so they are erased
//! with it and never visible to another user, agent or tree.
//!
//! `script_run` holds no execution permit itself. Each step is recorded on the run as a
//! nested pending call (`run.script_steps`) and dispatched through the ordinary foreground
//! path, which rechecks live policy, ability gates, session mode and approval and takes the
//! step's own permit. Steps are never human-approved, so a step that would ask is denied.
//!
//! Saved scripts last across conversations: one sealed document per user and agent
//! (`saved_scripts:{agent}`, in the acting user's own store). `script_save` with scope
//! `saved` only records a proposal; the user approves, rejects or deletes saved scripts
//! on the agent's Artifacts page through the routes at the end of this file. A saved
//! script runs exactly like a session script, so its steps meet current policy every run.
use crate::*;
use anyhow::bail;
use axum::Extension;
use webagent_abilities::scripts::{self, Script, Step};
use webagent_abilities_host::AbilitiesStore;

pub fn owns(name: &str) -> bool {
    scripts::TOOLS.contains(&name)
}

pub fn schemas() -> Vec<Value> {
    let step = json!({"type":"object","properties":{
        "tool":{"type":"string","description":"Tool name: a read-only tool, browser_read or browser_input"},
        "arguments":{"type":"object","description":"The tool's arguments, under 16 KiB as JSON; {{param}} inside a string is replaced at run time"}},
        "required":["tool","arguments"],"additionalProperties":false});
    [
        ("script_save", "Save a named sequence of read-only or Browser Control tool calls. Scope session (default): for this conversation and its sub-agents, saved at once; the same name replaces it. Scope saved: propose a reusable script for later conversations with this agent; it is saved only after the user approves it on the Artifacts page. A step that would need approval is refused", json!({
            "name":{"type":"string","pattern":"^[A-Za-z0-9_-]{1,64}$"},
            "scope":{"type":"string","enum":["session","saved"],"description":"session (default) for this work only; saved for a script worth reusing in other conversations"},
            "description":{"type":"string","minLength":1,"maxLength":1000,"description":"One short sentence, about 150 characters: what the script does. Describe each param in its own description"},
            "steps":{"type":"array","minItems":1,"items":step,"description":"1 to the configured step limit (20 by default)"},
            "params":{"type":"array","maxItems":10,"items":{"anyOf":[
                {"type":"object","properties":{
                    "name":{"type":"string","pattern":"^[A-Za-z0-9_]{1,32}$"},
                    "description":{"type":"string","maxLength":1000,"description":"A few words: what text to pass"}},
                    "required":["name"],"additionalProperties":false},
                {"type":"string","pattern":"^[A-Za-z0-9_]{1,32}$"}]},
                "description":"Up to 10 distinct params usable as {{name}} in step argument strings"}}),
            vec!["name","description","steps"]),
        ("script_list", "List this conversation tree's session scripts and this agent's approved saved scripts: scope, name, description and params, plus saved-script proposals awaiting the user's approval", json!({}), vec![]),
        ("script_run", "Run a session or saved script's steps in order as ordinary tool calls; stops at the first failed step and returns every step's result. Without scope, a session script of that name runs before a saved one", json!({
            "name":{"type":"string","pattern":"^[A-Za-z0-9_-]{1,64}$"},
            "scope":{"type":"string","enum":["session","saved"]},
            "args":{"type":"object","additionalProperties":{"type":"string","maxLength":2000},"description":"Text for each declared param, up to 2000 characters each"}}),
            vec!["name"]),
    ]
    .into_iter()
    .map(|(name, description, properties, required)| json!({
        "type":"function","name":name,"description":description,
        "parameters":{"type":"object","properties":properties,"required":required,"additionalProperties":false}
    }))
    .collect()
}

/// The tree's root session: the spawn record's root, else this session.
async fn root(app: &App, sid: &str) -> Result<String> {
    Ok(app
        .store
        .get(&format!("core_spawn_origin:{sid}"))
        .await?
        .and_then(|origin| origin["root_session_id"].as_str().map(str::to_owned))
        .unwrap_or_else(|| sid.to_owned()))
}

/// The tree's document key and its saved scripts; empty once the root is gone.
async fn load(app: &App, sid: &str) -> Result<(String, Value)> {
    let root = root(app, sid).await?;
    let agent = session_agent_id(app, sid).await?;
    let doc_key = format!("session_scripts:{root}:{agent}");
    let empty = json!({"session_id":root,"agent_id":agent,"scripts":{}});
    if app.store.get(&key("session", &root)).await?.is_none() {
        return Ok((doc_key, empty));
    }
    let doc = app.store.get(&doc_key).await?.unwrap_or(empty);
    Ok((doc_key, doc))
}

/// The acting user's store: saved scripts belong to the person, not the session owner.
async fn user_store(app: &App) -> Result<Store> {
    app.store.for_user(shared_chat::actor(app)).await
}

fn saved_key(agent: &str) -> String {
    format!("saved_scripts:{agent}")
}

/// This user's saved scripts for the session's agent.
async fn load_saved(app: &App, sid: &str) -> Result<Value> {
    let agent = session_agent_id(app, sid).await?;
    Ok(user_store(app)
        .await?
        .get(&saved_key(&agent))
        .await?
        .unwrap_or_else(scripts::saved_empty))
}

/// Whether `tool` would run now without approval for this session and run.
async fn runs_without_approval(app: &App, sid: &str, run: &Value, tool: &str) -> Result<bool> {
    let (policy, _, visibility, state) = ability_controls(app, sid).await?;
    let mode = session_mode(app, sid).await?;
    let agent = session_agent_id(app, sid).await?;
    let access = shared_chat::live_access(&app.store, shared_chat::actor(app), sid, &agent).await?;
    let decision = live_tool_decision(
        app,
        &access,
        tool,
        &policy,
        &visibility,
        &state,
        &run["loaded_abilities"],
        mode["mode"] == "read_only",
    )
    .for_execution_mode(&mode);
    Ok(access.admitted() && decision.permission == access::Permission::Auto)
}

/// Why `step` may not be saved, if it may not.
async fn refusal(app: &App, sid: &str, run: &Value, step: &Step) -> Result<Option<String>> {
    if !scripts::step_kind_allowed(&step.tool, tools::is_read(&step.tool)) {
        return Ok(Some(format!(
            "{} is not allowed: scripts may use read-only tools and browser_input only",
            step.tool
        )));
    }
    if !tools::schemas()
        .iter()
        .any(|s| s["name"] == step.tool.as_str())
    {
        return Ok(Some(format!("{} is not a known tool", step.tool)));
    }
    if !runs_without_approval(app, sid, run, &step.tool).await? {
        return Ok(Some(format!(
            "{} would need approval or is not available here; scripts may only use tools that run without asking",
            step.tool
        )));
    }
    Ok(None)
}

async fn save(app: &App, sid: &str, run: &Value, args: &Value) -> Result<Value> {
    let scope = args["scope"].as_str().unwrap_or("session");
    if !matches!(scope, "session" | "saved") {
        return Ok(json!({"status":"error","output":"scope must be session or saved"}));
    }
    let mut fields = args.clone();
    if let Some(object) = fields.as_object_mut() {
        object.remove("scope");
    }
    let script: Script = match serde_json::from_value(fields) {
        Ok(script) => script,
        Err(error) => {
            return Ok(json!({"status":"error","output":format!("Invalid script: {error}")}))
        }
    };
    let limits = loop_runtime::Config::read(&app.runtime_path)?.tools.scripts;
    if let Err(error) = script.validate(limits.max_steps) {
        return Ok(json!({"status":"error","output":format!("Invalid script: {error}")}));
    }
    for (index, step) in script.steps.iter().enumerate() {
        if let Some(reason) = refusal(app, sid, run, step).await? {
            return Ok(json!({"status":"denied","output":format!("Step {}: {reason}", index + 1)}));
        }
    }
    if scope == "saved" {
        return propose_saved(app, sid, &script, limits.max_saved).await;
    }
    let _guard = app.gate.lock().await;
    let (doc_key, mut doc) = load(app, sid).await?;
    let scripts = doc["scripts"]
        .as_object_mut()
        .expect("script documents hold an object");
    if !scripts.contains_key(&script.name) && scripts.len() >= limits.max_per_tree {
        return Ok(json!({"status":"denied","output":format!(
            "This conversation already has {} scripts; replace one by saving under its name",
            limits.max_per_tree)}));
    }
    let name = script.name.clone();
    scripts.insert(name.clone(), serde_json::to_value(&script)?);
    app.store.put(&doc_key, &doc).await?;
    Ok(json!({"status":"success","output":{"saved":name,"steps":script.steps.len()}}))
}

/// Records `script` as a saved-script proposal for the user to decide on the Artifacts page.
async fn propose_saved(app: &App, sid: &str, script: &Script, max_saved: usize) -> Result<Value> {
    let agent = session_agent_id(app, sid).await?;
    let id = uuid();
    let now = now();
    let mut change = |doc: &mut Value| {
        if doc.is_null() {
            *doc = scripts::saved_empty();
        }
        scripts::propose(doc, script, &id, sid, now, max_saved).map(|()| Value::Null)
    };
    if let Err(error) = user_store(app)
        .await?
        .saved_scripts_change(&agent, None, &mut change)
        .await
    {
        return Ok(json!({"status":"denied","output":error.to_string()}));
    }
    Ok(
        json!({"status":"success","output":{"proposed":script.name,"proposal_id":id,
        "saved":false,"awaiting_approval":true,
        "review":format!("/app?tab=agents&agent={agent}&agentView=artifacts"),
        "next":"Tell the user it awaits their approval on the agent's Artifacts page; until then use a session script"}}),
    )
}

/// One listed script: description cut to a line, params with short descriptions.
fn summary(scope: &str, name: &str, s: &Value) -> Value {
    let params: Vec<Value> = s["params"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|p| match p {
            // Scripts saved before params had descriptions store bare names.
            Value::String(name) => json!({"name":name}),
            p => json!({"name":p["name"],"description":brief(&p["description"])}),
        })
        .collect();
    json!({"scope":scope,"name":name,"description":brief(&s["description"]),"params":params})
}

async fn list(app: &App, sid: &str) -> Result<Value> {
    let (_, doc) = load(app, sid).await?;
    let saved = load_saved(app, sid).await?;
    let session = doc["scripts"].as_object().into_iter().flatten();
    let kept = saved["scripts"].as_object().into_iter().flatten();
    let scripts: Vec<Value> = session
        .map(|(name, s)| summary("session", name, s))
        .chain(kept.map(|(name, s)| summary("saved", name, &s["script"])))
        .collect();
    let pending: Vec<&Value> = saved["proposals"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(_, p)| &p["script"]["name"])
        .collect();
    Ok(json!({"status":"success","output":{"scripts":scripts,"awaiting_approval":pending}}))
}

/// A listed description, cut to about one line.
fn brief(text: &Value) -> String {
    const LIST_CHARS: usize = 160;
    let text = text.as_str().unwrap_or_default();
    if text.chars().count() <= LIST_CHARS {
        return text.to_owned();
    }
    let cut: String = text.chars().take(LIST_CHARS - 1).collect();
    format!("{}\u{2026}", cut.trim_end())
}

/// Record `step` as the run's nested pending call, or remove it (`None`).
async fn set_step(app: &App, rid: &str, step: Option<Value>) -> Result<()> {
    let _guard = app.gate.lock().await;
    let mut run = app.run(rid).await?;
    match step {
        Some(item) => run["script_steps"] = json!([item]),
        None => {
            if let Some(object) = run.as_object_mut() {
                object.remove("script_steps");
            }
        }
    }
    app.save(&mut run).await
}

async fn run_script(
    app: &App,
    call: tools::native::Call<'_>,
    run: &Value,
    args: &Value,
    cancel: CancellationToken,
) -> Result<Value> {
    let name = args["name"].as_str().unwrap_or("");
    let scope = args["scope"].as_str();
    let session = match scope {
        Some("saved") => Value::Null,
        _ => load(app, call.sid).await?.1["scripts"][name].clone(),
    };
    let found = if session.is_null() && scope != Some("session") {
        load_saved(app, call.sid).await?["scripts"][name]["script"].clone()
    } else {
        session
    };
    let Ok(script) = serde_json::from_value::<Script>(found) else {
        return Ok(
            json!({"status":"error","output":format!("No script named {name}; script_list shows this conversation's and this agent's scripts")}),
        );
    };
    let empty = serde_json::Map::new();
    let steps = match script.bind(args["args"].as_object().unwrap_or(&empty)) {
        Ok(steps) => steps,
        Err(error) => return Ok(json!({"status":"error","output":error.to_string()})),
    };
    // The step's pending call carries the script_run call's revisions, so a policy, mode or
    // ability change since the script started denies the step. It is never approved.
    let pending = &run["pending"];
    let mut results = Vec::new();
    let mut artifacts = Vec::new();
    let mut status = "success";
    for (index, step) in steps.iter().enumerate() {
        if !scripts::step_kind_allowed(&step.tool, tools::is_read(&step.tool)) {
            results.push(
                json!({"tool":step.tool,"arguments":step.arguments,"status":"denied",
                "result":"Not allowed in a script"}),
            );
            status = "error";
            break;
        }
        let step_id = uuid();
        let item = json!({"state":"executing","pending":{
            "id":step_id,"name":step.tool,"arguments":step.arguments,"state":"executing",
            "script_run":call.pending_id,"human_approved":false,
            "policy_version":pending["policy_version"],"mode_revision":pending["mode_revision"],
            "visibility_revision":pending["visibility_revision"],
            "ability_state_revision":pending["ability_state_revision"],
            "access_revision":pending["access_revision"]}});
        set_step(app, call.run_id, Some(item)).await?;
        let outcome = tools::call_foreground(
            app.clone(),
            call.sid.to_owned(),
            call.run_id.to_owned(),
            call.generation.to_owned(),
            step.tool.clone(),
            step.arguments.clone(),
            step_id,
            pending["policy_version"].as_u64().unwrap_or(0),
            cancel.clone(),
        )
        .await;
        set_step(app, call.run_id, None).await?;
        let content = match outcome {
            Ok(outcome) => outcome.content,
            Err(error) => json!({"status":"error","output":error.to_string()}),
        };
        if content["attachment"].is_object() {
            artifacts.push(json!({"step":index + 1,"attachment":content["attachment"]}));
        }
        for artifact in content["artifacts"].as_array().into_iter().flatten() {
            if artifact["attachment"].is_object() {
                artifacts.push(json!({"step":index + 1,"attachment":artifact["attachment"]}));
            }
        }
        let ok = content["status"] == "success";
        results.push(json!({"tool":step.tool,"arguments":step.arguments,"result":content}));
        if !ok {
            status = "error";
            break;
        }
        if cancel.is_cancelled() {
            bail!("tool execution cancelled");
        }
    }
    let mut output =
        json!({"script":name,"steps_run":results.len(),"steps_total":steps.len(),"steps":results});
    if status != "success" {
        output["stopped_at_step"] = json!(results.len());
    }
    Ok(json!({"status":status,"output":output,"artifacts":artifacts}))
}

pub async fn execute(
    app: &App,
    call: tools::native::Call<'_>,
    name: &str,
    args: &Value,
    cancel: CancellationToken,
) -> Result<Value> {
    let run = tools::core_authority::check(
        app,
        call.sid,
        call.run_id,
        call.generation,
        call.pending_id,
        name,
        args,
        &cancel,
    )
    .await?;
    match name {
        "script_save" => save(app, call.sid, &run, args).await,
        "script_list" => list(app, call.sid).await,
        "script_run" => Box::pin(run_script(app, call, &run, args, cancel)).await,
        _ => bail!("Unknown script tool"),
    }
}

/// The caller's own store, once they may use `agent` at all.
async fn page_store(
    app: &App,
    identity: &Value,
    agent: &str,
) -> std::result::Result<Store, ApiError> {
    let actor = identity["_user_id"]
        .as_str()
        .ok_or_else(|| bad("Caller required"))?;
    let rights = app
        .store
        .agent_authority(actor, agent)
        .await
        .map_err(|_| ApiError(StatusCode::NOT_FOUND, "Agent not found".into()))?;
    if rights["can_manage"] != true
        && !app
            .store
            .live_access(actor, agent)
            .await
            .is_ok_and(|a| a.admitted())
    {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "Agent access unavailable".into(),
        ));
    }
    Ok(app.store.for_user(actor).await?)
}

/// GET: the caller's saved scripts and pending proposals for one agent.
pub async fn page_read(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path(agent): Path<String>,
) -> Api {
    let store = page_store(&app, &identity, &agent).await?;
    let doc = store
        .get(&saved_key(&agent))
        .await?
        .unwrap_or_else(scripts::saved_empty);
    let limits = loop_runtime::Config::read(&app.runtime_path)?.tools.scripts;
    Ok(Json(
        json!({"agent_id":agent,"revision":doc["revision"],"scripts":doc["scripts"],
        "proposals":doc["proposals"],"max_saved":limits.max_saved}),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Decision {
    expected_revision: u64,
    approve: bool,
}

/// POST: approve (save exactly as proposed) or reject one pending proposal.
pub async fn page_decide(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path((agent, id)): Path<(String, String)>,
    Json(body): Json<Decision>,
) -> Api {
    let store = page_store(&app, &identity, &agent).await?;
    let limits = loop_runtime::Config::read(&app.runtime_path)?.tools.scripts;
    let now = now();
    let mut change = |doc: &mut Value| {
        if doc.is_null() {
            *doc = scripts::saved_empty();
        }
        scripts::decide(
            doc,
            &id,
            body.approve,
            now,
            limits.max_saved,
            limits.max_steps,
        )
    };
    store
        .saved_scripts_change(&agent, Some(body.expected_revision), &mut change)
        .await
        .map(Json)
        .map_err(|error| conflict(&error.to_string()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expected {
    expected_revision: u64,
}

/// DELETE: remove one saved script.
pub async fn page_delete(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path((agent, name)): Path<(String, String)>,
    Json(body): Json<Expected>,
) -> Api {
    let store = page_store(&app, &identity, &agent).await?;
    let mut change = |doc: &mut Value| {
        if doc.is_null() {
            *doc = scripts::saved_empty();
        }
        scripts::delete_saved(doc, &name).map(|()| json!({"status":"deleted"}))
    };
    store
        .saved_scripts_change(&agent, Some(body.expected_revision), &mut change)
        .await
        .map(Json)
        .map_err(|error| conflict(&error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A running run with an approved script tool call, as the run loop would leave it. The
    /// approval covers only that call; its steps are never approved.
    async fn running(app: &App, sid: &str, name: &str, args: &Value) -> (String, String, String) {
        let rid = crate::runtime_tests::queued(app, sid).await;
        let mut run = app.run(&rid).await.unwrap();
        let generation = uuid();
        assert!(app
            .store
            .claim_run(&mut run, "scripts-test", &generation)
            .await
            .unwrap());
        let (policy, mode_revision, visibility, state) = ability_controls(app, sid).await.unwrap();
        let agent = session_agent_id(app, sid).await.unwrap();
        let access = shared_chat::live_access(&app.store, shared_chat::actor(app), sid, &agent)
            .await
            .unwrap();
        let pending = uuid();
        run["pending"] = json!({"id":pending,"name":name,"arguments":args,"state":"executing",
            "human_approved":true,"policy_version":policy["version"],"mode_revision":mode_revision,
            "visibility_revision":visibility["revision"],"ability_state_revision":state["revision"],
            "access_revision":access.revision});
        app.store.put(&key("run", &rid), &run).await.unwrap();
        (rid, generation, pending)
    }

    async fn tool(app: &App, sid: &str, name: &str, args: Value) -> Value {
        let (rid, generation, pending) = running(app, sid, name, &args).await;
        let call = tools::native::Call {
            sid,
            run_id: &rid,
            generation: &generation,
            pending_id: &pending,
        };
        let result = execute(app, call, name, &args, CancellationToken::new())
            .await
            .unwrap();
        let mut run = app.run(&rid).await.unwrap();
        assert!(
            run.get("script_steps").is_none(),
            "nested step left on the run"
        );
        // End the run so the next call can start one in the same conversation.
        run["status"] = json!("completed");
        run["pending"] = Value::Null;
        app.save(&mut run).await.unwrap();
        result
    }

    /// The root fixture with Base granted and visible, as an operator's agent has it.
    async fn fixture() -> (tempfile::TempDir, App) {
        let (root, app) = crate::runtime_tests::fixture().await;
        let mut access_policy = app.store.read_access_policy().unwrap();
        access_policy["tiers"]["operator"]["ability_permissions"]["base"] = json!("auto");
        app.store.write_access_policy(&access_policy).unwrap();
        let levels = app.store.capability_levels("shared_default").await.unwrap();
        let mut level = levels.levels["operator"].clone();
        level
            .ability_permissions
            .insert("base".into(), access::Permission::Auto);
        app.store
            .save_capability_level("shared_default", level, levels.revision)
            .await
            .unwrap();
        let mut visibility = app.store.agent_abilities().await.unwrap();
        let revision = visibility["revision"].as_u64().unwrap();
        visibility["visibility"]["base"] = json!("visible");
        // Tools are discoverable by default; these tests run them as if already loaded.
        for tool in [
            "script_save",
            "script_list",
            "script_run",
            "calculate",
            "get_time",
        ] {
            visibility["tool_modes"][tool] = json!("visible");
        }
        visibility["revision"] = json!(revision + 1);
        app.store
            .save_agent_abilities(&visibility, revision)
            .await
            .unwrap();
        (root, app)
    }

    async fn session(app: &App) -> String {
        let sid = crate::runtime_tests::conversation(app).await;
        app.store
            .put(
                &format!("session_mode:{sid}"),
                &json!({"mode":"write","execution_mode":"auto","revision":0}),
            )
            .await
            .unwrap();
        sid
    }

    fn add() -> Value {
        json!({"name":"sum","description":"Add then fail","params":["x"],"steps":[
            {"tool":"calculate","arguments":{"operation":"add","operands":[1,2]}},
            {"tool":"get_time","arguments":{"timezone":"{{x}}"}},
            {"tool":"calculate","arguments":{"operation":"add","operands":[3,4]}}]})
    }

    #[test]
    fn save_schema_declares_the_limits() {
        let save = schemas()
            .into_iter()
            .find(|s| s["name"] == "script_save")
            .unwrap();
        let props = &save["parameters"]["properties"];
        assert_eq!(props["description"]["maxLength"], 1000);
        assert_eq!(props["params"]["maxItems"], 10);
        let run = schemas()
            .into_iter()
            .find(|s| s["name"] == "script_run")
            .unwrap();
        assert_eq!(
            run["parameters"]["properties"]["args"]["additionalProperties"]["maxLength"],
            2000
        );
    }

    #[test]
    fn list_descriptions_are_cut_to_one_line() {
        assert_eq!(brief(&json!("Short")), "Short");
        let long = brief(&json!("x".repeat(400)));
        assert_eq!(long.chars().count(), 160);
        assert!(long.ends_with('\u{2026}'));
    }

    #[tokio::test]
    async fn save_list_run_and_stop_at_first_failure() {
        let (_root, app) = fixture().await;
        let sid = session(&app).await;
        let saved = tool(&app, &sid, "script_save", add()).await;
        assert_eq!(saved["status"], "success", "{saved}");
        let listed = tool(&app, &sid, "script_list", json!({})).await;
        assert_eq!(listed["output"]["scripts"][0]["name"], "sum");
        assert_eq!(
            listed["output"]["scripts"][0]["params"],
            json!([{"name":"x","description":""}])
        );
        // An unknown time zone fails step 2; step 3 never runs.
        let run = tool(
            &app,
            &sid,
            "script_run",
            json!({"name":"sum","args":{"x":"Nowhere/None"}}),
        )
        .await;
        assert_eq!(run["status"], "error", "{run}");
        assert_eq!(run["output"]["steps_run"], 2);
        assert_eq!(run["output"]["stopped_at_step"], 2);
        assert_eq!(run["output"]["steps"][0]["result"]["status"], "success");
        assert_eq!(
            run["output"]["steps"][1]["arguments"]["timezone"],
            "Nowhere/None"
        );
        let run = tool(
            &app,
            &sid,
            "script_run",
            json!({"name":"sum","args":{"x":"UTC"}}),
        )
        .await;
        assert_eq!(run["status"], "success", "{run}");
        assert_eq!(run["output"]["steps_run"], 3);
        let missing = tool(&app, &sid, "script_run", json!({"name":"sum"})).await;
        assert_eq!(missing["status"], "error");
    }

    #[tokio::test]
    async fn save_refuses_writes_and_steps_that_would_ask() {
        let (_root, app) = fixture().await;
        let sid = session(&app).await;
        let write = json!({"name":"w","description":"Write","steps":[
            {"tool":"file_write","arguments":{"path":"x","content":"y"}}]});
        let result = tool(&app, &sid, "script_save", write).await;
        assert_eq!(result["status"], "denied", "{result}");
        // In the read-only Ask mode a tool the tier sets to ask would ask, so it is refused.
        let mut access_policy = app.store.read_access_policy().unwrap();
        access_policy["tiers"]["operator"]["tools"]["calculate"] = json!("ask");
        app.store.write_access_policy(&access_policy).unwrap();
        app.store
            .put(
                &format!("session_mode:{sid}"),
                &json!({"mode":"read_only","execution_mode":"ask","revision":1}),
            )
            .await
            .unwrap();
        let result = tool(&app, &sid, "script_save", add()).await;
        assert_eq!(result["status"], "denied", "{result}");
        let nested = json!({"name":"n","description":"Nested","steps":[
            {"tool":"script_run","arguments":{"name":"n"}}]});
        assert_eq!(
            tool(&app, &sid, "script_save", nested).await["status"],
            "denied"
        );
    }

    #[tokio::test]
    async fn saved_scripts_wait_for_approval_then_run_in_new_conversations() {
        let (_root, app) = fixture().await;
        let sid = session(&app).await;
        let mut proposal = add();
        proposal["scope"] = json!("saved");
        let proposed = tool(&app, &sid, "script_save", proposal).await;
        assert_eq!(proposed["output"]["awaiting_approval"], true, "{proposed}");
        let later = session(&app).await;
        let listed = tool(&app, &later, "script_list", json!({})).await;
        assert_eq!(
            listed["output"]["scripts"],
            json!([]),
            "a proposal is not saved"
        );
        assert_eq!(listed["output"]["awaiting_approval"], json!(["sum"]));
        let id = proposed["output"]["proposal_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let mut approve = |doc: &mut Value| scripts::decide(doc, &id, true, 1, 50, 20);
        app.store
            .saved_scripts_change("shared_default", Some(1), &mut approve)
            .await
            .unwrap();
        let listed = tool(&app, &later, "script_list", json!({})).await;
        assert_eq!(listed["output"]["scripts"][0]["scope"], "saved");
        let args = json!({"name":"sum","args":{"x":"UTC"}});
        let ran = tool(&app, &later, "script_run", args.clone()).await;
        assert_eq!(ran["status"], "success", "{ran}");
        let mut session_only = args;
        session_only["scope"] = json!("session");
        assert_eq!(
            tool(&app, &later, "script_run", session_only).await["status"],
            "error"
        );
        // Saved-scope proposals meet the same step rules as session scripts.
        let write = json!({"name":"w","description":"Write","scope":"saved","steps":[
            {"tool":"file_write","arguments":{"path":"x","content":"y"}}]});
        assert_eq!(
            tool(&app, &sid, "script_save", write).await["status"],
            "denied"
        );
    }

    #[tokio::test]
    async fn scripts_are_shared_within_a_tree_only() {
        let (_root, app) = fixture().await;
        let parent = session(&app).await;
        let child = session(&app).await;
        let sibling = session(&app).await;
        let other = session(&app).await;
        for sub in [&child, &sibling] {
            app.store
                .put(
                    &format!("core_spawn_origin:{sub}"),
                    &json!({"parent_session_id":parent,"root_session_id":parent,"depth":1}),
                )
                .await
                .unwrap();
        }
        assert_eq!(
            tool(&app, &child, "script_save", add()).await["status"],
            "success"
        );
        let seen = tool(&app, &sibling, "script_list", json!({})).await;
        assert_eq!(seen["output"]["scripts"][0]["name"], "sum");
        let seen = tool(&app, &parent, "script_list", json!({})).await;
        assert_eq!(seen["output"]["scripts"][0]["name"], "sum");
        let unseen = tool(&app, &other, "script_list", json!({})).await;
        assert_eq!(unseen["output"]["scripts"], json!([]));
        let run = tool(
            &app,
            &other,
            "script_run",
            json!({"name":"sum","args":{"x":"UTC"}}),
        )
        .await;
        assert_eq!(run["status"], "error");
    }
}
