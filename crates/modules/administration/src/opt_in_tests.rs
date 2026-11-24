//! The nightly opt-in test run as administrators see it: the Dashboard card's view and when the
//! watch raises an administrator alert.
//!
//! The broker's nightly timer (`buildctl nightly-tests`) runs every listed opt-in test group
//! (tests too load-sensitive for each release's suite) on the published source and writes a
//! summary beside the checkout; the build broker hands the app a projection
//! (`v3_opt_in_tests`). This module parses it again as untrusted input: an unknown field or a
//! value outside its bounds refuses the whole report. Report-only: nothing here runs anything.
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// Largest projection accepted (the broker's own limit on the file is 64 KiB).
pub const MAX_BYTES: usize = 64 * 1024;
/// Memory row key for the watch (see `webagent_notifications::alerts::memory`).
pub const MEMORY_KEY: &str = "opt_in_tests";
const MAX_GROUPS: usize = 16;
const MAX_FAILED: usize = 50;
/// A summary older than this is shown as stale (the timer runs nightly).
pub const STALE_SECONDS: f64 = 48.0 * 3600.0;

/// One group's result in the newest nightly run.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Group {
    pub group: String,
    /// `passed`, `failed`, `skipped` (still queued behind release builds) or `error`.
    pub status: String,
    pub job_id: Option<String>,
    pub inputs_sha256: Option<String>,
    pub tests: u64,
    pub failed_tests: Vec<String>,
}

/// The broker's projection of the newest nightly summary.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Report {
    pub available: bool,
    pub reason: Option<String>,
    pub status: Option<String>,
    pub checked_at: Option<f64>,
    #[serde(default)]
    pub groups: Vec<Group>,
}

fn name(value: &str, max: usize, colon: bool) -> bool {
    (1..=max).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || (colon && b == b':'))
}

fn hex_id(value: Option<&str>, len: usize, dash: bool) -> bool {
    value.is_none_or(|v| {
        v.len() == len
            && v.bytes()
                .all(|b| (b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) || (dash && b == b'-'))
    })
}

/// Parse and check the broker output.
///
/// # Errors
/// Oversized output, an unknown field, or any value outside its bounds.
pub fn parse(output: &Value) -> anyhow::Result<Report> {
    if serde_json::to_vec(output)?.len() > MAX_BYTES {
        anyhow::bail!("Opt-in test report too large");
    }
    let report: Report = serde_json::from_value(output.clone())?;
    let reason_ok = report
        .reason
        .as_deref()
        .is_none_or(|r| matches!(r, "missing" | "malformed" | "too_large" | "refused"));
    let status_ok = report
        .status
        .as_deref()
        .is_none_or(|s| matches!(s, "passed" | "failed" | "skipped"));
    let time_ok = report
        .checked_at
        .is_none_or(|t| t.is_finite() && (0.0..1e11).contains(&t));
    let groups_ok = report.groups.len() <= MAX_GROUPS
        && report.groups.iter().all(|g| {
            name(&g.group, 40, false)
                && matches!(g.status.as_str(), "passed" | "failed" | "skipped" | "error")
                && hex_id(g.job_id.as_deref(), 36, true)
                && hex_id(g.inputs_sha256.as_deref(), 64, false)
                && g.tests <= 10_000
                && g.failed_tests.len() <= MAX_FAILED
                && g.failed_tests.iter().all(|t| name(t, 160, true))
        });
    if !(reason_ok && status_ok && time_ok && groups_ok) {
        anyhow::bail!("Opt-in test report refused");
    }
    Ok(report)
}

/// What failed in a report, one entry per failed test (or `group:error` without names).
fn failures(report: &Report) -> Vec<String> {
    report
        .groups
        .iter()
        .filter(|g| matches!(g.status.as_str(), "failed" | "error"))
        .flat_map(|g| {
            if g.failed_tests.is_empty() {
                vec![format!("{}:{}", g.group, g.status)]
            } else {
                g.failed_tests
                    .iter()
                    .map(|t| format!("{}:{t}", g.group))
                    .collect()
            }
        })
        .collect()
}

/// What the watch has already alerted on (one memory row): the failure it alerted, if any.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Memory {
    /// Content-free fingerprint of the alerted set of failed tests.
    pub failure: Option<String>,
}

impl Memory {
    /// The stored row; an unreadable row is empty memory (at worst one repeated alert).
    #[must_use]
    pub fn from_value(value: Option<&Value>) -> Self {
        Self {
            failure: value
                .and_then(|v| v["failure"].as_str())
                .filter(|f| f.len() == 32 && f.bytes().all(|b| b.is_ascii_hexdigit()))
                .map(str::to_owned),
        }
    }

    /// The row to store.
    #[must_use]
    pub fn to_value(&self) -> Value {
        json!({"failure":self.failure})
    }
}

/// The alert one watch pass raises (if any), and the memory to store afterwards.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decision {
    /// Raise the failed alert with this one-line detail (group and test names only).
    pub alert: Option<String>,
    pub memory: Memory,
}

/// Decide the alert for `report` given what was already alerted.
///
/// - A failed run alerts once per new set of failed tests; the same failures night after
///   night, or a restart, do not repeat it.
/// - A passing run forgets the alerted failure, so a later failure alerts again.
/// - A skipped run (queued behind release builds) and an unreadable report change nothing.
#[must_use]
pub fn decide(report: &Report, memory: &Memory) -> Decision {
    if !report.available {
        return Decision {
            alert: None,
            memory: memory.clone(),
        };
    }
    match report.status.as_deref() {
        Some("passed") => Decision {
            alert: None,
            memory: Memory::default(),
        },
        Some("failed") => {
            let mut failed = failures(report);
            failed.sort();
            let digest = Sha256::digest(failed.join("\n").as_bytes());
            let fingerprint = hex::encode(&digest[..16]);
            if memory.failure.as_deref() == Some(fingerprint.as_str()) {
                return Decision {
                    alert: None,
                    memory: memory.clone(),
                };
            }
            let shown: Vec<&str> = failed.iter().take(3).map(String::as_str).collect();
            let mut detail = format!(
                "{} failed: {}",
                failed.len(),
                if shown.is_empty() {
                    "no test names recorded".to_owned()
                } else {
                    shown.join(", ")
                }
            );
            if failed.len() > shown.len() {
                detail.push_str(", …");
            }
            Decision {
                alert: Some(detail),
                memory: Memory {
                    failure: Some(fingerprint),
                },
            }
        }
        _ => Decision {
            alert: None,
            memory: memory.clone(),
        },
    }
}

/// The Dashboard card's view of a parsed report.
#[must_use]
pub fn view(report: &Report, now: f64) -> Value {
    let state = if !report.available {
        if report.reason.as_deref() == Some("missing") {
            "missing"
        } else {
            "unreadable"
        }
    } else if report
        .checked_at
        .is_none_or(|checked| now - checked > STALE_SECONDS)
    {
        "stale"
    } else {
        report.status.as_deref().unwrap_or("unreadable")
    };
    let groups: Vec<Value> = report
        .groups
        .iter()
        .map(|g| {
            json!({"group":g.group,"status":g.status,"job_id":g.job_id,"tests":g.tests,
            "failed_tests":g.failed_tests})
        })
        .collect();
    json!({"schema_version":1,"state":state,"checked_at":report.checked_at,"groups":groups})
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: f64 = 1_795_500_000.0;
    const JOB: &str = "11111111-2222-3333-4444-555555555555";

    fn report(status: &str, failed: &[&str]) -> Value {
        json!({"available":true,"reason":null,"status":status,"checked_at":NOW - 3600.0,
        "groups":[{"group":"multi_loop","status":status,"job_id":JOB,"inputs_sha256":"a".repeat(64),
        "tests":5,"failed_tests":failed}]})
    }

    #[test]
    fn a_new_failure_alerts_once_and_passing_forgets_it() {
        let failed = parse(&report("failed", &["multi_loop_tests::killed_loop"])).unwrap();
        let first = decide(&failed, &Memory::default());
        let detail = first.alert.clone().unwrap();
        assert!(detail.contains("multi_loop:multi_loop_tests::killed_loop"));
        // The same failure the next night (or after a restart): no second alert.
        let again = decide(&failed, &Memory::from_value(Some(&first.memory.to_value())));
        assert_eq!(again.alert, None);
        // A different failure is new.
        let other = parse(&report("failed", &["multi_loop_tests::fence"])).unwrap();
        assert!(decide(&other, &again.memory).alert.is_some());
        // Passing raises nothing and forgets, so the old failure coming back alerts again.
        let passed = parse(&report("passed", &[])).unwrap();
        let clean = decide(&passed, &again.memory);
        assert_eq!(clean.alert, None);
        assert_eq!(clean.memory, Memory::default());
        assert!(decide(&failed, &clean.memory).alert.is_some());
    }

    #[test]
    fn passing_skipped_and_unreadable_runs_never_alert() {
        for value in [
            report("passed", &[]),
            report("skipped", &[]),
            json!({"available":false,"reason":"missing"}),
        ] {
            let parsed = parse(&value).unwrap();
            assert_eq!(decide(&parsed, &Memory::default()).alert, None);
        }
        let failed = parse(&report("failed", &[])).unwrap();
        assert!(decide(&failed, &Memory::default())
            .alert
            .unwrap()
            .contains("multi_loop:failed"));
    }

    #[test]
    fn malformed_reports_are_refused() {
        let mut extra = report("passed", &[]);
        extra["groups"][0]["output"] = json!("panic text");
        let mut status = report("passed", &[]);
        status["status"] = json!("exploded");
        for bad in [
            report("failed", &["rm -rf /"]),
            extra,
            status,
            json!({"available":true,"status":"passed","checked_at":-1.0,"groups":[]}),
            json!({"available":true,"status":"passed","checked_at":NOW,"groups":[],"x":1}),
        ] {
            assert!(parse(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_card_view_shows_state_and_age() {
        let view_of = |value: Value, now: f64| view(&parse(&value).unwrap(), now);
        assert_eq!(view_of(report("failed", &["a::b"]), NOW)["state"], "failed");
        assert_eq!(view_of(report("passed", &[]), NOW)["state"], "passed");
        assert_eq!(
            view_of(report("passed", &[]), NOW + STALE_SECONDS)["state"],
            "stale"
        );
        assert_eq!(
            view_of(json!({"available":false,"reason":"missing"}), NOW)["state"],
            "missing"
        );
        assert_eq!(
            view_of(json!({"available":false,"reason":"refused"}), NOW)["state"],
            "unreadable"
        );
        let shown = view_of(report("failed", &["a::b"]), NOW);
        assert_eq!(shown["groups"][0]["failed_tests"][0], "a::b");
        assert!(shown["groups"][0].get("inputs_sha256").is_none());
    }
}
