//! Automatic, bounded source retrieval for the Code Index ability.
//! Candidate labels and snippets are untrusted source data; callers must journal
//! the returned value as an ordinary `code_index_lookup` tool result.
use crate::{ability_controls, live_tool_decision, session_agent_id, session_mode, App};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

const RUBRIC: [&str; 5] = [
    "No match",
    "Weak match",
    "Partial match",
    "Strong match",
    "Direct match",
];
const MAX_CANDIDATES: usize = 32;
const MAX_DECISION_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy)]
struct Settings {
    candidate_count: usize,
    result_count: usize,
    extra_candidates: usize,
    total_chars: usize,
    per_snippet_chars: usize,
    minimum_score: f64,
    minimum_confidence: f64,
    root: &'static str,
}

pub fn validate_settings(value: &Value) -> Result<()> {
    let object = value
        .as_object()
        .context("Code Index settings must be an object")?;
    const REQUIRED: [&str; 8] = [
        "enabled",
        "candidate_count",
        "result_count",
        "extra_candidates",
        "total_chars",
        "per_snippet_chars",
        "minimum_score",
        "minimum_confidence",
    ];
    if object
        .keys()
        .any(|key| !REQUIRED.contains(&key.as_str()) && key != "root")
        || REQUIRED.iter().any(|key| !object.contains_key(*key))
        || value
            .get("root")
            .is_some_and(|root| !matches!(root.as_str(), Some("published" | "webagent-v3")))
    {
        bail!("Invalid Code Index settings fields");
    }
    if !value["enabled"].is_boolean()
        || !(1..=MAX_CANDIDATES as u64).contains(&value["candidate_count"].as_u64().unwrap_or(0))
        || !(1..=5).contains(&value["result_count"].as_u64().unwrap_or(0))
        || value["extra_candidates"].as_u64().unwrap_or(u64::MAX) > 8
        || !(512..=24000).contains(&value["total_chars"].as_u64().unwrap_or(0))
        || !(256..=12000).contains(&value["per_snippet_chars"].as_u64().unwrap_or(0))
        || !value["minimum_score"]
            .as_f64()
            .is_some_and(|x| x.is_finite() && (0.0..=4.0).contains(&x))
        || !value["minimum_confidence"]
            .as_f64()
            .is_some_and(|x| x.is_finite() && (0.0..=1.0).contains(&x))
    {
        bail!("Code Index settings are outside their supported bounds");
    }
    Ok(())
}

fn settings(value: &Value) -> Result<Option<Settings>> {
    let Some(value) = value.get("settings").and_then(|v| v.get("code_index")) else {
        return Ok(None);
    };
    validate_settings(value)?;
    if value["enabled"] != true {
        return Ok(None);
    }
    Ok(Some(Settings {
        candidate_count: value["candidate_count"].as_u64().unwrap() as usize,
        result_count: value["result_count"].as_u64().unwrap() as usize,
        extra_candidates: value["extra_candidates"].as_u64().unwrap() as usize,
        total_chars: value["total_chars"].as_u64().unwrap() as usize,
        per_snippet_chars: value["per_snippet_chars"].as_u64().unwrap() as usize,
        minimum_score: value["minimum_score"].as_f64().unwrap(),
        minimum_confidence: value["minimum_confidence"].as_f64().unwrap(),
        root: if value
            .get("root")
            .and_then(Value::as_str)
            .unwrap_or("published")
            == "webagent-v3"
        {
            "webagent-v3"
        } else {
            "published"
        },
    }))
}

async fn require_lookup(
    app: &App,
    sid: &str,
    run_id: &str,
    expected_settings_sha256: &str,
) -> Result<(Value, String, String)> {
    let run = app.run(run_id).await?;
    if run["session_id"] != sid || run["status"] != "running" {
        bail!("Retrieval run is unavailable");
    }
    let agent = session_agent_id(app, sid).await?;
    if run["agent_id"].as_str().unwrap_or("shared_default") != agent {
        bail!("Retrieval session agent changed");
    }
    let mut scoped = app.clone();
    scoped.store = app.store.for_agent(&agent).await?;
    let latest_abilities = scoped.store.agent_abilities_for(&agent).await?;
    let latest_settings = &latest_abilities["settings"]["code_index"];
    if settings(&latest_abilities)?.is_none()
        || crate::digest(latest_settings) != expected_settings_sha256
    {
        bail!("Automatic Code Index retrieval settings changed");
    }
    let access = scoped
        .store
        .live_access(scoped.store.current_user_id(), &agent)
        .await?;
    if !access.admitted() {
        bail!("Retrieval access is unavailable");
    }
    let (policy, _, visibility, state) = ability_controls(&scoped, sid).await?;
    let mode = session_mode(&scoped, sid).await?;
    let decision = live_tool_decision(
        &scoped,
        &access,
        "code_index_lookup",
        &policy,
        &visibility,
        &state,
        &run["loaded_abilities"],
        mode["mode"] == "read_only",
    )
    .for_execution_mode(&mode);
    if decision.permission != crate::access::Permission::Auto {
        bail!("Automatic retrieval requires Auto code_index_lookup permission");
    }
    let jev = live_tool_decision(
        &scoped,
        &access,
        "decide_with_jev",
        &policy,
        &visibility,
        &state,
        &run["loaded_abilities"],
        mode["mode"] == "read_only",
    )
    .for_execution_mode(&mode);
    if jev.permission != crate::access::Permission::Auto {
        bail!("Automatic retrieval requires Auto decide_with_jev permission");
    }
    Ok((run, agent, scoped.store.current_user_id().to_owned()))
}

fn prompt_from_config(template_text: &str, live_bytes: Option<&[u8]>) -> Result<String> {
    let template: Value = serde_json::from_str(template_text)
        .context("Embedded prompt configuration is malformed")?;
    let fallback = template["code_index_retrieval"]
        .as_str()
        .filter(|text| !text.trim().is_empty())
        .context("Embedded Code Index retrieval prompt is invalid")?;
    let Some(bytes) = live_bytes else {
        return Ok(fallback.to_owned());
    };
    let live: Value =
        serde_json::from_slice(bytes).context("Live prompt configuration is malformed")?;
    match live.get("code_index_retrieval") {
        None => Ok(fallback.to_owned()),
        Some(Value::String(text)) if !text.trim().is_empty() => Ok(text.clone()),
        Some(_) => bail!("Live Code Index retrieval prompt is malformed"),
    }
}

fn retrieval_instructions() -> Result<String> {
    let root = std::path::PathBuf::from(
        std::env::var("V3_STATE_ROOT").unwrap_or_else(|_| "/state".into()),
    );
    let path = root.join("config/prompts.json");
    let bytes = match std::fs::read(path) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error).context("Could not read live prompt configuration"),
    };
    prompt_from_config(
        include_str!("../../../config/prompts.json"),
        bytes.as_deref(),
    )
}

fn decision_request(query: &str, candidates: &[Value], instructions: &str) -> Result<Value> {
    let mut entries = Vec::with_capacity(candidates.len());
    let mut questions = serde_json::Map::new();
    for (index, candidate) in candidates.iter().enumerate() {
        let id = format!("candidate_{index}");
        entries.push(json!({"id":id,"path":candidate["file_path"],"label":candidate["label"],"kind":candidate["kind"],"start_line":candidate["start_line"],"end_line":candidate["end_line"]}));
        questions.insert(
            id,
            json!({"type":"score","instructions":instructions,"criteria":RUBRIC}),
        );
    }
    let request =
        json!({"state":{"user_request":query,"candidates":entries},"questions":questions});
    if request.to_string().len() > MAX_DECISION_BYTES {
        bail!("Retrieval decision request exceeds 64 KiB");
    }
    Ok(request)
}

/// Recheck both effective grants immediately before the caller journals source excerpts.
pub(crate) async fn authorize_attachment(
    app: &App,
    sid: &str,
    run_id: &str,
    expected_settings_sha256: &str,
) -> Result<()> {
    let (_run, _agent, _user) = require_lookup(app, sid, run_id, expected_settings_sha256).await?;
    Ok(())
}

/// Retrieve source evidence once for a user message. Caller owns the once-per-message
/// journal marker and must persist the returned result as an ordinary tool interaction.
pub(crate) async fn retrieve(
    app: &App,
    sid: &str,
    run_id: &str,
    user_text: &str,
    cancel: CancellationToken,
) -> Result<Option<Value>> {
    if user_text.trim().is_empty() || user_text.len() > 16_000 {
        return Ok(None);
    }
    let mut query = String::new();
    for character in user_text.chars() {
        if query.len() + character.len_utf8() > 512 {
            break;
        }
        query.push(character);
    }
    let agent = session_agent_id(app, sid).await?;
    let agent_store = app.store.for_agent(&agent).await?;
    let agent_abilities = agent_store.agent_abilities_for(&agent).await?;
    let settings_value = &agent_abilities["settings"]["code_index"];
    let settings_sha256 = crate::digest(settings_value);
    let Some(config) = settings(&agent_abilities)? else {
        return Ok(None);
    };
    let (initial_run, initial_agent, initial_user) =
        require_lookup(app, sid, run_id, &settings_sha256).await?;
    if initial_agent != agent {
        bail!("Session agent changed before retrieval");
    }
    let lexical=super::execute_trusted("code_index_lookup",&json!({"action":"search","root":config.root,"query":query,"limit":config.candidate_count.saturating_add(config.extra_candidates).min(MAX_CANDIDATES),"offset":0}),sid,&app.tool_socket,None,cancel.clone()).await?;
    if lexical["status"] != "success" {
        bail!("Code Index candidate search unavailable");
    }
    let candidates = lexical["results"]
        .as_array()
        .context("Code Index candidate page malformed")?;
    if candidates.is_empty() {
        return Ok(None);
    }
    if candidates.len() > MAX_CANDIDATES {
        bail!("Code Index candidate page exceeds bound");
    }
    let instructions = retrieval_instructions()?;
    let request = decision_request(&query, candidates, &instructions)?;
    // The worker search can take time; recheck live grants and caller identity
    // immediately before sending query and candidate metadata to Jev.
    let (pre_decision_run, pre_decision_agent, pre_decision_user) =
        require_lookup(app, sid, run_id, &settings_sha256).await?;
    if pre_decision_agent != initial_agent
        || pre_decision_user != initial_user
        || pre_decision_run["id"] != initial_run["id"]
        || pre_decision_run["generation"] != initial_run["generation"]
    {
        bail!("Retrieval authority changed before Jev scoring");
    }
    let decision = super::core_decision::execute(
        app,
        sid,
        run_id,
        &json!({"request_json":request.to_string()}),
        cancel.clone(),
    )
    .await?;
    if decision["status"] != "success" {
        bail!("Jev retrieval scoring unavailable");
    }
    let answer = &decision["output"];
    let answers = answer["answers"]
        .as_object()
        .context("Jev retrieval scores missing")?;
    let mut ranked = Vec::with_capacity(candidates.len());
    for (index, candidate) in candidates.iter().enumerate() {
        let score = answers
            .get(&format!("candidate_{index}"))
            .and_then(|v| v["score"].as_f64())
            .context("Jev retrieval score invalid")?;
        let confidence = answers
            .get(&format!("candidate_{index}"))
            .and_then(|v| v["confidence"].as_f64())
            .context("Jev retrieval confidence invalid")?;
        if !score.is_finite()
            || !(0.0..=4.0).contains(&score)
            || !confidence.is_finite()
            || !(0.0..=1.0).contains(&confidence)
        {
            bail!("Jev score is outside the supported rubric");
        }
        ranked.push((index, score, confidence));
    }
    ranked.sort_by(|a, b| {
        b.1.total_cmp(&a.1)
            .then_with(|| b.2.total_cmp(&a.2))
            .then_with(|| {
                candidates[a.0]["file_path"]
                    .as_str()
                    .cmp(&candidates[b.0]["file_path"].as_str())
            })
    });
    let (current_run, current_agent, current_user) =
        require_lookup(app, sid, run_id, &settings_sha256).await?;
    if current_agent != initial_agent
        || current_user != initial_user
        || current_run["id"] != initial_run["id"]
        || current_run["session_id"] != initial_run["session_id"]
        || current_run["status"] != "running"
    {
        bail!("Retrieval run authority changed after scoring");
    }
    let count = config.result_count.min(5);
    let index_revision = if config.root == "published" {
        lexical["publication"]["release_id"]
            .as_str()
            .context("Published release identity missing")?
            .to_owned()
    } else {
        lexical["revision"]
            .as_u64()
            .context("Private index revision missing")?
            .to_string()
    };
    let ranked_summary:Vec<Value>=ranked.iter().take((config.result_count+config.extra_candidates).min(MAX_CANDIDATES))
        .map(|(index,score,confidence)|json!({"path":candidates[*index]["file_path"],"start_line":candidates[*index]["start_line"],
            "end_line":candidates[*index]["end_line"],"score":score,"confidence":confidence,
            "selected":*score>=config.minimum_score && *confidence>=config.minimum_confidence})).collect();
    let mut items = Vec::new();
    let mut used = 0usize;
    for (index, score, confidence) in ranked.into_iter().take(count) {
        if score < config.minimum_score
            || confidence < config.minimum_confidence
            || used >= config.total_chars
        {
            continue;
        }
        let candidate = &candidates[index];
        let start = candidate["start_line"]
            .as_u64()
            .context("Candidate start line missing")?;
        let end = candidate["end_line"]
            .as_u64()
            .context("Candidate end line missing")?
            .min(start.saturating_add(79));
        let budget = config.per_snippet_chars.min(config.total_chars - used);
        if budget < 256 {
            break;
        }
        let (_run, agent, user) = require_lookup(app, sid, run_id, &settings_sha256).await?;
        if agent != initial_agent || user != initial_user {
            bail!("Session authority changed before source read");
        }
        let excerpt=super::execute_trusted("code_index_lookup",&json!({"action":"snippet","root":config.root,"file_path":candidate["file_path"],"start_line":start,"end_line":end,"max_chars":budget}),sid,&app.tool_socket,None,cancel.clone()).await?;
        let same_index = if config.root == "published" {
            ["release_id", "source_sha256", "index_sha256"]
                .iter()
                .all(|key| excerpt["publication"][*key] == lexical["publication"][*key])
        } else {
            excerpt["revision"] == lexical["revision"]
        };
        if excerpt["status"] != "success" || !same_index {
            continue;
        }
        let source = excerpt["results"]
            .as_array()
            .and_then(|rows| rows.first())
            .context("Snippet result malformed")?;
        let text = source["text"].as_str().context("Snippet text missing")?;
        if source["stale"] == true || text.len() > budget {
            continue;
        }
        let path = candidate["file_path"]
            .as_str()
            .context("Candidate path invalid")?;
        if source["file_path"].as_str() != Some(path) {
            continue;
        }
        let hash = source["sha256"]
            .as_str()
            .context("Snippet source hash missing")?;
        let source_start = source["start_line"]
            .as_u64()
            .context("Snippet start line missing")?;
        let source_end = source["end_line"]
            .as_u64()
            .context("Snippet end line missing")?;
        let id = format!("code-index:{index_revision}:{hash}:{source_start}:{source_end}");
        let call_id = format!("code_index_auto_{run_id}_{}", items.len());
        let item_revision = if config.root == "published" {
            lexical["publication"].clone()
        } else {
            lexical["revision"].clone()
        };
        items.push(json!({"id":id,"call_id":call_id,"origin_tool":"code_index_lookup","root":config.root,"path":path,
            "start_line":source["start_line"],"end_line":source["end_line"],"sha256":hash,"index_revision":item_revision,
            "score":score,"confidence":confidence,"max_chars":budget,"retention_eligible":true,
            "recall_reference":call_id,"text":text,
            "source_kind":"untrusted_source_index"}));
        used += text.len();
    }
    if items.is_empty() {
        return Ok(None);
    }
    Ok(Some(
        json!({"status":"success","output":{"origin_tool":"code_index_lookup","source_kind":"untrusted_source_index",
        "root":config.root,"publication":lexical.get("publication").cloned().unwrap_or(Value::Null),
        "index_revision":if config.root == "published" { lexical["publication"].clone() } else { lexical["revision"].clone() },
        "settings_sha256":settings_sha256,"query":query,"candidate_limit":config.candidate_count.saturating_add(config.extra_candidates).min(MAX_CANDIDATES),
        "candidate_count":candidates.len(),"ranked_candidates":ranked_summary,
        "returned_count":items.len(),"total_chars":used,"items":items}}),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn defaults() -> Value {
        json!({"enabled":false,"candidate_count":16,"result_count":3,
        "extra_candidates":4,"total_chars":8000,"per_snippet_chars":4000,
        "minimum_score":2.0,"minimum_confidence":0.5})
    }
    #[test]
    fn retrieval_instructions_use_live_override_or_embedded_fallback_and_reject_malformed_values() {
        let template = r#"{"code_index_retrieval":"EMBEDDED"}"#;
        assert_eq!(prompt_from_config(template, None).unwrap(), "EMBEDDED");
        assert_eq!(
            prompt_from_config(template, Some(br#"{"other":"value"}"#)).unwrap(),
            "EMBEDDED"
        );
        assert_eq!(
            prompt_from_config(template, Some(br#"{"code_index_retrieval":"LIVE"}"#)).unwrap(),
            "LIVE"
        );
        assert!(prompt_from_config(template, Some(b"{")).is_err());
        assert!(prompt_from_config(template, Some(br#"{"code_index_retrieval":false}"#)).is_err());
    }
    #[test]
    fn settings_are_bounded_and_default_off() {
        let value = defaults();
        assert!(validate_settings(&value).is_ok());
        let mut published = value.clone();
        published["root"] = json!("published");
        assert!(validate_settings(&published).is_ok());
        published["root"] = json!("production");
        assert!(validate_settings(&published).is_err());
        assert!(settings(&json!({"settings":{"code_index":value}}))
            .unwrap()
            .is_none());
        let mut enabled = defaults();
        enabled["enabled"] = json!(true);
        assert!(settings(&json!({"settings":{"code_index":enabled}}))
            .unwrap()
            .is_some());
        let mut invalid = defaults();
        invalid["total_chars"] = json!(24001);
        assert!(validate_settings(&invalid).is_err());
        invalid = defaults();
        invalid["unbounded"] = json!(true);
        assert!(validate_settings(&invalid).is_err());
    }
}
