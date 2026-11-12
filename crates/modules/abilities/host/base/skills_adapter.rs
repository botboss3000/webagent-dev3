//! Read bundled guidance by exact catalog ID under fresh caller/agent policy.
//! Loading is evidence only: it never activates an ability or grants execution.
#[cfg(test)]
use webagent_accounts_host::AccountsStore;
// @code-index {"version":1,"feature":"live-skill-selection","aliases":["live context","source correspondence"],"purpose":"Native current-source skill reads and identity-only conversation receipts. Encrypted caller/agent policy remains authoritative input; fixed scope and correspondence checks are not bypass settings.","span_lines":80,"configuration":{"status":"not_applicable","references":[],"opportunities":[],"rationale":"Private scope, live-authority fences and identity-only receipt persistence are mandatory. Skill content and mode are current caller inputs, not configurable alternatives to these safety checks."}}
use crate::{ability_controls, live_tool_decision, session_agent_id, session_mode, App};
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

/// Route only fixed native skill discovery operations.
pub fn owns(name: &str) -> bool {
    matches!(name, "list_skills" | "load_skill")
}
/// Expose bounded selectors and metadata-only load acknowledgments.
pub fn schemas() -> Vec<Value> {
    let id = json!({"type":"string","minLength":1,"maxLength":64,"pattern":"^[a-z0-9_]+$"});
    vec![
        json!({"type":"function","name":"list_skills","description":"List inactive discoverable skills; include_available also shows visible and already loaded guidance",
            "parameters":{"type":"object","properties":{"ability_id":id,"query":{"type":"string","maxLength":200},"include_available":{"type":"boolean"}},"required":["ability_id"],"additionalProperties":false}}),
        json!({"type":"function","name":"load_skill","description":"Load discoverable guidance for this conversation. Returns only acknowledgment and metadata; full guidance is supplied in the next model context. Does not activate abilities or tools",
            "parameters":{"type":"object","properties":{"ability_id":id,"skill_id":id,
                "kind":{"type":"string","enum":["ability","tool","selectable","own"]},
                "expected_catalog_version":{"type":"string","minLength":64,"maxLength":64,"pattern":"^[a-f0-9]{64}$"}},
                "required":["ability_id","kind","skill_id"],"additionalProperties":false}}),
    ]
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListArgs {
    ability_id: String,
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    include_available: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LoadArgs {
    ability_id: String,
    kind: String,
    skill_id: String,
    #[serde(default)]
    expected_catalog_version: Option<String>,
}
/// Reject paths and accept only bounded catalog identifiers.
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}
/// Match task words against current skill metadata with the catalog lexical matcher.
fn skill_matches(skill: &Value, query: &str) -> bool {
    let words = crate::abilities::search_words(query);
    if words.is_empty() {
        return false;
    }
    let text = format!(
        "{} {} {}",
        skill["skill_id"].as_str().unwrap_or(""),
        skill["summary"].as_str().unwrap_or(""),
        skill["description"].as_str().unwrap_or("")
    );
    let candidates = crate::abilities::search_words(&text);
    words.iter().any(|word| {
        candidates
            .iter()
            .any(|candidate| crate::abilities::near_word(word, candidate))
    })
}
/// Rank skills for a task query: matcher scores when it answers, else the lexical matcher.
async fn ranked_skills(query: &str, skills: Vec<Value>) -> (Vec<Value>, &'static str) {
    use webagent_abilities::tool_match;
    let list = |skill: &Value, key: &str| {
        skill[key]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|item| item.as_str().map(str::to_owned))
            .collect::<Vec<_>>()
    };
    let keywords: Vec<Vec<String>> = skills.iter().map(|skill| list(skill, "keywords")).collect();
    let items = skills
        .iter()
        .map(|skill| {
            tool_match::texts(
                skill["skill_id"].as_str().unwrap_or(""),
                &format!(
                    "{} {}",
                    skill["summary"].as_str().unwrap_or(""),
                    skill["description"].as_str().unwrap_or("")
                ),
                &list(skill, "examples"),
            )
        })
        .collect();
    let (scores, by) = match crate::api::inference::rank(query, items).await {
        Some(scores) if scores.len() == skills.len() => (scores, "matcher"),
        _ => {
            tracing::warn!("Matcher unavailable; list_skills falls back to word matching");
            (
                skills
                    .iter()
                    .map(|skill| {
                        if skill_matches(skill, query) {
                            tool_match::WEAK
                        } else {
                            0.0
                        }
                    })
                    .collect(),
                "words",
            )
        }
    };
    let candidates: Vec<(&str, &[String])> = skills
        .iter()
        .zip(&keywords)
        .map(|(skill, keywords)| (skill["skill_id"].as_str().unwrap_or(""), &keywords[..]))
        .collect();
    let rows = tool_match::rank(query, &candidates, &scores, tool_match::SKILL_BEST)
        .into_iter()
        .map(|ranked| {
            let mut row = skills[ranked.index].clone();
            row["score"] = json!((ranked.score * 100.0).round() / 100.0);
            row["best_match"] = json!(ranked.best);
            if ranked.weak {
                row["match"] = json!("weak");
            }
            row
        })
        .collect();
    (rows, by)
}
/// Constrain kind, private scope and optional catalog receipt version before lookup.
fn validate_load(args: &LoadArgs) -> Result<()> {
    if !valid_id(&args.ability_id)
        || !valid_id(&args.skill_id)
        || !matches!(
            args.kind.as_str(),
            "ability" | "tool" | "selectable" | "own"
        )
        || args.kind == "own" && args.ability_id != "base"
        || args.expected_catalog_version.as_ref().is_some_and(|s| {
            s.len() != 64
                || !s
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
    {
        bail!("Invalid bundled skill selector");
    }
    Ok(())
}
struct View {
    app: App,
    access: crate::access_policy::LiveAccess,
    policy: Value,
    visibility: Value,
    state: Value,
    loaded: Value,
    read_only: bool,
    own: Value,
    loaded_skills: Value,
    version: String,
    fingerprint: String,
}
/// Resolve private caller/agent scope and fingerprint current source, selectors and live authority.
async fn view(app: &App, sid: &str, run_id: &str, name: &str) -> Result<View> {
    if crate::shared_chat::actor(app) != app.store.current_user_id() {
        bail!("Shared chat membership does not authorize caller-owned private records");
    }

    // Resolve the session in the authenticated user's plane before its agent.
    let agent = session_agent_id(app, sid).await?;
    let mut scoped = app.clone();
    scoped.store = app.store.for_agent(&agent).await?;
    let run = scoped.run(run_id).await?;
    if run["session_id"] != sid || run["agent_id"].as_str().unwrap_or("shared_default") != agent {
        bail!("Run does not belong to current session and agent");
    }
    let loaded = run["loaded_abilities"].clone();
    let loaded_skills = run["loaded_skills"].clone();
    let (policy, mode_revision, visibility, state) = ability_controls(&scoped, sid).await?;
    // Conversation selectors activate only this read's projection, never saved
    // session state. Current Off, suppression and access checks still decide.
    let state = scoped
        .abilities
        .selected_state(&visibility, &state, &loaded);
    let mode = session_mode(&scoped, sid).await?;
    let read_only = mode["mode"] == "read_only";
    let access = crate::shared_chat::live_access(
        &scoped.store,
        crate::shared_chat::actor(&scoped),
        sid,
        &agent,
    )
    .await?;
    if !access.admitted()
        || live_tool_decision(
            &scoped,
            &access,
            name,
            &policy,
            &visibility,
            &state,
            &loaded,
            read_only,
        )
        .permission
            == crate::access::Permission::Deny
    {
        bail!("Bundled skill tool unavailable under current access");
    }
    // Own-skill revision is part of the read fingerprint; a skill edit
    // invalidates outstanding list/load results before their text is returned.
    let own = scoped.store.list_own_skills().await?;
    let bundle_version = scoped.abilities.live_version();
    let version = crate::digest(&json!([bundle_version, own["revision"]]));
    let fingerprint = crate::digest(&json!([
        version,
        policy,
        mode_revision,
        visibility,
        state,
        loaded,
        loaded_skills,
        mode,
        access.context_fingerprint()
    ]));
    Ok(View {
        app: scoped,
        access,
        policy,
        visibility,
        state,
        loaded,
        read_only,
        own,
        loaded_skills,
        version,
        fingerprint,
    })
}
/// Compare stable identity; observational body hashes and revisions never pin later reads.
fn same_asset(left: &Value, right: &Value) -> bool {
    left["ability_id"] == right["ability_id"]
        && left["kind"] == right["kind"]
        && left["skill_id"] == right["skill_id"]
}
/// Describe current exposure or retained selection after authorized source resolution.
fn mark_availability(asset: &mut Value, loaded: &Value, tools: &Value) {
    let tool_selected = asset["kind"] == "tool"
        && tools.as_array().is_some_and(|items| {
            items.iter().any(|item| {
                item.as_str()
                    == Some(format!("tool:{}", asset["skill_id"].as_str().unwrap_or("")).as_str())
            })
        });
    let available = asset["availability"] == "visible"
        || asset["availability"] == "activated"
        || tool_selected
        || loaded
            .as_array()
            .is_some_and(|items| items.iter().any(|item| same_asset(item, asset)));
    if available && asset["availability"] != "visible" {
        asset["availability"] = json!("activated");
    }
    if !available {
        asset["availability"] = json!("discoverable");
    }
    asset["requires_activation"] = json!(!available);
}
/// Project a current selectable agent record with observation metadata and an untrusted body.
fn own_asset(record: &Value) -> Result<Value> {
    // Own-skill storage currently supports selectable mode only, never pinned text.
    if record["mode"] != "selectable" {
        bail!("Current-agent skill unavailable");
    }
    let text = record["instructions"]
        .as_str()
        .context("Invalid own-skill instructions")?;
    use sha2::{Digest, Sha256};
    Ok(
        json!({"ability_id":"base","kind":"own","skill_id":record["name"],
        "mode":"discoverable","availability":"discoverable","requires_activation":true,
        "revision":record["revision"],"bytes":text.len(),
        "sha256":hex::encode(Sha256::digest(text.as_bytes())),"text":text,
        "source_kind":"untrusted_agent_skill","activates":false}),
    )
}
/// Check tool-guidance read eligibility without persisting activation or granting execution.
fn skill_tool_allowed(current: &View, tool: &str) -> bool {
    // This selector permits reading tool guidance; it grants no execution state.
    let mut loaded = current.loaded.as_array().cloned().unwrap_or_default();
    loaded.push(json!(format!("tool:{tool}")));
    live_tool_decision(
        &current.app,
        &current.access,
        tool,
        &current.policy,
        &current.visibility,
        &current.state,
        &json!(loaded),
        current.read_only,
    )
    .permission
        != crate::access::Permission::Deny
}
/// Read current bundled or own guidance through catalog and private-record authority gates.
fn load_asset(current: &View, query: &LoadArgs) -> Result<Value> {
    let allow_ability =
        |id: &str| current.access.ability_permission(id) != crate::access::Permission::Deny;
    let allow_tool = |tool: &str| skill_tool_allowed(current, tool);
    if query.kind == "own" {
        current.app.abilities.bundled_skill_list(
            "base",
            &current.visibility,
            &current.state,
            allow_ability,
            allow_tool,
        )?;
        let record = current.own["skills"]
            .as_array()
            .context("Invalid own-skill list")?
            .iter()
            .find(|record| record["name"] == query.skill_id)
            .context("Current-agent skill unavailable")?;
        own_asset(record)
    } else {
        current.app.abilities.bundled_skill_load(
            &query.ability_id,
            &query.kind,
            &query.skill_id,
            &current.visibility,
            &current.state,
            allow_ability,
            allow_tool,
        )
    }
}
/// Persist selectors only after the caller fences the current native execution.
pub(crate) fn apply_selection(run: &mut Value, result: &Value) {
    if run["pending"]["name"] != "load_skill" || result["status"] != "success" {
        return;
    }
    let asset = &result["output"];
    let Ok(evidence) = serde_json::from_value::<SelectionEvidence>(asset.clone()) else {
        return;
    };
    let args = run["pending"]["arguments"].clone();
    let selector = if let Some(raw) = args.as_str() {
        serde_json::from_str::<LoadArgs>(raw)
    } else {
        serde_json::from_value::<LoadArgs>(args)
    };
    let Ok(selector) = selector else {
        return;
    };
    if validate_load(&selector).is_err()
        || evidence.activates
        || evidence.ability_id != selector.ability_id
        || evidence.kind != selector.kind
        || evidence.skill_id != selector.skill_id
        || !valid_hash(&evidence.sha256)
        || evidence.status != "loaded"
        || evidence.bytes == 0
        || evidence.context_source != "live_skill_catalog"
        || !valid_hash(&evidence.catalog_version)
        || !(evidence.source_kind == "untrusted_bundled_guidance"
            && evidence.kind != "own"
            && evidence.revision.is_none()
            || evidence.source_kind == "untrusted_agent_skill"
                && evidence.kind == "own"
                && evidence.revision.is_some_and(|r| r > 0))
    {
        return;
    }
    let mut selections = run["loaded_skills"].as_array().cloned().unwrap_or_default();
    if let Some(index) = selections.iter().position(|item| same_asset(item, asset)) {
        // Older runs may carry observational receipt fields. Normalize the same
        // identity in place so neither its order nor its future body is pinned.
        selections[index] = json!({"ability_id":asset["ability_id"],"kind":asset["kind"],
            "skill_id":asset["skill_id"]});
        run["loaded_skills"] = json!(selections);
        return;
    }
    selections.retain(|item| {
        !(item["ability_id"] == asset["ability_id"]
            && item["kind"] == asset["kind"]
            && item["skill_id"] == asset["skill_id"])
    });
    // Receipt hashes/revisions attest this read; they never pin future guidance.
    selections.push(
        json!({"ability_id":asset["ability_id"],"kind":asset["kind"],
        "skill_id":asset["skill_id"]}),
    );
    run["loaded_skills"] = json!(selections);
}
/// Preload the tools and skills a new user message clearly needs and keep the next few as
/// call-scoped suggestions. Best effort: any failure preloads nothing and never stops the run.
/// The engine stages `preload.selectors` as ordinary activation selectors, so preloads persist
/// like `load_tool` and `load_skill`; live policy, approval and read-only checks still decide.
pub(crate) async fn preload(app: &App, sid: &str, run: &mut Value) {
    if run["preload_checked"] == true {
        return;
    }
    run["preload_checked"] = json!(true);
    let text = crate::optimization_prompt::user_text(run);
    if text.trim().is_empty() || text.trim_start().starts_with('/') {
        run["preload"] = json!({"skipped":"no_request_text"});
        return;
    }
    let picked = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        preload_candidates(app, sid, run, &text),
    )
    .await;
    let (preload, suggest) = match picked {
        Ok(Ok(Some(picked))) => picked,
        other => {
            let reason = match other {
                Err(_) => "timeout",
                Ok(Err(_)) => "catalog_unavailable",
                _ => "matcher_unavailable",
            };
            tracing::warn!(reason, "Preload skipped");
            run["preload"] = json!({"skipped":reason});
            return;
        }
    };
    let mut selectors = Vec::new();
    for item in &preload {
        let selector = if item.kind == "tool" {
            json!({"type":"tool","tool_name":item.name})
        } else {
            let (ability, kind, skill) = split_skill_key(&item.name);
            json!({"type":"skill","ability_id":ability,"kind":kind,"skill_id":skill})
        };
        if webagent_assets_contracts::validate_selector(&selector).is_ok() {
            selectors.push(selector);
        }
    }
    let row = |c: &webagent_abilities::tool_match::Candidate| json!({"kind":c.kind,"name":c.name,"score":(c.score * 100.0).round() / 100.0});
    run["preload"] = json!({"preloaded":preload.iter().map(row).collect::<Vec<_>>(),
        "suggested":suggest.iter().map(row).collect::<Vec<_>>(),"selectors":selectors});
}

/// Skill candidates are keyed `ability:kind:skill_id`.
fn split_skill_key(key: &str) -> (&str, &str, &str) {
    let mut parts = key.splitn(3, ':');
    (
        parts.next().unwrap_or(""),
        parts.next().unwrap_or(""),
        parts.next().unwrap_or(""),
    )
}

/// Score every permitted tool and ability or selectable skill against each part of the message,
/// keeping each one's best score. `None` when the matcher does not answer.
async fn preload_candidates(
    app: &App,
    sid: &str,
    run: &Value,
    text: &str,
) -> Result<
    Option<(
        Vec<webagent_abilities::tool_match::Candidate>,
        Vec<webagent_abilities::tool_match::Candidate>,
    )>,
> {
    use webagent_abilities::tool_match::{self, Candidate};
    let agent = session_agent_id(app, sid).await?;
    let host = crate::tools::native_host::Host(app);
    let tools =
        crate::tools::native::tool_search::eligible(&host, sid, &agent, &run["loaded_abilities"])
            .await?;
    let mut skills = Vec::new();
    if let Ok(current) = view(
        app,
        sid,
        run["id"].as_str().unwrap_or_default(),
        "list_skills",
    )
    .await
    {
        let allow_ability =
            |id: &str| current.access.ability_permission(id) != crate::access::Permission::Deny;
        let allow_tool = |tool: &str| skill_tool_allowed(&current, tool);
        for entry in
            current
                .app
                .abilities
                .catalog_with(&current.policy, &current.visibility, &current.state)
        {
            let Some(id) = entry["id"].as_str() else {
                continue;
            };
            // Tool skills arrive with their tool, so only ability and selectable skills count.
            for mut skill in current
                .app
                .abilities
                .bundled_skill_list(
                    id,
                    &current.visibility,
                    &current.state,
                    allow_ability,
                    allow_tool,
                )
                .unwrap_or_default()
                .into_iter()
                .filter(|skill| skill["kind"] != "tool")
            {
                mark_availability(&mut skill, &current.loaded_skills, &current.loaded);
                skills.push(skill);
            }
        }
    }
    let list = |value: &Value, key: &str| {
        value[key]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|item| item.as_str().map(str::to_owned))
            .collect::<Vec<_>>()
    };
    // (kind, key, name for exact matching, keywords, loadable, texts)
    let mut items = Vec::new();
    for tool in &tools {
        let name = tool["name"].as_str().unwrap_or_default();
        let (examples, keywords) = tool_match::hints(name);
        let texts = tool_match::texts(
            name,
            tool["description"].as_str().unwrap_or_default(),
            &examples,
        );
        let loadable = tool["requires_activation"] == true;
        items.push((
            "tool",
            name.to_owned(),
            name.to_owned(),
            keywords,
            loadable,
            texts,
        ));
    }
    for skill in &skills {
        let id = skill["skill_id"].as_str().unwrap_or_default();
        let key = format!(
            "{}:{}:{id}",
            skill["ability_id"].as_str().unwrap_or_default(),
            skill["kind"].as_str().unwrap_or_default()
        );
        let texts = tool_match::texts(
            id,
            &format!(
                "{} {}",
                skill["summary"].as_str().unwrap_or(""),
                skill["description"].as_str().unwrap_or("")
            ),
            &list(skill, "examples"),
        );
        let loadable = skill["requires_activation"] == true;
        items.push((
            "skill",
            key,
            id.to_owned(),
            list(skill, "keywords"),
            loadable,
            texts,
        ));
    }
    if items.is_empty() {
        return Ok(None);
    }
    let texts: Vec<Vec<String>> = items.iter().map(|item| item.5.clone()).collect();
    let mut best = vec![0.0_f32; items.len()];
    for clause in tool_match::clauses(text) {
        let Some(scores) = crate::api::inference::rank(&clause, texts.clone()).await else {
            return Ok(None);
        };
        if scores.len() != items.len() {
            return Ok(None);
        }
        for (index, score) in scores.into_iter().enumerate() {
            let item = &items[index];
            best[index] = best[index].max(tool_match::adjusted(&clause, &item.2, &item.3, score));
        }
    }
    let candidates = items
        .into_iter()
        .zip(best)
        .map(|((kind, key, _, _, loadable, _), score)| Candidate {
            kind,
            name: key,
            score,
            loadable,
        })
        .collect();
    Ok(Some(tool_match::choose(candidates)))
}
/// Validate a lowercase digest receipt without granting future source authority.
fn valid_hash(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
#[derive(Deserialize)]
struct SelectionEvidence {
    ability_id: String,
    kind: String,
    skill_id: String,
    sha256: String,
    bytes: usize,
    source_kind: String,
    activates: bool,
    #[serde(default)]
    revision: Option<i64>,
    status: String,
    catalog_version: String,
    context_source: String,
}
/// Return scope and observation metadata while omitting complete instruction text.
fn skill_acknowledgment(asset: &Value, catalog_version: &str) -> Value {
    let available = asset["requires_activation"] == false;
    let mut ack = json!({"status":if available {"already_available"} else {"loaded"},
        "ability_id":asset["ability_id"],"kind":asset["kind"],"skill_id":asset["skill_id"],
        "sha256":asset["sha256"],"bytes":asset["bytes"],"source_kind":asset["source_kind"],
        "activates":false,"scope":"conversation","catalog_version":catalog_version,
        "context_source":"live_skill_catalog","requires_activation":false,
        "availability":if available {asset["availability"].as_str().unwrap_or("activated")} else {"activated"},
        "message":if available {"This guidance is already supplied to the current model context."}
            else {"Guidance selected for this conversation and refreshed in each model context."}});
    if let Some(revision) = asset.get("revision") {
        ack["revision"] = revision.clone();
    }
    ack
}
/// Rehydrate guidance under current authority at every model boundary.
pub(crate) async fn active_guidance(app: &App, sid: &str, run_id: &str) -> Result<String> {
    Ok(context_guidance(app, sid, run_id, false).await?.0)
}
/// Return the exact included selectors beside their rebuilt guidance. Follow-up
/// calls apply their read-only ceiling even if the session normally permits writes.
pub(crate) async fn context_guidance(
    app: &App,
    sid: &str,
    run_id: &str,
    read_only: bool,
) -> Result<(String, Vec<Value>)> {
    let mut current = match view(app, sid, run_id, "load_skill").await {
        Ok(view) => view,
        Err(_) => return Ok((String::new(), Vec::new())),
    };
    current.read_only |= read_only;
    let mut guidance = String::new();
    let mut included = Vec::new();
    for selected in current.loaded_skills.as_array().into_iter().flatten() {
        let Ok(query) =
            serde_json::from_value::<LoadArgs>(json!({"ability_id":selected["ability_id"],
            "kind":selected["kind"],"skill_id":selected["skill_id"]}))
        else {
            continue;
        };
        if validate_load(&query).is_err() {
            continue;
        }
        let Ok(mut asset) = load_asset(&current, &query) else {
            continue;
        };
        // Identity survives edits; current authority, source and metadata decide inclusion.
        if !same_asset(selected, &asset) {
            continue;
        }
        mark_availability(&mut asset, &Value::Null, &current.loaded);
        if asset["requires_activation"] == false {
            continue;
        }
        if let Some(text) = asset["text"].as_str() {
            guidance.push_str(&format!(
                "\n\n## Loaded untrusted guidance: {} / {} / {}\n{}",
                query.ability_id, query.kind, query.skill_id, text
            ));
            included.push(
                json!({"ability_id":query.ability_id,"kind":query.kind,"skill_id":query.skill_id,
                "bytes":text.len(),"mode":asset["mode"]}),
            );
        }
    }
    if view(app, sid, run_id, "load_skill").await?.fingerprint != current.fingerprint {
        bail!("Loaded skill access changed during context assembly");
    }
    Ok((guidance, included))
}
/// Resolve only selected agent-owned guidance for the pure capability component.
/// Private-record authority, current mode and own-skill revision remain host-owned;
/// a concurrent change rejects the entire read instead of returning stale bodies.
pub(crate) async fn own_capability_guidance(
    app: &App,
    sid: &str,
    run_id: &str,
    read_only: bool,
) -> Result<Vec<webagent_capability_contracts::Guidance>> {
    use webagent_capability_contracts::{Guidance, GuidanceKind};
    let mut current = match view(app, sid, run_id, "load_skill").await {
        Ok(view) => view,
        Err(_) => return Ok(Vec::new()),
    };
    current.read_only |= read_only;
    let mut guidance = Vec::new();
    for selected in current.loaded_skills.as_array().into_iter().flatten() {
        if selected["kind"] != "own" {
            continue;
        }
        let Ok(query) =
            serde_json::from_value::<LoadArgs>(json!({"ability_id":selected["ability_id"],
            "kind":selected["kind"],"skill_id":selected["skill_id"]}))
        else {
            continue;
        };
        if validate_load(&query).is_err() {
            continue;
        }
        let Ok(asset) = load_asset(&current, &query) else {
            continue;
        };
        if !same_asset(selected, &asset) {
            continue;
        }
        let Some(body) = asset["text"].as_str() else {
            continue;
        };
        let id = format!("own:base:{}", query.skill_id);
        if guidance.iter().any(|item: &Guidance| item.id == id) {
            continue;
        }
        guidance.push(Guidance {
            id,
            kind: GuidanceKind::Own,
            body: body.to_owned(),
            selected: true,
            natural: false,
        });
    }
    if view(app, sid, run_id, "load_skill").await?.fingerprint != current.fingerprint {
        bail!("Own skill access changed during capability preparation");
    }
    Ok(guidance)
}

/// Serve one current list/load view and reject raced source or authority before returning.
async fn work(app: &App, sid: &str, run_id: &str, name: &str, args: &Value) -> Result<Value> {
    if !owns(name) {
        bail!("Unknown bundled skill tool");
    }
    let current = view(app, sid, run_id, name).await?;
    let allow_ability =
        |id: &str| current.access.ability_permission(id) != crate::access::Permission::Deny;
    let allow_tool = |tool: &str| skill_tool_allowed(&current, tool);
    let output = match name {
        "list_skills" => {
            let query: ListArgs =
                serde_json::from_value(args.clone()).context("Invalid skill-list arguments")?;
            if !valid_id(&query.ability_id) {
                bail!("Invalid bundled ability ID");
            }
            let mut skills = current.app.abilities.bundled_skill_list(
                &query.ability_id,
                &current.visibility,
                &current.state,
                allow_ability,
                allow_tool,
            )?;
            if query.ability_id == "base" {
                for record in current.own["skills"]
                    .as_array()
                    .context("Invalid own-skill list")?
                {
                    if record["mode"] == "off" {
                        continue;
                    }
                    let mut metadata = own_asset(record)?;
                    metadata.as_object_mut().unwrap().remove("text");
                    metadata["description"] = record["description"].clone();
                    skills.push(metadata);
                }
            }
            for skill in &mut skills {
                mark_availability(skill, &current.loaded_skills, &current.loaded);
            }
            let term = query
                .query
                .as_deref()
                .map(str::trim)
                .filter(|term| !term.is_empty());
            let mut ranked_by = Value::Null;
            if let Some(term) = term {
                if term.len() > 200 {
                    bail!("Skill search query is too long");
                }
                // A query ranks every permitted skill, already-loaded ones included and marked.
                let (ranked, by) = ranked_skills(term, skills).await;
                skills = ranked;
                ranked_by = json!(by);
            } else if !query.include_available {
                skills.retain(|skill| skill["requires_activation"] == true);
            }
            json!({"ability_id":query.ability_id,"skills":skills,"include_available":query.include_available,"catalog_version":current.version,
                "ranked_by":ranked_by,"source_kind":"skill_metadata","activates":false})
        }
        "load_skill" => {
            let query: LoadArgs =
                serde_json::from_value(args.clone()).context("Invalid skill-load arguments")?;
            validate_load(&query)?;
            if query
                .expected_catalog_version
                .as_ref()
                .is_some_and(|v| v != &current.version)
            {
                bail!("Skill catalog changed; list skills again");
            }
            let mut skill = load_asset(&current, &query)?;
            mark_availability(&mut skill, &current.loaded_skills, &current.loaded);
            skill_acknowledgment(&skill, &current.version)
        }
        _ => unreachable!(),
    };
    // Reject a read that raced a tier, level, membership, mode or bundle change.
    if view(app, sid, run_id, name).await?.fingerprint != current.fingerprint {
        bail!("Bundled skill access changed during read");
    }
    Ok(output)
}
/// Cancel pure skill reads promptly and return only their bounded native result.
pub async fn execute(
    app: &App,
    sid: &str,
    run_id: &str,
    name: &str,
    args: &Value,
    cancel: CancellationToken,
) -> Result<Value> {
    if cancel.is_cancelled() {
        bail!("tool execution cancelled");
    }
    let result = tokio::select! {
        _=cancel.cancelled()=>bail!("tool execution cancelled"),
        result=work(app,sid,run_id,name,args)=>result,
    };
    if cancel.is_cancelled() {
        bail!("tool execution cancelled");
    }
    Ok(match result {
        Ok(output) => json!({"status":"success","output":output}),
        Err(error) => {
            json!({"status":"error","error_kind":"bundled_skill_rejected","output":error.to_string()})
        }
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    /// Provide a successful metadata-only receipt independent from persistence logic.
    fn ack_fixture() -> Value {
        json!({"status":"loaded","ability_id":"base","kind":"ability","skill_id":"base",
            "sha256":"a".repeat(64),"bytes":9,"source_kind":"untrusted_bundled_guidance",
            "activates":false,"catalog_version":"b".repeat(64),"context_source":"live_skill_catalog"})
    }
    #[test]
    /// Reject mismatched receipts and preserve idempotent content-free identities.
    fn loaded_skill_selectors_are_content_free_idempotent_and_bound_to_pending_call() {
        let result = json!({"status":"success","output":ack_fixture()});
        let mut run = json!({"pending":{"name":"load_skill","arguments":{
            "ability_id":"base","kind":"ability","skill_id":"base"}}});
        apply_selection(&mut run, &result);
        assert_eq!(run["loaded_skills"].as_array().unwrap().len(), 1);
        assert!(!run.to_string().contains("LOAD BODY"));
        assert_eq!(
            run["loaded_skills"],
            json!([{"ability_id":"base","kind":"ability","skill_id":"base"}])
        );
        let saved = run.clone();
        apply_selection(&mut run, &result);
        assert_eq!(run, saved);
        run["pending"]["arguments"]["skill_id"] = json!("different");
        let before = run.clone();
        apply_selection(&mut run, &result);
        assert_eq!(run, before);
        let mut forged = result;
        forged["output"]["sha256"] = json!("invalid");
        apply_selection(&mut run, &forged);
        assert_eq!(run, before);
    }
    #[test]
    /// Keep instruction text outside tool acknowledgments and durable selectors.
    fn acknowledgment_only_skill_load_selects_without_recording_the_body() {
        let mut asset = ack_fixture();
        asset["text"] = json!("PRIVATE_SKILL_BODY");
        asset["requires_activation"] = json!(true);
        let ack = skill_acknowledgment(&asset, &"a".repeat(64));
        assert_eq!(ack["status"], "loaded");
        assert!(ack.get("text").is_none());
        assert!(!ack.to_string().contains("PRIVATE_SKILL_BODY"));
        let initial = json!({"pending":{"name":"load_skill","arguments":{
            "ability_id":"base","kind":"ability","skill_id":"base"}}});
        let mut run = initial.clone();
        apply_selection(&mut run, &json!({"status":"success","output":ack}));
        assert_eq!(run["loaded_skills"].as_array().unwrap().len(), 1);
        for field in [
            "status",
            "catalog_version",
            "context_source",
            "sha256",
            "skill_id",
        ] {
            let mut invalid = ack.clone();
            invalid[field] = json!("invalid");
            let mut rejected = initial.clone();
            apply_selection(&mut rejected, &json!({"status":"success","output":invalid}));
            assert_eq!(rejected, initial, "invalid {field}");
        }
        asset["requires_activation"] = json!(false);
        asset["availability"] = json!("visible");
        let visible = skill_acknowledgment(&asset, &"a".repeat(64));
        assert_eq!(visible["status"], "already_available");
        let mut unchanged = initial.clone();
        apply_selection(
            &mut unchanged,
            &json!({"status":"success","output":visible}),
        );
        assert_eq!(unchanged, initial);
    }
    #[test]
    /// Retain selected availability across edits without merging distinct identities.
    fn availability_tracks_live_identity_across_guidance_edits() {
        let mut visible = json!({"ability_id":"base","kind":"ability","skill_id":"base",
            "sha256":"old","availability":"visible"});
        mark_availability(&mut visible, &Value::Null, &Value::Null);
        assert_eq!(visible["requires_activation"], false);
        assert_eq!(visible["availability"], "visible");
        let mut hidden = json!({"ability_id":"base","kind":"selectable","skill_id":"guide",
            "sha256":"old","availability":"discoverable"});
        let selected = json!([hidden.clone()]);
        mark_availability(&mut hidden, &selected, &Value::Null);
        assert_eq!(hidden["availability"], "activated");
        assert_eq!(hidden["requires_activation"], false);
        hidden["availability"] = json!("discoverable");
        hidden["sha256"] = json!("replacement");
        mark_availability(&mut hidden, &selected, &Value::Null);
        assert_eq!(hidden["requires_activation"], false);
        assert_eq!(hidden["availability"], "activated");
        hidden["skill_id"] = json!("another_guide");
        hidden["availability"] = json!("discoverable");
        mark_availability(&mut hidden, &selected, &Value::Null);
        assert_eq!(hidden["requires_activation"], true);
        let mut tool = json!({"ability_id":"files","kind":"tool","skill_id":"file_read",
            "sha256":"x","availability":"discoverable"});
        mark_availability(&mut tool, &Value::Null, &json!(["tool:file_read"]));
        assert_eq!(tool["availability"], "activated");
        assert_eq!(tool["requires_activation"], false);
        assert!(
            serde_json::from_value::<ListArgs>(json!({"ability_id":"base",
            "include_available":"true"}))
            .is_err()
        );
        assert!(
            !serde_json::from_value::<ListArgs>(json!({"ability_id":"base"}))
                .unwrap()
                .include_available
        );
    }
    #[test]
    /// Verify lexical metadata search including bounded near-word matching.
    fn skill_search_matches_summary_and_similar_terms() {
        let skill = json!({"skill_id":"ui_theming",
            "summary":"Use when changing WebAgent UI appearance and its palette"});
        assert!(skill_matches(&skill, "theme"));
        assert!(skill_matches(&skill, "appearence"));
        assert!(!skill_matches(&skill, "payments"));
    }

    #[test]
    /// Reject untrusted scope, paths and malformed catalog versions before source lookup.
    fn selectors_reject_paths_unknown_fields_and_unbounded_versions() {
        for skill in ["../secret", "file.skill.md", "/etc/passwd", "", "UPPER"] {
            assert!(validate_load(&LoadArgs {
                ability_id: "base".into(),
                kind: "selectable".into(),
                skill_id: skill.into(),
                expected_catalog_version: None
            })
            .is_err());
        }
        for kind in ["custom", "path", ""] {
            assert!(validate_load(&LoadArgs {
                ability_id: "base".into(),
                kind: kind.into(),
                skill_id: "base".into(),
                expected_catalog_version: None
            })
            .is_err());
        }
        assert!(validate_load(&LoadArgs {
            ability_id: "foreign".into(),
            kind: "own".into(),
            skill_id: "my_steps".into(),
            expected_catalog_version: None
        })
        .is_err());
        assert!(validate_load(&LoadArgs {
            ability_id: "base".into(),
            kind: "own".into(),
            skill_id: "my_steps".into(),
            expected_catalog_version: None
        })
        .is_ok());
        assert!(validate_load(&LoadArgs {
            ability_id: "base".into(),
            kind: "ability".into(),
            skill_id: "base".into(),
            expected_catalog_version: Some("a".repeat(65))
        })
        .is_err());
        assert!(
            serde_json::from_value::<LoadArgs>(json!({"ability_id":"base","kind":"ability",
            "skill_id":"base","session_id":"foreign"}))
            .is_err()
        );
        assert!(serde_json::from_value::<ListArgs>(
            json!({"ability_id":"base","user_id":"foreign"})
        )
        .is_err());
    }
    #[tokio::test]
    /// Prevent another caller from discovering private records through a foreign session.
    async fn caller_owned_session_is_required_even_before_skill_lookup() {
        let (_root, base) = crate::runtime_tests::fixture_uninitialized().await;
        let first = base
            .store
            .create_user_account("skill-first", "fixture-password-hash")
            .await
            .unwrap();
        let second = base
            .store
            .create_user_account("skill-second", "fixture-password-hash")
            .await
            .unwrap();
        let mut owner = base.clone();
        owner.store = base
            .store
            .for_user(first["user_id"].as_str().unwrap())
            .await
            .unwrap();
        let mut stranger = base.clone();
        stranger.store = base
            .store
            .for_user(second["user_id"].as_str().unwrap())
            .await
            .unwrap();
        owner.store.put("session:private",&json!({"id":"private","agent_id":"shared_default","title":"Private","created_at":1})).await.unwrap();
        let result = execute(
            &stranger,
            "private",
            "run",
            "list_skills",
            &json!({"ability_id":"base"}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(result["status"], "error");
        assert!(result["output"]
            .as_str()
            .unwrap()
            .contains("Session not found"));
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert!(execute(
            &owner,
            "private",
            "run",
            "list_skills",
            &json!({"ability_id":"base"}),
            cancel
        )
        .await
        .is_err());
    }
    #[test]
    /// Adopt own-skill revisions while preserving Off and exact selector boundaries.
    fn own_skill_identity_survives_revision_but_does_not_cross_selector_boundaries() {
        let selected = json!({"ability_id":"base","kind":"own","skill_id":"review","sha256":"old","revision":1});
        let record = json!({"name":"review","mode":"selectable","instructions":"CURRENT OWN BODY","revision":2});
        let mut asset = own_asset(&record).unwrap();
        assert!(same_asset(&selected, &asset));
        mark_availability(&mut asset, &json!([selected.clone()]), &Value::Null);
        assert_eq!(asset["requires_activation"], false);
        assert_eq!(asset["text"], "CURRENT OWN BODY");
        for field in ["ability_id", "kind", "skill_id"] {
            let mut different = selected.clone();
            different[field] = json!("other");
            assert!(!same_asset(&different, &asset));
        }
        let mut off = record.clone();
        off["mode"] = json!("off");
        assert!(own_asset(&off).is_err());
    }

    /// Receipt metadata must not pin an existing identity or disturb stable order.
    #[test]
    fn existing_legacy_skill_selector_is_normalized_without_reordering() {
        let mut run = json!({"pending":{"name":"load_skill","arguments":{
            "ability_id":"base","kind":"ability","skill_id":"base"}},
            "loaded_skills":[{"ability_id":"base","kind":"own","skill_id":"earlier"},
                {"ability_id":"base","kind":"ability","skill_id":"base","sha256":"old","revision":2},
                {"ability_id":"base","kind":"own","skill_id":"later"}]});
        apply_selection(
            &mut run,
            &json!({"status":"success","output":ack_fixture()}),
        );
        assert_eq!(
            run["loaded_skills"],
            json!([
            {"ability_id":"base","kind":"own","skill_id":"earlier"},
            {"ability_id":"base","kind":"ability","skill_id":"base"},
            {"ability_id":"base","kind":"own","skill_id":"later"}])
        );
        let normalized = run.clone();
        apply_selection(
            &mut run,
            &json!({"status":"success","output":ack_fixture()}),
        );
        assert_eq!(run, normalized);
    }
}

/// Private feedback reuses the same live selector checks as skill loading; no activation or body is persisted.
pub(crate) async fn rating_asset(app: &App, sid: &str, rid: &str, args: &Value) -> Result<Value> {
    let query: LoadArgs = serde_json::from_value(args.clone())?;
    validate_load(&query)?;
    let current = view(app, sid, rid, "rate_skill").await?;
    let mut asset = load_asset(&current, &query)?;
    if let Some(object) = asset.as_object_mut() {
        object.remove("text");
    }
    Ok(asset)
}
