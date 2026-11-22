//! Which user requests an action ledger record names, so a recorded action can be traced to
//! what the user asked even after its session is deleted. Requests are the task gate's
//! session labels (`u1`, `u2`, ...), counted whatever the gate setting.
use anyhow::{bail, Result};
use serde_json::{json, Value};
use webagent_chat::task_list::{List, Request, Requests, Status};

/// Request text kept per linked request when the policy file says nothing.
pub const DEFAULT_REQUEST_TEXT_MAX_CHARS: usize = 1000;
const MAX_REQUEST_TEXT_MAX_CHARS: usize = 20_000;
/// Linked requests kept per record; an inferred link takes the newest.
const MAX_LINKED: usize = 16;

/// How a record's requests were found: the in-progress task step cited them, they were
/// inferred as the requests since the session's previous ledger record, or there were none
/// (shown as flagged in the Actions panel).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkKind {
    Cited,
    Inferred,
    None,
}

impl LinkKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cited => "cited",
            Self::Inferred => "inferred",
            Self::None => "none",
        }
    }
}

/// The `ledger` section of the app policy file (`config/access/policy.json`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LedgerPolicy {
    pub request_text_max_chars: usize,
}

impl Default for LedgerPolicy {
    fn default() -> Self {
        Self {
            request_text_max_chars: DEFAULT_REQUEST_TEXT_MAX_CHARS,
        }
    }
}

impl LedgerPolicy {
    /// Read the section from the whole policy document; absent fields take their defaults.
    pub fn from_policy(policy: &Value) -> Result<Self> {
        let Some(section) = policy.get("ledger") else {
            return Ok(Self::default());
        };
        let Some(fields) = section.as_object() else {
            bail!("Policy ledger section must be an object");
        };
        if fields.keys().any(|key| key != "request_text_max_chars") {
            bail!("Policy ledger section allows only request_text_max_chars");
        }
        let mut policy = Self::default();
        if let Some(value) = fields.get("request_text_max_chars") {
            let chars = value
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .filter(|n| (1..=MAX_REQUEST_TEXT_MAX_CHARS).contains(n));
            let Some(chars) = chars else {
                bail!("ledger.request_text_max_chars must be 1..{MAX_REQUEST_TEXT_MAX_CHARS}");
            };
            policy.request_text_max_chars = chars;
        }
        Ok(policy)
    }

    /// Read the live policy file; a missing or invalid file keeps the defaults, since saves
    /// are validated and a ledger record must not fail on configuration.
    #[must_use]
    pub fn live() -> Self {
        webagent_configuration::live::parsed_from(
            &webagent_configuration::live::roots(),
            "config/access/policy.json",
            4 * 1024 * 1024,
            |bytes| Self::from_policy(&serde_json::from_slice(bytes)?),
        )
        .unwrap_or_default()
    }
}

/// The number in a request label (`u12` is 12).
#[must_use]
pub fn label_number(label: &str) -> Option<u32> {
    label.strip_prefix('u')?.parse().ok()
}

/// The requests a critical call answers: those the in-progress step of an open section
/// cites (or its section, when the step cites none), else every request labelled after
/// number `after` (the newest when the session had no earlier record).
#[must_use]
pub fn linked<'a>(
    list: Option<&List>,
    requests: &'a Requests,
    after: u32,
) -> (LinkKind, Vec<&'a Request>) {
    let cited: Vec<&String> = list
        .into_iter()
        .flat_map(|list| &list.sections)
        .filter(|section| section.status == Status::Open)
        .flat_map(|section| {
            section
                .steps
                .iter()
                .filter(|step| step.status == Status::InProgress)
                .map(move |step| {
                    if step.requests.is_empty() {
                        &section.requests
                    } else {
                        &step.requests
                    }
                })
        })
        .flatten()
        .collect();
    let found: Vec<&Request> = requests
        .items
        .iter()
        .filter(|request| cited.contains(&&request.id))
        .collect();
    if !found.is_empty() {
        return (LinkKind::Cited, newest(found));
    }
    let inferred: Vec<&Request> = requests
        .items
        .iter()
        .filter(|request| label_number(&request.id).is_some_and(|n| n > after))
        .collect();
    if inferred.is_empty() {
        (LinkKind::None, Vec::new())
    } else {
        (LinkKind::Inferred, newest(inferred))
    }
}

fn newest(mut requests: Vec<&Request>) -> Vec<&Request> {
    let skip = requests.len().saturating_sub(MAX_LINKED);
    requests.drain(..skip);
    requests
}

/// One linked request as the record stores it: its label, message and text cut to `max`
/// characters (marked when cut).
#[must_use]
pub fn request_entry(request: &Request, text: &str, max: usize) -> Value {
    let cut = text.chars().count() > max;
    let kept: String = text.chars().take(max).collect();
    json!({"label":request.id,"message":request.message,"at":request.at,"text":kept,"cut":cut})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn requests(count: u32) -> Requests {
        let mut requests = Requests::default();
        for n in 1..=count {
            requests.add(&format!("user:r{n}"), &format!("request {n}"), u64::from(n));
        }
        requests
    }

    fn list(section_requests: &[&str], step_requests: &[&str], status: &str) -> List {
        serde_json::from_value(json!({"revision":1,"next_section":2,"sections":[{
            "id":"s1","title":"Work","origin":"user","status":"open","next_step":2,
            "requests":section_requests,
            "steps":[{"id":"s1.1","label":"Do it","status":status,"requests":step_requests}]}]}))
        .unwrap()
    }

    #[test]
    fn the_in_progress_step_cites_before_its_section_and_inference() {
        let requests = requests(4);
        let (kind, found) = linked(Some(&list(&["u1"], &["u2"], "in_progress")), &requests, 3);
        assert_eq!(kind, LinkKind::Cited);
        assert_eq!(found[0].id, "u2");
        let (kind, found) = linked(Some(&list(&["u1"], &[], "in_progress")), &requests, 3);
        assert_eq!((kind, found[0].id.as_str()), (LinkKind::Cited, "u1"));
    }

    #[test]
    fn without_a_citing_step_the_requests_since_the_last_record_are_inferred() {
        let requests = requests(4);
        let (kind, found) = linked(Some(&list(&["u1"], &[], "open")), &requests, 2);
        assert_eq!(kind, LinkKind::Inferred);
        let labels: Vec<&str> = found.iter().map(|request| request.id.as_str()).collect();
        assert_eq!(labels, ["u3", "u4"]);
        assert_eq!(linked(None, &requests, 4), (LinkKind::None, Vec::new()));
    }

    #[test]
    fn request_text_is_cut_to_the_policy_limit_and_marked() {
        let requests = requests(1);
        let entry = request_entry(&requests.items[0], "abcdef", 4);
        assert_eq!(
            (entry["text"].as_str(), entry["cut"].as_bool()),
            (Some("abcd"), Some(true))
        );
        assert_eq!(request_entry(&requests.items[0], "abc", 4)["cut"], false);
    }

    #[test]
    fn policy_section_defaults_and_bounds() {
        assert_eq!(
            LedgerPolicy::from_policy(&json!({}))
                .unwrap()
                .request_text_max_chars,
            DEFAULT_REQUEST_TEXT_MAX_CHARS
        );
        let set = json!({"ledger":{"request_text_max_chars":250}});
        assert_eq!(
            LedgerPolicy::from_policy(&set)
                .unwrap()
                .request_text_max_chars,
            250
        );
        for bad in [
            json!({"ledger":{"request_text_max_chars":0}}),
            json!({"ledger":{"request_text_max_chars":20_001}}),
            json!({"ledger":{"other":1}}),
            json!({"ledger":[]}),
        ] {
            assert!(LedgerPolicy::from_policy(&bad).is_err(), "{bad}");
        }
    }
}
