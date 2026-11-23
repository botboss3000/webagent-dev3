//! The dependency (supply-chain) scan as administrators see it: the Dashboard card's view and
//! when the scan watch raises an administrator alert.
//!
//! The host scanner (`build-service/supply_chain_scan.py`, report-only) writes its summary
//! beside the checkout; the build broker reads it with file guards and hands the app a
//! projection (`v3_supply_chain`). This module parses that projection again as untrusted
//! input: unknown fields, out-of-range numbers or unexpected text refuse the whole report.
//! Nothing here changes or updates anything; a fix goes through an ordinary broker task.
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

/// Largest projection accepted (the broker's own limit on the file is 256 KiB).
pub const MAX_BYTES: usize = 256 * 1024;
/// Most findings and targets one report may carry.
pub const MAX_FINDINGS: usize = 200;
const MAX_TARGETS: usize = 16;
const SEVERITIES: [&str; 5] = ["CRITICAL", "HIGH", "MEDIUM", "LOW", "UNKNOWN"];
/// Memory row key for the watch (see `webagent_notifications::alerts::memory`).
pub const MEMORY_KEY: &str = "supply_chain";

/// One fixable high or critical finding.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Finding {
    pub package: String,
    pub installed: String,
    pub id: String,
    pub fixed: String,
}

/// Counts and fixable findings for one scanned target (cargo, ui, api, loop, gate).
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub by_severity: BTreeMap<String, u64>,
    pub fixable_by_severity: BTreeMap<String, u64>,
    pub fixable_high_critical: Vec<Finding>,
    /// Fixable high/critical findings beyond the listed ones (the broker lists at most
    /// `MAX_FINDINGS` across all targets, critical first; the counts above stay exact).
    #[serde(default)]
    pub fixable_not_shown: u64,
}

/// A UI library whose file header disagrees with the recorded version.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Mismatch {
    pub library: String,
    pub header: Option<String>,
    pub recorded: Option<String>,
}

/// The UI library version check.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Libraries {
    pub missing: bool,
    pub recorded: BTreeMap<String, String>,
    pub mismatch: Vec<Mismatch>,
}

/// The broker's projection of the newest scan summary.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Report {
    pub available: bool,
    pub reason: Option<String>,
    pub status: Option<String>,
    pub scanned_at: Option<f64>,
    pub checked_at: Option<f64>,
    pub failed_at: Option<f64>,
    pub error: Option<String>,
    pub release: Option<String>,
    #[serde(default)]
    pub targets: BTreeMap<String, Target>,
    pub ui_libraries: Option<Libraries>,
}

fn word(value: &str) -> bool {
    (1..=128).contains(&value.len())
        && value
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphanumeric())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._:+~@/, -".contains(&b))
}

fn target_name(value: &str) -> bool {
    (1..=32).contains(&value.len())
        && value.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

fn time(value: Option<f64>) -> bool {
    value.is_none_or(|t| t.is_finite() && (0.0..1e11).contains(&t))
}

fn counts(value: &BTreeMap<String, u64>) -> bool {
    value
        .iter()
        .all(|(key, n)| SEVERITIES.contains(&key.as_str()) && *n <= 10_000_000)
}

/// Parse and check the broker output.
///
/// # Errors
/// Oversized output, an unknown field, or any value outside its bounds.
pub fn parse(output: &Value) -> anyhow::Result<Report> {
    if serde_json::to_vec(output)?.len() > MAX_BYTES {
        anyhow::bail!("Scan report too large");
    }
    let report: Report = serde_json::from_value(output.clone())?;
    let reason_ok = report
        .reason
        .as_deref()
        .is_none_or(|r| matches!(r, "missing" | "malformed" | "too_large" | "refused"));
    let status_ok = report
        .status
        .as_deref()
        .is_none_or(|s| matches!(s, "ok" | "failed"));
    let release_ok = report
        .release
        .as_deref()
        .is_none_or(|r| r.len() == 36 && r.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-'));
    let error_ok = report
        .error
        .as_deref()
        .is_none_or(|e| e.len() <= 800 && !e.chars().any(char::is_control));
    let findings: usize = report
        .targets
        .values()
        .map(|t| t.fixable_high_critical.len())
        .sum();
    let targets_ok = report.targets.len() <= MAX_TARGETS
        && findings <= MAX_FINDINGS
        && report.targets.iter().all(|(name, target)| {
            target_name(name)
                && counts(&target.by_severity)
                && counts(&target.fixable_by_severity)
                && target.fixable_not_shown <= 10_000_000
                && target.fixable_high_critical.iter().all(|f| {
                    word(&f.package) && word(&f.installed) && word(&f.id) && word(&f.fixed)
                })
        });
    let libraries_ok = report.ui_libraries.as_ref().is_none_or(|l| {
        l.recorded.len() <= 32
            && l.mismatch.len() <= 32
            && l.recorded.iter().all(|(k, v)| word(k) && word(v))
            && l.mismatch.iter().all(|m| {
                word(&m.library)
                    && m.header.as_deref().is_none_or(word)
                    && m.recorded.as_deref().is_none_or(word)
            })
    });
    if !(reason_ok
        && status_ok
        && release_ok
        && error_ok
        && targets_ok
        && libraries_ok
        && time(report.scanned_at)
        && time(report.checked_at)
        && time(report.failed_at))
    {
        anyhow::bail!("Scan report refused");
    }
    Ok(report)
}

/// Where the scan stands for the card and the alerts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// A recent, successful scan.
    Ok,
    /// The scanner recorded a failure, or its report is missing or unreadable.
    Failed,
    /// The newest scan is older than `stale_days`, or no scanner run for `missed_check_hours`.
    Stale,
}

/// Thresholds from the live `admin-alerts.json` `supply_chain` section.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Thresholds {
    pub stale_days: u64,
    pub missed_check_hours: u64,
}

/// The report's state at `now` (seconds since the epoch).
#[must_use]
pub fn state(report: &Report, limits: Thresholds, now: f64) -> State {
    if !report.available || report.status.as_deref() == Some("failed") {
        return State::Failed;
    }
    let Some(scanned) = report.scanned_at else {
        return State::Failed;
    };
    #[allow(clippy::cast_precision_loss)]
    let (stale, missed) = (
        (limits.stale_days * 86_400) as f64,
        (limits.missed_check_hours * 3_600) as f64,
    );
    let checked = report.checked_at.unwrap_or(scanned).max(scanned);
    if now - scanned > stale || now - checked > missed {
        State::Stale
    } else {
        State::Ok
    }
}

/// Content-free fingerprint of one finding: the same package, version and identifier in two
/// images is one finding (one base-image refresh fixes both).
#[must_use]
pub fn fingerprint(finding: &Finding) -> String {
    let digest = Sha256::digest(
        format!("{}\0{}\0{}", finding.package, finding.installed, finding.id).as_bytes(),
    );
    hex::encode(&digest[..16])
}

fn current(report: &Report) -> BTreeSet<String> {
    report
        .targets
        .values()
        .flat_map(|t| t.fixable_high_critical.iter().map(fingerprint))
        .collect()
}

/// What the watch has already alerted on (stored as one memory row).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Memory {
    /// Fingerprints of fixable high/critical findings already alerted.
    pub findings: BTreeSet<String>,
    /// The problem state already alerted: `failed`, `stale` or none.
    pub problem: Option<String>,
}

impl Memory {
    /// The stored row; an unreadable row is empty memory (at worst one repeated alert).
    #[must_use]
    pub fn from_value(value: Option<&Value>) -> Self {
        let Some(value) = value else {
            return Self::default();
        };
        Self {
            findings: value["findings"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .filter(|f| f.len() == 32)
                .take(MAX_FINDINGS)
                .map(str::to_owned)
                .collect(),
            problem: value["problem"]
                .as_str()
                .filter(|p| matches!(*p, "failed" | "stale"))
                .map(str::to_owned),
        }
    }

    /// The row to store.
    #[must_use]
    pub fn to_value(&self) -> Value {
        json!({"findings":self.findings,"problem":self.problem})
    }
}

/// Alerts one watch pass raises, and the memory to store afterwards.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decision {
    /// New fixable high/critical findings (0: no fixable alert).
    pub new_findings: usize,
    /// Raise the failed-scan alert.
    pub failed: bool,
    /// Raise the stale-scan alert.
    pub stale: bool,
    /// One line for the raised alert: the fixable counts by severity and the most affected
    /// packages, or why the scan counts as failed. Package names are checked words.
    pub detail: Option<String>,
    pub memory: Memory,
}

/// The fixable summary: exact counts by severity across the scanned images, how many are not
/// listed, and the (at most three) packages with the most listed findings.
fn fixable_detail(report: &Report) -> String {
    let total = |severity: &str| -> u64 {
        report
            .targets
            .values()
            .filter_map(|t| t.fixable_by_severity.get(severity))
            .sum()
    };
    let mut packages: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    for finding in report
        .targets
        .values()
        .flat_map(|t| &t.fixable_high_critical)
    {
        packages
            .entry(finding.package.as_str())
            .or_default()
            .insert(fingerprint(finding));
    }
    let mut ranked: Vec<(&str, usize)> = packages.iter().map(|(p, f)| (*p, f.len())).collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let top: Vec<&str> = ranked.iter().take(3).map(|(p, _)| *p).collect();
    let hidden: u64 = report.targets.values().map(|t| t.fixable_not_shown).sum();
    let mut line = format!(
        "{} critical and {} high fixable in this scan",
        total("CRITICAL"),
        total("HIGH")
    );
    if hidden > 0 {
        line.push_str(&format!(", {hidden} not listed"));
    }
    if !top.is_empty() {
        line.push_str(&format!("; most affected: {}", top.join(", ")));
    }
    line
}

/// Why a report counts as failed, in words for the alert.
fn failed_detail(report: &Report) -> String {
    match (report.available, report.reason.as_deref()) {
        (false, Some("missing")) => "no scan report found",
        (false, Some("too_large")) => "the report is too large to read",
        (false, Some("refused")) => "the report file was refused",
        (false, _) => "the report was refused as malformed",
        (true, _) if report.status.as_deref() == Some("failed") => "the scanner recorded a failure",
        (true, _) => "the report has no scan time",
    }
    .to_owned()
}

/// Decide the alerts for `report` given what was already alerted.
///
/// - A fixable high or critical finding alerts once; it stays remembered while the scan
///   still lists it, so later scans of the same release or a restart do not repeat it.
/// - Unfixable findings (counts only) never alert.
/// - A failed or stale scan alerts once when it enters that state; a failed scan keeps the
///   remembered findings, so recovering does not repeat them.
#[must_use]
pub fn decide(report: &Report, limits: Thresholds, now: f64, memory: &Memory) -> Decision {
    let state = state(report, limits, now);
    let problem = match state {
        State::Ok => None,
        State::Failed => Some("failed".to_owned()),
        State::Stale => Some("stale".to_owned()),
    };
    let entered = problem.is_some() && problem != memory.problem;
    let (new_findings, findings) = if state == State::Failed {
        (0, memory.findings.clone())
    } else {
        let now_listed = current(report);
        (now_listed.difference(&memory.findings).count(), now_listed)
    };
    let failed = entered && state == State::Failed;
    let detail = if failed {
        Some(failed_detail(report))
    } else if new_findings > 0 {
        Some(fixable_detail(report))
    } else {
        None
    };
    Decision {
        new_findings,
        failed,
        stale: entered && state == State::Stale,
        detail,
        memory: Memory { findings, problem },
    }
}

/// The Dashboard card's view of a parsed report.
#[must_use]
pub fn view(report: &Report, limits: Thresholds, now: f64) -> Value {
    let state = match state(report, limits, now) {
        State::Ok => "ok",
        State::Failed => "failed",
        State::Stale => "stale",
    };
    let targets: Vec<Value> = report
        .targets
        .iter()
        .map(|(name, t)| {
            json!({"name":name,"by_severity":t.by_severity,"fixable_by_severity":t.fixable_by_severity,
            "fixable_not_shown":t.fixable_not_shown,
            "fixable_high_critical":t.fixable_high_critical.iter().map(|f| json!({
                "package":f.package,"installed":f.installed,"fixed":f.fixed,"id":f.id})).collect::<Vec<_>>()})
        })
        .collect();
    let libraries = report.ui_libraries.as_ref().map(|l| {
        json!({"missing":l.missing,"recorded":l.recorded,"mismatch":l.mismatch.iter().map(|m| json!({
            "library":m.library,"header":m.header,"recorded":m.recorded})).collect::<Vec<_>>()})
    });
    json!({"schema_version":1,"state":state,"available":report.available,"reason":report.reason,
    "scanned_at":report.scanned_at,"checked_at":report.checked_at,"failed_at":report.failed_at,
    "error":report.error,"release":report.release,"stale_days":limits.stale_days,
    "missed_check_hours":limits.missed_check_hours,"targets":targets,"ui_libraries":libraries})
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITS: Thresholds = Thresholds {
        stale_days: 8,
        missed_check_hours: 24,
    };
    const NOW: f64 = 1_795_441_914.0;

    fn report(findings: Value) -> Value {
        json!({"available":true,"status":"ok","scanned_at":NOW - 60.0,"checked_at":NOW - 30.0,
        "failed_at":null,"release":"9850cba2-4b43-4a2d-bd06-6a4c3b8d6e0a",
        "targets":{"api":{"by_severity":{"HIGH":1115,"CRITICAL":7},"fixable_by_severity":{"HIGH":1},
        "fixable_high_critical":findings}},
        "ui_libraries":{"missing":false,"recorded":{"marked":"12.0.0"},"mismatch":[]}})
    }

    fn apt() -> Value {
        json!([{"package":"apt","installed":"2.6.1","id":"CVE-2026-109518","fixed":"2.6.2"}])
    }

    #[test]
    fn new_fixable_finding_alerts_once_and_not_again() {
        let parsed = parse(&report(apt())).unwrap();
        let first = decide(&parsed, LIMITS, NOW, &Memory::default());
        assert_eq!(
            (first.new_findings, first.failed, first.stale),
            (1, false, false)
        );
        let stored = Memory::from_value(Some(&first.memory.to_value()));
        assert_eq!(stored, first.memory);
        let again = decide(&parsed, LIMITS, NOW + 900.0, &stored);
        assert_eq!(again.new_findings, 0);
        // The same finding in a second image is the same finding.
        let mut twice = report(apt());
        twice["targets"]["loop"] = twice["targets"]["api"].clone();
        assert_eq!(
            decide(&parse(&twice).unwrap(), LIMITS, NOW, &stored).new_findings,
            0
        );
    }

    #[test]
    fn unfixable_findings_never_alert() {
        let parsed = parse(&report(json!([]))).unwrap();
        let decision = decide(&parsed, LIMITS, NOW, &Memory::default());
        assert_eq!(
            (decision.new_findings, decision.failed, decision.stale),
            (0, false, false)
        );
    }

    #[test]
    fn failed_scan_alerts_once_and_keeps_remembered_findings() {
        let ok = decide(
            &parse(&report(apt())).unwrap(),
            LIMITS,
            NOW,
            &Memory::default(),
        );
        let mut failed = report(apt());
        failed["status"] = json!("failed");
        failed["error"] = json!("docker inspect failed");
        let parsed = parse(&failed).unwrap();
        let first = decide(&parsed, LIMITS, NOW, &ok.memory);
        assert!(first.failed && first.new_findings == 0);
        assert_eq!(first.memory.findings, ok.memory.findings);
        assert!(!decide(&parsed, LIMITS, NOW + 900.0, &first.memory).failed);
        let missing = parse(&json!({"available":false,"reason":"missing"})).unwrap();
        assert_eq!(state(&missing, LIMITS, NOW), State::Failed);
        // Recovery clears the problem without repeating the findings.
        let back = decide(&parse(&report(apt())).unwrap(), LIMITS, NOW, &first.memory);
        assert_eq!((back.new_findings, back.memory.problem), (0, None));
    }

    #[test]
    fn stale_scan_and_stopped_scanner_alert_once() {
        let mut old = report(json!([]));
        old["scanned_at"] = json!(NOW - 9.0 * 86_400.0);
        old["checked_at"] = json!(NOW - 60.0);
        let parsed = parse(&old).unwrap();
        assert_eq!(state(&parsed, LIMITS, NOW), State::Stale);
        let first = decide(&parsed, LIMITS, NOW, &Memory::default());
        assert!(first.stale && !first.failed);
        assert!(!decide(&parsed, LIMITS, NOW + 60.0, &first.memory).stale);
        let mut stopped = report(json!([]));
        stopped["scanned_at"] = json!(NOW - 2.0 * 86_400.0);
        stopped["checked_at"] = json!(NOW - 2.0 * 86_400.0);
        assert_eq!(state(&parse(&stopped).unwrap(), LIMITS, NOW), State::Stale);
        assert_eq!(view(&parsed, LIMITS, NOW)["state"], "stale");
    }

    #[test]
    fn malformed_or_oversized_reports_are_refused() {
        let mut cases = vec![
            json!([]),
            json!({"available":true,"extra":1}),
            json!({"available":"yes"}),
            json!({"available":false,"reason":"<b>"}),
        ];
        let mut bad = report(apt());
        bad["targets"]["api"]["fixable_high_critical"][0]["package"] = json!("<script>");
        cases.push(bad);
        let mut bad = report(apt());
        bad["targets"]["API"] =
            json!({"by_severity":{},"fixable_by_severity":{},"fixable_high_critical":[]});
        cases.push(bad);
        let mut bad = report(apt());
        bad["targets"]["api"]["by_severity"] = json!({"SEVERE":1});
        cases.push(bad);
        let mut bad = report(apt());
        bad["scanned_at"] = json!(-1.0);
        cases.push(bad);
        let mut bad = report(apt());
        bad["release"] = json!("../../etc");
        cases.push(bad);
        let mut bad = report(apt());
        bad["targets"]["api"]["fixable_high_critical"] =
            Value::Array(vec![apt()[0].clone(); MAX_FINDINGS + 1]);
        cases.push(bad);
        let mut bad = report(apt());
        bad["error"] = json!("x".repeat(MAX_BYTES));
        cases.push(bad);
        for case in cases {
            assert!(parse(&case).is_err(), "{case}");
        }
    }

    #[test]
    fn many_new_findings_are_one_summary_and_failures_say_why() {
        // 200 listed Chromium rows of 618 fixable: one decision, exact counts, top packages.
        let rows: Vec<Value> = (0..200)
            .map(|i| {
                let package = ["chromium", "chromium-common", "chromium-driver"][i % 3];
                json!({"package":package,"installed":"156.0.8078.79-1~deb12u1",
                "id":format!("CVE-2026-{}", 20000 + i / 3),"fixed":"156.0.8078.108-1~deb12u1"})
            })
            .collect();
        let mut value = report(Value::Array(rows));
        value["targets"]["api"]["fixable_by_severity"] = json!({"CRITICAL":6,"HIGH":612});
        value["targets"]["api"]["fixable_not_shown"] = json!(418);
        let parsed = parse(&value).unwrap();
        let decision = decide(&parsed, LIMITS, NOW, &Memory::default());
        assert_eq!(decision.new_findings, 200);
        assert_eq!(
            decision.detail.as_deref(),
            Some("6 critical and 612 high fixable in this scan, 418 not listed; most affected: chromium, chromium-common, chromium-driver")
        );
        assert_eq!(
            view(&parsed, LIMITS, NOW)["targets"][0]["fixable_not_shown"],
            418
        );
        // Seen again: nothing.
        let again = decide(&parsed, LIMITS, NOW + 900.0, &decision.memory);
        assert_eq!((again.new_findings, again.detail), (0, None));
        // A refused report alerts once, saying why, and keeps the findings remembered.
        let refused = parse(&json!({"available":false,"reason":"malformed"})).unwrap();
        let failed = decide(&refused, LIMITS, NOW, &decision.memory);
        assert!(failed.failed);
        assert_eq!(
            failed.detail.as_deref(),
            Some("the report was refused as malformed")
        );
        assert_eq!(failed.memory.findings, decision.memory.findings);
        assert!(!decide(&refused, LIMITS, NOW + 900.0, &failed.memory).failed);
        let mut bad = value.clone();
        bad["targets"]["api"]["fixable_not_shown"] = json!(-1);
        assert!(parse(&bad).is_err());
    }

    #[test]
    fn view_lists_targets_and_findings() {
        let parsed = parse(&report(apt())).unwrap();
        let card = view(&parsed, LIMITS, NOW);
        assert_eq!(card["state"], "ok");
        assert_eq!(card["targets"][0]["name"], "api");
        assert_eq!(
            card["targets"][0]["fixable_high_critical"][0]["fixed"],
            "2.6.2"
        );
        assert_eq!(card["ui_libraries"]["recorded"]["marked"], "12.0.0");
    }
}
