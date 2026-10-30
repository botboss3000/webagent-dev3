//! Explicit command preparation never grants execution; live dispatch remains authoritative.
use crate::{ability_controls, live_tool_decision, session_agent_id, session_mode, App};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

fn user_text(run: &Value) -> String {
    run["input"]
        .as_array()
        .into_iter()
        .flatten()
        .rev()
        .find(|item| item["role"] == "user")
        .map(|item| {
            item["content"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    item["content"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|part| part["text"].as_str())
                        .collect::<Vec<_>>()
                        .join("\n")
                })
        })
        .unwrap_or_default()
}

/// Test prospective activation under the real live resolver, instead of asking
/// whether an inactive discoverable ability is already callable.
async fn prospective(
    app: &App,
    sid: &str,
    run: &Value,
    dependencies: &[&str],
) -> Result<Option<(Value, Value)>> {
    let agent = session_agent_id(app, sid).await?;
    if run["session_id"] != sid || run["agent_id"].as_str().unwrap_or("shared_default") != agent {
        bail!("Optimization session changed");
    }
    let access = app
        .store
        .live_access(app.store.current_user_id(), &agent)
        .await?;
    if !access.admitted() {
        return Ok(None);
    }
    let (policy, _, visibility, state) = ability_controls(app, sid).await?;
    let mode = session_mode(app, sid).await?;
    let mut trial_state = state.clone();
    let mut active = state["active"]
        .as_array()
        .cloned()
        .context("Invalid session ability state")?;
    let mut loaded = run["loaded_abilities"]
        .as_array()
        .cloned()
        .context("Invalid run ability state")?;
    for name in dependencies {
        let owners =
            app.abilities
                .tool_abilities(name, &visibility, &state, &run["loaded_abilities"]);
        if owners.is_empty() {
            return Ok(None);
        }
        for id in owners {
            if visibility["visibility"][&id] == "off"
                || state["suppressed"]
                    .as_array()
                    .is_some_and(|items| items.contains(&json!(id)))
            {
                return Ok(None);
            }
            if !active.contains(&json!(id)) {
                active.push(json!(id));
            }
        }
        let marker = json!(crate::tools::native::tool_search::marker(name));
        if !loaded.contains(&marker) {
            loaded.push(marker);
        }
    }
    trial_state["active"] = json!(active);
    for name in dependencies {
        let decision = live_tool_decision(
            app,
            &access,
            name,
            &policy,
            &visibility,
            &trial_state,
            &json!(loaded),
            mode["mode"] == "read_only",
        )
        .for_execution_mode(&mode);
        if decision.permission == crate::access::Permission::Deny {
            return Ok(None);
        }
    }
    Ok(Some((trial_state, json!(loaded))))
}

const EXPERIMENT_GUIDANCE: &str = "This run is an explicit /optimize software resource experiment. Use an isolated broker task and performance_work: measure the original representative workload, obtain supported CPU/allocation profiles, make only evidence-backed software changes, and run full baseline/candidate correctness and configured coverage. Compare candidates to the original and current best, respecting regression limits and every effective budget. Report unsupported tooling, exhaustion and host contention honestly. Do not change prompts, skills, instructions, model behavior or tool-selection workflows. Dependency changes require effective allow_dependencies and live policy. Finish with receipts, measurements and a candidate for user review; never release or deploy it automatically.";

/// Caller holds the dispatch gate, serializing activation with session controls.
/// The request settings are pinned once; preferences and command overrides remain separate.
pub(crate) async fn prepare(app: &App, sid: &str, run: &mut Value) -> Result<()> {
    if run["optimization_checked"] != true {
        let (_, _, visibility, _) = ability_controls(app, sid).await?;
        let preferences = match visibility["settings"].get("optimization") {
            Some(value) => serde_json::from_value(value.clone()),
            None => Ok(crate::optimization_settings::OptimizationSettings::default()),
        };
        let input = user_text(run);
        // Ordinary requests do not depend on optimization settings validity.
        if !input.trim_start().starts_with("/optimize")
            || input
                .trim_start()
                .strip_prefix("/optimize")
                .is_some_and(|rest| !rest.is_empty() && !rest.starts_with(char::is_whitespace))
        {
            run["optimization_checked"] = json!(true);
            return Ok(());
        }
        let parsed = preferences
            .map_err(|_| "Invalid saved Optimization settings".to_owned())
            .and_then(|settings| crate::optimization_settings::parse_optimize(&input, &settings));
        run["optimization_checked"] = json!(true);
        match parsed {
            Ok(Some(request)) => {
                run["optimization_request"] = json!({"settings": request.settings, "prompt": request.prompt,
                    "status":"pending","guidance":EXPERIMENT_GUIDANCE});
            }
            Ok(None) => return Ok(()),
            Err(error) => {
                run["optimization_request"] = json!({"status":"invalid","guidance":format!("The explicit /optimize request was rejected: {error}. Report this validation failure; do not run an optimization experiment.")});
                return Ok(());
            }
        }
    }
    if run["optimization_request"].is_null() || run["optimization_request"]["status"] == "invalid" {
        return Ok(());
    }
    let generation = crate::optimization::generation();
    let Some((mut state, loaded)) =
        prospective(app, sid, run, &["performance_work", "file_edit"]).await?
    else {
        run["optimization_request"]["status"] = json!("unavailable");
        run["optimization_request"]["guidance"] = json!("The explicit /optimize request cannot proceed under current plugin, ability, tool, session-mode or permission controls. Report unavailable access and stop; do not bypass it or perform the experiment.");
        return Ok(());
    };
    if generation.is_none() || crate::optimization::generation() != generation {
        run["optimization_request"]["status"] = json!("unavailable");
        run["optimization_request"]["guidance"] = json!(
            "Optimization plugin changed during admission. Report unavailable access and stop."
        );
        return Ok(());
    }
    // Commit no activation until every dependency has passed prospective checks.
    let previous = ability_controls(app, sid).await?.3;
    if state["active"] != previous["active"] {
        state["revision"] = json!(
            previous["revision"]
                .as_u64()
                .context("Invalid ability revision")?
                + 1
        );
        app.store
            .put(&crate::key("session_ability_state", sid), &state)
            .await?;
    }
    run["loaded_abilities"] = loaded;
    run["optimization_request"]["status"] = json!("ready");
    let settings = serde_json::to_string(&run["optimization_request"]["settings"])?;
    run["optimization_request"]["guidance"] = json!(format!(
        "{EXPERIMENT_GUIDANCE}\nEffective pinned Optimization settings: {settings}"
    ));
    Ok(())
}

pub(crate) fn guidance(run: &Value) -> &str {
    run["optimization_request"]["guidance"]
        .as_str()
        .unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn fixture() -> (tempfile::TempDir, App, String, Value) {
        let (root, app) = crate::runtime_tests::fixture().await;
        let sid = crate::runtime_tests::conversation(&app).await;
        let id = crate::runtime_tests::queued(&app, &sid).await;
        let run = app.run(&id).await.unwrap();
        let mut settings = app.store.agent_abilities().await.unwrap();
        let revision = settings["revision"].as_u64().unwrap();
        settings["visibility"]["codebase_admin"] = json!("discoverable");
        settings["tool_modes"]["file_edit"] = json!("discoverable");
        settings["revision"] = json!(revision + 1);
        app.store
            .save_agent_abilities(&settings, revision)
            .await
            .unwrap();
        app.store
            .put(
                "policy",
                &json!({"version":1,"rules":{"file_edit":"require_approval"}}),
            )
            .await
            .unwrap();
        app.store
            .put(
                &crate::key("session_mode", &sid),
                &json!({"mode":"write","revision":1}),
            )
            .await
            .unwrap();
        (root, app, sid, run)
    }

    #[tokio::test]
    async fn discoverable_inactive_dependency_passes_prospective_checks_without_side_effects() {
        let (_root, app, sid, run) = fixture().await;
        let state = ability_controls(&app, &sid).await.unwrap().3;
        // The builtin Ask composer mode is read-only. This fixture explicitly
        // keeps writes enabled while the installation policy requires approval.
        assert_eq!(session_mode(&app, &sid).await.unwrap()["mode"], "write");
        assert!(!app.abilities.tool_available(
            "file_edit",
            &app.store.agent_abilities().await.unwrap(),
            &state,
            &run["loaded_abilities"]
        ));
        let (trial, loaded) = prospective(&app, &sid, &run, &["file_edit"])
            .await
            .unwrap()
            .unwrap();
        assert!(trial["active"]
            .as_array()
            .unwrap()
            .contains(&json!("codebase_admin")));
        assert!(loaded
            .as_array()
            .unwrap()
            .contains(&json!("tool:file_edit")));
        let (policy, _, visibility, _) = ability_controls(&app, &sid).await.unwrap();
        let access = app
            .store
            .live_access(app.store.current_user_id(), "shared_default")
            .await
            .unwrap();
        assert_eq!(
            live_tool_decision(
                &app,
                &access,
                "file_edit",
                &policy,
                &visibility,
                &trial,
                &loaded,
                false
            )
            .permission,
            crate::access::Permission::Ask
        );
        assert_eq!(ability_controls(&app, &sid).await.unwrap().3, state);
    }

    #[tokio::test]
    async fn off_suppressed_and_permission_denial_never_activate() {
        let (_root, app, sid, run) = fixture().await;
        app.store
            .put(
                &crate::key("session_ability_state", &sid),
                &json!({"revision":1,"active":[],"suppressed":["codebase_admin"]}),
            )
            .await
            .unwrap();
        assert!(prospective(&app, &sid, &run, &["file_edit"])
            .await
            .unwrap()
            .is_none());
        app.store
            .put(
                &crate::key("session_ability_state", &sid),
                &json!({"revision":2,"active":[],"suppressed":[]}),
            )
            .await
            .unwrap();
        let mut settings = app.store.agent_abilities().await.unwrap();
        let revision = settings["revision"].as_u64().unwrap();
        settings["visibility"]["codebase_admin"] = json!("off");
        settings["revision"] = json!(revision + 1);
        app.store
            .save_agent_abilities(&settings, revision)
            .await
            .unwrap();
        assert!(prospective(&app, &sid, &run, &["file_edit"])
            .await
            .unwrap()
            .is_none());
        settings["visibility"]["codebase_admin"] = json!("discoverable");
        settings["revision"] = json!(revision + 2);
        app.store
            .save_agent_abilities(&settings, revision + 1)
            .await
            .unwrap();
        let permissions = app
            .store
            .agent_tool_permissions("shared_default")
            .await
            .unwrap();
        app.store
            .set_agent_tool_permissions(
                "shared_default",
                permissions["revision"].as_u64().unwrap(),
                [("file_edit".to_owned(), crate::access::Permission::Deny)].into(),
            )
            .await
            .unwrap();
        assert!(prospective(&app, &sid, &run, &["file_edit"])
            .await
            .unwrap()
            .is_none());
        assert!(ability_controls(&app, &sid).await.unwrap().3["active"]
            .as_array()
            .unwrap()
            .is_empty());
    }
}

/// The review-first contract is enforced at dispatch, including approved calls.
pub(crate) fn allows_tool(run: &Value, name: &str, args: &Value) -> bool {
    if run.get("optimization_request").is_none() {
        return true;
    }
    if run["optimization_request"]["status"] != "ready" {
        return crate::tools::is_read(name);
    }
    match name {
        "performance_work" => true,
        "file_edit" | "file_write" => {
            args["task_id"].as_str().is_some_and(|id| !id.is_empty())
                && args["path"].as_str().is_some_and(|path| {
                    !path.ends_with(".skill.md")
                        && !path.contains("/config/")
                        && !path.starts_with("config/")
                        && !path.contains("/plugins/abilities/")
                        && !path.starts_with("plugins/abilities/")
                })
        }
        "build_work" => matches!(
            args["action"].as_str(),
            Some("begin" | "status" | "preflight")
        ),
        "prompt_slot_write" | "edit_own_prompt" | "save_own_skill" | "remove_own_skill" => false,
        "v3_build_image" | "v3_worker_submit" | "v3_build_work" | "promotion_propose"
        | "source_patch" | "shell" | "python" => false,
        _ => crate::tools::is_read(name),
    }
}

#[cfg(test)]
mod scope_tests {
    use super::*;
    #[test]
    fn optimization_never_releases_or_changes_guidance() {
        let run = json!({"optimization_request":{"status":"ready"}});
        assert!(!allows_tool(
            &run,
            "build_work",
            &json!({"action":"finish"})
        ));
        assert!(!allows_tool(
            &run,
            "build_work",
            &json!({"action":"decide"})
        ));
        assert!(!allows_tool(&run, "prompt_slot_write", &json!({})));
        assert!(!allows_tool(&run, "shell", &json!({})));
        assert!(!allows_tool(
            &run,
            "file_write",
            &json!({"path":"src/main.rs"})
        ));
        assert!(!allows_tool(
            &run,
            "file_edit",
            &json!({"task_id":"isolated","path":"config/prompts.json"})
        ));
        assert!(allows_tool(
            &run,
            "file_edit",
            &json!({"task_id":"isolated","path":"src/main.rs"})
        ));
        assert!(allows_tool(
            &json!({}),
            "build_work",
            &json!({"action":"finish"})
        ));
    }
}

pub(crate) async fn bind_task(app: &App, run_id: &str, run: &Value, args: &Value) -> Result<bool> {
    if run.get("optimization_request").is_none() {
        return Ok(true);
    }
    let Some(task_id) = args["task_id"].as_str().filter(|id| !id.is_empty()) else {
        return Ok(true);
    };
    if !(8..=80).contains(&task_id.len())
        || !task_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    {
        return Ok(false);
    }
    // Serialize concurrent calls without saving an old agent-loop run snapshot.
    // This independent durable binding survives restart and compaction.
    let _guard = app.gate.lock().await;
    let key = format!("optimization_task:{run_id}");
    match app.store.get(&key).await? {
        Some(binding) => Ok(binding["task_id"] == task_id),
        None => {
            app.store.put(&key, &json!({"task_id":task_id})).await?;
            Ok(true)
        }
    }
}

#[cfg(test)]
mod task_binding_tests {
    use super::*;

    #[tokio::test]
    async fn one_optimization_run_cannot_reset_budgets_by_switching_tasks() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let run = json!({"optimization_request":{"status":"ready"}});
        assert!(
            bind_task(&app, "run-one", &run, &json!({"task_id":"optimize-task-a"}))
                .await
                .unwrap()
        );
        assert!(
            bind_task(&app, "run-one", &run, &json!({"task_id":"optimize-task-a"}))
                .await
                .unwrap()
        );
        assert!(
            !bind_task(&app, "run-one", &run, &json!({"task_id":"optimize-task-b"}))
                .await
                .unwrap()
        );
        assert!(
            bind_task(&app, "run-two", &run, &json!({"task_id":"optimize-task-b"}))
                .await
                .unwrap()
        );
        assert!(bind_task(
            &app,
            "run-one",
            &json!({}),
            &json!({"task_id":"normal-task-c"})
        )
        .await
        .unwrap());
        assert_eq!(
            app.store
                .get("optimization_task:run-one")
                .await
                .unwrap()
                .unwrap()["task_id"],
            "optimize-task-a"
        );
        // A read without task selection neither resets nor replaces the binding.
        assert!(bind_task(&app, "run-one", &run, &json!({})).await.unwrap());
        assert!(
            !bind_task(&app, "run-one", &run, &json!({"task_id":"optimize-task-c"}))
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn concurrent_first_calls_publish_only_one_task_binding() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let run = json!({"optimization_request":{"status":"ready"}});
        let a = json!({"task_id":"optimize-task-a"});
        let b = json!({"task_id":"optimize-task-b"});
        let (first, second) = tokio::join!(
            bind_task(&app, "concurrent-run", &run, &a),
            bind_task(&app, "concurrent-run", &run, &b)
        );
        assert_ne!(first.unwrap(), second.unwrap());
        let binding = app
            .store
            .get("optimization_task:concurrent-run")
            .await
            .unwrap()
            .unwrap();
        assert!(binding["task_id"] == "optimize-task-a" || binding["task_id"] == "optimize-task-b");
    }
}
