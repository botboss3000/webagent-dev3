//! App Default editing: which ability source files administrators may change from the
//! Abilities page, each administrator's draft set, and the broker task a Save opens.
//!
//! Drafts are plain source text (tool definitions, skills, manifests, icons), kept per
//! administrator in the app's state folder, never in agent databases. A Save writes them to
//! the live ability files once the release check passes on the result (the broker commits
//! them). The broker-task Save that releases drafts like any other source change remains
//! for a draft set no live file can hold.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// The broker workspace every Save uses; the task key also carries the administrator.
pub const SESSION: &str = "00000000-0000-0000-0000-00000000ad0f";
/// How Saves appear as their owning session on the Builds page.
pub const SESSION_NAME: &str = "App defaults";
pub const MAX_FILE_BYTES: usize = 512 * 1024;
pub const MAX_DRAFT_FILES: usize = 200;
const MAX_NOTE_CHARS: usize = 120;

/// Whether checkout-relative `path` is an ability file App Default edits: a group's
/// `_group.json` or icon, an ability's `<ability>.json` or icon, an ability skill, or a
/// tool's definition or skill. Nothing outside `abilities/`, no hidden or odd names.
pub fn editable(path: &str) -> bool {
    let parts: Vec<&str> = path.split('/').collect();
    if path.len() > 300
        || parts.first() != Some(&"abilities")
        || parts.iter().any(|part| {
            part.is_empty()
                || part.starts_with('.')
                || !part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
        })
    {
        return false;
    }
    let name = parts[parts.len() - 1];
    match parts.len() {
        3 => name == "_group.json" || name == "_group.svg",
        4 => name == format!("{}.json", parts[2]) || name == format!("{}.svg", parts[2]),
        5 => parts[3] == "skills" && name.ends_with(".skill.md") && name.len() > ".skill.md".len(),
        6 => {
            parts[3] == "tools"
                && (name == format!("{}.tool.json", parts[4])
                    || name == format!("{}.skill.md", parts[4]))
        }
        _ => false,
    }
}

/// Whether a Save can write `path` straight to the live ability files (`live-files.json`):
/// JSON, Markdown and icons, which the catalog checks again when it reads them.
pub fn saves_live(path: &str) -> bool {
    editable(path) && (path.ends_with(".json") || path.ends_with(".md") || path.ends_with(".svg"))
}

/// The checkout an administrator edits, as the broker task starts from it.
pub fn checkout_file(path: &str) -> Option<PathBuf> {
    editable(path).then(|| webagent_configuration::paths::checkout().join(path))
}

/// One administrator's draft file: the folder holds no other data.
pub fn drafts_file(user: &str) -> PathBuf {
    webagent_configuration::paths::state()
        .join("app-defaults")
        .join(format!(
            "{}.json",
            hex::encode(&Sha256::digest(user.as_bytes())[..16])
        ))
}

#[must_use]
pub fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// One drafted file: new content, or a deletion, and the checkout revision it started from.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Draft {
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub delete: bool,
    /// SHA-256 of the checkout file when drafting began; None when it did not exist.
    #[serde(default)]
    pub base_sha256: Option<String>,
    #[serde(default)]
    pub updated_at: u64,
}

/// The Save in progress or the last one that did not complete.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Release {
    pub task_id: String,
    #[serde(default)]
    pub job_id: Option<String>,
    pub started_at: u64,
    /// `releasing` while the job runs; `failed` keeps the drafts and the task for repair.
    pub status: String,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub paths: Vec<String>,
}

/// Everything one administrator has drafted. `revision` fences concurrent page writes.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct DraftSet {
    #[serde(default)]
    pub user: String,
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub files: BTreeMap<String, Draft>,
    #[serde(default)]
    pub release: Option<Release>,
}

impl DraftSet {
    /// A Save is running: new edits are kept, but Save waits for it.
    pub fn releasing(&self) -> bool {
        self.release
            .as_ref()
            .is_some_and(|r| r.status == "releasing")
    }
}

/// Why a drafted file cannot be saved, if it cannot.
pub fn draft_problem(path: &str, draft: &Draft) -> Option<String> {
    if !editable(path) {
        return Some(format!("{path} is not an App Default file"));
    }
    match (&draft.content, draft.delete) {
        (Some(_), true) | (None, false) => {
            return Some(format!(
                "{path}: give content or delete, not both or neither"
            ));
        }
        (None, true) => return None,
        (Some(text), false) => {
            if text.len() > MAX_FILE_BYTES || text.contains('\0') {
                return Some(format!(
                    "{path}: content is limited to {MAX_FILE_BYTES} bytes of text"
                ));
            }
            if path.ends_with(".json") && serde_json::from_str::<serde_json::Value>(text).is_err() {
                return Some(format!("{path}: content is not valid JSON"));
            }
            if path.ends_with(".svg") {
                return webagent_abilities::icons::svg_problem(text)
                    .map(|problem| format!("{path}: {problem}"));
            }
        }
    }
    None
}

/// The broker task label for one Save: unique per administrator and time.
pub fn task_label(user: &str, now: u64) -> String {
    let user: String = user
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(20)
        .collect();
    format!(
        "appdef-{}-{now:x}",
        if user.is_empty() { "admin" } else { &user }
    )
}

/// The broker task summary: what changed, the optional note, and who saved it.
pub fn summary(paths: &[String], note: &str, user: &str) -> String {
    let names: Vec<&str> = paths
        .iter()
        .map(|path| path.rsplit('/').next().unwrap_or(path))
        .collect();
    let mut text = format!(
        "App defaults: {}",
        names.iter().take(3).copied().collect::<Vec<_>>().join(", ")
    );
    if names.len() > 3 {
        text += &format!(" +{} more", names.len() - 3);
    }
    let note: String = note
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_NOTE_CHARS)
        .collect();
    if !note.is_empty() {
        text += &format!(" — {note}");
    }
    let user: String = user
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .take(30)
        .collect();
    let tag = format!(" [origin=app; session={SESSION_NAME} ({user})]");
    let room = 200 - tag.len();
    if text.chars().count() > room {
        text = text.chars().take(room - 1).collect::<String>() + "…";
    }
    text + &tag
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_ability_source_files_are_editable() {
        for good in [
            "abilities/Core/_group.json",
            "abilities/Core/_group.svg",
            "abilities/Core/base/base.json",
            "abilities/Core/base/base.svg",
            "abilities/Core/base/skills/notes.skill.md",
            "abilities/Core/base/tools/calculate/calculate.tool.json",
            "abilities/Core/base/tools/calculate/calculate.skill.md",
        ] {
            assert!(editable(good), "{good}");
        }
        for bad in [
            "src/main.rs",
            "abilities/Core/base/tools/calculate/calculate.rs",
            "abilities/Core/base/tools/calculate/other.tool.json",
            "abilities/Core/base/plugin.json",
            "abilities/Core/base/Cargo.toml",
            "abilities/../src/x.json",
            "abilities/Core/.hidden/_group.json",
            "abilities/Core/base/skills/.skill.md",
            "abilities//Core/_group.json",
            "abilities/Core/base/skills/a b.skill.md",
        ] {
            assert!(!editable(bad), "{bad}");
        }
    }

    #[test]
    fn ability_files_save_live() {
        assert!(saves_live("abilities/Core/_group.json"));
        assert!(saves_live("abilities/Core/base/skills/notes.skill.md"));
        assert!(saves_live("abilities/Core/base/base.svg"));
        assert!(saves_live("abilities/Core/_group.svg"));
        assert!(!saves_live("src/main.json"));
    }

    #[test]
    fn drafts_are_checked_before_save() {
        let text = |content: &str| Draft {
            content: Some(content.into()),
            ..Draft::default()
        };
        let json = "abilities/Core/_group.json";
        assert_eq!(draft_problem(json, &text("{\"name\":\"Core\"}")), None);
        assert!(draft_problem(json, &text("{not json")).is_some());
        assert!(draft_problem(
            json,
            &Draft {
                content: Some("{}".into()),
                delete: true,
                ..Draft::default()
            }
        )
        .is_some());
        assert!(draft_problem(json, &Draft::default()).is_some());
        assert_eq!(
            draft_problem(
                json,
                &Draft {
                    delete: true,
                    ..Draft::default()
                }
            ),
            None
        );
        assert!(draft_problem("src/app.rs", &text("x")).is_some());
        let icon = "abilities/Core/base/base.svg";
        assert_eq!(
            draft_problem(
                icon,
                &text("<svg viewBox=\"0 0 24 24\"><path d=\"M0 0h24\"/></svg>")
            ),
            None
        );
        for bad in [
            "<svg><script>x</script></svg>",
            "<svg onload=\"x()\"></svg>",
            "<svg><a href=\"javascript:x\"/></svg>",
            "<div/>",
        ] {
            assert!(draft_problem(icon, &text(bad)).is_some(), "{bad}");
        }
    }

    #[test]
    fn save_label_and_summary_fit_the_broker() {
        let label = task_label("ad.min!", 0x1234);
        assert_eq!(label, "appdef-admin-1234");
        let paths: Vec<String> = (0..5)
            .map(|i| format!("abilities/Core/base/tools/t{i}/t{i}.tool.json"))
            .collect();
        let text = summary(&paths, "  tighten limits\n", "admin");
        assert!(
            text.starts_with(
                "App defaults: t0.tool.json, t1.tool.json, t2.tool.json +2 more — tighten limits"
            ),
            "{text}"
        );
        assert!(text.ends_with(" [origin=app; session=App defaults (admin)]"));
        assert!(summary(&paths, &"x".repeat(500), "admin").chars().count() <= 200);
    }
}
