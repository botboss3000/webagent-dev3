//! task_list_read: the session's list as one line per section, one section's steps, an
//! earlier revision, or the changes between two revisions.
use super::{list_at, rules};
use crate::{Call, NativeHost};
use anyhow::{anyhow, Result};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReadArgs {
    #[serde(default)]
    section: Option<String>,
    #[serde(default)]
    revision: Option<u64>,
    #[serde(default)]
    compare_to: Option<u64>,
}
pub(super) async fn run(host: &dyn NativeHost, call: Call<'_>, args: &Value) -> Result<Value> {
    let query: ReadArgs = serde_json::from_value(args.clone())?;
    let list = list_at(host, call, query.revision).await?;
    if let Some(other) = query.compare_to {
        let before = list_at(host, call, Some(other)).await?;
        return Ok(json!({"revision":list.revision,"compared_to":other,
            "changes":rules::compare(&before, &list)}));
    }
    if let Some(id) = query.section {
        let section = list
            .sections
            .iter()
            .find(|section| section.id == id)
            .ok_or_else(|| anyhow!("No section {id}"))?;
        return Ok(json!({"revision":list.revision,"section":section}));
    }
    Ok(json!({"revision":list.revision,"sections":rules::overview(&list)}))
}
