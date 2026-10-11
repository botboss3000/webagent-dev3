use super::*;

fn automatic_code_index_journal(
    evidence: &Value,
    run_id: &str,
) -> (Vec<Value>, Vec<Value>, &'static str) {
    let mut calls = Vec::new();
    let mut results = Vec::new();
    if let Some(items) = evidence["items"].as_array() {
        for item in items.iter().take(5) {
            let Some(call_id) = item["call_id"].as_str() else {
                continue;
            };
            let arguments = json!({
                "action":"snippet",
                "root":item["root"],
                "file_path":item["path"],
                "start_line":item["start_line"],
                "end_line":item["end_line"],
                "max_chars":item["max_chars"]
            });
            calls.push(json!({
                "type":"function_call",
                "call_id":call_id,
                "name":"code_index_lookup",
                "arguments":arguments.to_string()
            }));
            results.push(json!({
                "type":"function_call_output",
                "call_id":call_id,
                "output":json!({
                    "status":"success",
                    "source_kind":"untrusted_source_index",
                    "output":item
                }).to_string()
            }));
        }
    }
    let ranked = evidence["ranked_candidates"].as_array();
    if let Some(ranked) = ranked.filter(|items| !items.is_empty()) {
        let candidate_id = format!("code_index_auto_{run_id}_candidates");
        calls.push(json!({
            "type":"function_call",
            "call_id":candidate_id,
            "name":"code_index_lookup",
            "arguments":json!({
                "action":"search",
                "root":evidence["root"],
                "query":evidence["query"],
                "limit":evidence["candidate_limit"],
                "offset":0
            }).to_string()
        }));
        results.push(json!({
            "type":"function_call_output",
            "call_id":candidate_id,
            "output":json!({
                "status":"success",
                "source_kind":"untrusted_source_index",
                "output":{
                    "ranked_candidates":ranked,
                    "index_revision":evidence["index_revision"]
                }
            }).to_string()
        }));
    }
    let status = if evidence["items"].as_array().is_some_and(|items| !items.is_empty()) {
        "attached"
    } else if ranked.is_some_and(|items| !items.is_empty()) {
        "attached_candidates"
    } else {
        "no_matches"
    };
    (calls, results, status)
}
#[cfg(test)]
mod automatic_code_index_journal_tests {
    use super::*;
    #[test]
    fn ranked_candidates_are_journaled_without_snippets_as_untrusted_pairs() {
        let evidence=json!({"root":"published","query":"private query","candidate_limit":16,
            "index_revision":{"release_id":"release","source_sha256":"sha","index_sha256":"index"},
            "ranked_candidates":[{"path":"src/a.rs","start_line":3,"end_line":3,"score":1.0,"confidence":0.9,"selected":false}],"items":[]});
        let (calls,results,status)=automatic_code_index_journal(&evidence,"run-1");
        assert_eq!(status,"attached_candidates");assert_eq!(calls.len(),1);assert_eq!(results.len(),1);
        assert_eq!(calls[0]["type"],"function_call");assert_eq!(results[0]["type"],"function_call_output");
        assert_eq!(calls[0]["call_id"],results[0]["call_id"]);assert_eq!(calls[0]["name"],"code_index_lookup");
        let output:Value=serde_json::from_str(results[0]["output"].as_str().unwrap()).unwrap();
        assert_eq!(output["source_kind"],"untrusted_source_index");
        assert_eq!(output["output"]["ranked_candidates"],evidence["ranked_candidates"]);
        assert!(output["output"].get("query").is_none());
    }
}
// A read-only tool can be interrupted after the model call was journaled. Pair
// that call with a content-free error result so any later context remains valid.
// Writes retain the existing uncertainty/recovery behavior because their effects
// cannot be inferred from a lost transport response.
// Fixed reason codes only; raw worker errors stay private.
fn record_tool_interruption(run: &mut Value, reason: &str) {
    let reason = match reason {
        "timeout" => "timeout", "cancelled" => "cancelled",
        "transport_error" => "transport_error", _ => "uncertain",
    };
    run["pending"]["interruption"] = json!({"reason":reason,"recorded_at":now()});
}
fn record_interrupted_tool(run: &mut Value, name: &str) -> Result<Option<Value>> {
    if tools::is_read(name) {
        let result=json!({"status":"error",
            "output":"Read interrupted before a result was received; no data was returned."});
        append_tool(run,&result)?;
        run["status"]=json!("failed");
        run["error"]=json!("Read tool interrupted");
        Ok(Some(result))
    } else {
        record_tool_interruption(run, "transport_error");
        run["uncertain"]=json!(true);
        run["status"]=json!("recovery");
        run["error"]=json!("Tool outcome unknown; review before continuing");
        Ok(None)
    }
}

// Recheck all context authority after credential retrieval, under the dispatch gate.
async fn dispatch_asset_key(app:&App,sid:&str,agent_id:&str,loaded:&Value,selected:&Value,skills:&Value)->Result<String> {
    let access=app.store.live_access(app.store.current_user_id(),agent_id).await?;
    let (policy,_,visibility,state)=ability_controls(app,sid).await?;
    let mode=session_mode(app,sid).await?;
    let own=app.store.list_own_skills().await?;
    Ok(digest(&json!([app.abilities.live_version(),own["revision"],policy,visibility,state,loaded,selected,skills,
        mode,access.context_fingerprint()])))
}
pub(crate) async fn execute_run(
    app: App,
    id: String,
    generation: String,
    cancel: CancellationToken,
) -> Result<()> {
    let mut context_accounting = context_accounting::ContextAccounting::default();
    // CONTEXT-ASSET-CACHE: one entry per executing run, never shared across callers.
    // Fresh live access and all shaping controls below invalidate this derived data;
    // actual tool execution still performs its own live authorization checks.
    let mut context_assets: Option<(String, Vec<Value>, String, Vec<abilities::AbilityInclusion>)> = None;
    loop {
        let _guard = app.gate.lock().await;
        let mut run = app.run(&id).await?;
        if run["generation"] != generation || status(&run) != "running" || cancel.is_cancelled() {
            return Ok(());
        }
        // Apply a composer choice before validating the next dispatch. This never
        // cancels/replays the model response or tool effects from the previous call.
        if run["phase"]=="model" {
            match apply_chat_model_preference(&app,&mut run).await {
                Ok(true)=>{context_accounting.reset();app.save(&mut run).await?;}
                Ok(false)=>{},
                Err(ref error)=>{
                    tracing::warn!(
                        run_id=%id,
                        stage=crate::bounded_model_authorization_failure_stage(error),
                        "Composer model preference could not be applied"
                    );
                    run["status"]=json!("paused");
                    run["error"]=json!("Selected model unavailable; choose a permitted connected model and resume");
                    app.save(&mut run).await?;
                    return Ok(());
                }
            }
        }
        let agent_id=run["agent_id"].as_str().unwrap_or("shared_default").to_owned();
        let access=app.store.live_access(app.store.current_user_id(),&agent_id).await;
        match &access {
            Err(error) => tracing::warn!(
                run_id=%id,
                stage=crate::bounded_model_authorization_failure_stage(error),
                "Live model access check failed"
            ),
            Ok(access) if !access.admitted() => tracing::warn!(
                run_id=%id,
                stage="access_denied",
                "Live model access check denied"
            ),
            _ => {}
        }
        if access.as_ref().map_or(true, |access| !access.admitted()) || !app.run_model_authorized(&run).await {
            run["status"]=json!("stopped");
            run["error"]=json!("Access changed; run stopped");
            app.save(&mut run).await?;
            return Ok(());
        }
        let access=access.unwrap();
        let mut app=app.clone();
        app.store=match app.store.for_agent(&agent_id).await {
            Ok(store)=>store,
            Err(_)=>{
                run["status"]=json!("paused");
                run["error"]=json!("Agent storage unavailable; review required");
                app.save(&mut run).await?;
                return Ok(());
            }
        };
        if app.shutoff().await? {
            run["status"] = json!("stopped");
            app.save(&mut run).await?;
            return Ok(());
        }
        let sid = run["session_id"].as_str().unwrap().to_owned();
        let workflow_policy = match current_workflow_policy(&app).await {
            Ok(policy) => policy,
            Err(_) => {
                run["status"] = json!("paused");
                run["error"] = json!("Invalid workflow policy; administrator review required");
                app.save(&mut run).await?;
                return Ok(());
            }
        };
        if run["ability_catalog_version"] != app.abilities.version() {
            run["loaded_abilities"] = json!([]);
            run["ability_catalog_version"] = json!(app.abilities.version());
            app.save(&mut run).await?;
        }
        if run["phase"] == "model" {
            if run.get("tool_context_state").is_none() {
                let session=app.store.get(&format!("session:{sid}")).await?;
                run["tool_context_state"]=session.as_ref().and_then(|s|s.get("tool_context_state")).cloned()
                    .unwrap_or_else(||json!({"clock":0,"entries":[]}));
            }
            // Persist once-per-message admission before any optional retrieval work.
            // No source read or Decision request may hold the shared dispatch gate.
            if run["code_index_retrieval_attempted"]!=true && run["post_run_followup"]!=true {
                let _ = tools::code_index_retrieval::prepare_run_dependencies(
                    &app, &sid, &mut run,
                )
                .await;
                run["code_index_retrieval_attempted"]=json!(true);
                run["code_index_retrieval_stage"]=json!("preflight");
                let user_text=run["input"].as_array().unwrap().iter().rev()
                    .find(|item|item["role"]=="user")
                    .map(|item| {
                        if let Some(text)=item["content"].as_str() {text.to_owned()}
                        else {item["content"].as_array().map(|parts|parts.iter()
                            .filter_map(|part|part["text"].as_str()).collect::<Vec<_>>().join("\n"))
                            .unwrap_or_default()}
                    }).unwrap_or_default();
                app.save(&mut run).await?;
                drop(_guard);
                let retrieval=tokio::time::timeout(Duration::from_secs(40),
                    tools::code_index_retrieval::retrieve(&app,&sid,&id,&user_text,cancel.clone())).await;
                let _retrieval_guard=app.gate.lock().await;
                let mut latest=app.run(&id).await?;
                if latest["generation"]!=generation || status(&latest)!="running"
                    || latest["phase"]!="model" || cancel.is_cancelled() || app.shutoff().await? {
                    return Ok(());
                }
                if let Ok(Ok(Some(evidence)))=retrieval {
                    if tools::code_index_retrieval::authorize_attachment(&app,&sid,&id,evidence["output"]["settings_sha256"].as_str().unwrap_or("")).await.is_err() {
                        latest["code_index_retrieval_status"]=json!("access_changed");
                        latest["code_index_retrieval_stage"]=json!("authorization");
                        app.save(&mut latest).await?;
                        continue;
                    }
                    // These are private source read records, never trusted instructions
                    // and never an external-effect replay. Journal each recallable pair.
                    let evidence=&evidence["output"];
                    let (calls,results,retrieval_status)=automatic_code_index_journal(evidence,&id);
                    if !calls.is_empty() {
                        latest["input"].as_array_mut().unwrap().extend(calls.iter().cloned());
                        // Turn zero is reserved here; working model calls begin at one.
                        app.store.save_run_observed(&mut latest,"model_output",&json!({"output":calls})).await?;
                        for result in results {
                            latest["input"].as_array_mut().unwrap().push(result);
                            app.save(&mut latest).await?;
                        }
                    }
                    latest["code_index_retrieval_status"]=json!(retrieval_status);
                    latest["code_index_retrieval_stage"]=json!("complete");
                } else {
                    let (status,stage)=match retrieval {
                        Ok(Ok(None))=>("disabled_or_no_matches","complete"),
                        Err(_)=>("timeout","unknown"),
                        Ok(Err(ref error))=>("unavailable",tools::code_index_retrieval::bounded_failure_stage(error)),
                        _=>("unavailable","unknown"),
                    };
                    latest["code_index_retrieval_status"]=json!(status);
                    latest["code_index_retrieval_stage"]=json!(stage);
                }
                app.save(&mut latest).await?;
                context_accounting.reset();
                continue;
            }
            if crate::background_tasks::collect(&app,&mut run).await? {
                context_accounting.reset();
            }
            // Older runs had no selection; bind once before any model dispatch.
            let selection = match run.get("provider_selection") {
                Some(value) => provider::ProviderSelection::parse(value, DEFAULT_AGENT_MODEL),
                None => app.selection().await,
            };
            let selection = match selection {
                Ok(selection) => selection,
                Err(_) => {
                    run["status"] = json!("paused");
                    run["error"] =
                        json!("Unsupported or invalid provider selection; review this run");
                    app.save(&mut run).await?;
                    return Ok(());
                }
            };
            if run.get("provider_selection").is_none() {
                run["provider_selection"] = selection.value();
                app.save(&mut run).await?;
            }
            if run.get("instructions").is_none() {
                run["instructions"] = json!(assembled_agent_instructions(&app.store,
                    run["agent_id"].as_str().unwrap_or("shared_default")).await?);
                app.save(&mut run).await?;
            }
            let instructions = provider::Provider::instructions_for_turn(
                run["instructions"].as_str()
                    .ok_or_else(|| anyhow!("Invalid pinned agent instructions"))?);
            if run["turns"].as_u64().unwrap() >= if run["post_run_followup"]==true {workflow_policy.max_turns.min(3)} else {workflow_policy.max_turns} || run["attempt"].as_u64().unwrap() >= workflow_policy.max_attempts {
                run["status"] = json!(if run["post_run_followup"]==true {"failed"} else {"paused"});
                run["error"] = json!(if run["post_run_followup"]==true {"Read-only follow-up budget exhausted"} else {"Run limit reached; explicit continuation required"});
                app.save(&mut run).await?;
                return Ok(());
            }
            let (policy, _, visibility, ability_state) = ability_controls(&app, &sid).await?;
            let mode = session_mode(&app, &sid).await?;
            let authorization_mode=if run["post_run_followup"]==true {
                json!({"mode":"read_only","execution_mode":"ask","revision":mode["revision"]})
            } else {mode.clone()};
            let read_only=authorization_mode["mode"]=="read_only";
            // CONTEXT-ASSET-KEY: include full freshly resolved access (including
            // grants), live bundle validity, policy, visibility, loaded state and session mode.
            // Hash only in memory; never publish private grant data in reports.
            let own_skills=app.store.list_own_skills().await?;
            let asset_key = digest(&json!([app.abilities.live_version(), own_skills["revision"], policy, visibility,
                ability_state, run["loaded_abilities"], run["selected_tools"], run["loaded_skills"], mode, access.context_fingerprint()]));
            if context_assets.as_ref().map(|entry| &entry.0) != Some(&asset_key) {
                let schemas = app.abilities.schemas_with(&policy, &run["loaded_abilities"], &visibility, &ability_state)
                    .into_iter().filter(|schema| {
                        let name=schema["name"].as_str().unwrap_or("");
                        if run["post_run_followup"]==true && !crate::tool_execution::background_capable(name) {return false;}
                        if abilities::is_discovery(name) {
                            let installation=access::Permission::from_policy(policy_decision(&policy,name))
                                .unwrap_or(access::Permission::Deny);
                            access.tool_decision(name,installation,true,true,read_only,true)
                                .permission!=access::Permission::Deny
                        } else {
                            live_tool_decision(&app,&access,name,&policy,&visibility,
                                &ability_state,&run["loaded_abilities"],read_only)
                                .permission!=access::Permission::Deny
                        }
                    }).collect::<Vec<_>>();
                // Build the model menu from the same filtered catalog and schemas
                // used for this caller; the menu never grants execution.
                let mut entries=app.abilities.catalog_with(&policy,&visibility,&ability_state);
                entries.retain_mut(|entry|filter_ability_entry(&app,&access,entry,
                    &policy,&visibility,&ability_state,&run["loaded_abilities"],read_only));
                // Search adds tool markers; live visibility and loaded abilities
                // continue to expose their authorized schemas and guidance.
                let index=abilities::Catalog::model_index(&entries,&schemas);
                let permitted_tools=entries.iter().flat_map(|entry|entry["tools"].as_array().into_iter().flatten())
                    .filter_map(|tool|tool["name"].as_str()).collect::<std::collections::HashSet<_>>();
                let visible=app.abilities.visible_guidance_authorized(&policy,&visibility,
                    &ability_state,
                    |id|access.ability_permission(id)!=access::Permission::Deny,
                    |name|permitted_tools.contains(name),
                    |name|live_tool_decision(&app,&access,name,&policy,&visibility,
                        &ability_state,&run["loaded_abilities"],read_only)
                        .permission!=access::Permission::Deny);
                let extra=tools::core_tool_search::additional_guidance(&app,&schemas,&visibility,&run["loaded_abilities"],&visible);
                let (loaded_guidance,included_skills)=tools::core_skills::context_guidance(&app,&sid,&id,read_only).await?;
                let visible=format!("{visible}{extra}{loaded_guidance}");
                let names=schemas.iter().filter_map(|schema|schema["name"].as_str().map(str::to_owned)).collect::<Vec<_>>();
                let execution=crate::tool_execution::guidance(&visibility,&names)?;
                let guidance=format!("{index}{visible}\n{execution}");
                let mut inclusion = app.abilities.context_inclusion(&entries,&schemas,&visibility,&ability_state,&run["loaded_abilities"],&json!([]));
                if let Some(base)=inclusion.iter_mut().find(|entry|entry.id=="base") {
                    for skill in own_skills["skills"].as_array().into_iter().flatten() {
                        let Some(body)=skill["instructions"].as_str() else {continue;};
                        let included=included_skills.iter().any(|item|item["ability_id"]=="base"
                            && item["kind"]=="own" && item["skill_id"]==skill["name"]);
                        let bytes=if included {body.len()} else {0};
                        base.guidance_bytes+=bytes;
                        base.skills.push(abilities::SkillInclusion {kind:"own".into(),
                            id:skill["name"].as_str().unwrap_or("").into(),mode:"discoverable".into(),included,
                            content_bytes:bytes,reason:if included {"included"} else {"discoverable"}.into()});
                    }
                }
                for asset in &included_skills {
                    if asset["kind"]=="own" {continue;}
                    if let Some(entry)=inclusion.iter_mut().find(|entry|asset["ability_id"]==entry.id) {
                        if let Some(skill)=entry.skills.iter_mut().find(|skill|asset["kind"]==skill.kind&&asset["skill_id"]==skill.id) {
                            if !skill.included {
                                skill.included=true;skill.reason="included".into();
                                skill.content_bytes=asset["bytes"].as_u64().unwrap_or(0) as usize;
                                entry.guidance_bytes+=skill.content_bytes;
                            }
                        }
                    }
                }
                context_assets = Some((asset_key, schemas, guidance, inclusion));
            }
            let (assembled_asset_key, base_schemas, guidance, inclusion) = context_assets.as_ref().unwrap();
            let assembled_asset_key=assembled_asset_key.clone();
            let mut gate_schemas = base_schemas.clone();
            for schema in &mut gate_schemas {
                let name=schema["name"].as_str().unwrap_or("");
                if crate::tool_execution::background_capable(name) && run["post_run_followup"]!=true
                    && crate::tool_execution::resolve(&visibility,name)?.mode=="model" {
                    schema["parameters"]["properties"]["_execution_mode"]=json!({"type":"string","enum":["foreground","background"],"description":"Return a durable task handle immediately when background is selected"});
                }
            }
            let mut mode_content = format!("Current session capability mode: {} (revision {}). Recheck authorization before every tool call.",
                if run["post_run_followup"]==true {"read_only (post-run ceiling)"} else {mode["mode"].as_str().unwrap()}, mode["revision"].as_u64().unwrap());
            if let Some(label)=mode["definition"]["label"].as_str().filter(|_|run["post_run_followup"]!=true) {
                mode_content.push_str(&format!("\nExecution mode: {label}\n{}",mode["definition"]["prompt"].as_str().unwrap_or("")));
            }
            let call_number = run["attempt"].as_u64().unwrap_or(0) + 1;
            run["ability_context"] = serde_json::to_value(abilities::AbilityContext {
                turn:run["turns"].as_u64().unwrap_or(0), call:call_number, abilities:inclusion.clone() })?;
            let mut contributions = vec![context_input::Contribution {
                id: "session_mode".into(), source: "native_session_mode".into(),
                lifetime: context_input::Lifetime::Call, content: mode_content,
                turn: run["turns"].as_u64().unwrap_or(0), call: call_number,
            }];
            // GATE-INJECTION: Rust reads validated gate JSON and adds trusted
            // call-scoped guidance. The model sees private control schemas, but
            // their replies return to Rust for validation and durable storage.
            let before_model = gate_registry::selected(&workflow_policy,
                workflow::Checkpoint::ModelDispatch)?;
            let agent_visibility = gate_controls::agent_visibility(&app.store, &agent_id).await?;
            let promotion_mode=if before_model.iter().any(|item|item.id==gate_registry::MESSAGE_VISIBILITY) {
                agent_visibility.mode
            } else { gate_controls::PromotionMode::None };
            if promotion_mode==gate_controls::PromotionMode::Manual && run["post_run_followup"]!=true {
                let mut config=gate_registry::visibility_config()?;
                if !agent_visibility.guidance.is_empty() {
                    config.guidance.push_str("\nAdditional agent guidance:\n");
                    config.guidance.push_str(&agent_visibility.guidance);
                }
                gate_schemas.push(gate_controls::working_schema(&config));
                contributions.push(context_input::Contribution {
                    id:"gate_message_visibility".into(),source:"native_gate_config".into(),
                    lifetime:context_input::Lifetime::Call,content:config.guidance,
                    turn:run["turns"].as_u64().unwrap_or(0),call:call_number,
                });
            }
            let before_publication = gate_registry::selected(&workflow_policy,
                workflow::Checkpoint::Publication)?;
            if run["post_run_followup"]!=true && before_publication.iter().any(|item|item.id==gate_registry::CLOSING_STATUS) {
                let config=gate_registry::closing_config()?;
                gate_schemas.push(gate_controls::closing_schema(&config));
                // Closing notification guidance accompanies the fixed status protocol.
                let closing=gate_controls::agent_closing(&app.store,&agent_id).await?;
                contributions.push(context_input::Contribution {
                    id:"gate_closing_status".into(),source:"native_gate_config".into(),
                    lifetime:context_input::Lifetime::Call,content:gate_controls::closing_guidance(&config,&closing),
                    turn:run["turns"].as_u64().unwrap_or(0),call:call_number,
                });
            }
            // COMPACTION CONFIG: reread the single external JSON at each call boundary.
            // Agent Identity can override the two history thresholds. Hold the
            // resolved settings through planning and summary validation for this call.
            let defaults = match context_compaction::Config::read(&app.compaction_config_path) {
                Ok(config) => config,
                Err(error) => {
                    run["status"] = json!("paused");
                    run["error"] = json!(format!("Invalid context-compaction.json: {error}"));
                    app.save(&mut run).await?;
                    return Ok(());
                }
            };
            let mut compaction_config = match context_compaction::agent_thresholds(&app.store,&agent_id).await
                .and_then(|settings| match settings {
                    Some(settings) => settings.apply(&defaults),
                    None => Ok(defaults),
                }) {
                Ok(config) => config,
                Err(_) => {
                    run["status"] = json!("paused");
                    run["error"] = json!("Invalid agent compaction thresholds; review Identity settings");
                    app.save(&mut run).await?;
                    return Ok(());
                }
            };
            // SYSTEM-COMPACTION-BUDGET: cap automatic/manual folds by the
            // current pinned System candidate, including after backup selection.
            // Missing System still pauses only when compaction is needed.
            if let Ok(system_run) = system_dispatch_run(&run) {
                let system_selection = provider::ProviderSelection::parse(
                    &system_run["provider_selection"], DEFAULT_AGENT_MODEL)?;
                if let Some(limits) = &system_selection.limits {
                    compaction_config.summary_tokens = compaction_config.summary_tokens.min(limits.output_tokens);
                }
                compaction_config.system_input_budget = Some(
                    loop_runtime::Config::read(&app.runtime_path)?.context
                        .ceiling(&system_selection)?.min(workflow_policy.max_context_bytes));
            }
            // The worker may return more than a model or summarizer can accept.
            // Preserve the encrypted tool interaction and bound only provider replay.
            if context_compaction::project_tool_results(
                run["input"].as_array_mut().unwrap(), compaction_config.tool_result_preview_bytes)?
            {
                context_accounting.reset();
                app.save(&mut run).await?;
            }
            // Project replay only: retain the original journal/input and live policy authority.
            let retention_policy=match crate::tool_output::effective(&app.store,&agent_id).await {
                Ok(policy)=>policy,
                Err(_)=>{run["status"]=json!("paused");run["error"]=json!("Invalid tool output policy; review Identity");app.save(&mut run).await?;return Ok(());}
            };
            let retention_prompts=match crate::tool_output::Prompts::read(&app.store.root) {
                Ok(prompts)=>prompts,
                Err(_)=>{run["status"]=json!("paused");run["error"]=json!("Invalid tool output prompts; review app prompts configuration");app.save(&mut run).await?;return Ok(());}
            };
            run["tool_context_policy"]=serde_json::to_value(&retention_policy)?;
            if crate::retained_recall::restore(&app,&sid,&agent_id,&mut run,&retention_policy).await? {
                context_accounting.reset();
            }
            crate::tool_output::synchronize(&mut run,&retention_policy);
            if retention_policy.enabled {
                gate_schemas.push(crate::tool_output::schema());
                contributions.push(context_input::Contribution{id:"tool_output_retention".into(),
                    source:"native_context_policy".into(),lifetime:context_input::Lifetime::Call,
                    content:retention_prompts.guidance.clone(),turn:run["turns"].as_u64().unwrap_or(0),call:call_number});
            }
            let retention_input=crate::tool_output::project(&run,&retention_policy,&retention_prompts.placeholder)?;
            if retention_input.is_some() {context_accounting.reset();}
            // MODEL-CONTEXT-CEILING: runtime profiles bound the entire envelope;
            // compaction JSON still owns folding policy and summary reserves.
            let model_ceiling = loop_runtime::Config::read(&app.runtime_path)?
                .context.ceiling(&selection)?.min(workflow_policy.max_context_bytes);
            let assembly_request = context_input::Request {
                history: retention_input.as_deref().unwrap_or(run["input"].as_array().unwrap()), base: &instructions,
                guidance, schemas: &gate_schemas, contributions: &contributions,
                identity: context_input::CallIdentity {
                    turn: run["turns"].as_u64().unwrap_or(0),
                    call: run["attempt"].as_u64().unwrap_or(0) + 1,
                },
                selection: &selection, max_bytes: model_ceiling,
                compaction: &compaction_config,
            };
            let prepared = context_assembly::prepare_incremental(assembly_request, &mut context_accounting);
            let assembly_diagnostic=diagnostics::assembly(&app.store,&run,&assembly_request,&prepared);
            let assembled = match prepared {
                Ok(prepared) => prepared,
                Err(error) => {
                    if let Some(report)=assembly_diagnostic {run["latest_context_assembly"]=report;}
                    let (code,stage)=context_assembly::failure_code(&error);
                    run["status"] = json!("paused");
                    run["error"] = json!(format!("Context assembly paused: {stage}/{code}; review this run"));
                    app.save(&mut run).await?;
                    return Ok(());
                }
            };
            // A manual request is only acted on after its tool result has been
            // saved, so no pending call is split across the summary boundary.
            let assembled=if run["manual_compaction_requested"]==true {
                match assembled {
                    context_assembly::Prepared::Ready(ref envelope) => {
                        match context_compaction::plan_manual(run["input"].as_array().unwrap(),
                            envelope.report.history_budget_bytes,&compaction_config) {
                            Ok(Some(plan)) => context_assembly::Prepared::Compact(plan),
                            _ => {
                                run["status"]=json!("paused");
                                run["error"]=json!("Manual compaction has no safe completed boundary; review this run");
                                if let Some(report)=diagnostics::validation(&app.store,&run,"manual_no_safe_boundary","compaction_plan") {run["latest_context_assembly"]=report;}
                                app.save(&mut run).await?;
                                return Ok(());
                            }
                        }
                    }
                    prepared => prepared,
                }
            } else { assembled };
            // The assembler only plans; the loop owns summarization and persistence.
            if let context_assembly::Prepared::Compact(plan) = assembled {
                if let Some(report)=assembly_diagnostic {run["latest_context_assembly"]=report;}
                // Compaction dispatches only its admission-pinned System role and backup cursor.
                let system_run=match system_dispatch_run(&run) {
                    Ok(value)=>value,
                    Err(error)=>{
                        run["status"]=json!("paused");
                        run["error"]=json!(error.to_string());
                        if let Some(report)=diagnostics::validation(&app.store,&run,"system_summary_input_unavailable","summary_input") {run["latest_context_assembly"]=report;}
                        app.save(&mut run).await?;
                        return Ok(());
                    }
                };
                let selection=provider::ProviderSelection::parse(&system_run["provider_selection"],DEFAULT_AGENT_MODEL)?;
                let prompt = match compaction_config.prompt(&plan) {
                    Ok(prompt) => prompt,
                    Err(_) => {
                        run["status"] = json!("paused");
                        run["error"] = json!("Context summary source is too large; review this run");
                        if let Some(report)=diagnostics::validation(&app.store,&run,"summary_source_limit","summary_input") {run["latest_context_assembly"]=report;}
                        app.save(&mut run).await?;
                        return Ok(());
                    }
                };
                // Validate the exact summary envelope against System, including on a backup retry.
                let summary_context=loop_runtime::Config::read(&app.runtime_path)?.context;
                let summary_input=vec![json!({"role":"user","content":prompt})];
                let validation=validate_system_summary_input(&summary_context,&selection,&summary_input,workflow_policy.max_context_bytes);
                let summary_request=context_input::Request{history:&summary_input,base:provider::DEFAULT_INSTRUCTIONS,guidance:"",schemas:&[],contributions:&[],
                    identity:context_input::CallIdentity{turn:run["turns"].as_u64().unwrap_or(0),call:run["attempt"].as_u64().unwrap_or(0)+1},
                    selection:&selection,max_bytes:summary_context.ceiling(&selection)?.min(workflow_policy.max_context_bytes),compaction:&compaction_config};
                if let Some(report)=diagnostics::summary(&app.store,&run,&summary_request,&validation){run["latest_context_assembly"]=report;app.save(&mut run).await?;}
                if validation.is_err(){
                    run["status"]=json!("paused");run["error"]=json!("System summary input exceeds its context budget; review this run");
                    app.save(&mut run).await?;return Ok(());
                }
                drop(_guard);
                let tokens = match app.tokens_for(&selection, &system_run).await {
                    Ok(value) => value,
                    Err(error) => {
                        let _guard = app.gate.lock().await;
                        let mut run = app.run(&id).await?;
                        if run["generation"] == generation && status(&run) == "running" {
                            trusted_model_failure(&app.store, &mut run, &error, true)?;
                            app.save(&mut run).await?;
                        }
                        return Ok(());
                    }
                };
                let (sender, mut receiver) = tokio::sync::mpsc::channel::<String>(32);
                let drain = tokio::spawn(async move { while receiver.recv().await.is_some() {} });
                // Preserve the same shutoff/dispatch ordering as normal model calls.
                let dispatch_guard = app.gate.lock().await;
                let mut current = app.run(&id).await?;
                if current["generation"] != generation
                    || status(&current) != "running"
                    || app.shutoff().await?
                    || cancel.is_cancelled()
                {
                    drop(sender);
                    drop(dispatch_guard);
                    let _ = drain.await;
                    return Ok(());
                }
                if !app.run_model_authorized(&system_dispatch_run(&current)?).await {
                    current["status"]=json!("stopped");
                    current["error"]=json!("Access changed before model dispatch");
                    app.save(&mut current).await?;
                    drop(sender);
                    drop(dispatch_guard);
                    let _=drain.await;
                    return Ok(());
                }
                // A user choice made during credential retrieval is a new envelope,
            // not an access failure. Reassemble before sending anything upstream.
            let live_mode=session_mode(&app,&sid).await?;
            let live_choice=app.store.get(&format!("session_chat_controls:{sid}")).await?;
            if live_mode["revision"]!=mode["revision"]
                || live_choice.as_ref().and_then(|v|v["revision"].as_u64()).unwrap_or(0)!=current["chat_model_revision"].as_u64().unwrap_or(0) {
                drop(sender);drop(dispatch_guard);let _=drain.await;continue;
            }
            if !dispatch_asset_key(&app,&sid,&agent_id,&current["loaded_abilities"],&current["selected_tools"],&current["loaded_skills"]).await
                    .is_ok_and(|key|key==assembled_asset_key) {
                    current["status"]=json!("stopped");
                    current["error"]=json!("Ability access or bundle changed before model dispatch; review and resume");
                    app.save(&mut current).await?;
                    drop(sender);
                    drop(dispatch_guard);
                    let _=drain.await;
                    return Ok(());
                }
                app.run_candidate(&system_dispatch_run(&current)?).await?;
                let mut completion = Box::pin(async {
                    let default_instructions = crate::provider::Provider::configured_base_instructions();
                    let selection = &selection;
                    let tokens = &tokens;
                    let cancel = cancel.clone();
                    let ready = crate::provider::CallDraft::new(
                        selection,
                        summary_input,
                        Vec::new(),
                        &sid,
                        &default_instructions,
                    )
                    .validate()?
                    .bind_credentials(tokens, &cancel)?;
                    app.provider.dispatch(ready, sender, cancel).await
                });
                let mut immediate = None;
                futures_util::future::poll_fn(|cx| {
                    use std::future::Future;
                    if let std::task::Poll::Ready(value) = completion.as_mut().poll(cx) {
                        immediate = Some(value);
                    }
                    std::task::Poll::Ready(())
                })
                .await;
                drop(dispatch_guard);
                let response = if let Some(value) = immediate {
                    value
                } else {
                    tokio::select! {
                        biased;
                        _ = cancel.cancelled() => Err(anyhow!("Context summary cancelled")),
                        result = &mut completion => result
                    }
                };
                drop(completion);
                let _ = drain.await;
                let _guard = app.gate.lock().await;
                let mut run = app.run(&id).await?;
                if run["generation"] != generation || status(&run) != "running" {
                    return Ok(());
                }
                if !app.run_model_authorized(&system_dispatch_run(&run)?).await {
                    run["status"]=json!("stopped");
                    run["error"]=json!("Access changed during context summary");
                    app.save(&mut run).await?;
                    return Ok(());
                }
                if app.shutoff().await? || cancel.is_cancelled() {
                    run["status"] = json!("stopped");
                    run["error"] = json!("Context summary interrupted");
                    app.save(&mut run).await?;
                    return Ok(());
                }
                if let Err(error) = &response {
                    trusted_model_failure(&app.store, &mut run, error, true)?;
                    app.save(&mut run).await?;
                    return Ok(());
                }
                if let Ok(value)=&response{usage::record(&mut run,value);}
                let summary = response.ok().and_then(|value| {
                    value["output"].as_array().map(|output| {
                        output
                            .iter()
                            .filter(|item| item["type"] == "message")
                            .flat_map(|item| item["content"].as_array().into_iter().flatten())
                            .filter_map(|item| item["text"].as_str())
                            .collect::<Vec<_>>()
                            .join("")
                    })
                });
                let compacted = summary.as_deref().ok_or_else(|| context_assembly::Failure::new("summary_missing", "summary_validation").into())
                    .and_then(|summary| context_compaction::apply_summary(retention_input.as_deref().unwrap_or(run["input"].as_array().unwrap()), &plan, summary).map_err(context_assembly::compaction_error));
                let compacted = match compacted {
                    Ok(compacted)=>compacted,
                    Err(error)=>{
                    run["status"] = json!("paused");
                    let (code,stage)=context_assembly::failure_code(&error);
                    run["error"] = json!(format!("Context summary paused: {stage}/{code}; review this run"));
                    if let Some(report)=diagnostics::validation(&app.store,&run,code,stage) {run["latest_context_assembly"]=report;}
                    app.save(&mut run).await?;
                    return Ok(());
                    }
                };
                // COMPACTION PERSISTENCE: Store::save_run below atomically saves run,
                // event and session checkpoint under its existing ownership fences.
                // Original interactions stay intact; continue reassembles before dispatch.
                run["input"] = json!(compacted);
                context_accounting.reset();
                run["compactions"] = json!(run["compactions"].as_u64().unwrap_or(0) + 1);
                run["manual_compaction_requested"] = json!(false);
                run["updated_at"] = json!(now());
                run["error"] = Value::Null;
                let segment_id=format!("{}-{}",id,run["compactions"].as_u64().unwrap());
                app.store
                    .save_run_observed(
                        &mut run,
                        "context_compacted",
                        &json!({"run_id":id,"source_digest":plan.source_digest,
                            "segment_id":segment_id,"source":plan.source,
                            "dropped_items":plan.drop_until,"summary_bytes":summary.unwrap().len()}),
                    )
                    .await?;
                continue;
            }
            let context_assembly::Prepared::Ready(envelope) = assembled else {
                unreachable!("compaction path continues before model dispatch")
            };
            let report = serde_json::to_value(&envelope.report)?;
            let instructions = envelope.instructions;
            // DISPATCH-HISTORY-TRANSFER: fingerprint the borrowed history before
            // metadata mutation, then transfer its allocation only after saves.
            // Durable input remains intact; this local run is reloaded after dispatch.
            let input_digest = digest(envelope.input);
            let schemas = envelope.schemas.to_vec();
            if let Some(report)=assembly_diagnostic {run["latest_context_assembly"]=report;}
            run["attempt"] = json!(run["attempt"].as_u64().unwrap() + 1);
            run["context_assembly_report"] = report;
            run["dispatch_mode"]=json!({"execution_mode":mode["execution_mode"],"mode":mode["mode"],"revision":mode["revision"]});
            run["context_controls"]=json!({"trigger_tokens":compaction_config.trigger_tokens,"tail_tokens":compaction_config.tail_tokens,"characters_per_token":compaction_config.characters_per_token});
            app.save(&mut run).await?;
            let snapshot = workflow::ModelSnapshot {
                input_digest,
                instructions_digest:digest(&instructions),
                schemas_digest:digest(&schemas),
            };
            workflow::record(&mut run, &workflow_policy,
                &workflow::Input::ContextAssembly(snapshot.clone()), workflow::Decision::Pass)?;
            app.save(&mut run).await?;
            drop(_guard);
            let tokens = match app.tokens_for(&selection, &run).await {
                Ok(v) => v,
                Err(error) => {
                    let _guard = app.gate.lock().await;
                    let mut run = app.run(&id).await?;
                    if run["generation"] == generation && status(&run) == "running" {
                        trusted_model_failure(&app.store, &mut run, &error, false)?;
                        app.save(&mut run).await?;
                    }
                    drop(_guard);
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    return Ok(());
                }
            };
            // DISPATCH-HISTORY-TRANSFER: no save may use this consumed local run.
            // Ownership/generation checks below load their own authoritative run.
            let input = match retention_input.map(Value::Array).unwrap_or_else(||std::mem::take(&mut run["input"])) {
                Value::Array(items) => items,
                _ => return Err(anyhow!("Dispatch history must be an array")),
            };
            let (sender, mut receiver) = tokio::sync::mpsc::channel::<String>(32);
            // Provider fragments remain private; only a final chat row is streamed.
            let collector = tokio::spawn(async move { while receiver.recv().await.is_some() {} });

            // First poll initiates local dispatch while holding the same gate as shutoff.
            let dispatch_guard = app.gate.lock().await;
            let mut current = app.run(&id).await?;
            if current["generation"] != generation
                || status(&current) != "running"
                || app.shutoff().await?
                || cancel.is_cancelled()
            {
                drop(sender);
                drop(dispatch_guard);
                let _ = collector.await;
                return Ok(());
            }
            if !app.run_model_authorized(&current).await {
                current["status"]=json!("stopped");
                current["error"]=json!("Access changed before model dispatch");
                app.save(&mut current).await?;
                drop(sender);
                drop(dispatch_guard);
                let _=collector.await;
                return Ok(());
            }
            // Refresh choices which arrived during credential retrieval before dispatch.
            let live_mode=session_mode(&app,&sid).await?;
            let live_choice=app.store.get(&format!("session_chat_controls:{sid}")).await?;
            if live_mode["revision"]!=mode["revision"]
                || live_choice.as_ref().and_then(|v|v["revision"].as_u64()).unwrap_or(0)!=current["chat_model_revision"].as_u64().unwrap_or(0) {
                drop(sender);drop(dispatch_guard);let _=collector.await;continue;
            }
            if !dispatch_asset_key(&app,&sid,&agent_id,&current["loaded_abilities"],&current["selected_tools"],&current["loaded_skills"]).await
                .is_ok_and(|key|key==assembled_asset_key) {
                current["status"]=json!("stopped");
                current["error"]=json!("Ability access or bundle changed before model dispatch; review and resume");
                app.save(&mut current).await?;
                drop(sender);
                drop(dispatch_guard);
                let _=collector.await;
                return Ok(());
            }
            app.run_candidate(&current).await?;
            if current_workflow_policy(&app).await?.revision()? != workflow_policy.revision()? {
                drop(sender);
                drop(dispatch_guard);
                let _ = collector.await;
                continue;
            }
            crate::tool_output::advance(&mut current)?;
            if crate::tool_output::effective(&app.store,&agent_id).await?!=retention_policy
                || crate::tool_output::Prompts::read(&app.store.root)?!=retention_prompts {
                drop(sender);drop(dispatch_guard);let _=collector.await;continue;
            }
            workflow::record(&mut current, &workflow_policy,
                &workflow::Input::ModelDispatch(snapshot), workflow::Decision::Pass)?;
            // Pin publication routing to the call whose text will return. A
            // later settings edit changes the next call, not this response.
            current["promotion_mode"]=json!(promotion_mode);
            app.save(&mut current).await?;
            // DISPATCH-RELEASE-SNAPSHOTS: fencing is committed; do not retain its
            // reconstructed history or consumed local metadata during network waits.
            // Response handling below reloads authoritative state under the gate.
            drop(current);
            drop(run);
            let mut completion = Box::pin(async {
                let selection = &selection;
                let tokens = &tokens;
                let cancel = cancel.clone();
                let ready = crate::provider::CallDraft::new(selection, input, schemas, &sid, &instructions)
                    .validate()?
                    .bind_credentials(tokens, &cancel)?;
                app.provider.dispatch(ready, sender, cancel).await
            });
            let mut immediate = None;
            futures_util::future::poll_fn(|cx| {
                use std::future::Future;
                if let std::task::Poll::Ready(value) = completion.as_mut().poll(cx) {
                    immediate = Some(value);
                }
                std::task::Poll::Ready(())
            })
            .await;
            drop(dispatch_guard);
            let response = if let Some(value) = immediate {
                value
            } else {
                tokio::select! {
                    biased;
                    _=cancel.cancelled()=>Err(anyhow!("Execution cancelled")),
                    r=&mut completion=>r
                }
            };
            drop(completion);
            let _ = collector.await;
            let _guard = app.gate.lock().await;
            let mut run = app.run(&id).await?;
            if run["generation"] != generation || status(&run) != "running" {
                return Ok(());
            }
            if !app.run_model_authorized(&run).await {
                run["status"]=json!("stopped");
                run["error"]=json!("Access changed during model call");
                app.save(&mut run).await?;
                return Ok(());
            }
            if cancel.is_cancelled() || app.shutoff().await? {
                run["status"] = json!("stopped");
                run["error"] = json!("Provider response discarded after cancellation or shutoff");
                app.save(&mut run).await?;
                return Ok(());
            }
            match response {
                Err(error) => {
                    if cancel.is_cancelled() {
                        run["status"] = json!("stopped");
                        run["error"] = json!("Provider request interrupted");
                    } else {
                        trusted_model_failure(&app.store, &mut run, &error, false)?;
                    }
                    let payload=json!({"run_id":id,
                        "failover_state":run["failover_state"],"error":run["error"]});
                    app.store.save_run_observed(&mut run, "model_selection_failure", &payload).await?;
                    drop(_guard);
                    if !cancel.is_cancelled() {
                        tokio::time::sleep(Duration::from_secs(2)).await;
                    }
                    return Ok(());
                }
                Ok(response) => {
                    usage::record(&mut run,&response);
                    if run["failover_state"].is_object() {
                        run["failover_state"]["response_started"] = json!(true);
                    }
                    let mut output=response["output"].as_array()
                        .ok_or_else(||anyhow!("Missing provider output"))?.clone();
                    let controls=output.iter().filter(|v|v["type"]=="function_call"
                        && v["name"]==crate::tool_output::CONTROL_NAME).cloned().collect::<Vec<_>>();
                    for call in controls {
                        let enabled=run["tool_context_policy"]["enabled"]==true;
                        let result=crate::tool_output::control(&mut run,&call,enabled);
                        output.push(json!({"type":"function_call_output","call_id":call["call_id"],
                            "output":result.to_string()}));
                    }
                    let output=&output;
                    let mut calls = output
                        .iter()
                        .filter(|v| v["type"] == "function_call" && v["name"]!=crate::tool_output::CONTROL_NAME)
                        .cloned()
                        .collect::<Vec<_>>();
                    for item in output {
                        run["input"].as_array_mut().unwrap().push(item.clone());
                    }
                    let text = output
                        .iter()
                        .filter(|v| v["type"] == "message")
                        .flat_map(|v| v["content"].as_array().into_iter().flatten())
                        .filter_map(|v| v["text"].as_str())
                        .collect::<Vec<_>>()
                        .join("");
                    let publication_policy = current_workflow_policy(&app).await?;
                    workflow::record(&mut run, &publication_policy,
                        &workflow::Input::ModelOutput { output_digest:digest(&json!(output)) },
                        workflow::Decision::Pass)?;
                    run["turns"] = json!(run["turns"].as_u64().unwrap() + 1);
                    run["attempt"] = json!(0);
                    run["calls"] = json!(calls);
                    run["call_index"] = json!(0);
                    run["phase"] = json!("tools");
                    let closing_enabled=gate_registry::selected(&publication_policy,
                        workflow::Checkpoint::Publication)?.iter()
                        .any(|item|item.id==gate_registry::CLOSING_STATUS);
                    // CLOSING-SAME-RESPONSE: the private status tool may accompany
                    // the final prose. Validate it here, then let the existing
                    // terminal transaction publish that prose exactly once.
                    if run["post_run_followup"]!=true && closing_enabled && calls.len()==1
                        && calls[0]["name"]=="set_closer_status"
                        && !text.trim().is_empty() {
                        let config=gate_registry::closing_config()?;
                        let call_id=calls[0]["call_id"].as_str()
                            .filter(|value|!value.is_empty() && value.len()<=128
                                && !value.chars().any(char::is_control));
                        let arguments=calls[0]["arguments"].as_str()
                            .and_then(|raw|serde_json::from_str::<Value>(raw).ok());
                        let accepted=call_id.and_then(|_|arguments.as_ref())
                            .and_then(|args|gate_controls::closing_status(args,&config).ok());
                        run["closing"]=accepted.clone()
                            .unwrap_or_else(||gate_controls::fallback_status(&config));
                        if let Some(call_id)=call_id {
                            let result=json!({"status":if accepted.is_some() {"ok"} else {"denied"},
                                "output":if accepted.is_some() {"Closing status recorded"}
                                    else {"Closing status invalid; unverified fallback recorded"}});
                            run["input"].as_array_mut().unwrap().push(json!({
                                "type":"function_call_output","call_id":call_id,
                                "output":result.to_string()}));
                        }
                        calls.clear();
                        run["calls"]=json!([]);
                    }
                    if calls.is_empty() {
                        if text.trim().is_empty() {
                            let reasoning_only = !output.is_empty()
                                && output.iter().all(|item| item["type"] == "reasoning");
                            let continuations = run["reasoning_only_completions"].as_u64().unwrap_or(0);
                            let context_only=output.iter().any(|item|item["name"]==crate::tool_output::CONTROL_NAME);
                            if (reasoning_only || context_only) && continuations < 2 {
                                run["reasoning_only_completions"] = json!(continuations + 1);
                                run["phase"] = json!("model");
                                app.store.save_run_observed(&mut run, "model_output", &json!({"output":output})).await?;
                                continue;
                            }
                            let kinds = output.iter().map(|item| item["type"].as_str().unwrap_or("unknown"))
                                .collect::<Vec<_>>();
                            tracing::warn!(run_id=%id, output_types=?kinds,
                                "Provider completed without an assistant message or tool call");
                            run["status"] = json!("failed");
                            run["error"] = json!("Provider completed without a final reply; verify the selected model");
                            app.store
                                .save_run_observed(&mut run, "model_output", &json!({"output":output}))
                                .await?;
                            return Ok(());
                        }
                        if closing_enabled {
                            // CLOSING-FALLBACK: missing or malformed model status
                            // never blocks a valid final reply or starts a repair turn.
                            let config=gate_registry::closing_config()?;
                            let valid=run["closing"]["flag"].as_str()
                                .is_some_and(|flag|config.flags.iter().any(|item|item==flag))
                                && run["closing"]["notification"].as_str()
                                    .is_some_and(|note|!note.trim().is_empty()
                                        && note.chars().count()<=config.notification_max_chars
                                        && note.split_whitespace().count()<=config.notification_max_words);
                            if !valid {
                                run["closing"]=gate_controls::fallback_status(&config);
                            }
                        }
                        if text.len() > publication_policy.max_publication_bytes {
                            workflow::record(&mut run, &publication_policy,
                                &workflow::Input::Publication { text_digest:digest(&json!(text)) },
                                workflow::Decision::Reject { reason: "Publication exceeds configured limit".into() })?;
                            run["status"] = json!("paused");
                            run["error"] = json!("Publication exceeds configured limit; review required");
                            app.store.save_run_observed(&mut run, "model_output", &json!({"output":output})).await?;
                            return Ok(());
                        }
                        let gate_input=gate_protocol::PublicationGateInput {
                            run_id:id.clone(),
                            generation:generation.clone(),
                            policy_revision:publication_policy.revision()?,
                            input_digest:workflow::digest_input(&workflow::Input::Publication {
                                text_digest:digest(&json!(text)) })?,
                            turn:run["turns"].as_u64().unwrap_or(0),
                            call_index:run["call_index"].as_u64().unwrap_or(0),
                            text_bytes:text.len(),
                        };
                        let gate_socket=PathBuf::from(std::env::var("V3_GATE_SOCKET")
                            .unwrap_or_else(|_|gate_protocol::SOCKET_PATH.into()));
                        match gate_protocol::evaluate_publication(
                            &gate_socket,&publication_policy,&gate_input).await? {
                            gate_protocol::GateOutcome::Pass=>{},
                            gate_protocol::GateOutcome::Reject{module_id,reason_code}=>{
                                workflow::record(&mut run,&publication_policy,
                                    &workflow::Input::Publication {
                                        text_digest:digest(&json!(text)) },
                                    workflow::Decision::Reject {
                                        reason:format!("Publication module {module_id} rejected: {reason_code}") })?;
                                run["status"]=json!("paused");
                                run["error"]=json!("Publication rejected by configured gate");
                                app.store.save_run_observed(&mut run,"model_output",&json!({"output":output})).await?;
                                return Ok(());
                            },
                            gate_protocol::GateOutcome::Skipped{module_id,reason_code}=>{
                                run["workflow"]["module_skips"]["publication"]=
                                    json!({"module_id":module_id,"reason_code":reason_code});
                            },
                        }
                        publication::terminal(&app.store, &mut run, output, &text, &publication_policy).await?;
                        return Ok(());
                    }
                    let auto_promote=run["promotion_mode"]=="auto";
                    if auto_promote && output.iter().filter(|item|item["type"]=="message")
                        .any(|item|item["content"].as_array()
                            .map(|parts|parts.iter().filter_map(|part|part["text"].as_str())
                                .map(str::len).sum::<usize>()>16384)
                            .unwrap_or(false)) {
                        // Keep the provider output private and pause explicitly. A
                        // rejected public row must not leave a half-saved model turn.
                        run["status"]=json!("paused");
                        run["error"]=json!("Auto promotion message exceeds 16384 bytes");
                        app.store.save_run_observed(&mut run,"model_output",&json!({"output":output})).await?;
                        return Ok(());
                    }
                    app.store
                        .save_run_observed(&mut run, "model_output", &json!({"output":output,
                            "auto_promote":auto_promote}))
                        .await?;
                }
            }
        } else {
            if run["calls"].as_array().is_some_and(|calls|calls.len()>1
                && !calls.iter().any(|call|matches!(call["name"].as_str(),
                    Some("publish_main_message" | "set_closer_status" | "compact_context" | "v3_build_work" | "build_work")))) {
                // NATIVE-GATE-SERIAL: private controls keep provider order and
                // never enter a parallel batch or the Python tool worker.
                run["closing"]=Value::Null;
                parallel_batch::execute_parallel_batch(&app,&id,&generation,&cancel,&workflow_policy,
                    &access,run,_guard).await?;
                continue;
            }
            let idx = run["call_index"].as_u64().unwrap() as usize;
            let calls = run["calls"].as_array().unwrap();
            if idx >= calls.len() {
                run["phase"] = json!("model");
                app.save(&mut run).await?;
                continue;
            }
            let call = calls[idx].clone();
            let name = call["name"].as_str().unwrap_or("").to_owned();
            let args = match call["arguments"]
                .as_str()
                .map(serde_json::from_str::<Value>)
            {
                Some(Ok(v)) if v.is_object() => v,
                _ => json!({"invalid_arguments":true}),
            };
            if run["post_run_followup"]==true && !crate::tool_execution::background_capable(&name) {
                workflow::record(&mut run,&workflow_policy,&workflow::Input::PreTool(workflow::PreToolInput {
                    name:name.clone(),arguments_digest:digest(&args),policy_version:0,mode_revision:0,visibility_revision:0,ability_state_revision:0,
                }),workflow::Decision::Reject {reason:"Post-run follow-up permits vetted reads only".into()})?;
                run["pending"]=json!({"call_id":call["call_id"]});
                let result=json!({"status":"denied","output":"Post-run follow-up cannot change state, escalate mode, or launch tasks"});
                append_tool(&mut run,&result)?;
                app.store.save_run_observed(&mut run,"tool_result",&json!({"run_id":id,"name":name,"result":result})).await?;
                continue;
            }
            if name != "set_closer_status" {
                // A later tool means the earlier closer no longer describes
                // the eventual final reply.
                run["closing"]=Value::Null;
            }
            if matches!(name.as_str(),"publish_main_message" | "set_closer_status") {
                // NATIVE-CONTROL-DISPATCH: reserved calls never request approval
                // or enter the Python worker. Rust checks activation and arguments.
                let call_id=call["call_id"].as_str().filter(|value|!value.is_empty()
                    && value.len()<=128 && !value.chars().any(char::is_control));
                let Some(call_id)=call_id else {
                    run["status"]=json!("paused");
                    run["error"]=json!("Invalid private control call ID");
                    app.save(&mut run).await?;
                    return Ok(());
                };
                let checkpoint=if name=="publish_main_message" {
                    workflow::Checkpoint::ModelDispatch
                } else {workflow::Checkpoint::Publication};
                let module_id=if name=="publish_main_message" {
                    gate_registry::MESSAGE_VISIBILITY
                } else {gate_registry::CLOSING_STATUS};
                let enabled=gate_registry::selected(&workflow_policy,checkpoint)?
                    .iter().any(|item|item.id==module_id)
                    && (name != "publish_main_message" || run["promotion_mode"]=="manual");
                workflow::record(&mut run,&workflow_policy,
                    &workflow::Input::PreTool(workflow::PreToolInput {
                        name:name.clone(),arguments_digest:digest(&args),
                        policy_version:0,mode_revision:0,visibility_revision:0,
                        ability_state_revision:0,
                    }),workflow::Decision::Pass)?;
                run["pending"]=json!({"call_id":call_id});
                let mut working_text=None;
                let result=if !enabled {
                    run["closing"]=Value::Null;
                    json!({"status":"denied","output":"Private control is not enabled"})
                } else if name=="publish_main_message" {
                    match gate_controls::working_text(&args) {
                        Ok(text)=>{
                            working_text=Some(text);
                            json!({"status":"ok","output":"Working message published"})
                        },
                        Err(_)=>json!({"status":"denied","output":"Invalid working message"}),
                    }
                } else {
                    let config=gate_registry::closing_config()?;
                    match gate_controls::closing_status(&args,&config) {
                        Ok(status)=>{
                            run["closing"]=status;
                            json!({"status":"ok","output":"Closing status recorded"})
                        },
                        Err(_)=>{
                            run["closing"]=gate_controls::fallback_status(&config);
                            json!({"status":"denied","output":"Invalid closing status; unverified fallback recorded"})
                        },
                    }
                };
                append_tool(&mut run,&result)?;
                if let Some(text)=working_text {
                    let message_id=format!("working:{id}:{}:{idx}",run["turns"]);
                    app.store.save_run_observed(&mut run,"working_message",
                        &json!({"id":message_id,"text":text})).await?;
                } else {
                    app.store.save_run_observed(&mut run,"tool_result",
                        &json!({"run_id":id,"name":name,"result":result})).await?;
                }
                continue;
            }
            let (policy, mode_revision, visibility, mut ability_state) =
                ability_controls(&app, &sid).await?;
            let mode=session_mode(&app,&sid).await?;
            let authorization_mode=if run["post_run_followup"]==true {
                json!({"mode":"read_only","execution_mode":"ask","revision":mode["revision"]})
            } else {mode.clone()};
            let read_only=authorization_mode["mode"]=="read_only";
            let mut effective=if abilities::is_discovery(&name) {
                let installation=access::Permission::from_policy(policy_decision(&policy,&name))
                    .unwrap_or(access::Permission::Deny);
                access.tool_decision(&name,installation,true,true,read_only,true)
            } else {
                live_tool_decision(&app,&access,&name,&policy,&visibility,
                    &ability_state,&run["loaded_abilities"],read_only)
            };
            effective=effective.for_execution_mode(&authorization_mode);
            workflow::record(&mut run, &workflow_policy,
                &workflow::Input::PreTool(workflow::PreToolInput {
                    name:name.clone(), arguments_digest:digest(&args),
                    policy_version:policy["version"].as_u64().unwrap_or(0),
                    mode_revision, visibility_revision:visibility["revision"].as_u64().unwrap_or(0),
                    ability_state_revision:ability_state["revision"].as_u64().unwrap_or(0),
                }), workflow::Decision::Pass)?;
            if run["pending"].is_null() {
                run["pending"] = json!({"id":uuid(),"name":name,"arguments":args,"call_id":call["call_id"],"digest":digest(&args),
                    "policy_version":policy["version"],"mode_revision":mode_revision,
                    "visibility_revision":visibility["revision"],
                    "ability_state_revision":ability_state["revision"],
                    "access_revision":access.revision.clone(),
                    "state":"requested","decision":null});
            }
            // A model may narrow its own session without approval. Restoring
            // write authority from read_only always crosses a human checkpoint,
            // even when an administrator otherwise grants automatic use.
            if name=="set_execution_mode" && read_only && {
                let target=args["mode"].as_str().unwrap_or("");
                target=="write" || crate::execution_modes::resolve(&crate::execution_modes::config(&app.store,&agent_id).await?,target)
                    .is_ok_and(|m|m["permission_policy"]=="write")
            }
                && effective.permission!=access::Permission::Deny {
                effective.permission=access::Permission::Ask;
            }
            if run["pending"]["policy_version"] != policy["version"]
                || run["pending"]["access_revision"] != json!(access.revision)
                || run["pending"]["mode_revision"].as_u64().unwrap_or(0) != mode_revision
                || run["pending"]["visibility_revision"].as_u64().unwrap_or(0)
                    != visibility["revision"].as_u64().unwrap()
                || run["pending"]["ability_state_revision"]
                    .as_u64()
                    .unwrap_or(0)
                    != ability_state["revision"].as_u64().unwrap()
            {
                run["pending"]["id"] = json!(uuid());
                run["pending"]["decision"] = Value::Null;
                run["pending"]["access_revision"] = json!(access.revision.clone());
                run["pending"]["policy_version"] = policy["version"].clone();
                run["pending"]["mode_revision"] = json!(mode_revision);
                run["pending"]["visibility_revision"] = visibility["revision"].clone();
                run["pending"]["ability_state_revision"] = ability_state["revision"].clone();
            }
            if abilities::is_discovery(&name) {
                if effective.permission == access::Permission::Deny {
                    let result=json!({"status":"denied","output":"Ability discovery unavailable"});
                    append_tool(&mut run,&result)?;
                    app.store.save_run_observed(&mut run,"tool_result",
                        &json!({"run_id":id,"name":name,"result":result})).await?;
                    continue;
                }
                if effective.permission == access::Permission::Ask
                    && run["pending"]["decision"] != "approve" {
                    run["status"]=json!("waiting_approval");
                    run["pending"]["state"]=json!("waiting");
                    let mut payload=run["pending"].clone();
                    payload["run_id"]=json!(id);
                    app.store.save_run_observed(&mut run,"approval_required",&payload).await?;
                    return Ok(());
                }
                let mut result = app.abilities.discover_with(
                    &name,
                    &args,
                    &mut run,
                    &policy,
                    &visibility,
                    &ability_state,
                );
                if name=="list_abilities" {
                    if let Some(entries)=result["abilities"].as_array_mut() {
                        entries.retain_mut(|entry|filter_ability_entry(&app,&access,entry,
                            &policy,&visibility,&ability_state,&run["loaded_abilities"],read_only));
                    }
                } else if matches!(result["status"].as_str(), Some("ok" | "already_available")) {
                    // Authorization uses the fresh catalog, never the loader acknowledgment.
                    let allowed=app.abilities.catalog_with(&policy,&visibility,&ability_state)
                        .into_iter().find(|entry|entry["id"]==args["ability_id"])
                        .is_some_and(|mut entry|filter_ability_entry(&app,&access,&mut entry,
                            &policy,&visibility,&ability_state,&run["loaded_abilities"],read_only));
                    if !allowed {
                        if let Some(loaded)=run["loaded_abilities"].as_array_mut() {
                            loaded.retain(|item|item!=&args["ability_id"]);
                        }
                        result=json!({"status":"denied","output":"Ability unavailable under current access"});
                    }
                }
                if name == "load_ability" && result["status"] == "ok" {
                    let ability_id = args["ability_id"].as_str().unwrap();
                    if !ability_state["active"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|v| v.as_str() == Some(ability_id))
                    {
                        ability_state["active"]
                            .as_array_mut()
                            .unwrap()
                            .push(json!(ability_id));
                        ability_state["revision"] =
                            json!(ability_state["revision"].as_u64().unwrap() + 1);
                        app.store
                            .put(&key("session_ability_state", &sid), &ability_state)
                            .await?;
                    }
                }
                append_tool(&mut run, &result)?;
                app.store
                    .save_run_observed(
                        &mut run,
                        "tool_result",
                        &json!({"run_id":id,"name":name,"result":result}),
                    )
                    .await?;
                continue;
            }
            if effective.permission == access::Permission::Deny
                || run["pending"]["decision"] == "deny"
                || args.get("invalid_arguments").is_some()
            {
                run["workflow"]["receipts"]["pre_tool"]["result"] =
                    json!(workflow::Decision::Reject {
                        reason: "Tool invocation denied by policy or invalid arguments".into()
                    });
                let result = json!({"status":"denied","output":"Tool invocation denied by policy or invalid arguments"});
                append_tool(&mut run, &result)?;
                app.store
                    .save_run_observed(
                        &mut run,
                        "tool_result",
                        &json!({"run_id":id,"name":name,"result":result}),
                    )
                    .await?;
                continue;
            }
            if effective.permission == access::Permission::Ask && run["pending"]["decision"] != "approve" {
                run["status"] = json!("waiting_approval");
                run["pending"]["state"] = json!("waiting");
                let mut payload = run["pending"].clone();
                payload["run_id"] = json!(id);
                app.store
                    .save_run_observed(&mut run, "approval_required", &payload)
                    .await?;
                return Ok(());
            }
            if run["pending"]["digest"] != digest(&args) {
                return Err(anyhow!("Tool argument digest mismatch"));
            }
            run["pending"]["state"] = json!("executing");
            run["pending"]["human_approved"] = json!(run["pending"]["decision"] == "approve");
            run["pending"]["decision"] = json!("consumed");
                let valid=app.store.live_access(app.store.current_user_id(),&agent_id).await
                .is_ok_and(|current|current.admitted()
                    && json!(current.revision)==run["pending"]["access_revision"])
                && app.run_model_authorized(&run).await;
            if !valid {
                run["pending"]["state"]=json!("requested");
                run["pending"]["decision"]=Value::Null;
                run["status"]=json!("stopped");
                run["error"]=json!("Access changed before tool dispatch");
                app.save(&mut run).await?;
                return Ok(());
            }
            app.save(&mut run).await?;
            drop(_guard);
            let result = tools::call(
                app.clone(), sid.clone(), id.clone(), generation.clone(), name.clone(), args.clone(),
                run["pending"]["id"].as_str().unwrap().to_owned(),
                run["pending"]["policy_version"].as_u64().unwrap_or(0),
                cancel.clone(),
            ).await;
            let _guard = app.gate.lock().await;
            let mut run = app.run(&id).await?;
            if run["generation"] != generation {
                return Ok(());
            }
            if status(&run) != "running" {
                if !terminal(status(&run)) && matches!(name.as_str(),"v3_build_work"|"build_work") && args["action"]=="finish"
                    && run["pending"]["state"]=="executing"
                    && result.as_ref().map_or(true,|value|value["status"]=="success") {
                    // A pause or revocation may race the broker acknowledgement.
                    // Keep the job reference; a queued receipt is not the final tool result.
                    run["pending"]["external_job"]=json!({"kind":"v3_build",
                        "task_id":args["task_id"],"expected_inputs_sha256":args["expected_inputs_sha256"],
                        "job_id":result.as_ref().ok().map(|value|value["output"]["job_id"].clone()),
                        "submitted_at":now()});
                    run["uncertain"]=json!(false);
                    let restricted=run["error"]=="Cancelled by user"
                        || run["error"].as_str().unwrap_or("").starts_with("Access changed")
                        || app.shutoff().await?;
                    run["status"]=json!(if restricted {"stopped"}else{"waiting_external"});
                    app.save(&mut run).await?;
                    return Ok(());
                }
                // An already committed result can resolve uncertainty without restarting the run.
                if reconcile_late_tool_result(&mut run, result)? {
                    app.save(&mut run).await?;
                }
                return Ok(());
            }
            match result {
                Ok(value) => {
                    if matches!(name.as_str(),"v3_build_work"|"build_work") && args["action"]=="finish" && value["status"]=="success"
                        && value["output"]["status"]=="closed" {
                        // The broker owns the job across our own container replacement.
                        // Keep this model call pending; recovery reads its receipt and
                        // returns one result under the original call_id.
                        run["pending"]["external_job"]=json!({"kind":"v3_build",
                            "task_id":args["task_id"],"expected_inputs_sha256":args["expected_inputs_sha256"],
                            "job_id":value["output"]["job_id"],"submitted_at":now()});
                        run["status"]=json!("waiting_external");
                        run["error"]=Value::Null;
                        app.save(&mut run).await?;
                        return Ok(());
                    }
                    let uncertain = value["uncertain"]==true || matches!(
                        value["status"].as_str(),
                        Some("cancelled" | "timeout" | "uncertain")
                    );
                    if uncertain && !tools::is_read(&name) {
                        record_tool_interruption(&mut run, value["status"].as_str().unwrap_or("uncertain"));
                        run["uncertain"] = json!(true);
                        run["status"] = json!("recovery");
                        run["error"] = json!("Tool interrupted; review its outcome");
                        app.store.save_run_observed(&mut run, "run_status", &json!({"run_id":id})).await?;
                        return Ok(());
                    }
                    append_tool(&mut run, &value)?;
                    if name=="compact_context" && value["status"]=="success"
                        && value["control"]=="manual_compaction_requested" {
                        run["manual_compaction_requested"]=json!(true);
                    }
                    app.store
                        .save_run_observed(
                            &mut run,
                            "tool_result",
                            &json!({"run_id":id,"name":name,"result":value}),
                        )
                        .await?;
                }
                Err(_) => {
                    if matches!(name.as_str(),"v3_build_work"|"build_work") && args["action"]=="finish" {
                        // The request may have reached the broker before the socket
                        // closed. Reconcile by task ID; never submit finish again.
                        run["pending"]["external_job"]=json!({"kind":"v3_build",
                            "task_id":args["task_id"],"expected_inputs_sha256":args["expected_inputs_sha256"],
                            "submitted_at":now()});
                        run["status"]=json!("waiting_external");
                        run["error"]=Value::Null;
                        app.save(&mut run).await?;
                        return Ok(());
                    }
                    if let Some(receipt)=record_interrupted_tool(&mut run,&name)? {
                        app.store.save_run_observed(&mut run,"tool_result",
                            &json!({"run_id":id,"name":name,"result":receipt})).await?;
                    } else {
                        app.store.save_run_observed(&mut run, "run_status", &json!({"run_id":id})).await?;
                    }
                    return Ok(());
                }
            }
        }
    }
}

#[cfg(test)]
mod interrupted_tool_tests {
    use super::*;

    fn pending(name:&str)->Value {
        json!({"id":"run-1","session_id":"session-1","generation":"generation-1",
            "status":"running","turns":1,"call_index":0,"uncertain":false,
            "input":[{"type":"function_call","call_id":"call-1","name":name,"arguments":"{}"}],
            "pending":{"name":name,"call_id":"call-1","state":"executing"},
            "workflow":{"receipts":{"pre_tool":{"checkpoint":"pre_tool",
                "generation":"generation-1","policy_revision":"policy-1","input_digest":"digest",
                "turn":1,"call_index":0,"result":{"decision":"pass"}}}}})
    }

    #[test]
    fn interruption_reason_is_bounded_and_preserves_call() {
        for (reason, expected) in [("timeout","timeout"),("cancelled","cancelled"),
            ("transport_error","transport_error"),("PRIVATE_ERROR","uncertain")] {
            let mut run = pending("shell");
            record_tool_interruption(&mut run, reason);
            assert_eq!(run["pending"]["interruption"]["reason"], expected);
            assert!(run["pending"]["interruption"]["recorded_at"].as_i64().is_some());
            assert_eq!(run["pending"]["call_id"], "call-1");
            assert!(!run.to_string().contains("PRIVATE_ERROR"));
            assert_eq!(run["input"].as_array().unwrap().len(), 1);
        }
    }
    #[test]
    fn interrupted_read_gets_matching_safe_result_and_fails_closed() {
        let mut run=pending("file_read");
        let result=record_interrupted_tool(&mut run,"file_read").unwrap().unwrap();
        assert_eq!(run["status"],"failed");
        assert_eq!(run["error"],"Read tool interrupted");
        assert!(run["pending"].is_null());
        assert_eq!(run["uncertain"],false);
        assert_eq!(result["status"],"error");
        assert!(result["output"].as_str().unwrap().contains("no data was returned"));
        let input=run["input"].as_array().unwrap();
        assert_eq!(input.len(),2);
        assert_eq!(input[0]["type"],"function_call");
        assert_eq!(input[1]["type"],"function_call_output");
        assert_eq!(input[0]["call_id"],input[1]["call_id"]);
        let receipt:Value=serde_json::from_str(input[1]["output"].as_str().unwrap()).unwrap();
        assert_eq!(receipt,result);
        let workflow_result=&run["workflow"]["receipts"]["tool_result"];
        assert_eq!(workflow_result["checkpoint"],"tool_result");
        assert!(!workflow_result["input_digest"].as_str().unwrap_or("").is_empty());
    }

    #[test]
    fn interrupted_write_stays_uncertain_without_a_result_or_retry() {
        let mut run=pending("file_write");
        assert!(record_interrupted_tool(&mut run,"file_write").unwrap().is_none());
        assert_eq!(run["status"],"recovery");
        assert_eq!(run["uncertain"],true);
        assert_eq!(run["input"].as_array().unwrap().len(),1);
        assert_eq!(run["pending"]["call_id"],"call-1");
        assert_eq!(run["workflow"]["receipts"]["tool_result"],Value::Null);
    }

    #[tokio::test]
    async fn interrupted_read_persists_a_paired_result_in_next_context() {
        let (_root,app)=crate::runtime_tests::fixture().await;
        let sid=crate::runtime_tests::conversation(&app).await;
        let rid=crate::runtime_tests::queued(&app,&sid).await;
        let generation=uuid();
        let mut run=app.run(&rid).await.unwrap();
        assert!(app.store.claim_run(&mut run,"interrupted-read-test",&generation).await.unwrap());

        let call=json!({"type":"function_call","call_id":"call-interrupted-read",
            "name":"file_read","arguments":"{}"});
        run["input"].as_array_mut().unwrap().push(call.clone());
        run["turns"]=json!(1);
        run["pending"]=json!({"name":"file_read","call_id":"call-interrupted-read","state":"executing"});
        let policy=workflow::Policy::resolve(None).unwrap();
        workflow::record(&mut run,&policy,&workflow::Input::PreTool(workflow::PreToolInput {
            name:"file_read".into(),arguments_digest:"fixture".into(),policy_version:1,
            mode_revision:0,visibility_revision:0,ability_state_revision:0,
        }),workflow::Decision::Pass).unwrap();
        app.store.save_run_observed(&mut run,"model_output",&json!({"output":[call]})).await.unwrap();

        let result=record_interrupted_tool(&mut run,"file_read").unwrap().unwrap();
        app.store.save_run_observed(&mut run,"tool_result",
            &json!({"run_id":rid,"name":"file_read","result":result})).await.unwrap();

        let context=app.store.context(&sid).await.unwrap();
        let call_index=context.iter().position(|item|item["call_id"]=="call-interrupted-read").unwrap();
        assert_eq!(context[call_index]["type"],"function_call");
        assert_eq!(context[call_index+1]["type"],"function_call_output");
        assert_eq!(context[call_index+1]["call_id"],"call-interrupted-read");
        assert!(context[call_index+1]["output"].as_str().unwrap().contains("no data was returned"));
        assert!(context_compaction::plan_manual(&context,262144,
            &context_compaction::Config::configured().unwrap()).is_ok());
    }
}
