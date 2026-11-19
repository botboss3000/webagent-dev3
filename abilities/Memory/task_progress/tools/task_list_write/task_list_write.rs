//! task_list_write: add or update one section of the session's list and its steps.
use super::{rules, save};
use crate::{Call, NativeHost};
use anyhow::Result;
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WriteArgs {
    expected_revision: u64,
    section: rules::SectionWrite,
    #[serde(default)]
    steps: Vec<rules::StepWrite>,
}
pub(super) async fn run(host: &dyn NativeHost, call: Call<'_>, args: &Value) -> Result<Value> {
    let request: WriteArgs = serde_json::from_value(args.clone())?;
    let write = rules::Write {
        section: request.section,
        steps: request.steps,
    };
    save(host, call, "tasks", Some(request.expected_revision), write).await
}
