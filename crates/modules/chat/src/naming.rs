//! Session auto-naming configuration (`config/prompts.json#/session_naming`), title normalization and state transitions.
use anyhow::{anyhow, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{io::Write, path::Path};
use webagent_models::model_selection::{FailoverState, ModelRole, SelectionPlan};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub prompt: String,
    pub model_role: ModelRole,
    pub attempts_per_job: u64,
    pub retry_delay_ms: u64,
    pub request_timeout_seconds: u64,
    pub recovery_retry_delay_seconds: u64,
    pub max_job_attempts: u64,
}

impl Config {
    pub fn defaults_at(source: Option<&Path>, fallback_directory: &Path) -> Result<Self> {
        let bytes =
            webagent_configuration::prompts::prompt_defaults_at(source, fallback_directory)?;
        let prompts: Value = serde_json::from_slice(&bytes)?;
        let result: Self = serde_json::from_value(prompts["session_naming"].clone())
            .map_err(|_| anyhow!("Missing or invalid naming defaults"))?;
        result.validate()?;
        Ok(result)
    }

    pub fn validate(&self) -> Result<()> {
        if self.prompt.trim().is_empty()
            || self.prompt.len() > 8192
            || !matches!(
                self.model_role,
                ModelRole::System | ModelRole::Standard | ModelRole::Premium
            )
            || !(1..=10).contains(&self.attempts_per_job)
            || self.retry_delay_ms > 60_000
            || !(1..=300).contains(&self.request_timeout_seconds)
            || !(1..=86_400).contains(&self.recovery_retry_delay_seconds)
            || !(1..=20).contains(&self.max_job_attempts)
        {
            return Err(anyhow!("Invalid session naming settings"));
        }
        Ok(())
    }

    /// Source/default sections are complete snapshots and never inherit another publication.
    pub fn parse_complete(value: &Value) -> Result<Self> {
        let result: Self = serde_json::from_value(value.clone())
            .map_err(|_| anyhow!("Invalid complete naming configuration"))?;
        result.validate()?;
        Ok(result)
    }

    pub fn parse_at(
        value: &Value,
        source: Option<&Path>,
        fallback_directory: &Path,
    ) -> Result<Self> {
        // Accept the earlier prompt-only format; missing settings use shipped defaults.
        let supplied = if value.is_string() {
            json!({"prompt":value})
        } else {
            value.clone()
        };
        if let Ok(result) = serde_json::from_value::<Self>(supplied.clone()) {
            result.validate()?;
            return Ok(result);
        }
        let mut resolved = serde_json::to_value(Self::defaults_at(source, fallback_directory)?)?;
        let fields = supplied
            .as_object()
            .ok_or_else(|| anyhow!("Invalid naming section"))?;
        for (key, value) in fields {
            resolved[key] = value.clone();
        }
        let result: Self = serde_json::from_value(resolved)?;
        result.validate()?;
        Ok(result)
    }

    pub fn load_at(root: &Path, source: Option<&Path>, fallback_directory: &Path) -> Result<Self> {
        let prompts =
            webagent_configuration::prompts::resolved_prompts_at(root, source, fallback_directory)?;
        Self::parse_at(
            prompts
                .get("session_naming")
                .ok_or_else(|| anyhow!("Missing naming configuration"))?,
            source,
            fallback_directory,
        )
    }

    pub fn for_session_at(
        session: &Value,
        source: Option<&Path>,
        fallback_directory: &Path,
    ) -> Result<Self> {
        if session["auto_naming"]["config"].is_null() {
            return Self::defaults_at(source, fallback_directory);
        }
        let result: Self = serde_json::from_value(session["auto_naming"]["config"].clone())?;
        result.validate()?;
        Ok(result)
    }
}

pub fn initialize_at(root: &Path, source: Option<&Path>, fallback_directory: &Path) -> Result<()> {
    let directory = root.join("config");
    std::fs::create_dir_all(&directory)?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(directory.join(".prompts.lock"))?;
    lock.lock_exclusive()?;
    let path = directory.join("prompts.json");
    let original = match std::fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let mut prompts: Value = match original.as_deref() {
        Some(bytes) => serde_json::from_slice(bytes)?,
        None => serde_json::from_slice(&webagent_configuration::prompts::prompt_defaults_at(
            source,
            fallback_directory,
        )?)?,
    };
    let object = prompts
        .as_object_mut()
        .ok_or_else(|| anyhow!("Invalid prompts file"))?;
    if object.contains_key("session_naming") && original.is_some() {
        return Ok(());
    }
    object.insert(
        "session_naming".into(),
        serde_json::to_value(Config::defaults_at(source, fallback_directory)?)?,
    );
    let mut temporary = tempfile::NamedTempFile::new_in(&directory)?;
    if let Ok(metadata) = std::fs::metadata(&path) {
        temporary
            .as_file()
            .set_permissions(metadata.permissions())?;
    }
    serde_json::to_writer_pretty(&mut temporary, &prompts)?;
    temporary.write_all(b"\n")?;
    temporary.as_file().sync_all()?;
    if Some(std::fs::read(&path).unwrap_or_default()) != original && original.is_some() {
        return Err(anyhow!("Prompts changed; retry initialization"));
    }
    if original.is_none() && path.exists() {
        return Err(anyhow!("Prompts changed; retry initialization"));
    }
    temporary.persist(&path).map_err(|error| error.error)?;
    std::fs::File::open(directory)?.sync_all()?;
    Ok(())
}

pub fn validate_settings_at(
    value: &Value,
    source: Option<&Path>,
    fallback_directory: &Path,
) -> Result<()> {
    Config::parse_at(value, source, fallback_directory).map(|_| ())
}

pub fn validate_source_settings(value: &Value) -> Result<()> {
    Config::parse_complete(value).map(|_| ())
}

/// Build a Unicode-safe placeholder title from normalized first-message text when model naming is unavailable.
pub fn fallback(text: &str) -> String {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let excerpt: String = normalized
        .chars()
        .filter(|ch| !ch.is_control())
        .take(60)
        .collect();
    format!(
        "New: {}",
        if excerpt.is_empty() {
            "Attachments"
        } else {
            &excerpt
        }
    )
}

/// Select the first nonempty generated title and apply word, character and byte limits before metadata publication.
pub fn clean(raw: &str) -> String {
    let line = raw
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .trim();
    let line = line
        .trim_matches(['"', '\''])
        .trim()
        .trim_end_matches(['.', ',', '!', '?', ';', ':']);
    let words = line
        .split_whitespace()
        .take(10)
        .collect::<Vec<_>>()
        .join(" ");
    let mut title = String::new();
    // Keep v2's 80-character cap and v3's 200-byte title contract.
    for ch in words.chars().filter(|ch| !ch.is_control()).take(80) {
        if title.len() + ch.len_utf8() > 200 {
            break;
        }
        title.push(ch);
    }
    title.trim().to_owned()
}

/// Build a private naming run from the pinned role plan and its independent failover cursor, preserving requester/participation identity.
pub fn dispatch_at(
    session: &Value,
    state: &FailoverState,
    source: Option<&Path>,
    fallback_directory: &Path,
) -> Result<Value> {
    let plan: SelectionPlan =
        serde_json::from_value(session["auto_naming"]["system_plan"].clone())?;
    if plan.role != Config::for_session_at(session, source, fallback_directory)?.model_role
        || plan.agent_id != session["agent_id"].as_str().unwrap_or("")
    {
        return Err(anyhow!("Invalid naming System plan"));
    }
    let candidate = plan
        .candidates
        .get(state.candidate_index)
        .ok_or_else(|| anyhow!("Naming candidates exhausted"))?;
    Ok(json!({
        "id": session["auto_naming"]["run_id"],
        "session_id": session["id"],
        "agent_id": session["agent_id"],
        "requester_user_id": session["auto_naming"]["requester_user_id"],
        "participation_generation": session["auto_naming"]["participation_generation"].as_u64().unwrap_or(0),
        "selection_plan": plan,
        "failover_state": state,
        "provider_selection": webagent_models::provider::ProviderSelection {
            limits: candidate.limits.clone(),
            provider: candidate.transport.provider_id(),
            model: candidate.upstream_model.clone(),
        }.value(),
    }))
}

pub fn naming_due(session: &Value, recovery: bool, now: u64) -> bool {
    if session["status"] != "active" || !naming_participation(session) {
        return false;
    }
    let naming = &session["auto_naming"];
    match naming["stage"].as_str() {
        Some("queued") => true,
        Some("failed") => recovery && naming["retry_at"].as_u64().unwrap_or(u64::MAX) <= now,
        Some("generating") => recovery && naming["lease_until"].as_u64().unwrap_or(u64::MAX) <= now,
        _ => false,
    }
}

pub fn naming_participation(session: &Value) -> bool {
    session["participation"]["active"] != false
        && session["participation"]["generation"].as_u64().unwrap_or(0)
            == session["auto_naming"]["participation_generation"]
                .as_u64()
                .unwrap_or(0)
}

pub fn naming_owned(session: &Value, claim: &str, now: u64) -> bool {
    session["status"] == "active"
        && naming_participation(session)
        && session["auto_naming"]["stage"] == "generating"
        && session["auto_naming"]["claim"] == claim
        && session["auto_naming"]["lease_until"].as_u64().unwrap_or(0) > now
}
