//! Human escalation: hand a conversation to the agent's staff or leave them a
//! message. Messages live in the agent plane so staff see every visitor's message.
use crate::{Call, NativeHost};
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
use webagent_access::Relationship;
use webagent_host_core::{digest, now};

const MESSAGE_KINDS: [&str; 5] = ["question", "booking", "callback", "feedback", "other"];
const MESSAGE_STATUSES: [&str; 4] = ["new", "contacted", "resolved", "closed"];

/// Whether this family executes tool `name`.
#[must_use]
pub fn owns(name: &str) -> bool {
    TOOLS.contains(&name)
}
/// Tools this adapter executes; their schemas live in the ability's `tools/<tool>/tool.json`.
pub const TOOLS: &[&str] = &[
    "request_human",
    "staff_message_send",
    "staff_message_list",
    "staff_message_update",
];

pub(crate) fn text(value: &str, limit: usize) -> Result<String> {
    let value = value.trim();
    if value.len() > limit
        || value
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        bail!("Text exceeds {limit} bytes or contains control characters");
    }
    Ok(value.to_owned())
}
fn contact(email: Option<&str>, phone: Option<&str>) -> Result<(String, String)> {
    let email = text(email.unwrap_or(""), 200)?;
    let phone = text(phone.unwrap_or(""), 40)?;
    if !email.is_empty()
        && (email.contains(char::is_whitespace)
            || email.split('@').count() != 2
            || !email.split('@').nth(1).unwrap_or("").contains('.'))
    {
        bail!("Invalid email address");
    }
    if !phone.is_empty()
        && (phone.chars().filter(char::is_ascii_digit).count() < 7
            || !phone
                .chars()
                .all(|c| c.is_ascii_digit() || " +-().".contains(c)))
    {
        bail!("Invalid phone number");
    }
    if email.is_empty() && phone.is_empty() {
        bail!("An email address or phone number is required");
    }
    Ok((email, phone))
}
/// Run human escalation tool `name` under fresh run authority at `call`'s position.
pub async fn execute(
    host: &dyn NativeHost,
    call: Call<'_>,
    name: &str,
    args: &Value,
    cancel: CancellationToken,
) -> Result<Value> {
    let (sid, rid, pending) = (call.sid, call.run_id, call.pending_id);
    let run = host.authorize(call, name, args, &cancel).await?;
    let agent = run["agent_id"]
        .as_str()
        .unwrap_or("shared_default")
        .to_owned();
    let actor = host.actor().to_owned();
    let relationship = host.relationship(sid, &agent).await?;
    // Staff tools are checked here as well as by the capability ladder, so a
    // visitor-level grant of this ability never exposes other visitors' messages.
    if matches!(name, "staff_message_list" | "staff_message_update")
        && !matches!(relationship, Relationship::Owner | Relationship::Member)
    {
        return Ok(json!({"status":"denied","output":"Staff access required"}));
    }
    let store = host.agent_store(&agent).await?;
    let output = match name {
        "staff_message_send" => {
            let (email, phone) = contact(args["email"].as_str(), args["phone"].as_str())?;
            let kind = args["kind"]
                .as_str()
                .filter(|k| MESSAGE_KINDS.contains(k))
                .ok_or_else(|| anyhow!("Invalid message kind"))?;
            let visitor_name = text(args["name"].as_str().unwrap_or(""), 120)?;
            let message = text(args["message"].as_str().unwrap_or(""), 2000)?;
            if visitor_name.is_empty() || message.is_empty() {
                bail!("Name and message are required");
            }
            // The call identity makes retries of the same tool call idempotent.
            let id = digest(&json!([agent, sid, rid, pending]))[..24].to_owned();
            let key = format!("agent:{agent}:staff_message:{id}");
            if let Some(existing) = store.get(&key).await? {
                json!({"message_id":id,"saved":true,"revision":existing["revision"]})
            } else {
                if store
                    .list(&format!("agent:{agent}:staff_message:"))
                    .await?
                    .len()
                    >= 5000
                {
                    bail!("Staff message storage is full; ask staff to close old messages");
                }
                store.put(&key, &json!({"id":id,"revision":1,"status":"new","kind":kind,"name":visitor_name,"email":email,
                    "phone":phone,"message":message,"session_id":sid,"visitor_id":actor,"created_at":now(),"notes":[]})).await?;
                json!({"message_id":id,"saved":true,"revision":1})
            }
        }
        "staff_message_list" => {
            let limit = args["limit"].as_u64().unwrap_or(20).clamp(1, 50) as usize;
            let mut messages = store.list(&format!("agent:{agent}:staff_message:")).await?;
            if let Some(status) = args["status"].as_str() {
                messages.retain(|m| m["status"] == status);
            }
            messages.sort_by_key(|m| std::cmp::Reverse(m["created_at"].as_u64().unwrap_or(0)));
            let total = messages.len();
            messages.truncate(limit);
            json!({"messages":messages,"total":total,"authority":"visitor_supplied_data"})
        }
        "staff_message_update" => {
            let id = args["message_id"]
                .as_str()
                .filter(|v| {
                    !v.is_empty() && v.len() <= 64 && v.bytes().all(|b| b.is_ascii_hexdigit())
                })
                .ok_or_else(|| anyhow!("Invalid message ID"))?;
            let status = args["status"]
                .as_str()
                .filter(|s| MESSAGE_STATUSES.contains(s))
                .ok_or_else(|| anyhow!("Invalid status"))?;
            let key = format!("agent:{agent}:staff_message:{id}");
            let mut message = store
                .get(&key)
                .await?
                .ok_or_else(|| anyhow!("Message not found"))?;
            if message["last_pending_id"] == pending {
                return Ok(json!({"status":"success","output":message}));
            }
            if message["revision"].as_u64() != args["expected_revision"].as_u64() {
                bail!("Message changed; list it again before updating");
            }
            message["status"] = json!(status);
            message["revision"] = json!(message["revision"].as_u64().unwrap_or(1) + 1);
            message["last_pending_id"] = json!(pending);
            let note = text(args["note"].as_str().unwrap_or(""), 1000)?;
            if !note.is_empty() {
                let notes = message["notes"]
                    .as_array_mut()
                    .ok_or_else(|| anyhow!("Invalid message notes"))?;
                if notes.len() >= 50 {
                    notes.remove(0);
                }
                notes.push(json!({"by":actor,"at":now(),"text":note}));
            }
            store.put(&key, &message).await?;
            message
        }
        "request_human" => {
            if !host.shared_chat_policy(&agent).await?.help {
                return Ok(
                    json!({"status":"denied","output":"Human handoff is not enabled for this agent; offer to leave staff a message instead"}),
                );
            }
            text(args["reason"].as_str().unwrap_or(""), 300)?;
            let owner = host.shared_chat_owner(sid, &actor).await?;
            let owner = host.user_store(&owner).await?;
            let request = host
                .shared_help_request(&owner, sid, &actor, pending)
                .await?;
            json!({"help_request":request["status"],"note":"Staff were notified in the app. Tell the visitor a team member will join this chat; keep helping meanwhile."})
        }
        _ => bail!("Unknown human escalation tool"),
    };
    Ok(json!({"status":"success","output":output}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn contact_requires_one_valid_channel() {
        assert!(contact(None, None).is_err());
        assert!(contact(Some("not-an-email"), None).is_err());
        assert!(contact(None, Some("12ab")).is_err());
        assert_eq!(
            contact(Some(" a@b.example "), None).unwrap().0,
            "a@b.example"
        );
        assert_eq!(
            contact(None, Some("(555) 010-2233")).unwrap().1,
            "(555) 010-2233"
        );
    }
    #[test]
    fn schemas_match_owned_tools() {
        let names: Vec<_> = crate::schemas(TOOLS)
            .iter()
            .map(|s| s["name"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(names.len(), 4);
        assert!(names.iter().all(|n| owns(n)));
    }
}
