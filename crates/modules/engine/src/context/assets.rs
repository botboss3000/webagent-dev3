//! Engine current-owner authority and source composition, over Context-owned materialization.
use crate::context::abilities::Catalog;
use crate::ports::*;
use crate::runtime::{config, models::ModelRecords, ports::*, Runtime};
use crate::{context_assembly, context_input, provider};
use anyhow::{bail, Result};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
use webagent_assets_contracts::State;
use webagent_capability_contracts::{Guidance, PreparedCapability};
use webagent_provider_contracts::ProfileV1;

pub use webagent_context::assets::{record_tool_operation, Working};
impl<
        S: ScopedStorage
            + ModelRecords
            + ChatEffects
            + AdmissionEffects
            + NotificationEffects
            + UsageEffects
            + RunObservation
            + RunPresentation,
        C: config::SchemaCatalog,
        B: Catalog,
        A: crate::runtime::actions::Actions<S> + Clone + PartialEq,
        H: ChatAuthority<S>,
        M: ModuleBindings<S>,
        X: AgentPolicy<S>
            + NativePolicy
            + CredentialBindings<S>
            + SkillSources<S>
            + PromptSlots<S>
            + MainGuidance<S>
            + crate::runtime::native::NativeDispatch<S, C, B, H::Binding>,
    > Runtime<S, C, B, A, H, M, X>
{
    pub fn asset_catalog(&self) -> Result<B> {
        self.abilities.current()
    }
    pub fn source_fence(&self) -> Result<String> {
        let app = self;
        let fresh = app.asset_catalog()?;
        if fresh.version() != app.abilities.version()
            || fresh.live_version() != app.abilities.live_version()
        {
            bail!("Context assets source changed");
        }
        let base = provider::Provider::instructions_for_snapshot(
            provider::DEFAULT_INSTRUCTIONS,
            crate::ports::AgentMetadata::root(app.store.records()),
            fresh.source_root(),
        )?;
        let engines = crate::components::catalog()?;
        let controls =
            crate::components::controls(crate::ports::AgentMetadata::root(app.store.records()))?;
        Ok(crate::digest(&json!([
            app.abilities.version(),
            app.abilities.live_version(),
            fresh.version(),
            fresh.live_version(),
            base,
            engines.revision,
            controls
        ])))
    }
    fn asset_identity(&self, run: &Value) -> Result<(String, String, String)> {
        webagent_context::assets::identity(run, self.store.current_user_id())
    }
    async fn asset_legacy_seed(&self, run: &mut Value, state: &State) -> Result<bool> {
        if !webagent_context::assets::legacy_seed_needed(run, state) {
            return Ok(false);
        }
        let (_sid, _agent, _requester) = self.asset_identity(run)?;
        let rid = run["id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Context assets run missing"))?
            .to_owned();
        let canonical = self.run(&rid).await?;
        let operation = webagent_context::assets::legacy_operation(
            run,
            &canonical,
            self.store.current_user_id(),
        )?;
        run["_context_assets_operation"] = json!(operation);
        self.save(run).await?;
        Ok(true)
    }
    pub async fn hydrate_context(&self, run: &mut Value) -> Result<State> {
        let app = self;
        let (sid, agent, requester) = app.asset_identity(run)?;
        let scoped = app.store.for_agent(&agent).await?;
        let mut state = scoped.context_assets(&sid, &agent, &requester).await?;
        if app.asset_legacy_seed(run, &state).await? {
            state = scoped.context_assets(&sid, &agent, &requester).await?;
        }
        let changed = webagent_context::assets::hydrate_selectors(run, &state, X::tool_marker)?;
        if changed {
            app.save(run).await?;
        }
        Ok(state)
    }
    pub async fn context_fingerprint(
        &self,
        sid: &str,
        agent_id: &str,
        loaded: &Value,
        selected: &Value,
        skills: &Value,
        optimization: &Value,
        requester: &str,
    ) -> Result<String> {
        let app = self;
        let fresh = app.asset_catalog()?;
        let access = app
            .chat
            .live_access(app.store.records(), requester, sid, agent_id)
            .await?;
        let (mode_revision, visibility, state) = app.ability_controls_for(sid, requester).await?;
        let mode = app.session_mode(sid).await?;
        let scoped = app.store.for_agent(agent_id).await?;
        let ledger = scoped.context_assets(sid, agent_id, requester).await?;
        let own = app
            .native
            .own_skills(scoped.for_user(requester).await?.records())
            .await?;
        let base = provider::Provider::instructions_for_snapshot(
            provider::DEFAULT_INSTRUCTIONS,
            crate::ports::AgentMetadata::root(app.store.records()),
            fresh.source_root(),
        )?;
        let slots = app.native.prompt_slots(scoped.records(), agent_id).await?;
        let main_chat =
            Box::pin(app.native.main_guidance(app.store.records(), agent_id, sid)).await?;
        let engines = crate::components::catalog()?;
        let controls =
            crate::components::controls(crate::ports::AgentMetadata::root(app.store.records()))?;
        Ok(crate::digest(&json!([
            fresh.version(),
            fresh.live_version(),
            mode_revision,
            visibility,
            state,
            mode,
            loaded,
            selected,
            skills,
            optimization,
            access.context_fingerprint(),
            ledger.selectors,
            own["revision"],
            own,
            base,
            slots,
            main_chat,
            engines.revision,
            controls
        ])))
    }
    pub async fn prepare_capability(
        &self,
        run: &Value,
        schemas: &[Value],
        guidance: Vec<Guidance>,
        menu: String,
        sourcefingerprint: &str,
        cancel: &CancellationToken,
    ) -> Result<PreparedCapability> {
        let app = self;
        let (sid, agent, requester) = app.asset_identity(run)?;
        let scoped = app.store.for_agent(&agent).await?;
        let fresh = app.asset_catalog()?;
        let slots = app.native.prompt_slots(scoped.records(), &agent).await?;
        let live_slots = context_assembly::pinned_instructions("", &slots)?;
        // There is no base prompt: instructions are the agent's prompt slots and skills.
        // The Main Notes guidance leads the Main Notes section instead.
        let base_prompt = String::new();
        let ledger = scoped.context_assets(&sid, &agent, &requester).await?;
        let request = webagent_context::assets::capability_request(
            base_prompt,
            live_slots,
            ledger,
            schemas,
            guidance,
            menu,
            sourcefingerprint,
        )?;
        crate::components::capability_prepare(
            crate::ports::AgentMetadata::root(app.store.records()),
            &request,
            cancel,
        )
        .await
    }
    pub async fn project_context(
        &self,
        run: &mut Value,
        cap: &PreparedCapability,
        profile: &ProfileV1,
        schemas: &[Value],
        history: &[Value],
        contributions: &[context_input::Contribution],
        call: context_input::CallIdentity,
        cancel: &CancellationToken,
    ) -> Result<Working> {
        let (sid, agent, requester) = self.asset_identity(run)?;
        let scoped = self.store.for_agent(&agent).await?;
        let ledger = scoped.context_assets(&sid, &agent, &requester).await?;
        let session = scoped
            .get(&format!("session:{sid}"))
            .await?
            .ok_or_else(|| anyhow::anyhow!("Context session missing"))?;
        let basis = webagent_context::assets::notes_basis(&session);
        let (mut notes, updates) = Box::pin(self.native.main_notes(
            self.store.records(),
            &agent,
            &sid,
            &requester,
            &basis,
        ))
        .await?;
        // The session task list follows the notes, pinned at the same basis; its one-line
        // state, and the reminder to file tool work done since the user's last message
        // without a list write, join this step's guidance message only.
        let since_user = history
            .iter()
            .rev()
            .take_while(|item| item["role"] != "user")
            .filter(|item| item["type"] == "function_call");
        let (calls, filed) = since_user.fold((0, false), |(calls, filed), item| {
            (calls + 1, filed || item["name"] == "task_list_write")
        });
        let (tasks, task_lines, task_updates) =
            Box::pin(
                self.native
                    .session_tasks(self.store.records(), &sid, &basis, calls, filed),
            )
            .await?;
        notes.extend(tasks);
        let mut contributions = contributions.to_vec();
        for (index, content) in task_lines.into_iter().enumerate() {
            contributions.push(context_input::Contribution {
                id: format!("session_tasks_{index}"),
                source: "native_session_tasks".into(),
                lifetime: context_input::Lifetime::Call,
                content,
                turn: call.turn,
                call: call.call,
            });
        }
        // Another session's Main Notes changes join the history as tool-call rows, once each,
        // where they stay (and stay cached); a fold keeps them ahead of its block.
        let mut rows = Vec::new();
        // Each update reads as a main_notes_read of the changes since the version before it.
        for (version, text) in updates {
            let id = format!("main_notes_read-{}", uuid::Uuid::new_v4());
            let arguments =
                json!({"since_version":version.saturating_sub(1),"changed_by":"another session"});
            rows.push(
                json!({"type":"function_call","call_id":id,"name":"main_notes_read",
                "arguments":arguments.to_string()}),
            );
            rows.push(json!({"type":"function_call_output","call_id":id,"output":text}));
        }
        // The user's task list edits join the history the same way, once each.
        for (revision, text) in task_updates {
            let id = format!("task_list_read-{}", uuid::Uuid::new_v4());
            let arguments = json!({"revision":revision,"changed_by":"the user"});
            rows.push(
                json!({"type":"function_call","call_id":id,"name":"task_list_read",
                "arguments":arguments.to_string()}),
            );
            rows.push(json!({"type":"function_call_output","call_id":id,"output":text}));
        }
        // The history always shows the current execution mode: at the start, after the user
        // changes it, and after compaction the server adds a set_execution_mode result, unless
        // the latest one in the history (the agent's own call included) already shows it.
        let mode = self.session_mode(&sid).await?;
        let revision = mode["revision"].as_u64().unwrap_or(0);
        let shown = latest_mode_revision(history);
        if shown != Some(revision) {
            let id = mode["definition"]["id"].as_str().unwrap_or("ask");
            let (changed_by, reason) = match shown {
                Some(_) => (
                    "user",
                    "The user changed the execution mode; you did not call this.",
                ),
                None if revision == 0 => {
                    ("session_start", "The session's starting execution mode.")
                }
                None => ("session", "The session's current execution mode."),
            };
            let mut output =
                json!({"status":"success","mode":id,"revision":revision,"reason":reason});
            if webagent_context::defaults::mode_guidance() != "per_step" {
                output["guidance"] = json!(format!(
                    "Execution mode: {}\n{}",
                    mode["definition"]["label"].as_str().unwrap_or(id),
                    mode["definition"]["prompt"].as_str().unwrap_or("").trim()
                ));
            }
            let call_id = format!("set_execution_mode-{}", uuid::Uuid::new_v4());
            rows.push(
                json!({"type":"function_call","call_id":call_id,"name":"set_execution_mode",
                "arguments":json!({"mode":id,"changed_by":changed_by}).to_string()}),
            );
            rows.push(json!({"type":"function_call_output","call_id":call_id,"output":output.to_string()}));
        }
        let mut extended;
        let history = if rows.is_empty() {
            history
        } else {
            extended = history.to_vec();
            if let Some(input) = run["input"].as_array_mut() {
                input.extend(rows.iter().cloned());
            }
            extended.extend(rows.iter().cloned());
            // Journal them so later runs of this session rebuild them from its history.
            self.store
                .save_run_observed(run, "history_rows", &json!({"items":rows}))
                .await?;
            extended.as_slice()
        };
        // The size report measured the history before these entries; count them as projection
        // growth (each array item plus its separating comma).
        let added: usize = rows
            .iter()
            .map(|row| serde_json::to_vec(row).map_or(0, |bytes| bytes.len() + 1))
            .sum();
        let mut projected = webagent_context::assets::project_context(
            run,
            cap,
            profile,
            schemas,
            history,
            &contributions,
            &notes,
            call,
            ledger,
            &session,
            &agent,
            &requester,
            crate::ports::AgentMetadata::root(self.store.records()),
            &crate::context::bridge::Planner,
            cancel,
        )
        .await?;
        projected.extra_json_bytes += added;
        Ok(projected)
    }
}

/// The mode revision in the latest successful set_execution_mode result in `history`.
fn latest_mode_revision(history: &[Value]) -> Option<u64> {
    let calls: std::collections::HashSet<&str> = history
        .iter()
        .filter(|item| item["type"] == "function_call" && item["name"] == "set_execution_mode")
        .filter_map(|item| item["call_id"].as_str())
        .collect();
    history.iter().rev().find_map(|item| {
        if item["type"] != "function_call_output"
            || !item["call_id"]
                .as_str()
                .is_some_and(|id| calls.contains(id))
        {
            return None;
        }
        let output: Value = serde_json::from_str(item["output"].as_str()?).ok()?;
        if output["status"] != "success" {
            return None;
        }
        output["revision"]
            .as_u64()
            .or_else(|| output["output"]["revision"].as_u64())
    })
}
