//! Root compatibility for Engine-owned Context asset assembly and permanent selection staging.
use crate::*;
use webagent_assets_contracts::{Operation, State};
use webagent_capability_contracts::{Guidance, PreparedCapability};
pub use webagent_engine::context::assets::{record_tool_operation, Working};
use webagent_provider_contracts::ProfileV1;
pub(crate) fn catalog(app: &App) -> Result<crate::abilities::Catalog> {
    app.engine().asset_catalog()
}
pub(crate) fn source_fence(app: &App) -> Result<String> {
    app.engine().source_fence()
}
pub(crate) async fn hydrate(app: &App, run: &mut Value) -> Result<State> {
    app.engine().hydrate_context(run).await
}
pub(crate) async fn fingerprint(
    app: &App,
    sid: &str,
    agent_id: &str,
    loaded: &Value,
    selected: &Value,
    skills: &Value,
    optimization: &Value,
    requester: &str,
) -> Result<String> {
    app.engine()
        .context_fingerprint(
            sid,
            agent_id,
            loaded,
            selected,
            skills,
            optimization,
            requester,
        )
        .await
}
pub(crate) async fn prepare_capability(
    app: &App,
    run: &Value,
    schemas: &[Value],
    guidance: Vec<Guidance>,
    menu: String,
    sourcefingerprint: &str,
    cancel: &CancellationToken,
) -> Result<PreparedCapability> {
    app.engine()
        .prepare_capability(run, schemas, guidance, menu, sourcefingerprint, cancel)
        .await
}
pub(crate) async fn project(
    app: &App,
    run: &mut Value,
    cap: &PreparedCapability,
    profile: &ProfileV1,
    schemas: &[Value],
    history: &[Value],
    contributions: &[context_input::Contribution],
    call: context_input::CallIdentity,
    cancel: &CancellationToken,
) -> Result<Working> {
    app.engine()
        .project_context(
            run,
            cap,
            profile,
            schemas,
            history,
            contributions,
            call,
            cancel,
        )
        .await
}
#[cfg(test)]
mod tests {
    use super::*;
    /// Native output and changed current projections must agree before any activation is staged.

    /// Legacy identity conversion discards source pins and cannot import automatic ability IDs.

    /// Initialize a canonical queued run without a provider or worker; all state uses the real Store.
    #[cfg(feature = "linked-context")]
    async fn bridge_fixture() -> (tempfile::TempDir, App, Value) {
        let (root, app) = crate::runtime_tests::fixture().await;
        app.store
            .put(
                "session:assets-bridge",
                &json!({"id":"assets-bridge","agent_id":"shared_default"}),
            )
            .await
            .unwrap();
        let mut run = json!({"id":"assets-run","session_id":"assets-bridge","agent_id":"shared_default",
   "status":"queued","run_revision":0,"lease_epoch":0,"generation":"fixture",
   "requester_user_id":app.store.current_user_id(),"input":[{"role":"user","content":"durable user input"}]});
        app.save(&mut run).await.unwrap();
        (root, app, run)
    }
    /// Resolve a real capability through the host bridge with a selected, untrusted file body.
    #[cfg(feature = "linked-context")]
    async fn bridge_capability(app: &App, run: &Value) -> PreparedCapability {
        prepare_capability(
            app,
            run,
            &[json!({"type":"function","name":"file_read","parameters":{"type":"object"}})],
            vec![Guidance {
                id: "fixture-skill".into(),
                kind: webagent_capability_contracts::GuidanceKind::Selectable,
                body: "file guidance sentinel".into(),
                selected: true,
                natural: false,
            }],
            "temporary menu sentinel".into(),
            &"a".repeat(64),
            &CancellationToken::new(),
        )
        .await
        .unwrap()
    }
    /// The Main Notes guidance leads the Main Notes section (not the prompt section), follows
    /// live overrides, and never modifies transcript documents.
    #[cfg(feature = "linked-context")]
    #[tokio::test]
    async fn main_chat_skill_is_live_prefix_without_transcript_accumulation() {
        let (_root, app, run) = bridge_fixture().await;
        let sid = run["session_id"].as_str().unwrap();
        let before = app.store.get(&format!("session:{sid}")).await.unwrap();
        app.store
            .main_chat_change(0, "skill-prefix", "skill-prefix", vec![], |state| {
                state["settings"]["enabled"] = json!(true);
                state["session_id"] = json!(sid);
                state["skill_overrides"] =
                    json!({"contribute":"working sentinel","review":"review prefix sentinel"});
                Ok(Value::Null)
            })
            .await
            .unwrap();
        let notes = |app: &App| {
            let app = app.clone();
            let sid = sid.to_owned();
            async move {
                crate::main_chat::notes_context_store(
                    &app.store,
                    "shared_default",
                    &sid,
                    app.store.current_user_id(),
                    "basis",
                )
                .await
                .unwrap()
                .0
            }
        };
        let first = Box::pin(bridge_capability(&app, &run)).await;
        assert!(!first.main.contains("review prefix sentinel"));
        let section = Box::pin(notes(&app)).await;
        assert_eq!(section[0].0, "main-notes-guidance");
        assert!(section[0].1.contains("review prefix sentinel"));
        assert!(!section[0].1.contains("working sentinel"));
        app.store
            .main_chat_change(1, "skill-updated", "skill-updated", vec![], |state| {
                state["skill_overrides"]["review"] = json!("updated prefix sentinel");
                Ok(Value::Null)
            })
            .await
            .unwrap();
        let section = Box::pin(notes(&app)).await;
        assert!(section[0].1.contains("updated prefix sentinel"));
        assert!(!section[0].1.contains("review prefix sentinel"));
        assert_eq!(
            app.store.get(&format!("session:{sid}")).await.unwrap(),
            before
        );
    }
    /// Exercise activation, projection, compaction and unload through App/Store without persisting synthetic text.
    #[cfg(feature = "linked-context")]
    #[tokio::test]
    async fn projected_guidance_stays_out_of_durable_history() {
        let (_root, app, mut run) = bridge_fixture().await;
        let selected = json!({"type":"tool","tool_name":"file_read"});
        run["context_assets_revision"] = json!(0);
        // Tool selectors no longer come from a loader; older ledgers can still hold them.
        run["_context_assets_operation"] = json!(Operation::Activate {
            selectors: vec![selected.clone()],
            expected_revision: 0,
            request_id: "fixture-load".into()
        });
        app.save(&mut run).await.unwrap();
        let state = hydrate(&app, &mut run).await.unwrap();
        assert_eq!(state.selectors, vec![selected.clone()]);
        let cap = Box::pin(bridge_capability(&app, &run)).await;
        let profile: ProfileV1 = serde_json::from_value(json!({"instruction_role":"developer",
   "transport":"responses","reasoning_replay":"responses_items","cache_control":"implicit_prefix"}))
        .unwrap();
        // As in a real session, the history already shows the current mode, so the
        // projection adds no set_execution_mode entry of its own.
        if let Some(input) = run["input"].as_array_mut() {
            input.push(
                json!({"type":"function_call","call_id":"set_execution_mode-start",
                "name":"set_execution_mode","arguments":"{\"mode\":\"ask\"}"}),
            );
            input.push(
                json!({"type":"function_call_output","call_id":"set_execution_mode-start",
                "output":json!({"status":"success","mode":"ask","revision":0}).to_string()}),
            );
        }
        let original = run["input"].as_array().unwrap().clone();
        let schemas =
            vec![json!({"type":"function","name":"file_read","parameters":{"type":"object"}})];
        // Every profile keeps skills and the menu in the instructions; only the call's own
        // notes follow the history, for this call only.
        let note = context_input::Contribution {
            id: "mode".into(),
            source: "native".into(),
            lifetime: context_input::Lifetime::Call,
            content: "call note sentinel".into(),
            turn: 1,
            call: 1,
        };
        let instructions_profile: ProfileV1 =
            serde_json::from_value(json!({"instruction_role":"system",
   "transport":"chat_completions","reasoning_replay":"none","cache_control":"none"}))
            .unwrap();
        for (profile, role) in [(&profile, "developer"), (&instructions_profile, "system")] {
            let mut probe = run.clone();
            let working = Box::pin(project(
                &app,
                &mut probe,
                &cap,
                profile,
                &schemas,
                &original,
                std::slice::from_ref(&note),
                context_input::CallIdentity { turn: 1, call: 1 },
                &CancellationToken::new(),
            ))
            .await
            .unwrap();
            assert!(working.contributions_projected);
            assert!(working.guidance.contains("file guidance sentinel"));
            assert!(working.guidance.contains("temporary menu sentinel"));
            assert!(!working.guidance.contains("call note sentinel"));
            assert_eq!(working.input.len(), original.len() + 1);
            assert_eq!(working.input[..original.len()], original[..]);
            assert_eq!(working.input[original.len()]["role"], role);
            assert_eq!(
                working.input[original.len()]["content"],
                "call note sentinel"
            );
            assert_eq!(
                working.extra_json_bytes,
                serde_json::to_vec(&working.input).unwrap().len()
                    - serde_json::to_vec(&original).unwrap().len()
            );
        }
        // A projection with no call notes leaves the history exactly as it is.
        let working = Box::pin(project(
            &app,
            &mut run,
            &cap,
            &profile,
            &schemas,
            &original,
            &[],
            context_input::CallIdentity { turn: 1, call: 2 },
            &CancellationToken::new(),
        ))
        .await
        .unwrap();
        assert_eq!(working.input, original);
        app.save(&mut run).await.unwrap();
        assert_eq!(run["input"], json!(original));
        assert_eq!(
            app.run("assets-run").await.unwrap()["input"],
            json!(original)
        );
        let ledger = app
            .store
            .context_assets(
                "assets-bridge",
                "shared_default",
                app.store.current_user_id(),
            )
            .await
            .unwrap();
        // The ledger still records each included definition (at input 0), as bookkeeping;
        // its body is in the instructions, not in the history. The owner's session task
        // list snapshot is placed even while the list is empty.
        let placed: Vec<_> = ledger.placements.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(placed, ["fixture-skill", "session-tasks-0"]);
        assert!(ledger.placements.iter().all(|p| p.after_input == 0));
        run["input"] = json!([{"role":"assistant","content":"canonical summary"}]);
        run["compactions"] = json!(1);
        app.store.save_run(&mut run,"context_compacted",&json!({"source":original,"source_digest":crate::digest(&original),"dropped_items":original.len(),"segment_id":"assets-run-1"})).await.unwrap();
        let rebased = hydrate(&app, &mut run).await.unwrap();
        assert_eq!(rebased.selectors, vec![selected.clone()]);
        assert!(rebased.placements.is_empty());
        let operation = Operation::Unload {
            selector: selected.clone(),
            expected_revision: rebased.revision,
            request_id: "fixture-unload".into(),
        };
        run["pending"] = json!({"name":"context_assets","call_id":"unload-fixture","arguments":{"action":"unload","selector":selected,"expected_revision":rebased.revision,"request_id":"fixture-unload"}});
        let tools = run["selected_tools"].clone();
        let skills = run["loaded_skills"].clone();
        record_tool_operation(
            &mut run,
            &json!({"status":"success","output":{"operation":operation}}),
            &tools,
            &skills,
        )
        .unwrap();
        run.as_object_mut().unwrap().remove("pending");
        app.save(&mut run).await.unwrap();
        let unloaded = hydrate(&app, &mut run).await.unwrap();
        assert!(unloaded.selectors.is_empty());
        assert_eq!(run["selected_tools"], json!([]));
        let saved = app.run("assets-run").await.unwrap();
        assert_eq!(
            saved["input"],
            json!([{"role":"assistant","content":"canonical summary"}])
        );
        assert!(
            saved.get("_context_assets_write").is_none()
                && saved.get("_context_assets_operation").is_none()
        );
    }
}
