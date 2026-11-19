//! staff_message_send: validate a visitor's contact details and save their message.
use super::text;
use anyhow::{anyhow, bail, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use webagent_host_core::{digest, now, StoreCore};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SendArgs {
    name: String,
    kind: String,
    message: String,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    phone: Option<String>,
}

const MESSAGE_KINDS: [&str; 5] = ["question", "booking", "callback", "feedback", "other"];

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
pub(super) async fn run(
    store: &StoreCore,
    args: &Value,
    agent: &str,
    actor: &str,
    sid: &str,
    rid: &str,
    pending: &str,
) -> Result<Value> {
    let sent: SendArgs = webagent_abilities::tool_arguments::read("staff_message_send", args)?;
    let (email, phone) = contact(sent.email.as_deref(), sent.phone.as_deref())?;
    let kind = Some(sent.kind.as_str())
        .filter(|k| MESSAGE_KINDS.contains(k))
        .ok_or_else(|| anyhow!("Invalid message kind"))?;
    let visitor_name = text(&sent.name, 120)?;
    let message = text(&sent.message, 2000)?;
    if visitor_name.is_empty() || message.is_empty() {
        bail!("Name and message are required");
    }
    // The call identity makes retries of the same tool call idempotent.
    let id = digest(&json!([agent, sid, rid, pending]))[..24].to_owned();
    let key = format!("agent:{agent}:staff_message:{id}");
    let output = if let Some(existing) = store.get(&key).await? {
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
}
