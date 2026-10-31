//! Intent identity: action metadata, semantic effect keys, resources and the candidate record a
//! tool call proposes before the ledger decides whether it is new, existing or a duplicate.
use anyhow::{anyhow, bail, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use webagent_configuration::digest;

/// Longest accepted action ID, in bytes.
pub const ID_MAX: usize = 128;

/// The model-supplied `_action` metadata of one tool call.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Metadata {
    /// Stable action ID chosen by the caller for status and retry.
    pub id: String,
    /// Prior action this call claims to be a separate, newly intended effect from.
    #[serde(default)]
    pub separate_from: Option<String>,
}

/// Read a required string field of an action record.
pub fn field<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .ok_or_else(|| anyhow!("Invalid action record field: {key}"))
}

/// Whether `id` is a non-empty, bounded ASCII identifier (`[A-Za-z0-9_-]`).
#[must_use]
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= ID_MAX
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Tool arguments without the `_action` metadata.
#[must_use]
pub fn strip(args: &Value) -> Value {
    let mut result = args.clone();
    if let Some(object) = result.as_object_mut() {
        object.remove("_action");
    }
    result
}

/// Parse and validate the optional `_action` metadata; `null` counts as absent.
pub fn metadata(args: &Value) -> Result<Option<Metadata>> {
    let meta: Option<Metadata> = args
        .get("_action")
        .filter(|v| !v.is_null())
        .map(|v| serde_json::from_value(v.clone()))
        .transpose()?;
    if meta.as_ref().is_some_and(|m| {
        !valid_id(&m.id) || m.separate_from.as_ref().is_some_and(|id| !valid_id(id))
    }) {
        bail!("Invalid action identity");
    }
    Ok(meta)
}

/// The caller's action ID, or the one derived from the pending tool call.
#[must_use]
pub fn action_id(meta: Option<&Metadata>, pending_id: &str) -> String {
    meta.map(|m| m.id.clone())
        .unwrap_or_else(|| format!("action-{pending_id}"))
}

// Semantic effect identity deliberately excludes precondition revisions. Different
// content remains a separate action; merely targeting the same tool is not a match.
fn identity(name: &str, args: &Value) -> Value {
    match name {
        "test_owner_purchase" => {
            json!({"family":"purchase","merchant":args["merchant"],"item":args["item"],"recipient":args["recipient"]})
        }
        "edit_own_prompt" => json!({"family":"prompt","slot_name":args["slot_name"],"action":"put",
            "content":args["content"],"merge_mode":args["mode"].as_str().unwrap_or("replace")}),
        "prompt_slot_write" => {
            json!({"family":"prompt","slot_name":args["slot_name"],"action":args["action"],
            "content":args["content"],"merge_mode":args["merge_mode"].as_str().unwrap_or("replace"),
            "order_index":args["order_index"],"lock":args["lock"]})
        }
        "save_own_skill" => {
            json!({"family":name,"name":args["name"],"description":args["description"],
            "instructions":args["instructions"],"mode":args["mode"].as_str().unwrap_or("selectable")})
        }
        "remove_own_skill" => json!({"family":name,"name":args["name"]}),
        _ => json!({"family":name,"effect":args}),
    }
}

/// Digest of the semantic effect of `name` with metadata-free arguments `clean`.
#[must_use]
pub fn intent_key(name: &str, clean: &Value) -> String {
    let mut effect = identity(name, clean);
    // Empty optional prompt metadata is equivalent across both prompt adapters.
    if effect["family"] == "prompt" && effect["order_index"].is_null() && effect["lock"].is_null() {
        if let Some(object) = effect.as_object_mut() {
            object.remove("order_index");
            object.remove("lock");
        }
    }
    digest(&effect)
}

/// The resource an action of `name` affects for `agent`.
#[must_use]
pub fn resource(agent: &str, name: &str) -> String {
    if name == "test_owner_purchase" {
        "provider:test".into()
    } else {
        format!("agent:{agent}")
    }
}

/// Inputs of a proposed action record.
pub struct Candidate<'a> {
    /// Action ID from metadata or the pending call.
    pub id: &'a str,
    /// Requesting user; the ledger owner.
    pub requester: &'a str,
    /// Tool name.
    pub tool: &'a str,
    /// Agent that runs the tool.
    pub agent: &'a str,
    /// Session of the run.
    pub session_id: &'a str,
    /// Run that proposed the call.
    pub run_id: &'a str,
    /// Human message that authorized the run, or `null` for automated work.
    pub origin: Value,
    /// Arguments without `_action` metadata.
    pub arguments: &'a Value,
    /// Pending tool-call identity the native receipts are keyed by.
    pub pending_id: &'a str,
    /// Creation time, Unix seconds.
    pub created_at: u64,
}

/// The candidate record, including intent and provider idempotency keys.
#[must_use]
pub fn candidate(c: Candidate<'_>) -> Value {
    let resource = resource(c.agent, c.tool);
    json!({"schema_version":1,"id":c.id,"requester":c.requester,"tool":c.tool,
        "resource":resource,"agent_id":c.agent,"session_id":c.session_id,"run_id":c.run_id,
        "origin":c.origin,"arguments":c.arguments,"arguments_digest":digest(c.arguments),
        "intent_key":intent_key(c.tool,c.arguments),"provider_key":format!("wa-{}",digest(&json!([c.requester,resource,c.id]))),
        "pending_id":c.pending_id,"created_at":c.created_at})
}

/// Whether a run is executable work rather than a fresh human message: a scheduled prompt,
/// module binding or agent message cannot authorize another matching consequential action.
#[must_use]
pub fn automated_run(run: &Value) -> bool {
    run["module_binding"].is_object()
        || run["received_agent_message"] == true
        || run["origin_agent_message"].is_object()
}

/// Whether message metadata marks an agent-to-agent message.
#[must_use]
pub fn agent_message(meta: &Value) -> bool {
    meta["agent_message"].is_object()
}

/// Origin of a run's human user message.
#[must_use]
pub fn origin(
    storage_owner: &str,
    session_id: &str,
    message_id: &str,
    seq: i64,
    created_at: i64,
) -> Value {
    json!({"storage_owner":storage_owner,"session_id":session_id,"message_id":message_id,"seq":seq,"created_at":created_at})
}
