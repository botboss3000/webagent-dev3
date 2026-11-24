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

/// The message arguments of agent-messaging tools: free in a "these values" approval, which
/// fixes the destination and delivery, not the words.
pub const MESSAGE_ARGS: [&str; 2] = ["text", "title"];

/// Whether a call may be remembered or run on a saved approval. A messaging call in a run
/// that an agent message started never may, so one approval cannot relay agent messages.
pub fn rememberable_call(name: &str, critical: bool, messaging: bool, onward: bool) -> bool {
    rememberable_tool(name, critical) && !(messaging && onward)
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
    /// Arguments left free whatever their length (`MESSAGE_ARGS` for messaging tools).
    pub always_free: &'a [String],
}

impl NewEntry<'_> {
    pub fn to_value(&self) -> Value {
        // "These values" fixes the short arguments and records the names of the long ones,
        // which a later call may change; it may add no other argument.
        let (fixed, free) = if self.choice.values {
            let mut fixed = fixed_values(self.args);
            fixed.retain(|name, _| !self.always_free.contains(name));
            let mut free: Vec<&String> = self
                .args
                .as_object()
                .map(|fields| {
                    fields
                        .keys()
                        .filter(|name| !fixed.contains_key(*name))
                        .collect()
                })
                .unwrap_or_default();
            let extra: Vec<&String> = self
                .always_free
                .iter()
                .filter(|name| !free.contains(name))
                .collect();
            free.extend(extra);
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

/// The owner's entries for the agent's Permissions & Actions page. `current(tool)` is the
/// tool's permission fingerprint now, or `None` when it can no longer be remembered (critical,
/// removed); an entry whose fingerprint differs is marked `lapsed` (ignored until approved
/// again).
pub fn view(doc: &Value, owner: &str, current: &dyn Fn(&str) -> Option<String>) -> Vec<Value> {
    doc["entries"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .filter(|entry| entry["owner"] == owner)
                .map(|entry| {
                    let tool = entry["tool"].as_str().unwrap_or("");
                    let lapsed = current(tool)
                        .is_none_or(|fingerprint| entry["permission_fingerprint"] != fingerprint);
                    let mut shown = entry.clone();
                    shown["lapsed"] = json!(lapsed);
                    shown
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A change the owner makes on the page.
pub enum Edit {
    Revoke,
    /// New match and reach. "These values" needs the values the entry already fixes.
    Change(Choice),
    /// Approve a lapsed entry again under the tool's permission now (its fingerprint).
    Reapprove(String),
}

/// The document after applying `edit` to the owner's entry `id`, with the revision raised.
pub fn edit(doc: &Value, owner: &str, id: &str, edit: Edit, settings: Settings) -> Result<Value> {
    let mut entries: Vec<Value> = doc["entries"].as_array().cloned().unwrap_or_default();
    let Some(index) = entries
        .iter()
        .position(|entry| entry["id"] == id && entry["owner"] == owner)
    else {
        bail!("Saved approval not found");
    };
    match edit {
        Edit::Revoke => {
            entries.remove(index);
        }
        Edit::Change(choice) => {
            let entry = &mut entries[index];
            if choice.agent_wide && !settings.agent_wide {
                bail!("Agent-wide saved approvals are switched off");
            }
            if choice.values && !entry["fixed"].is_object() {
                bail!("This approval recorded no values; approve a call with the values to keep");
            }
            if !choice.values {
                entry["fixed"] = Value::Null;
                entry["free"] = Value::Null;
            }
            entry["match"] = json!(if choice.values { "values" } else { "any" });
            entry["reach"] = json!(if choice.agent_wide { "agent" } else { "chat" });
        }
        Edit::Reapprove(fingerprint) => {
            entries[index]["permission_fingerprint"] = json!(fingerprint);
            entries[index]["via"] = json!("page");
        }
    }
    Ok(json!({"revision":doc["revision"].as_u64().unwrap_or(0) + 1,"entries":entries}))
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
            always_free: &[],
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
    fn messaging_values_fix_the_destination_not_the_words_and_never_relay() {
        let free: Vec<String> = MESSAGE_ARGS.iter().map(|name| (*name).to_owned()).collect();
        let saved = json!({"session_id":"s9","text":"first","delivery":"queue"});
        let e = NewEntry {
            id: "m1",
            choice: Choice {
                values: true,
                agent_wide: false,
            },
            tool: "message_session",
            args: &saved,
            session_id: "s1",
            agent_id: "shared_default",
            owner: "admin",
            fingerprint: "fp",
            created_ms: 1,
            always_free: &free,
        }
        .to_value();
        assert_eq!(e["fixed"], json!({"session_id":"s9","delivery":"queue"}));
        let later = json!({"session_id":"s9","text":"second","delivery":"queue"});
        let mut call2 = Call {
            tool: "message_session",
            args: &later,
            session_id: "s1",
            owner: "admin",
            fingerprint: "fp",
            settings: Settings::default(),
        };
        assert!(matches(&e, &call2));
        let other = json!({"session_id":"s8","text":"second","delivery":"queue"});
        call2.args = &other;
        assert!(!matches(&e, &call2));
        let now = json!({"session_id":"s9","text":"second","delivery":"now"});
        call2.args = &now;
        assert!(!matches(&e, &call2));
        assert!(rememberable_call("message_session", false, true, false));
        assert!(!rememberable_call("message_session", false, true, true));
        assert!(rememberable_call("file_write", false, false, true));
    }

    #[test]
    fn critical_tools_and_the_auto_switch_are_never_rememberable() {
        assert!(rememberable_tool("file_write", false));
        assert!(!rememberable_tool("checkout", true));
        assert!(!rememberable_tool("set_execution_mode", false));
    }

    #[test]
    fn the_page_lists_own_entries_marks_lapsed_and_edits_them() {
        let args = json!({"path":"a.md","content":"x"});
        let saved = entry(true, false, &args);
        let doc = add(&json!(null), saved);
        let fresh = |_: &str| Some("fp".to_owned());
        let moved = |_: &str| Some("f2".to_owned());
        let gone = |_: &str| None;
        assert_eq!(view(&doc, "admin", &fresh)[0]["lapsed"], false);
        assert_eq!(view(&doc, "admin", &moved)[0]["lapsed"], true);
        assert_eq!(view(&doc, "admin", &gone)[0]["lapsed"], true);
        assert!(view(&doc, "u2", &fresh).is_empty());

        let on = Settings::default();
        let off = Settings {
            enabled: true,
            agent_wide: false,
        };
        let wide = Choice {
            values: true,
            agent_wide: true,
        };
        let any = Choice {
            values: false,
            agent_wide: false,
        };
        // Another user's id, an unknown entry and agent-wide while it is switched off fail.
        assert!(edit(&doc, "u2", "e1", Edit::Revoke, on).is_err());
        assert!(edit(&doc, "admin", "nope", Edit::Revoke, on).is_err());
        assert!(edit(&doc, "admin", "e1", Edit::Change(wide), off).is_err());
        let changed = edit(&doc, "admin", "e1", Edit::Change(wide), on).unwrap();
        assert_eq!(changed["revision"], 2);
        assert_eq!(changed["entries"][0]["reach"], "agent");
        let opened = edit(&changed, "admin", "e1", Edit::Change(any), on).unwrap();
        assert_eq!(opened["entries"][0]["match"], "any");
        assert!(opened["entries"][0]["fixed"].is_null());
        // "Any values" cannot go back to values it never recorded.
        let back = Choice {
            values: true,
            agent_wide: false,
        };
        assert!(edit(&opened, "admin", "e1", Edit::Change(back), on).is_err());
        let again = edit(&doc, "admin", "e1", Edit::Reapprove("f2".into()), on).unwrap();
        assert_eq!(again["entries"][0]["permission_fingerprint"], "f2");
        assert_eq!(again["entries"][0]["via"], "page");
        let revoked = edit(&doc, "admin", "e1", Edit::Revoke, on).unwrap();
        assert_eq!(revoked["entries"], json!([]));
    }
}
