//! task_list_write: add or update one section of the session's list and its steps.
use super::{rules, save};
use crate::{Call, NativeHost};
use anyhow::{bail, Result};
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WriteArgs {
    expected_revision: u64,
    #[serde(default)]
    section: rules::SectionWrite,
    #[serde(default)]
    steps: Vec<rules::StepWrite>,
    /// User requests that need no task, each with its reason.
    #[serde(default)]
    set_aside: Vec<rules::SetAside>,
    /// On the write that starts a section's first step: fold all earlier work.
    #[serde(default)]
    fold_earlier: bool,
    /// What the summary of the folded work must keep.
    #[serde(default)]
    fold_focus: String,
}
pub(super) async fn run(host: &dyn NativeHost, call: Call<'_>, args: &Value) -> Result<Value> {
    let request: WriteArgs = serde_json::from_value(args.clone())?;
    let focus = request.fold_focus.trim();
    if focus.chars().count() > 300 || focus.contains('\n') {
        bail!("fold_focus must be one line of 300 characters or fewer");
    }
    let write = rules::Write {
        section: request.section,
        steps: request.steps,
        set_aside: request.set_aside,
        ..rules::Write::default()
    };
    let fold = request.fold_earlier.then_some(focus);
    save(
        host,
        call,
        "tasks",
        Some(request.expected_revision),
        write,
        fold,
    )
    .await
}
