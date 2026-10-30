//! Front desk for an embedded business agent: business profile, visitor leads,
//! human handoff and reply choices. Records live in the agent plane so every
//! visitor session of the agent reads the same facts and staff see every lead.
use crate::{Call, NativeHost};
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
use webagent_access::Relationship;
use webagent_host_core::{digest, now};

const PROFILE_MAX_BYTES: usize = 32 * 1024;
const LEAD_KINDS: [&str; 6] = [
    "inquiry", "booking", "event", "catering", "callback", "other",
];
const LEAD_STATUSES: [&str; 4] = ["new", "contacted", "won", "closed"];

/// Whether this family executes tool `name`.
#[must_use]
pub fn owns(name: &str) -> bool {
    matches!(
        name,
        "business_profile_get"
            | "business_profile_update"
            | "lead_capture"
            | "lead_list"
            | "lead_update"
            | "request_human"
            | "offer_choices"
    )
}
/// Tools this adapter executes; their schemas live in the ability's `tools/<tool>/tool.json`.
pub const TOOLS: &[&str] = &[
    "business_profile_get",
    "business_profile_update",
    "lead_capture",
    "lead_list",
    "lead_update",
    "request_human",
    "offer_choices",
];

fn text(value: &str, limit: usize) -> Result<String> {
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
fn profile(value: &Value) -> Result<Value> {
    if !value.is_object() {
        bail!("Profile must be a JSON object");
    }
    if serde_json::to_vec(value)?.len() > PROFILE_MAX_BYTES {
        bail!("Profile exceeds 32 KiB");
    }
    Ok(value.clone())
}
fn choices(id: &str, prompt: &str, options: &[Value]) -> Result<Value> {
    if options.is_empty() || options.len() > 8 {
        bail!("Expected 1..8 options");
    }
    let mut clean = Vec::new();
    for (n, option) in options.iter().enumerate() {
        let label = text(
            option
                .as_str()
                .ok_or_else(|| anyhow!("Option must be text"))?,
            80,
        )?;
        if label.is_empty() || label.contains('\n') {
            bail!("Option labels must be one non-empty line");
        }
        clean.push(json!({"id":format!("option-{}",n+1),"label":label}));
    }
    // user_reply: the browser sends the clicked label as the user's own message
    // through normal admission; the card itself carries no authority.
    Ok(
        json!({"id":id,"type":"choice","title":"Choose","placement":"inline","data":{"prompt":text(prompt,200)?,"options":clean},
        "lifecycle":{},"authority":"user_reply"}),
    )
}

/// Run front desk tool `name` under fresh run authority at `call`'s position.
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
    // visitor-level grant of this ability never exposes leads or profile edits.
    if matches!(
        name,
        "business_profile_update" | "lead_list" | "lead_update"
    ) && !matches!(relationship, Relationship::Owner | Relationship::Member)
    {
        return Ok(json!({"status":"denied","output":"Staff access required"}));
    }
    let store = host.agent_store(&agent).await?;
    let profile_key = format!("agent:{agent}:business_profile");
    let output = match name {
        "business_profile_get" => store.get(&profile_key).await?.unwrap_or(
            json!({"revision":0,"profile":null,"note":"No business profile has been saved yet"}),
        ),
        "business_profile_update" => {
            let next = profile(&args["profile"])?;
            let expected = args["expected_revision"]
                .as_u64()
                .ok_or_else(|| anyhow!("Revision required"))?;
            // shortcut: read-compare-write is not atomic across the API and loop processes;
            // acceptable for rare operator edits, move to a conditional agent-plane write if edits become concurrent.
            let current = store.get(&profile_key).await?;
            let revision = current
                .as_ref()
                .and_then(|v| v["revision"].as_u64())
                .unwrap_or(0);
            if current.as_ref().is_some_and(|v| v["pending_id"] == pending) {
                return Ok(json!({"status":"success","output":current}));
            }
            if revision != expected {
                bail!("Profile changed; read it again before updating");
            }
            let saved = json!({"revision":revision+1,"profile":next,"updated_by":actor,"updated_at":now(),"pending_id":pending});
            store.put(&profile_key, &saved).await?;
            saved
        }
        "lead_capture" => {
            let (email, phone) = contact(args["email"].as_str(), args["phone"].as_str())?;
            let kind = args["kind"]
                .as_str()
                .filter(|k| LEAD_KINDS.contains(k))
                .ok_or_else(|| anyhow!("Invalid lead kind"))?;
            let lead_name = text(args["name"].as_str().unwrap_or(""), 120)?;
            let message = text(args["message"].as_str().unwrap_or(""), 2000)?;
            if lead_name.is_empty() || message.is_empty() {
                bail!("Name and message are required");
            }
            // The call identity makes retries of the same tool call idempotent.
            let id = digest(&json!([agent, sid, rid, pending]))[..24].to_owned();
            let lead_key = format!("agent:{agent}:lead:{id}");
            if let Some(existing) = store.get(&lead_key).await? {
                json!({"lead_id":id,"saved":true,"revision":existing["revision"]})
            } else {
                if store.list(&format!("agent:{agent}:lead:")).await?.len() >= 5000 {
                    bail!("Lead storage is full; ask staff to close old leads");
                }
                store.put(&lead_key, &json!({"id":id,"revision":1,"status":"new","kind":kind,"name":lead_name,"email":email,
                    "phone":phone,"message":message,"session_id":sid,"visitor_id":actor,"created_at":now(),"notes":[]})).await?;
                json!({"lead_id":id,"saved":true,"revision":1})
            }
        }
        "lead_list" => {
            let limit = args["limit"].as_u64().unwrap_or(20).clamp(1, 50) as usize;
            let mut leads = store.list(&format!("agent:{agent}:lead:")).await?;
            if let Some(status) = args["status"].as_str() {
                leads.retain(|l| l["status"] == status);
            }
            leads.sort_by_key(|l| std::cmp::Reverse(l["created_at"].as_u64().unwrap_or(0)));
            let total = leads.len();
            leads.truncate(limit);
            json!({"leads":leads,"total":total,"authority":"visitor_supplied_data"})
        }
        "lead_update" => {
            let id = args["lead_id"]
                .as_str()
                .filter(|v| {
                    !v.is_empty() && v.len() <= 64 && v.bytes().all(|b| b.is_ascii_hexdigit())
                })
                .ok_or_else(|| anyhow!("Invalid lead ID"))?;
            let status = args["status"]
                .as_str()
                .filter(|s| LEAD_STATUSES.contains(s))
                .ok_or_else(|| anyhow!("Invalid status"))?;
            let lead_key = format!("agent:{agent}:lead:{id}");
            let mut lead = store
                .get(&lead_key)
                .await?
                .ok_or_else(|| anyhow!("Lead not found"))?;
            if lead["last_pending_id"] == pending {
                return Ok(json!({"status":"success","output":lead}));
            }
            if lead["revision"].as_u64() != args["expected_revision"].as_u64() {
                bail!("Lead changed; list it again before updating");
            }
            lead["status"] = json!(status);
            lead["revision"] = json!(lead["revision"].as_u64().unwrap_or(1) + 1);
            lead["last_pending_id"] = json!(pending);
            let note = text(args["note"].as_str().unwrap_or(""), 1000)?;
            if !note.is_empty() {
                let notes = lead["notes"]
                    .as_array_mut()
                    .ok_or_else(|| anyhow!("Invalid lead notes"))?;
                if notes.len() >= 50 {
                    notes.remove(0);
                }
                notes.push(json!({"by":actor,"at":now(),"text":note}));
            }
            store.put(&lead_key, &lead).await?;
            lead
        }
        "request_human" => {
            if !host.shared_chat_policy(&agent).await?.help {
                return Ok(
                    json!({"status":"denied","output":"Human handoff is not enabled for this agent; offer to take the visitor's contact details instead"}),
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
        "offer_choices" => {
            let id = format!("choices-{}", &digest(&json!(pending))[..24]);
            let spec = choices(
                &id,
                args["prompt"].as_str().unwrap_or(""),
                args["options"].as_array().map(Vec::as_slice).unwrap_or(&[]),
            )?;
            // A session lists at most 64 components; keep headroom for other displays.
            if host.chat_components(host.store(), sid).await?.len() >= 48 {
                bail!("This conversation has too many choice cards; ask in text instead");
            }
            let generation = run["generation"]
                .as_str()
                .ok_or_else(|| anyhow!("Run generation missing"))?;
            host.write_artifact(Call { generation, ..call }, "component", &id, 0, &spec)
                .await?
        }
        _ => bail!("Unknown front desk tool"),
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
    fn choices_are_bounded_reply_cards() {
        let spec = choices(
            "choices-1",
            "Pick a time",
            &[json!("10:00"), json!("11:30")],
        )
        .unwrap();
        assert_eq!(spec["id"], "choices-1");
        assert_eq!(spec["authority"], "user_reply");
        assert_eq!(spec["data"]["options"][1]["label"], "11:30");
        assert!(choices("c", "x", &[]).is_err());
        assert!(choices("c", "x", &[json!("a\nb")]).is_err());
        assert!(choices("c", "x", &vec![json!("a"); 9]).is_err());
    }
    #[test]
    fn profile_must_be_a_bounded_object() {
        assert!(profile(&json!([1])).is_err());
        assert!(profile(&json!({"notes":"x".repeat(PROFILE_MAX_BYTES)})).is_err());
        assert!(profile(&json!({"name":"Hoshizora Ramen"})).is_ok());
    }
    #[test]
    fn schemas_match_owned_tools() {
        let names: Vec<_> = crate::schemas(TOOLS)
            .iter()
            .map(|s| s["name"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(names.len(), 7);
        assert!(names.iter().all(|n| owns(n)));
    }
}
