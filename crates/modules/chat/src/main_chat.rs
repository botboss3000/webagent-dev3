//! Permanent coordination identity and cross-session Main Notes state machine.
//! Note content is untrusted evidence; neither approving a topic nor reviewing it grants effects.
use crate::digest;
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// The Main Chat and Main Notes texts and default names (`prompts/main_chat.json`, read live).
const TEXTS_FILE: &str = "config/configuration/prompts/main_chat.json";
/// The same file as built, used only when the live one is missing or invalid.
const PACKAGED_TEXTS: &str =
    include_str!("../../../../config/configuration/prompts/main_chat.json");

pub fn texts() -> Value {
    webagent_configuration::live::parsed(TEXTS_FILE, 256 * 1024, |bytes| {
        Ok(serde_json::from_slice::<Value>(bytes)?)
    })
    .or_else(|_| serde_json::from_str(PACKAGED_TEXTS).map_err(anyhow::Error::from))
    .unwrap_or_default()
}

/// The per-agent override keys and the `main_chat.json` field each one replaces.
pub const TEXT_FIELDS: [(&str, &str); 2] = [
    ("contribute", "notes_guidance"),
    ("review", "main_chat_prompt"),
];

/// Text `key` for this agent, with {chat} and {notes} replaced by the agent's two names: for an
/// override key its own override when set, else the file's; any other key is a file field.
pub fn text(state: &Value, key: &str) -> String {
    let overridable = TEXT_FIELDS.iter().find(|(name, _)| *name == key);
    let field = overridable.map_or(key, |(_, field)| *field);
    let raw = overridable
        .and_then(|_| state["skill_overrides"][key].as_str())
        .filter(|text| !text.trim().is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| texts()[field].as_str().unwrap_or("").to_owned());
    let name = |setting: &str, field: &str| {
        state["settings"][setting]
            .as_str()
            .filter(|name| !name.trim().is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| texts()[field].as_str().unwrap_or("").to_owned())
    };
    raw.replace("{chat}", &name("chat_name", "main_chat_name"))
        .replace("{notes}", &name("notes_name", "main_notes_name"))
}

/// Guidance text `key` (`review` or `contribute`) under its bold heading, which carries the
/// agent's names so the guidance itself can stay name-free; empty when the text is.
pub fn guidance(state: &Value, key: &str) -> String {
    let body = text(state, key);
    let heading = text(
        state,
        if key == "review" {
            "main_chat_heading"
        } else {
            "notes_heading"
        },
    );
    if body.trim().is_empty() || heading.trim().is_empty() {
        return body;
    }
    format!("**{}**\n\n{body}", heading.trim())
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub enabled: bool,
    pub chat_name: String,
    pub notes_name: String,
    pub write_mode: String,
    /// How Main Chat's scheduling calls are approved: "tool" follows the tool permission and
    /// session mode; "each", "recurring" (cron only) and "free" override both.
    pub schedule_approval: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            chat_name: texts()["main_chat_name"]
                .as_str()
                .unwrap_or("Main Chat")
                .into(),
            notes_name: texts()["main_notes_name"]
                .as_str()
                .unwrap_or("Main Notes")
                .into(),
            write_mode: "automatic".into(),
            schedule_approval: "tool".into(),
        }
    }
}

impl Settings {
    pub fn validate(&self) -> Result<()> {
        for name in [&self.chat_name, &self.notes_name] {
            if name.trim().is_empty() || name.len() > 200 || name.chars().any(char::is_control) {
                return Err(anyhow!("Invalid Main Chat name"));
            }
        }
        if !["every_write", "new_topics", "automatic"].contains(&self.write_mode.as_str())
            || !["tool", "each", "recurring", "free"].contains(&self.schedule_approval.as_str())
        {
            return Err(anyhow!("Invalid Main Chat configuration"));
        }
        Ok(())
    }
}

pub fn empty() -> Value {
    json!({
        "schema_version": 1,
        "revision": 0,
        "notes_version": 0,
        "settings": Settings::default(),
        "skill_overrides": {"contribute": null, "review": null},
        "session_id": null,
        "topics": {},
        "proposals": {},
        "changes": {},
        "receipts": {}
    })
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub id: String,
    pub text: String,
    pub state: String,
    #[serde(default)]
    pub sources: Vec<String>,
    pub subtopic: Option<String>,
    pub timing: Option<Value>,
    pub condition: Option<String>,
    pub progress: Option<String>,
    pub outcome: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Contribution {
    pub topic_id: String,
    pub title: String,
    pub scope: String,
    #[serde(default)]
    pub groups: Vec<String>,
    pub entry: Entry,
}

pub fn identifier(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
}

impl Contribution {
    pub fn validate(&self) -> Result<()> {
        if !identifier(&self.topic_id)
            || !identifier(&self.entry.id)
            || self.title.trim().is_empty()
            || self.title.len() > 200
            || self.scope.trim().is_empty()
            || self.scope.len() > 4096
            || self.entry.text.trim().is_empty()
            || self.entry.text.len() > 8192
            || self.groups.len() > 16
            || self.groups.iter().any(|v| v.len() > 200)
            || self.entry.sources.len() > 16
            || self.entry.sources.iter().any(|v| v.len() > 1024)
            || self.entry.subtopic.as_ref().is_some_and(|v| v.len() > 200)
            || self
                .entry
                .condition
                .as_ref()
                .is_some_and(|v| v.len() > 2048)
            || self.entry.progress.as_ref().is_some_and(|v| v.len() > 4096)
            || self.entry.outcome.as_ref().is_some_and(|v| v.len() > 4096)
            || ![
                "open",
                "waiting",
                "blocked",
                "scheduled",
                "in_progress",
                "done",
                "cancelled",
            ]
            .contains(&self.entry.state.as_str())
            || self
                .entry
                .timing
                .as_ref()
                .is_some_and(|v| serde_json::to_vec(v).map_or(true, |b| b.len() > 1024))
        {
            return Err(anyhow!("Invalid Main Notes contribution"));
        }
        Ok(())
    }
}

/// Merge by topic identity, also folding exact normalized titles to prevent parallel duplicate topics.
pub fn target(state: &Value, c: &Contribution) -> String {
    if state["topics"].get(&c.topic_id).is_some() {
        return c.topic_id.clone();
    }
    state["topics"]
        .as_object()
        .into_iter()
        .flatten()
        .find(|(_, topic)| {
            topic["title"]
                .as_str()
                .is_some_and(|title| title.trim().to_lowercase() == c.title.trim().to_lowercase())
        })
        .map(|(id, _)| id.clone())
        .unwrap_or_else(|| c.topic_id.clone())
}

pub fn apply(
    state: &mut Value,
    c: &Contribution,
    origin: &str,
    approved: bool,
    now: u64,
) -> Result<Value> {
    c.validate()?;
    let id = target(state, c);
    let new = state["topics"].get(&id).is_none();
    if new && state["topics"].as_object().is_none_or(|v| v.len() >= 256) {
        return Err(anyhow!("Topic capacity reached"));
    }
    // The change history is uncapped: its versions only order changes for other sessions.
    // Fold independent IDs only for the same unfinished intent and timing.
    let entries = &state["topics"][&id]["entries"];
    let entry_id = if entries.get(&c.entry.id).is_some() {
        c.entry.id.clone()
    } else {
        entries
            .as_object()
            .into_iter()
            .flatten()
            .find(|(_, e)| {
                e["text"]
                    .as_str()
                    .is_some_and(|text| text.trim() == c.entry.text.trim())
                    && e["timing"] == json!(c.entry.timing)
                    && e["condition"] == json!(c.entry.condition)
                    && !matches!(e["state"].as_str(), Some("done" | "cancelled"))
            })
            .map(|(key, _)| key.clone())
            .unwrap_or_else(|| c.entry.id.clone())
    };
    let old = entries[&entry_id].clone();
    let old_title = (!new).then(|| {
        state["topics"][&id]["title"]
            .as_str()
            .unwrap_or(&id)
            .to_owned()
    });
    if new {
        state["topics"][&id] = json!({"id":id,"entries":{},"revision":0});
    }
    let entries = &state["topics"][&id]["entries"];
    if entries.get(&entry_id).is_none() && entries.as_object().is_none_or(|v| v.len() >= 256) {
        return Err(anyhow!("Topic entry capacity reached"));
    }
    let scope_changed = state["topics"][&id]["scope"] != c.scope;
    let topic_changed = state["topics"][&id]["title"] != c.title.trim()
        || state["topics"][&id]["groups"] != json!(c.groups);
    state["topics"][&id]["title"] = json!(c.title.trim());
    state["topics"][&id]["scope"] = json!(c.scope);
    if approved {
        state["topics"][&id]["approved_scope"] = json!(c.scope);
    }
    state["topics"][&id]["groups"] = json!(c.groups);
    let mut entry = serde_json::to_value(&c.entry)?;
    entry["id"] = json!(entry_id);
    if entry_id != c.entry.id {
        let mut sources = old["sources"].as_array().cloned().unwrap_or_default();
        for source in &c.entry.sources {
            let value = json!(source);
            if !sources.contains(&value) {
                sources.push(value);
            }
        }
        if sources.len() > 16 {
            return Err(anyhow!("Source reference capacity reached"));
        }
        entry["sources"] = json!(sources);
        for field in ["progress", "outcome"] {
            if entry[field].is_null() {
                entry[field] = old[field].clone();
            }
        }
        if entry["state"] == "open" && !old.is_null() {
            entry["state"] = old["state"].clone();
        }
    }
    entry["origin_session_id"] = json!(origin);
    entry["updated_at"] = json!(now);
    entry["automation_id"] = json!(format!(
        "main-notes-action-{}",
        &digest(&json!([state["agent_id"], id, entry_id]))[..40]
    ));
    state["topics"][&id]["entries"][&entry_id] = entry;
    let revision = state["topics"][&id]["revision"].as_u64().unwrap_or(0) + 1;
    state["topics"][&id]["revision"] = json!(revision);
    let version = state["notes_version"].as_u64().unwrap_or(0) + 1;
    state["notes_version"] = json!(version);
    // Only state/progress transitions from the coordinator are quiet. Changed intent,
    // timing, conditions or scope still wakes review even when Main Chat authored it.
    let progress_only = !new
        && !scope_changed
        && !topic_changed
        && old["text"] == c.entry.text
        && old["timing"] == json!(c.entry.timing)
        && old["condition"] == json!(c.entry.condition)
        && old["subtopic"] == json!(c.entry.subtopic)
        && old["sources"] == json!(c.entry.sources);
    let review = state["session_id"] != origin
        || !progress_only
        || (old["state"] != "open"
            && state["topics"][&id]["entries"][&entry_id]["state"] == "open");
    let event = json!({
        "id": format!("main-notes-{}-{version}", state["agent_id"].as_str().unwrap_or("")),
        "agent_id": state["agent_id"],
        "version": version,
        "topic_id": id,
        "entry_id": entry_id,
        "scope_changed": scope_changed,
        "topic_changed": topic_changed,
        "origin_session_id": origin,
        "review_required": review,
        // The entry's line before and after, so other sessions receive this change as a diff.
        "before": if old.is_null() { Value::Null } else { json!(entry_line(&old)) },
        "after": entry_line(&state["topics"][&id]["entries"][&entry_id]),
        // The topic's title before a rename, so the update can show it.
        "title_before": old_title
            .filter(|_| topic_changed)
            .map_or(Value::Null, Value::from),
    });
    state["changes"][version.to_string()] = event.clone();
    Ok(json!({"status":"committed","change":event}))
}

pub fn contribute(
    state: &mut Value,
    c: &Contribution,
    origin: &str,
    proposal_id: &str,
    human: bool,
    now: u64,
) -> Result<Value> {
    c.validate()?;
    let topic = &state["topics"][target(state, c)];
    let mode = state["settings"]["write_mode"]
        .as_str()
        .unwrap_or("every_write");
    let approval = !human
        && match mode {
            "automatic" => false,
            "new_topics" => topic.is_null() || topic["approved_scope"] != c.scope,
            _ => true,
        };
    if approval {
        if state["proposals"]
            .as_object()
            .is_none_or(|v| v.len() >= 256)
        {
            return Err(anyhow!("Proposal capacity reached"));
        }
        state["proposals"][proposal_id] = json!({
            "id": proposal_id,
            "contribution": c,
            "origin_session_id": origin,
            "topic_revision": topic["revision"].as_u64().unwrap_or(0),
            "status": "pending",
            "created_at": now
        });
        return Ok(
            json!({"status":"awaiting_approval","proposal_id":proposal_id,"execution_authorized":false}),
        );
    }
    apply(state, c, origin, human, now)
}

/// Deletes a topic and its entries. A topic with an unfinished timed entry is kept: its
/// scheduled action would outlive the note, so the entry must be done or cancelled first.
/// A pending proposal for the topic then recreates it if approved, or fails as stale.
pub fn delete_topic(state: &mut Value, id: &str) -> Result<Value> {
    let Some(topic) = state["topics"].get(id) else {
        return Err(anyhow!("Topic not found"));
    };
    let scheduled = topic["entries"]
        .as_object()
        .into_iter()
        .flatten()
        .any(|(_, e)| {
            !e["timing"].is_null() && !matches!(e["state"].as_str(), Some("done" | "cancelled"))
        });
    if scheduled {
        return Err(anyhow!(
            "Mark this topic's timed entries done or cancelled before deleting it"
        ));
    }
    // Other sessions receive the deletion as an update listing what was removed.
    let heading = topic["title"].as_str().unwrap_or(id).to_owned();
    let removed: Vec<String> = topic["entries"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(_, entry)| entry_line(entry))
        .collect();
    let version = state["notes_version"].as_u64().unwrap_or(0) + 1;
    state["notes_version"] = json!(version);
    let event = json!({
        "id": format!("main-notes-{}-{version}", state["agent_id"].as_str().unwrap_or("")),
        "agent_id": state["agent_id"],
        "version": version,
        "topic_id": id,
        "origin_session_id": "",
        "review_required": false,
        "deleted_topic": heading,
        "removed": removed,
    });
    state["changes"][version.to_string()] = event;
    if let Some(topics) = state["topics"].as_object_mut() {
        topics.remove(id);
    }
    Ok(json!({"status":"deleted","topic_id":id}))
}

/// Scheduling identity is host-derived; terminal or retimed intents cannot admit another occurrence.
pub fn action_intent(topic: &Value, entry: &Value) -> String {
    digest(&json!([
        topic["scope"],
        entry["text"],
        entry["timing"],
        entry["condition"]
    ]))
}

pub fn scheduled_entry_current(notes: &Value, aid: &str, definition: &Value) -> bool {
    definition["destination"]["session_id"] == notes["session_id"]
        && notes["topics"]
            .as_object()
            .into_iter()
            .flatten()
            .any(|(_, topic)| {
                topic["entries"]
                    .as_object()
                    .into_iter()
                    .flatten()
                    .any(|(_, entry)| {
                        entry["automation_id"] == aid
                            && (entry["timing"] == definition["trigger"]
                                || entry["timing"]["kind"] == "delay"
                                    && definition["trigger"]["kind"] == "once"
                                    && definition["trigger"]["at_ms"].as_i64().is_some())
                            && !entry["timing"].is_null()
                            && definition["main_notes_intent"] == action_intent(topic, entry)
                            && !matches!(entry["state"].as_str(), Some("done" | "cancelled"))
                    })
            })
}

/// One entry as a context line: ID, state, intent and any progress, outcome, timing or condition.
fn entry_line(entry: &Value) -> String {
    let mut line = format!(
        "- [entry {}] ({}) {}",
        entry["id"].as_str().unwrap_or(""),
        entry["state"].as_str().unwrap_or(""),
        entry["text"].as_str().unwrap_or("").trim()
    );
    for (label, field) in [
        ("progress", "progress"),
        ("outcome", "outcome"),
        ("if", "condition"),
    ] {
        if let Some(text) = entry[field].as_str().filter(|t| !t.trim().is_empty()) {
            line.push_str(&format!(" | {label}: {}", text.trim()));
        }
    }
    if !entry["timing"].is_null() {
        line.push_str(&format!(" | timing: {}", entry["timing"]));
    }
    line
}

/// A topic's heading with its ID and scope.
fn topic_heading(id: &str, topic: &Value) -> String {
    format!(
        "## {} [topic {id}] scope: {}",
        topic["title"].as_str().unwrap_or(id),
        topic["scope"].as_str().unwrap_or("").trim()
    )
}

/// Every Main Notes topic and entry as model context, or `None` when there are none.
/// Notes are quoted data: they never grant authority.
pub fn render_notes(state: &Value) -> Option<String> {
    let topics = state["topics"].as_object().filter(|t| !t.is_empty())?;
    let mut text = self::text(state, "notes_header")
        .replace("{version}", &state["notes_version"].to_string())
        .replace("{revision}", &state["revision"].to_string());
    let mut ordered: Vec<(&String, &Value)> = topics.iter().collect();
    ordered.sort_by_key(|(id, topic)| (topic["title"].as_str().unwrap_or("").to_lowercase(), *id));
    for (id, topic) in ordered {
        text.push_str("\n\n");
        text.push_str(&topic_heading(id, topic));
        for entry in topic["entries"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(_, e)| e)
        {
            text.push('\n');
            text.push_str(&entry_line(entry));
        }
    }
    Some(text)
}

/// One update per notes version after `after` that a session other than `sid` wrote, as
/// `(version, text)` diffs: the entry's line before (`-`) and after (`+`). Older change
/// records without lines show the entry's current line as added.
pub fn render_updates(state: &Value, after: u64, sid: &str) -> Vec<(u64, String)> {
    let mut versions: Vec<(u64, &Value)> = state["changes"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(version, change)| Some((version.parse().ok()?, change)))
        .filter(|(version, change)| *version > after && change["origin_session_id"] != sid)
        .collect();
    versions.sort_by_key(|(version, _)| *version);
    versions
        .into_iter()
        .filter_map(|(version, change)| {
            let mut text = self::text(state, "update_heading")
                .replace("{version}", &version.to_string())
                .replace(
                    "{revision}",
                    &state["revision"].as_u64().unwrap_or(0).to_string(),
                );
            // A deleted topic: its title and every entry it held.
            if let Some(title) = change["deleted_topic"].as_str() {
                text.push_str(&format!("\n## {title}"));
                let removed = change["removed"].as_array().map_or(0, Vec::len);
                if removed == 0 {
                    text.push_str("\nRemoved: the topic");
                }
                for line in change["removed"].as_array().into_iter().flatten() {
                    text.push_str(&format!(
                        "\nRemoved: {}",
                        plain(line.as_str().unwrap_or(""))
                    ));
                }
                return Some((version, text));
            }
            let topic_id = change["topic_id"].as_str().unwrap_or("");
            let topic = &state["topics"][topic_id];
            let title = topic["title"].as_str().unwrap_or(topic_id);
            match change["title_before"].as_str() {
                Some(before) if before != title => {
                    text.push_str(&format!("\n## {title} (was {before})"));
                }
                _ => text.push_str(&format!("\n## {title}")),
            }
            let after = match change["after"].as_str() {
                Some(after) => after.to_owned(),
                None => {
                    let entry = &topic["entries"][change["entry_id"].as_str().unwrap_or("")];
                    if entry.is_null() {
                        return None;
                    }
                    entry_line(entry)
                }
            };
            let (before, after) = (change["before"].as_str().map(plain), plain(&after));
            if before.as_deref() != Some(after.as_str()) {
                if let Some(before) = before {
                    text.push_str(&format!("\nRemoved: {before}"));
                }
                text.push_str(&format!("\nAdded: {after}"));
            }
            Some((version, text))
        })
        .collect()
}

/// An entry line as plain text for an update: its words, with its state when not open and
/// any progress, outcome, condition or timing ("- [entry id] (state) text | …" becomes
/// "text (state) | …").
fn plain(line: &str) -> String {
    let line = line.strip_prefix("- ").unwrap_or(line);
    let Some(rest) = line
        .strip_prefix("[entry ")
        .and_then(|r| r.split_once("] ("))
    else {
        return line.to_owned();
    };
    let Some((entry_state, text)) = rest.1.split_once(") ") else {
        return line.to_owned();
    };
    let (words, details) = text
        .split_once(" | ")
        .map_or((text, None), |(w, d)| (w, Some(d)));
    let mut out = words.to_owned();
    if entry_state != "open" {
        out.push_str(&format!(" ({entry_state})"));
    }
    if let Some(details) = details {
        out.push_str(&format!(" | {details}"));
    }
    out
}

pub fn public(state: &Value) -> Value {
    let mut value = state.clone();
    value.as_object_mut().unwrap().remove("receipts");
    value
}

pub fn request_fields(body: &Value) -> Result<(u64, String)> {
    let revision = body["expected_revision"]
        .as_u64()
        .ok_or_else(|| anyhow!("Expected revision required"))?;
    let request = body["request_id"]
        .as_str()
        .filter(|v| identifier(v))
        .ok_or_else(|| anyhow!("Stable request ID required"))?;
    Ok((revision, request.into()))
}

/// Stable agent qualification keeps subscriptions/actions distinct across agents.
pub fn review_id(agent: &str) -> String {
    format!("main-notes-review-{}", &digest(&json!(agent))[..40])
}

pub fn owns(name: &str) -> bool {
    matches!(name, "main_notes_read" | "main_notes_write")
}
