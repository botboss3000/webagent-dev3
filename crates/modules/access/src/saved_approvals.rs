//! Saved approvals: "Remember this approval" on a tool approval card. A saved approval lets
//! later matching calls of a tool that requires approval run without asking, in one chat or
//! for one agent in all the owner's chats, until revoked or lapsed.
//!
//! One document per agent, `saved_approvals:{agent_id}` in the chat owner's store, holds every
//! entry with an integer revision raised on each write. An entry records a fingerprint of the
//! tool's permission inputs when it was saved; a different fingerprint now means the tool's
//! permission changed and the entry has lapsed (ignored, kept for review). These rules are
//! pure; the engine reads the document only for calls that would otherwise ask.
use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

/// Entries one agent's document keeps; the oldest are dropped first.
pub const MAX_ENTRIES: usize = 64;
/// Longest argument value (characters, or serialized characters for lists and objects) that a
/// "these values" approval fixes. Longer values, such as file contents, are not part of the
/// match.
pub const MAX_VALUE_CHARS: usize = 1000;

/// The access policy's `saved_approvals` section; a missing section allows both.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    /// Approval cards offer "Remember this approval" and saved approvals are used.
    pub enabled: bool,
    /// Saved approvals may cover the agent in all the owner's chats, not just one chat.
    pub agent_wide: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: true,
            agent_wide: true,
        }
    }
}

impl Settings {
    /// Interpret the access-policy document.
    pub fn from_policy(policy: &Value) -> Result<Self> {
        let Some(section) = policy.get("saved_approvals") else {
            return Ok(Self::default());
        };
        if !section.is_object() {
            bail!("Invalid saved_approvals policy: expected an object");
        }
        serde_json::from_value(section.clone())
            .map_err(|error| anyhow!("Invalid saved_approvals policy: {error}"))
    }
}

/// The document key for one agent's saved approvals.
pub fn key(agent_id: &str) -> String {
    format!("saved_approvals:{agent_id}")
}

/// Whether a tool can ever be remembered. Critical tools always ask; the switch to Auto has
/// its own card.
pub fn rememberable_tool(name: &str, critical: bool) -> bool {
    !critical && name != "set_execution_mode"
}

/// What the user chose on the card.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Choice {
    /// Only calls with the same key argument values (`true`), or any values.
    pub values: bool,
    /// Every chat with this agent (`true`), or only this chat.
    pub agent_wide: bool,
}

impl Choice {
    /// Read `{"match":"values"|"any","reach":"chat"|"agent"}`.
    pub fn parse(value: &Value) -> Result<Self> {
        let Some(fields) = value.as_object() else {
            bail!("Invalid remember choice");
        };
        if fields.keys().any(|key| key != "match" && key != "reach") {
            bail!("Invalid remember choice");
        }
        let values = match value["match"].as_str() {
            Some("values") => true,
            Some("any") => false,
            _ => bail!("Invalid remember match"),
        };
        let agent_wide = match value["reach"].as_str() {
            Some("chat") => false,
            Some("agent") => true,
            _ => bail!("Invalid remember reach"),
        };
        Ok(Self { values, agent_wide })
    }
}

/// The argument values a "these values" approval fixes: every top-level argument whose
/// value is at most `MAX_VALUE_CHARS` long.
pub fn fixed_values(args: &Value) -> Map<String, Value> {
    args.as_object()
        .map(|fields| {
            fields
                .iter()
                .filter(|(_, value)| match value {
                    Value::String(text) => text.chars().count() <= MAX_VALUE_CHARS,
                    Value::Array(_) | Value::Object(_) => {
                        value.to_string().chars().count() <= MAX_VALUE_CHARS
                    }
                    _ => true,
                })
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect()
        })
        .unwrap_or_default()
}

/// One new entry. `session_id` is the chat it was saved in; it limits a chat-wide entry.
pub struct NewEntry<'a> {
    pub id: &'a str,
    pub choice: Choice,
    pub tool: &'a str,
    pub args: &'a Value,
    pub session_id: &'a str,
    pub agent_id: &'a str,
    pub owner: &'a str,
    pub fingerprint: &'a str,
    pub created_ms: u64,
}

impl NewEntry<'_> {
    pub fn to_value(&self) -> Value {
        // "These values" fixes the short arguments and records the names of the long ones,
        // which a later call may change; it may add no other argument.
        let (fixed, free) = if self.choice.values {
            let fixed = fixed_values(self.args);
            let free: Vec<&String> = self
                .args
                .as_object()
                .map(|fields| {
                    fields
                        .keys()
                        .filter(|name| !fixed.contains_key(*name))
                        .collect()
                })
                .unwrap_or_default();
            (Value::Object(fixed.clone()), json!(free))
        } else {
            (Value::Null, Value::Null)
        };
        json!({"id":self.id,"tool":self.tool,
            "match":if self.choice.values {"values"} else {"any"},"fixed":fixed,"free":free,
            "reach":if self.choice.agent_wide {"agent"} else {"chat"},
            "session_id":self.session_id,"agent_id":self.agent_id,"owner":self.owner,
            "permission_fingerprint":self.fingerprint,"via":"card","created_ms":self.created_ms})
    }
}

/// The document after adding `entry`: the revision is raised, an entry with the same tool,
/// match, values and reach (and chat, for a chat-wide one) is replaced, and the oldest entries
/// beyond `MAX_ENTRIES` are dropped.
pub fn add(doc: &Value, entry: Value) -> Value {
    let same = |other: &Value| {
        ["tool", "match", "fixed", "reach"]
            .iter()
            .all(|field| other[field] == entry[field])
            && (entry["reach"] == "agent" || other["session_id"] == entry["session_id"])
    };
    let mut entries: Vec<Value> = doc["entries"]
        .as_array()
        .map(|items| items.iter().filter(|e| !same(e)).cloned().collect())
        .unwrap_or_default();
    entries.push(entry);
    let excess = entries.len().saturating_sub(MAX_ENTRIES);
    entries.drain(..excess);
    json!({"revision":doc["revision"].as_u64().unwrap_or(0) + 1,"entries":entries})
}

/// A call that would otherwise wait for approval.
pub struct Call<'a> {
    pub tool: &'a str,
    pub args: &'a Value,
    pub session_id: &'a str,
    pub owner: &'a str,
    /// The tool's permission fingerprint now (`LiveAccess::tool_permission_fingerprint`).
    pub fingerprint: &'a str,
    pub settings: Settings,
}

/// Whether `entry` lets `call` run without asking. An entry whose fingerprint differs has
/// lapsed; anything missing or malformed does not match.
pub fn matches(entry: &Value, call: &Call<'_>) -> bool {
    if !call.settings.enabled
        || entry["tool"] != call.tool
        || entry["owner"] != call.owner
        || entry["permission_fingerprint"] != call.fingerprint
    {
        return false;
    }
    let reach = match entry["reach"].as_str() {
        Some("chat") => entry["session_id"] == call.session_id,
        Some("agent") => call.settings.agent_wide,
        _ => false,
    };
    reach
        && match entry["match"].as_str() {
            Some("any") => true,
            Some("values") => entry["fixed"].as_object().is_some_and(|fixed| {
                // Every fixed value must be present and equal; any other argument must be one
                // the approval left free (it was long then).
                let free = entry["free"].as_array();
                fixed
                    .iter()
                    .all(|(name, value)| call.args.get(name) == Some(value))
                    && call.args.as_object().is_some_and(|fields| {
                        fields.keys().all(|name| {
                            fixed.contains_key(name)
                                || free.is_some_and(|free| free.iter().any(|f| f == name))
                        })
                    })
            }),
            _ => false,
        }
}

/// The first entry of `doc` that lets `call` run without asking.
pub fn find<'a>(doc: &'a Value, call: &Call<'_>) -> Option<&'a Value> {
    doc["entries"]
        .as_array()?
        .iter()
        .find(|entry| matches(entry, call))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(values: bool, agent_wide: bool, args: &Value) -> Value {
        NewEntry {
            id: "e1",
            choice: Choice { values, agent_wide },
            tool: "file_write",
            args,
            session_id: "s1",
            agent_id: "shared_default",
            owner: "admin",
            fingerprint: "fp",
            created_ms: 1,
        }
        .to_value()
    }

    fn call<'a>(args: &'a Value, session_id: &'a str) -> Call<'a> {
        Call {
            tool: "file_write",
            args,
            session_id,
            owner: "admin",
            fingerprint: "fp",
            settings: Settings::default(),
        }
    }

    #[test]
    fn values_match_only_the_same_short_arguments() {
        let long = "x".repeat(MAX_VALUE_CHARS + 1);
        let saved = json!({"path":"a.md","content":long});
        let e = entry(true, false, &saved);
        assert_eq!(e["fixed"], json!({"path":"a.md"}));
        assert!(matches(
            &e,
            &call(&json!({"path":"a.md","content":"other text"}), "s1")
        ));
        assert!(!matches(
            &e,
            &call(&json!({"path":"b.md","content":"x"}), "s1")
        ));
        assert!(!matches(&e, &call(&json!({"content":"x"}), "s1")));
        assert!(matches(&e, &call(&json!({"path":"a.md"}), "s1")));
        assert!(!matches(
            &e,
            &call(&json!({"path":"a.md","content":"x","mode":"append"}), "s1")
        ));
    }

    #[test]
    fn any_values_match_every_call_of_the_tool() {
        let e = entry(false, false, &json!({"path":"a.md"}));
        assert!(e["fixed"].is_null());
        assert!(matches(&e, &call(&json!({"path":"b.md"}), "s1")));
        let empty = json!({});
        let mut other = call(&empty, "s1");
        other.tool = "file_delete";
        assert!(!matches(&e, &other));
    }

    #[test]
    fn chat_reach_stays_in_its_chat_and_agent_reach_follows_the_setting() {
        let args = json!({"path":"a.md"});
        let chat = entry(true, false, &args);
        assert!(!matches(&chat, &call(&args, "s2")));
        let agent = entry(true, true, &args);
        assert!(matches(&agent, &call(&args, "s2")));
        let mut off = call(&args, "s2");
        off.settings.agent_wide = false;
        assert!(!matches(&agent, &off));
        assert!(matches(
            &chat,
            &Call {
                session_id: "s1",
                ..off
            }
        ));
    }

    #[test]
    fn lapsed_disabled_or_foreign_entries_never_match() {
        let args = json!({"path":"a.md"});
        let e = entry(false, true, &args);
        let mut changed = call(&args, "s1");
        changed.fingerprint = "fp2";
        assert!(!matches(&e, &changed));
        let mut disabled = call(&args, "s1");
        disabled.settings.enabled = false;
        assert!(!matches(&e, &disabled));
        let mut other = call(&args, "s1");
        other.owner = "bob";
        assert!(!matches(&e, &other));
        assert!(!matches(&json!({"tool":"file_write"}), &call(&args, "s1")));
    }

    #[test]
    fn add_raises_the_revision_replaces_duplicates_and_bounds_the_list() {
        let args = json!({"path":"a.md"});
        let one = add(&Value::Null, entry(true, false, &args));
        assert_eq!(one["revision"], 1);
        let again = add(&one, entry(true, false, &args));
        assert_eq!(again["revision"], 2);
        assert_eq!(again["entries"].as_array().map(Vec::len), Some(1));
        let mut doc = again;
        for n in 0..MAX_ENTRIES + 3 {
            doc = add(&doc, entry(true, false, &json!({"path":format!("{n}.md")})));
        }
        let entries = doc["entries"].as_array().map(Vec::len);
        assert_eq!(entries, Some(MAX_ENTRIES));
        assert_eq!(doc["entries"][0]["fixed"]["path"], "3.md");
    }

    #[test]
    fn choice_parse_refuses_unknown_shapes() {
        let ok = Choice::parse(&json!({"match":"any","reach":"agent"})).unwrap();
        assert_eq!(
            ok,
            Choice {
                values: false,
                agent_wide: true
            }
        );
        for bad in [
            json!(true),
            json!({"match":"any"}),
            json!({"match":"some","reach":"chat"}),
            json!({"match":"any","reach":"chat","extra":1}),
        ] {
            assert!(Choice::parse(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn settings_default_on_and_refuse_bad_sections() {
        assert_eq!(
            Settings::from_policy(&json!({})).unwrap(),
            Settings::default()
        );
        let off = Settings::from_policy(&json!({"saved_approvals":{"agent_wide":false}})).unwrap();
        assert!(off.enabled && !off.agent_wide);
        assert!(Settings::from_policy(&json!({"saved_approvals":[]})).is_err());
        assert!(Settings::from_policy(&json!({"saved_approvals":{"other":true}})).is_err());
    }

    #[test]
    fn critical_tools_and_the_auto_switch_are_never_rememberable() {
        assert!(rememberable_tool("file_write", false));
        assert!(!rememberable_tool("checkout", true));
        assert!(!rememberable_tool("set_execution_mode", false));
    }
}
