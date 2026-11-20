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
}

/// A session's task list at one revision.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct List {
    pub revision: u64,
    pub sections: Vec<Section>,
    /// The number the next new section gets.
    pub next_section: u32,
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
}

/// One write: a section and the steps to add or change in it; steps left out are untouched.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Write {
    pub section: SectionWrite,
    #[serde(default)]
    pub steps: Vec<StepWrite>,
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
    let index = match write.section.id.as_deref() {
        Some(id) => next
            .sections
            .iter()
            .position(|section| section.id == id)
            .ok_or_else(|| anyhow!("No section {id}; read the list for current ids"))?,
        None => {
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
            };
            changes.push(format!("{} added: {}", section.id, section.title));
            next.sections.push(section);
            next.sections.len() - 1
        }
    };
    let section = &mut next.sections[index];
    if write.section.id.is_some() {
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
    for step in write.steps {
        changes.push(write_step(section, step, now)?);
    }
    if let Some(summary) = write.section.summary.as_deref() {
        section.summary = text(summary, 20, 1200, "A section summary")?;
    }
    if let Some(status) = write
        .section
        .status
        .filter(|status| *status != section.status)
    {
        set_section_status(section, status)?;
        changes.push(format!("{} {}", section.id, status.word()));
    } else if section.status == Status::Done && write.section.summary.is_some() {
        changes.push(format!("{} summary updated", section.id));
    }
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

fn write_step(section: &mut Section, write: StepWrite, now: u64) -> Result<String> {
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
    Ok(if new {
        format!("{} added: {}", step.id, step.label)
    } else {
        format!("{} {}", step.id, step.status.word())
    })
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
/// active steps and open count, or `None` with no open work. It carries no revision, so it
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
            Some(if active.is_empty() {
                format!(
                    "{} \"{}\", {open} open, none in progress",
                    section.id, section.title
                )
            } else {
                format!(
                    "{} \"{}\", {open} open, active {}",
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

/// The end-of-turn reminder when open sections still have open or in-progress steps (up to
/// eight named), or `None`.
#[must_use]
pub fn end_reminder(list: &List, texts: &Value) -> Option<String> {
    let open = list
        .sections
        .iter()
        .filter(|section| section.status == Status::Open)
        .flat_map(|section| section.steps.iter())
        .filter(|step| step.status.unfinished())
        .collect::<Vec<_>>();
    let template = text_field(texts, "end_reminder");
    if open.is_empty() || template.is_empty() {
        return None;
    }
    let mut steps = open
        .iter()
        .take(8)
        .map(|step| format!("{} \"{}\"", step.id, step.label))
        .collect::<Vec<_>>()
        .join(", ");
    if open.len() > 8 {
        steps.push_str(&format!(" and {} more", open.len() - 8));
    }
    Some(template.replace("{steps}", &steps))
}

/// The reminder to file tool work in the list, once `calls` tool calls since the user's last
/// message reach the configured threshold without a `task_list_write` among them.
#[must_use]
pub fn unfiled_reminder(calls: u64, filed: bool, texts: &Value) -> Option<String> {
    let threshold = texts["unfiled_after_calls"].as_u64().unwrap_or(0);
    let text = text_field(texts, "unfiled_reminder");
    (threshold > 0 && calls >= threshold && !filed && !text.is_empty()).then_some(text)
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
            "Session tasks: s1 \"Fix login\", 2 open, active s1.2 \"Patch\"."
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
        assert_eq!(
            end_reminder(&list, &texts).unwrap(),
            "Before you end this turn: s1.2 \"Patch\", s1.3 \"Verify\" are still open in the \
             session task list. Finish them, mark them blocked or cancelled with \
             task_list_write, or answer without calling a tool to end the turn with your \
             earlier reply."
        );
        assert_eq!(end_reminder(&List::default(), &texts), None);
        assert!(unfiled_reminder(8, false, &texts).is_some());
        assert_eq!(unfiled_reminder(7, false, &texts), None);
        assert_eq!(unfiled_reminder(9, true, &texts), None);
    }
}
