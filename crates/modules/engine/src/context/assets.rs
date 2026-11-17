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
    /// The session's mode guidance: the text pinned ahead of the history (set when the
    /// history basis changes, so at the start and after each compaction) and, when the mode
    /// changed since the last call, the new guidance to add to the history.
    async fn mode_guidance(
        &self,
        sid: &str,
        basis: &str,
    ) -> Result<(Option<String>, Option<String>)> {
        let mode = self.session_mode(sid).await?;
        let revision = mode["revision"].as_u64().unwrap_or(0);
        let text = mode["definition"]["label"].as_str().map(|label| {
            format!(
                "Execution mode: {label}\n{}",
                mode["definition"]["prompt"].as_str().unwrap_or("").trim()
            )
        });
        let key = format!("session_mode_view:{sid}");
        let saved = self.store.get(&key).await?;
        let (view, change) = match &saved {
            Some(view) if view["history_basis"] == basis => {
                if view["revision"].as_u64() == Some(revision) {
                    (view.clone(), None)
                } else {
                    let mut next = view.clone();
                    next["revision"] = json!(revision);
                    (next, text.clone())
                }
            }
            _ => (
                json!({"history_basis":basis,"revision":revision,"pinned":text}),
                None,
            ),
        };
        if saved.as_ref() != Some(&view) {
            self.store.put(&key, &view).await?;
        }
        Ok((view["pinned"].as_str().map(str::to_owned), change))
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
        let basis = webagent_context::assets::history_basis(&session);
        let (mut notes, updates) = Box::pin(self.native.main_notes(
            self.store.records(),
            &agent,
            &sid,
            &requester,
            &basis,
        ))
        .await?;
        let (mode_pinned, mode_change) = self.mode_guidance(&sid, &basis).await?;
        if let Some(text) = mode_pinned {
            // Instruction sections are joined as they are, so each ends with its own break.
            notes.insert(0, ("mode-guidance".to_owned(), format!("{}\n\n", text.trim_end())));
        }
        // Another session's Main Notes changes and this session's mode changes join the
        // history as tool-call rows, once each, where they stay (and stay cached) until
        // compaction pins the current notes and mode ahead of the history again.
        let mut rows = Vec::new();
        let mut row = |name: &str, text: String| {
            let id = format!("{name}-{}", uuid::Uuid::new_v4());
            rows.push(json!({"type":"function_call","call_id":id,"name":name,"arguments":"{}"}));
            rows.push(json!({"type":"function_call_output","call_id":id,"output":text}));
        };
        if let Some(text) = mode_change {
            row("mode_guidance", text);
        }
        for text in updates {
            row("main_notes_update", text);
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
        webagent_context::assets::project_context(
            run,
            cap,
            profile,
            schemas,
            history,
            contributions,
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
        .await
    }
}
