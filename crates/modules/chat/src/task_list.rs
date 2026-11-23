//! Session task list rules: one list per session, split into sections of steps. A write
//! adds or updates one section by id (as `main_notes_write` does for an entry); the list's
//! content is the agent's or user's claim about the work, never verified evidence.
use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The task list's texts (`prompts/task_list.json`, read live).
const TEXTS_FILE: &str = "config/configuration/prompts/task_list.json";
/// The same file as built, used only when the live one is missing or invalid.
const PACKAGED_TEXTS: &str =
    include_str!("../../../../config/configuration/prompts/task_list.json");

/// Sections one session's list keeps.
pub const MAX_SECTIONS: usize = 64;
/// Steps one section keeps, cancelled ones included.
pub const MAX_STEPS: usize = 32;
/// Evidence result ids one step keeps.
const MAX_EVIDENCE: usize = 16;

/// Status of a section or step; the same words Main Notes uses for its entries.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    #[default]
    Open,
    InProgress,
    Blocked,
    Done,
    Cancelled,
}

impl Status {
    fn word(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::InProgress => "in_progress",
            Self::Blocked => "blocked",
            Self::Done => "done",
            Self::Cancelled => "cancelled",
        }
    }
    /// Whether work on it is still expected (open or in progress).
    #[must_use]
    pub fn unfinished(self) -> bool {
        matches!(self, Self::Open | Self::InProgress)
    }
}

/// Who asked for a section's work.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    User,
    #[default]
    Agent,
}

/// One step of a section. `started_at` and `finished_at` (milliseconds) bound the tool
/// calls made while it was in progress, which are its evidence unless `evidence` narrows it.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct Step {
    pub id: String,
    pub label: String,
    pub status: Status,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub outcome: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<u64>,
    /// The user requests (`u7`) this step answers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requests: Vec<String>,
}

/// One piece of work: a user request or the agent's own investigation.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct Section {
    pub id: String,
    pub title: String,
    pub origin: Origin,
    pub status: Status,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub notes: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub summary: String,
    pub steps: Vec<Step>,
    /// The number the next new step gets.
    pub next_step: u32,
    /// Whether an approved plan created it; its progress then also updates the plan.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub plan: bool,
    /// The user requests (`u7`) this section answers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requests: Vec<String>,
}

/// A session's task list at one revision.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct List {
    pub revision: u64,
    pub sections: Vec<Section>,
    /// The number the next new section gets.
    pub next_section: u32,
    /// User requests the agent decided need no task, each with its reason.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub set_aside: Vec<SetAside>,
}

/// A user request set aside: it needs no task, for the stated reason.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SetAside {
    pub request: String,
    pub reason: String,
}

/// The section part of a write: `id` names an existing section, otherwise a new one.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SectionWrite {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub origin: Option<Origin>,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub status: Option<Status>,
    #[serde(default)]
    pub summary: Option<String>,
    /// User requests (`u7`) this section answers, added to those it cites.
    #[serde(default)]
    pub requests: Option<Vec<String>>,
}

/// One step of a write: `id` names an existing step of the section, otherwise a new one.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepWrite {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub status: Option<Status>,
    #[serde(default)]
    pub outcome: Option<String>,
    #[serde(default)]
    pub evidence: Option<Vec<String>>,
    /// User requests (`u7`) this step answers, added to those it cites.
    #[serde(default)]
    pub requests: Option<Vec<String>>,
}

/// One write: a section and the steps to add or change in it (steps left out are
/// untouched), and user requests to set aside or bring back. A write may only set aside
/// or restore requests and leave out the section.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Write {
    #[serde(default)]
    pub section: SectionWrite,
    #[serde(default)]
    pub steps: Vec<StepWrite>,
    #[serde(default)]
    pub set_aside: Vec<SetAside>,
    /// Set-aside requests to make unfiled again (the user's undo in the Tasks panel).
    #[serde(default)]
    pub restore: Vec<String>,
    /// Set only by the server when an approved plan seeds the section.
    #[serde(skip)]
    pub plan: bool,
}

fn text(value: &str, min: usize, max: usize, what: &str) -> Result<String> {
    let value = value.trim();
    if value.len() < min
        || value.len() > max
        || value
            .chars()
            .any(|ch| ch.is_control() && ch != '\n' && ch != '\t')
    {
        bail!("{what} needs {min}..{max} bytes without control characters");
    }
    Ok(value.to_owned())
}

/// A user request label: `u` and its number (`u7`).
fn request_label(label: &str) -> Result<String> {
    let digits = label.strip_prefix('u').unwrap_or("");
    if digits.is_empty()
        || digits.len() > 6
        || digits.starts_with('0')
        || !digits.bytes().all(|b| b.is_ascii_digit())
    {
        bail!("{label:?} is not a request label; use the u-labels the list names (u7)");
    }
    Ok(label.to_owned())
}

/// Add `labels` to `cited`, each once; the labels newly added.
fn cite(cited: &mut Vec<String>, labels: Vec<String>) -> Result<Vec<String>> {
    if labels.len() > MAX_EVIDENCE {
        bail!("One write cites at most {MAX_EVIDENCE} requests per section or step");
    }
    let mut added = Vec::new();
    for label in labels {
        let label = request_label(&label)?;
        if !cited.contains(&label) {
            cited.push(label.clone());
            added.push(label);
        }
    }
    Ok(added)
}

fn evidence(ids: Vec<String>) -> Result<Vec<String>> {
    if ids.len() > MAX_EVIDENCE {
        bail!("A step keeps at most {MAX_EVIDENCE} evidence ids");
    }
    ids.iter()
        .map(|id| text(id, 1, 128, "An evidence id"))
        .collect()
}

/// Apply `write` to `list` at time `now` (milliseconds): the next revision and one line per
/// change, or why the write is refused. Nothing changes unless every part is valid.
pub fn apply(list: &List, write: Write, now: u64) -> Result<(List, Vec<String>)> {
    let mut next = list.clone();
    next.revision = list
        .revision
        .checked_add(1)
        .ok_or_else(|| anyhow!("Task list revision exhausted"))?;
    let mut changes = Vec::new();
    for label in &write.restore {
        let label = request_label(label)?;
        let before = next.set_aside.len();
        next.set_aside.retain(|aside| aside.request != label);
        if next.set_aside.len() == before {
            bail!("{label} is not set aside");
        }
        changes.push(format!("{label} restored"));
    }
    for aside in write.set_aside {
        let request = request_label(&aside.request)?;
        let reason = text(&aside.reason, 3, 200, "A set-aside reason")?;
        next.set_aside.retain(|old| old.request != request);
        changes.push(format!("{request} set aside: {reason}"));
        next.set_aside.push(SetAside { request, reason });
    }
    let section_part = write.section.id.is_some()
        || write.section.title.is_some()
        || write.section.origin.is_some()
        || write.section.notes.is_some()
        || write.section.status.is_some()
        || write.section.summary.is_some()
        || write.section.requests.is_some()
        || !write.steps.is_empty();
    if !section_part {
        if changes.is_empty() {
            bail!("The write changes nothing");
        }
        return Ok((next, changes));
    }
    // An id the list does not know, given with a title, is a new section: the server picks
    // its id, so a guessed id costs no retry.
    let found = write
        .section
        .id
        .as_deref()
        .and_then(|id| next.sections.iter().position(|section| section.id == id));
    let existing = found.is_some();
    let index = match (found, write.section.id.as_deref()) {
        (Some(index), _) => index,
        (None, Some(id)) if write.section.title.is_none() => {
            bail!(
                "No section {id}; the session task list in your instructions names the current ids"
            )
        }
        (None, _) => {
            if next.sections.len() >= MAX_SECTIONS {
                bail!("This session's list already keeps {MAX_SECTIONS} sections");
            }
            let title = write
                .section
                .title
                .as_deref()
                .ok_or_else(|| anyhow!("A new section needs a title"))?;
            next.next_section += 1;
            let section = Section {
                id: format!("s{}", next.next_section),
                title: text(title, 3, 120, "A section title")?,
                origin: write.section.origin.unwrap_or_default(),
                status: Status::Open,
                notes: String::new(),
                summary: String::new(),
                steps: Vec::new(),
                next_step: 0,
                plan: write.plan,
                requests: Vec::new(),
            };
            changes.push(format!("{} added: {}", section.id, section.title));
            next.sections.push(section);
            next.sections.len() - 1
        }
    };
    let section = &mut next.sections[index];
    if existing {
        if let Some(title) = write.section.title.as_deref() {
            section.title = text(title, 3, 120, "A section title")?;
            changes.push(format!("{} renamed: {}", section.id, section.title));
        }
        if write
            .section
            .origin
            .is_some_and(|origin| origin != section.origin)
        {
            bail!("A section's origin cannot change");
        }
    }
    if let Some(notes) = write.section.notes.as_deref() {
        section.notes = text(notes, 0, 1200, "Section notes")?;
        changes.push(format!("{} notes updated", section.id));
    }
    let mut cited = Vec::new();
    if let Some(labels) = write.section.requests {
        let added = cite(&mut section.requests, labels)?;
        if !added.is_empty() {
            changes.push(format!("{} cites {}", section.id, added.join(", ")));
        }
        cited.extend(added);
    }
    for step in write.steps {
        let labels = step.requests.clone();
        let (index, line) = write_step(section, step, now)?;
        changes.push(line);
        if let Some(labels) = labels {
            let step = &mut section.steps[index];
            let added = cite(&mut step.requests, labels)?;
            if !added.is_empty() {
                changes.push(format!("{} cites {}", step.id, added.join(", ")));
            }
            cited.extend(added);
        }
    }
    if let Some(summary) = write.section.summary.as_deref() {
        section.summary = text(summary, 20, 1200, "A section summary")?;
    }
    // A section is open, done or cancelled; `in_progress` on a section means open.
    if let Some(status) = write
        .section
        .status
        .map(|status| {
            if status == Status::InProgress {
                Status::Open
            } else {
                status
            }
        })
        .filter(|status| *status != section.status)
    {
        set_section_status(section, status)?;
        changes.push(format!("{} {}", section.id, status.word()));
    } else if section.status == Status::Done && write.section.summary.is_some() {
        changes.push(format!("{} summary updated", section.id));
    }
    // A cited request is no longer set aside.
    next.set_aside
        .retain(|aside| !cited.contains(&aside.request));
    if changes.is_empty() {
        bail!("The write changes nothing");
    }
    Ok((next, changes))
}

fn set_section_status(section: &mut Section, status: Status) -> Result<()> {
    match status {
        Status::Done => {
            if let Some(step) = section.steps.iter().find(|step| step.status.unfinished()) {
                bail!(
                    "{} is still {}: finish, block or cancel it before closing {}",
                    step.id,
                    step.status.word(),
                    section.id
                );
            }
            if section.summary.is_empty() {
                bail!("Closing {} needs a summary of what was done", section.id);
            }
        }
        Status::InProgress | Status::Blocked => {
            bail!("A section is open, done or cancelled; set its steps' status instead");
        }
        Status::Open | Status::Cancelled => {}
    }
    section.status = status;
    Ok(())
}

/// Apply one step write; the step's index and the change line.
fn write_step(section: &mut Section, write: StepWrite, now: u64) -> Result<(usize, String)> {
    if section.status != Status::Open {
        bail!(
            "{} is {}; reopen it before changing its steps",
            section.id,
            section.status.word()
        );
    }
    let index = match write.id.as_deref() {
        Some(id) => section
            .steps
            .iter()
            .position(|step| step.id == id)
            .ok_or_else(|| anyhow!("No step {id} in {}", section.id))?,
        None => {
            if section.steps.len() >= MAX_STEPS {
                bail!("{} already keeps {MAX_STEPS} steps", section.id);
            }
            let label = write
                .label
                .as_deref()
                .ok_or_else(|| anyhow!("A new step needs a label"))?;
            section.next_step += 1;
            section.steps.push(Step {
                id: format!("{}.{}", section.id, section.next_step),
                label: text(label, 3, 240, "A step label")?,
                status: Status::Open,
                outcome: String::new(),
                evidence: Vec::new(),
                started_at: None,
                finished_at: None,
                requests: Vec::new(),
            });
            section.steps.len() - 1
        }
    };
    let new = write.id.is_none();
    let step = &mut section.steps[index];
    if !new {
        if let Some(label) = write.label.as_deref() {
            step.label = text(label, 3, 240, "A step label")?;
        }
    }
    if let Some(outcome) = write.outcome.as_deref() {
        step.outcome = text(outcome, 0, 240, "A step outcome")?;
    }
    if let Some(ids) = write.evidence {
        step.evidence = evidence(ids)?;
    }
    if let Some(status) = write.status {
        if status == Status::InProgress && step.status != Status::InProgress {
            step.started_at = Some(now);
            step.finished_at = None;
        } else if !status.unfinished() && step.status.unfinished() {
            step.started_at.get_or_insert(now);
            step.finished_at = Some(now);
        } else if status == Status::Open {
            step.finished_at = None;
        }
        step.status = status;
    }
    let line = if new {
        format!("{} added: {}", step.id, step.label)
    } else {
        format!("{} {}", step.id, step.status.word())
    };
    Ok((index, line))
}

/// The write that seeds a user-requested section from an approved plan's checklist.
#[must_use]
pub fn plan_section(title: &str, notes: &str, labels: &[String]) -> Write {
    Write {
        section: SectionWrite {
            title: Some(title.to_owned()),
            origin: Some(Origin::User),
            notes: (!notes.trim().is_empty()).then(|| notes.to_owned()),
            ..SectionWrite::default()
        },
        steps: labels
            .iter()
            .map(|label| StepWrite {
                label: Some(label.clone()),
                ..StepWrite::default()
            })
            .collect(),
        plan: true,
        ..Write::default()
    }
}

/// Whether a section's execution has begun: one of its steps has started.
fn started(section: &Section) -> bool {
    section.steps.iter().any(|step| step.started_at.is_some())
}

/// The sections a write from `before` to `after` created, started (its first step began)
/// and closed (done or cancelled), as `{"created":[..],"started":[..],"closed":[..]}` with
/// empty lists left out. Compaction finds task boundaries in the history by these marks.
#[must_use]
pub fn marks(before: &List, after: &List) -> Value {
    let (mut created, mut begun, mut closed) = (Vec::new(), Vec::new(), Vec::new());
    for section in &after.sections {
        let old = before.sections.iter().find(|old| old.id == section.id);
        if old.is_none() {
            created.push(section.id.clone());
        }
        if started(section) && !old.is_some_and(started) {
            begun.push(section.id.clone());
        }
        if matches!(section.status, Status::Done | Status::Cancelled)
            && old.is_none_or(|old| old.status == Status::Open)
        {
            closed.push(section.id.clone());
        }
    }
    let mut marks = serde_json::Map::new();
    for (key, ids) in [("created", created), ("started", begun), ("closed", closed)] {
        if !ids.is_empty() {
            marks.insert(key.to_owned(), serde_json::json!(ids));
        }
    }
    Value::Object(marks)
}

/// The section `id` of `list`, if it has one.
#[must_use]
pub fn section<'a>(list: &'a List, id: &str) -> Option<&'a Section> {
    list.sections.iter().find(|section| section.id == id)
}

/// The plan artifact's checklist text for `section`: one numbered line per step, its box
/// showing the status (`x` done, `>` in progress, `!` blocked, `-` cancelled).
#[must_use]
pub fn plan_checklist(section: &Section) -> String {
    section
        .steps
        .iter()
        .enumerate()
        .map(|(index, step)| {
            let mark = match step.status {
                Status::Open => ' ',
                Status::InProgress => '>',
                Status::Blocked => '!',
                Status::Done => 'x',
                Status::Cancelled => '-',
            };
            format!("{}. [{mark}] {}", index + 1, step.label)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The plan artifact `plan` (its title and sections) with its checklist rewritten from its
/// task list `section`, or why it has no checklist to rewrite.
pub fn plan_with_checklist(plan: &Value, section: &Section) -> Result<Value> {
    let mut sections = plan["sections"]
        .as_array()
        .cloned()
        .ok_or_else(|| anyhow!("The plan has no sections"))?;
    let item = sections
        .iter_mut()
        .find(|item| item["label"] == "Checklist")
        .ok_or_else(|| anyhow!("The plan has no checklist"))?;
    item["value"] = Value::String(plan_checklist(section));
    Ok(serde_json::json!({"title":plan["title"],"sections":sections}))
}

/// The task list texts, the live file's when it is valid.
#[must_use]
pub fn texts() -> Value {
    webagent_configuration::live::parsed(TEXTS_FILE, 64 * 1024, |bytes| {
        Ok(serde_json::from_slice::<Value>(bytes)?)
    })
    .or_else(|_| serde_json::from_str(PACKAGED_TEXTS).map_err(anyhow::Error::from))
    .unwrap_or_default()
}

/// User requests one session's labels record keeps; the oldest drop first.
pub const MAX_REQUESTS: usize = 500;
/// Characters of a request's text kept to name it.
const REQUEST_PREVIEW: usize = 80;

/// One counted user request: a message the task list must cite or set aside before a
/// change is made (the task gate).
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct Request {
    /// Its label, `u` and a number that is never reused in the session.
    pub id: String,
    /// The chat message (or run) it came from.
    pub message: String,
    /// Its first words, to name it.
    pub text: String,
    pub at: u64,
}

/// A session's counted user requests, oldest first.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct Requests {
    pub next: u32,
    pub items: Vec<Request>,
}

impl Requests {
    /// Count message `message` with text `text` as the next request; `false` when it is
    /// already counted.
    pub fn add(&mut self, message: &str, text: &str, at: u64) -> bool {
        if self.items.iter().any(|item| item.message == message) {
            return false;
        }
        self.next += 1;
        let mut preview: String = text
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(REQUEST_PREVIEW)
            .collect();
        if text.chars().count() > REQUEST_PREVIEW {
            preview.push('…');
        }
        self.items.push(Request {
            id: format!("u{}", self.next),
            message: message.to_owned(),
            text: preview,
            at,
        });
        let skip = self.items.len().saturating_sub(MAX_REQUESTS);
        self.items.drain(..skip);
        true
    }
}

/// Every request label the list cites, in sections or steps.
fn cited(list: &List) -> std::collections::HashSet<&str> {
    list.sections
        .iter()
        .flat_map(|section| {
            section
                .requests
                .iter()
                .chain(section.steps.iter().flat_map(|step| step.requests.iter()))
        })
        .map(String::as_str)
        .collect()
}

/// The requests neither cited by a section or step nor set aside, oldest first.
#[must_use]
pub fn unfiled<'a>(list: &List, requests: &'a Requests) -> Vec<&'a Request> {
    let cited = cited(list);
    requests
        .items
        .iter()
        .filter(|item| {
            !cited.contains(item.id.as_str())
                && !list.set_aside.iter().any(|aside| aside.request == item.id)
        })
        .collect()
}

/// Labels a list cites or sets aside that `requests` does not know, so a write naming a
/// request that was never counted is refused. Only labels new since `before` are checked.
#[must_use]
pub fn unknown_requests(before: &List, after: &List, requests: &Requests) -> Vec<String> {
    let known = |label: &str| requests.items.iter().any(|item| item.id == label);
    let old = cited(before);
    let mut unknown: Vec<String> = cited(after)
        .into_iter()
        .chain(after.set_aside.iter().map(|aside| aside.request.as_str()))
        .filter(|label| !old.contains(label) && !known(label))
        .filter(|label| !before.set_aside.iter().any(|aside| aside.request == *label))
        .map(str::to_owned)
        .collect();
    unknown.sort();
    unknown.dedup();
    unknown
}

/// Requests one filing write cites; older unfiled ones are set aside instead.
const MAX_FILED: usize = MAX_EVIDENCE;

/// What the task gate does with a call that needs it.
#[derive(Debug)]
pub enum Gate {
    /// Every request is cited or set aside.
    Pass,
    /// This server write files the unfiled requests, and the call goes ahead.
    File(Write),
}

/// The task gate for one call that needs it (see [`Gate`]). It never refuses: unfiled
/// requests are cited on the newest open section (the agent may move them in a later
/// write), or on a new section titled from the first one when none is open. With
/// `every_tool` (the gate covers every tool call, not only changes) a newest request that
/// was set aside is cited too, so a request that gets tool calls is always on the list.
/// Unfiled requests beyond the newest [`MAX_FILED`] are earlier messages the turn-end
/// settling missed; they are set aside rather than cited.
#[must_use]
pub fn gate(list: &List, requests: &Requests, every_tool: bool) -> Gate {
    let mut pending = unfiled(list, requests);
    if every_tool {
        if let Some(newest) = requests.items.last().filter(|newest| {
            !cited(list).contains(newest.id.as_str())
                && list
                    .set_aside
                    .iter()
                    .any(|aside| aside.request == newest.id)
        }) {
            pending.push(newest);
        }
    }
    if pending.is_empty() {
        return Gate::Pass;
    }
    let skip = pending.len().saturating_sub(MAX_FILED);
    let set_aside = pending[..skip]
        .iter()
        .map(|item| SetAside {
            request: item.id.clone(),
            reason: "earlier message, answered before this work".to_owned(),
        })
        .collect();
    let filed = &pending[skip..];
    let labels = Some(filed.iter().map(|item| item.id.clone()).collect());
    if let Some(open) = list
        .sections
        .iter()
        .rev()
        .find(|section| section.status == Status::Open)
    {
        return Gate::File(Write {
            section: SectionWrite {
                id: Some(open.id.clone()),
                requests: labels,
                ..SectionWrite::default()
            },
            set_aside,
            ..Write::default()
        });
    }
    let title = filed
        .first()
        .map(|item| item.text.trim_end_matches('…').trim())
        .filter(|title| title.len() >= 3)
        .map_or_else(
            || "Requested work".to_owned(),
            |title| {
                // Section titles are at most 120 bytes; cut at a character boundary.
                let mut cut = String::new();
                for ch in title.chars() {
                    if cut.len() + ch.len_utf8() > 120 {
                        break;
                    }
                    cut.push(ch);
                }
                cut
            },
        );
    Gate::File(Write {
        section: SectionWrite {
            title: Some(title),
            origin: Some(Origin::User),
            requests: labels,
            ..SectionWrite::default()
        },
        set_aside,
        ..Write::default()
    })
}

/// The write that settles session requests when a turn ends: every request still unfiled
/// was answered in the reply without tool work, so it is set aside; `None` when none is.
#[must_use]
pub fn settle(list: &List, requests: &Requests) -> Option<Write> {
    let unfiled = unfiled(list, requests);
    (!unfiled.is_empty()).then(|| Write {
        set_aside: unfiled
            .iter()
            .map(|item| SetAside {
                request: item.id.clone(),
                reason: "answered in the reply".to_owned(),
            })
            .collect(),
        ..Write::default()
    })
}

fn text_field(texts: &Value, key: &str) -> String {
    texts[key].as_str().unwrap_or("").trim().to_owned()
}

/// The list as the instructions show it: the heading and rule, then each section with its
/// notes, closing summary and steps (closed sections show only their summary).
#[must_use]
pub fn render_snapshot(list: &List, texts: &Value) -> String {
    let mut lines = vec![
        text_field(texts, "snapshot_heading").replace("{revision}", &list.revision.to_string()),
        text_field(texts, "snapshot_rule"),
    ];
    if list.sections.is_empty() {
        lines.push(text_field(texts, "snapshot_empty"));
    }
    for (section, line) in list.sections.iter().zip(overview(list)) {
        lines.push(line);
        if section.status != Status::Open {
            continue;
        }
        if !section.notes.is_empty() {
            lines.push(format!("  notes: {}", section.notes));
        }
        for step in &section.steps {
            let mut line = format!("  {} [{}] {}", step.id, step.status.word(), step.label);
            if !step.outcome.is_empty() {
                line.push_str(&format!(" → {}", step.outcome));
            }
            lines.push(line);
        }
    }
    lines.retain(|line| !line.is_empty());
    lines.join("\n")
}

/// The one-line task state for each step's guidance message: each open section with its
/// active steps, next open step and open count, or `None` with no open work. It carries no revision, so it
/// changes only when a section or step starts or finishes.
#[must_use]
pub fn step_line(list: &List, texts: &Value) -> Option<String> {
    let parts = list
        .sections
        .iter()
        .filter(|section| section.status == Status::Open)
        .filter_map(|section| {
            let open = section
                .steps
                .iter()
                .filter(|step| step.status.unfinished())
                .count();
            if open == 0 {
                return None;
            }
            let active = section
                .steps
                .iter()
                .filter(|step| step.status == Status::InProgress)
                .map(|step| format!("{} \"{}\"", step.id, step.label))
                .collect::<Vec<_>>();
            // The next step to start, so the agent always sees what comes after this one.
            let next = section
                .steps
                .iter()
                .find(|step| step.status == Status::Open)
                .map(|step| format!(", next {} \"{}\"", step.id, step.label))
                .unwrap_or_default();
            Some(if active.is_empty() {
                format!(
                    "{} \"{}\", {open} open, none in progress{next}",
                    section.id, section.title
                )
            } else {
                format!(
                    "{} \"{}\", {open} open, active {}{next}",
                    section.id,
                    section.title,
                    active.join(", ")
                )
            })
        })
        .collect::<Vec<_>>();
    let template = text_field(texts, "step_line");
    // A section waiting to start after finished work: its first write may fold that work.
    let closed = list
        .sections
        .iter()
        .any(|section| matches!(section.status, Status::Done | Status::Cancelled));
    let waiting = list.sections.iter().find(|section| {
        section.status == Status::Open && !section.steps.is_empty() && !started(section)
    });
    let hint = text_field(texts, "fold_hint");
    (!parts.is_empty() && !template.is_empty()).then(|| {
        let mut line = template.replace("{sections}", &parts.join("; "));
        if let Some(section) = waiting.filter(|_| closed && !hint.is_empty()) {
            line.push(' ');
            line.push_str(&hint.replace("{section}", &section.id));
        }
        line
    })
}

/// One line per section: id, title, origin, status and step counts.
#[must_use]
pub fn overview(list: &List) -> Vec<String> {
    list.sections
        .iter()
        .map(|section| {
            let done = section
                .steps
                .iter()
                .filter(|step| step.status == Status::Done)
                .count();
            let open = section
                .steps
                .iter()
                .filter(|step| step.status.unfinished())
                .count();
            let mut line = format!(
                "{} {} [{} · {}] {done} done, {open} open",
                section.id,
                section.title,
                match section.origin {
                    Origin::User => "user",
                    Origin::Agent => "agent",
                },
                section.status.word()
            );
            if !section.summary.is_empty() {
                line.push_str(&format!(" · summary: {}", section.summary));
            }
            line
        })
        .collect()
}

/// One line per difference from `before` to `after`: sections and steps added, removed
/// or changed (status, label, title, notes, summary or outcome).
#[must_use]
pub fn compare(before: &List, after: &List) -> Vec<String> {
    let mut lines = Vec::new();
    for section in &after.sections {
        let Some(old) = before.sections.iter().find(|old| old.id == section.id) else {
            lines.push(format!("{} added: {}", section.id, section.title));
            for step in &section.steps {
                lines.push(format!(
                    "{} added: {} ({})",
                    step.id,
                    step.label,
                    step.status.word()
                ));
            }
            continue;
        };
        if old.title != section.title {
            lines.push(format!(
                "{} renamed: {} → {}",
                section.id, old.title, section.title
            ));
        }
        if old.status != section.status {
            lines.push(format!(
                "{} {} → {}",
                section.id,
                old.status.word(),
                section.status.word()
            ));
        }
        for (what, was, now) in [
            ("notes", &old.notes, &section.notes),
            ("summary", &old.summary, &section.summary),
        ] {
            if was != now {
                lines.push(format!("{} {what}: {now}", section.id));
            }
        }
        for step in &section.steps {
            let Some(was) = old.steps.iter().find(|was| was.id == step.id) else {
                lines.push(format!(
                    "{} added: {} ({})",
                    step.id,
                    step.label,
                    step.status.word()
                ));
                continue;
            };
            if was.label != step.label {
                lines.push(format!(
                    "{} relabelled: {} → {}",
                    step.id, was.label, step.label
                ));
            }
            if was.status != step.status {
                lines.push(format!(
                    "{} {} → {}",
                    step.id,
                    was.status.word(),
                    step.status.word()
                ));
            }
            if was.outcome != step.outcome {
                lines.push(format!("{} outcome: {}", step.id, step.outcome));
            }
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(value: serde_json::Value) -> Write {
        serde_json::from_value(value).expect("test write parses")
    }

    #[test]
    fn sections_and_steps_get_stable_ids_and_record_their_times() {
        let (list, changes) = apply(
            &List::default(),
            write(serde_json::json!({"section":{"title":"Fix login"},
                "steps":[{"label":"Reproduce"},{"label":"Patch","status":"in_progress"}]})),
            10,
        )
        .unwrap();
        assert_eq!(list.revision, 1);
        assert_eq!(changes[0], "s1 added: Fix login");
        assert_eq!(list.sections[0].steps[1].id, "s1.2");
        assert_eq!(list.sections[0].steps[1].started_at, Some(10));
        let (list, _) = apply(
            &list,
            write(serde_json::json!({"section":{"id":"s1"},
                "steps":[{"id":"s1.2","status":"done","outcome":"cookie path fixed"}]})),
            20,
        )
        .unwrap();
        let step = &list.sections[0].steps[1];
        assert_eq!((step.started_at, step.finished_at), (Some(10), Some(20)));
        assert_eq!(step.outcome, "cookie path fixed");
    }

    #[test]
    fn closing_needs_finished_steps_and_a_summary_and_reopening_allows_edits() {
        let (list, _) = apply(
            &List::default(),
            write(serde_json::json!({"section":{"title":"Check queue"},
                "steps":[{"label":"Read board"}]})),
            1,
        )
        .unwrap();
        let close = serde_json::json!({"section":{"id":"s1","status":"done",
            "summary":"Queue was held by another task; released"}});
        let refused = apply(&list, write(close.clone()), 2)
            .unwrap_err()
            .to_string();
        assert!(refused.contains("s1.1 is still open"), "{refused}");
        let (list, _) = apply(
            &list,
            write(serde_json::json!({"section":{"id":"s1"},
                "steps":[{"id":"s1.1","status":"done"}]})),
            3,
        )
        .unwrap();
        let (closed, _) = apply(&list, write(close), 4).unwrap();
        assert_eq!(closed.sections[0].status, Status::Done);
        let edit = serde_json::json!({"section":{"id":"s1"},"steps":[{"label":"More"}]});
        assert!(apply(&closed, write(edit.clone()), 5).is_err());
        let (reopened, _) = apply(
            &closed,
            write(serde_json::json!({"section":{"id":"s1","status":"open"}})),
            6,
        )
        .unwrap();
        assert!(apply(&reopened, write(edit), 7).is_ok());
    }

    #[test]
    fn bad_writes_change_nothing() {
        let (list, _) = apply(
            &List::default(),
            write(serde_json::json!({"section":{"title":"Work"},"steps":[{"label":"One"}]})),
            1,
        )
        .unwrap();
        for bad in [
            serde_json::json!({"section":{"id":"s9"}}),
            serde_json::json!({"section":{}}),
            serde_json::json!({"section":{"id":"s1"}}),
            serde_json::json!({"section":{"id":"s1","origin":"user"}}),
            serde_json::json!({"section":{"id":"s1","status":"blocked"}}),
            serde_json::json!({"section":{"id":"s1"},"steps":[{"id":"s1.7"}]}),
            serde_json::json!({"section":{"id":"s1"},"steps":[{"label":"no"}]}),
        ] {
            assert!(apply(&list, write(bad.clone()), 2).is_err(), "{bad}");
        }
        assert!(serde_json::from_value::<Write>(serde_json::json!({"section":{"x":1}})).is_err());
    }

    #[test]
    fn plan_seeds_a_user_section_and_compare_names_each_change() {
        let labels = vec!["Reproduce".to_owned(), "Patch".to_owned()];
        let (seeded, _) = apply(
            &List::default(),
            plan_section("Plan", "keep v2", &labels),
            1,
        )
        .unwrap();
        assert_eq!(seeded.sections[0].origin, Origin::User);
        assert_eq!(seeded.sections[0].notes, "keep v2");
        let (after, _) = apply(
            &seeded,
            write(serde_json::json!({"section":{"id":"s1"},
                "steps":[{"id":"s1.1","status":"done"},{"label":"Verify"}]})),
            2,
        )
        .unwrap();
        assert_eq!(
            compare(&seeded, &after),
            ["s1.1 open → done", "s1.3 added: Verify (open)"]
        );
        assert_eq!(overview(&after), ["s1 Plan [user · open] 1 done, 2 open"]);
        assert!(after.sections[0].plan);
        assert_eq!(
            plan_checklist(&after.sections[0]),
            "1. [x] Reproduce\n2. [ ] Patch\n3. [ ] Verify"
        );
    }

    #[test]
    fn marks_name_created_started_and_closed_sections() {
        let (one, _) = apply(
            &List::default(),
            write(serde_json::json!({"section":{"title":"Plan work"},
                "steps":[{"label":"First"}]})),
            1,
        )
        .unwrap();
        assert_eq!(
            marks(&List::default(), &one),
            serde_json::json!({"created":["s1"]})
        );
        let (two, _) = apply(
            &one,
            write(serde_json::json!({"section":{"id":"s1"},
                "steps":[{"id":"s1.1","status":"in_progress"}]})),
            2,
        )
        .unwrap();
        assert_eq!(marks(&one, &two), serde_json::json!({"started":["s1"]}));
        let (three, _) = apply(
            &two,
            write(serde_json::json!({"section":{"id":"s1","status":"done",
                "summary":"First step finished and checked"},
                "steps":[{"id":"s1.1","status":"done"}]})),
            3,
        )
        .unwrap();
        assert_eq!(marks(&two, &three), serde_json::json!({"closed":["s1"]}));
    }

    /// A section waiting to start after a closed one carries the fold hint until it starts.
    #[test]
    fn step_line_offers_the_fold_before_a_new_task_starts() {
        let texts = texts();
        let (one, _) = apply(
            &List::default(),
            write(
                serde_json::json!({"section":{"title":"Old work","status":"done",
                "summary":"Old work finished and checked"},
                "steps":[{"label":"Only step","status":"done"}]}),
            ),
            1,
        )
        .unwrap();
        let (two, _) = apply(
            &one,
            write(serde_json::json!({"section":{"title":"New work"},
                "steps":[{"label":"First"}]})),
            2,
        )
        .unwrap();
        let line = step_line(&two, &texts).unwrap();
        assert!(
            line.contains("Before starting s2") && line.contains("fold_earlier"),
            "{line}"
        );
        let (three, _) = apply(
            &two,
            write(serde_json::json!({"section":{"id":"s2"},
                "steps":[{"id":"s2.1","status":"in_progress"}]})),
            3,
        )
        .unwrap();
        assert!(!step_line(&three, &texts).unwrap().contains("fold_earlier"));
    }

    #[test]
    fn snapshot_and_step_line_follow_the_list() {
        let texts = texts();
        let empty = render_snapshot(&List::default(), &texts);
        assert!(
            empty.contains("revision 0") && empty.contains("No sections yet"),
            "{empty}"
        );
        assert_eq!(step_line(&List::default(), &texts), None);
        let (list, _) = apply(
            &List::default(),
            write(
                serde_json::json!({"section":{"title":"Fix login","notes":"keep Secure"},
                "steps":[{"label":"Reproduce","status":"done","outcome":"path /api"},
                    {"label":"Patch","status":"in_progress"},{"label":"Verify"}]}),
            ),
            1,
        )
        .unwrap();
        let snapshot = render_snapshot(&list, &texts);
        assert!(snapshot.contains("  notes: keep Secure"), "{snapshot}");
        assert!(
            snapshot.contains("  s1.1 [done] Reproduce → path /api"),
            "{snapshot}"
        );
        assert_eq!(
            step_line(&list, &texts).unwrap(),
            "Session tasks: s1 \"Fix login\", 2 open, active s1.2 \"Patch\", next s1.3 \"Verify\"."
        );
        // An outcome edit changes nothing the line shows.
        let (edited, _) = apply(
            &list,
            write(serde_json::json!({"section":{"id":"s1"},
                "steps":[{"id":"s1.1","outcome":"cookie path /api"}]})),
            2,
        )
        .unwrap();
        assert_eq!(step_line(&edited, &texts), step_line(&list, &texts));
    }

    /// Requests get stable labels. The gate never refuses: it files unfiled requests on the
    /// newest open section, or opens one titled from the first, and needs no step in
    /// progress; the turn-end settling sets aside what the reply answered without tools.
    #[test]
    fn the_gate_files_requests_and_the_turn_end_settles_the_rest() {
        let mut requests = Requests::default();
        assert!(requests.add("user:a", "Fix the   login\nredirect please", 1));
        assert!(!requests.add("user:a", "again", 2));
        assert!(requests.add("user:b", "Also check mobile", 3));
        assert_eq!(requests.items[0].id, "u1");
        assert_eq!(requests.items[0].text, "Fix the login redirect please");
        assert_eq!(requests.items[1].id, "u2");

        let empty = List::default();
        let Gate::File(opening) = gate(&empty, &requests, false) else {
            panic!("unfiled requests are filed");
        };
        let (opened, _) = apply(&empty, opening, 10).unwrap();
        assert_eq!(opened.sections[0].title, "Fix the login redirect please");
        assert_eq!(opened.sections[0].requests, vec!["u1", "u2"]);
        assert_eq!(opened.sections[0].origin, Origin::User);
        assert!(opened.sections[0].steps.is_empty());
        // No step in progress is needed.
        assert!(matches!(gate(&opened, &requests, false), Gate::Pass));

        // A follow-up goes on the newest open section.
        requests.add("user:c", "Now the CSS", 20);
        let Gate::File(follow) = gate(&opened, &requests, false) else {
            panic!("a follow-up is filed");
        };
        assert_eq!(follow.section.id.as_deref(), Some("s1"));
        let (followed, _) = apply(&opened, follow, 21).unwrap();
        assert_eq!(followed.sections[0].requests, vec!["u1", "u2", "u3"]);

        // A message answered without tools is set aside when the turn ends.
        requests.add("user:d", "Thanks!", 30);
        let settling = settle(&followed, &requests).unwrap();
        let (settled, changes) = apply(&followed, settling, 31).unwrap();
        assert_eq!(changes, vec!["u4 set aside: answered in the reply"]);
        assert!(settle(&settled, &requests).is_none());
        assert!(matches!(gate(&settled, &requests, false), Gate::Pass));
        // When the gate covers every tool, a set-aside newest request getting tool calls is
        // cited instead, which takes it off the set-aside list.
        let Gate::File(recite) = gate(&settled, &requests, true) else {
            panic!("a set-aside newest request is cited for tool work");
        };
        let (recited, _) = apply(&settled, recite, 32).unwrap();
        assert!(recited.set_aside.is_empty());
        assert!(matches!(gate(&recited, &requests, true), Gate::Pass));

        // With no section open, the next request opens a new one.
        let (closed, _) = apply(
            &recited,
            write(serde_json::json!({"section":{"id":"s1","status":"done",
                "summary":"Login redirect and CSS fixed."}})),
            33,
        )
        .unwrap();
        requests.add("user:e", "Add a dark theme", 40);
        let Gate::File(second) = gate(&closed, &requests, false) else {
            panic!("a new request opens a section");
        };
        assert_eq!(second.section.id, None);
        assert_eq!(second.section.title.as_deref(), Some("Add a dark theme"));

        let (unknown, _) = apply(
            &recited,
            write(serde_json::json!({"section":{"id":"s1","requests":["u9"]}})),
            41,
        )
        .unwrap();
        assert_eq!(unknown_requests(&recited, &unknown, &requests), vec!["u9"]);
        assert!(apply(
            &recited,
            write(serde_json::json!({"set_aside":[{"request":"x1","reason":"bad"}]})),
            42
        )
        .is_err());
    }

    /// Only the newest requests are cited by one filing write; older unfiled ones are set
    /// aside, so a backlog never needs another write on the next call.
    #[test]
    fn a_backlog_of_unfiled_requests_is_set_aside_except_the_newest() {
        let mut requests = Requests::default();
        for index in 0..20 {
            requests.add(&format!("user:{index}"), &format!("Message {index}"), index);
        }
        let Gate::File(filing) = gate(&List::default(), &requests, false) else {
            panic!("unfiled requests are filed");
        };
        let (filed, _) = apply(&List::default(), filing, 50).unwrap();
        assert_eq!(filed.sections[0].requests.len(), MAX_FILED);
        assert_eq!(filed.set_aside.len(), 20 - MAX_FILED);
        assert_eq!(filed.sections[0].requests[0], "u5");
        assert!(matches!(gate(&filed, &requests, false), Gate::Pass));
    }

    /// The usual slips cost no retry: `in_progress` on a section means open, and a guessed
    /// id for a new titled section gets the server's id.
    #[test]
    fn common_write_slips_are_accepted() {
        let (list, _) = apply(
            &List::default(),
            write(
                serde_json::json!({"section":{"id":"s1","title":"List the files",
                "status":"in_progress"},"steps":[{"label":"Run ls","status":"in_progress"}]}),
            ),
            1,
        )
        .unwrap();
        assert_eq!(list.sections[0].id, "s1");
        assert_eq!(list.sections[0].status, Status::Open);
        let (again, _) = apply(
            &list,
            write(serde_json::json!({"section":{"id":"s7","title":"Another task"}})),
            2,
        )
        .unwrap();
        assert_eq!(again.sections[1].id, "s2");
        assert!(apply(&list, write(serde_json::json!({"section":{"id":"s9"}})), 3).is_err());
        let (same, changes) = apply(
            &list,
            write(
                serde_json::json!({"section":{"id":"s1","status":"in_progress",
                "notes":"keep it short"}}),
            ),
            4,
        )
        .unwrap();
        assert_eq!(same.sections[0].status, Status::Open);
        assert_eq!(changes, vec!["s1 notes updated"]);
    }
}
