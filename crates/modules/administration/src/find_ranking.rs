//! `file_search` `find` ranking settings, read live from `find-ranking.json` in the tool's
//! folder so administrators can tune ranking without a release.
use anyhow::{bail, Result};
use serde_json::{json, Value};

/// Live settings file, relative to the checkout.
pub const LIVE_RANKING: &str =
    "abilities/Administrator/codebase_admin/tools/file_search/find-ranking.json";
const MAX_BYTES: u64 = 16 * 1024;

/// How `find` scans, keeps and blends candidates, and how long its log keeps rows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ranking {
    /// Share of a block's score from its word match; the matcher's meaning score has the rest.
    pub words_weight: f64,
    /// Blocks shown to the agent.
    pub shown_blocks: usize,
    /// Most candidates one area keeps for the matcher.
    pub candidates_per_area: u64,
    /// Most candidates all areas keep together; each costs one matcher text.
    pub total_candidates: u64,
    /// Files and folders one area's scan may visit.
    pub scan_entries: u64,
    /// Seconds one area's scan may take.
    pub scan_seconds: u64,
    /// Days a `find` log row is kept.
    pub log_days: u64,
}

impl Default for Ranking {
    /// Used when the live file cannot be read.
    fn default() -> Self {
        Self {
            words_weight: 0.2,
            shown_blocks: 8,
            candidates_per_area: 200,
            total_candidates: 400,
            scan_entries: 10_000,
            scan_seconds: 6,
            log_days: 90,
        }
    }
}

fn bounded(value: &Value, key: &str, min: u64, max: u64) -> Result<u64> {
    match value.get(key).and_then(Value::as_u64) {
        Some(number) if (min..=max).contains(&number) => Ok(number),
        _ => bail!("{key} must be an integer from {min} to {max}"),
    }
}

impl Ranking {
    /// Parses and bounds the live file; unknown keys are refused.
    ///
    /// # Errors
    /// Invalid JSON, an unknown key or a value out of range.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let value: Value = serde_json::from_slice(bytes)?;
        let Some(object) = value.as_object() else {
            bail!("Find ranking must be an object");
        };
        const KEYS: [&str; 9] = [
            "_instructions",
            "schema_version",
            "words_weight",
            "shown_blocks",
            "candidates_per_area",
            "total_candidates",
            "scan_entries",
            "scan_seconds",
            "log_days",
        ];
        if object.keys().any(|key| !KEYS.contains(&key.as_str())) {
            bail!("Unknown find ranking setting");
        }
        if value["schema_version"] != 1 {
            bail!("schema_version must be 1");
        }
        let words_weight = match value["words_weight"].as_f64() {
            Some(weight) if (0.0..=1.0).contains(&weight) => weight,
            _ => bail!("words_weight must be a number from 0 to 1"),
        };
        let candidates_per_area = bounded(&value, "candidates_per_area", 1, 1000)?;
        Ok(Self {
            words_weight,
            shown_blocks: usize::try_from(bounded(&value, "shown_blocks", 1, 30)?)?,
            candidates_per_area,
            total_candidates: bounded(&value, "total_candidates", candidates_per_area, 2000)?,
            scan_entries: bounded(&value, "scan_entries", 100, 100_000)?,
            scan_seconds: bounded(&value, "scan_seconds", 1, 30)?,
            log_days: bounded(&value, "log_days", 1, 3650)?,
        })
    }

    /// The live settings; the defaults when the file is missing or invalid.
    #[must_use]
    pub fn load() -> Self {
        let path = webagent_configuration::paths::checkout().join(LIVE_RANKING);
        std::fs::metadata(&path)
            .ok()
            .filter(|meta| meta.len() <= MAX_BYTES)
            .and_then(|_| std::fs::read(&path).ok())
            .and_then(|bytes| Self::parse(&bytes).ok())
            .unwrap_or_default()
    }

    /// The scan budgets the worker takes as `find_limits`.
    #[must_use]
    pub fn worker_limits(&self) -> Value {
        json!({"candidates_per_area": self.candidates_per_area,
            "total_candidates": self.total_candidates,
            "scan_entries": self.scan_entries, "scan_seconds": self.scan_seconds})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_live_file_parses_and_matches_the_defaults() {
        let bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../abilities/Administrator/codebase_admin/tools/file_search/find-ranking.json"
        ))
        .unwrap();
        assert_eq!(Ranking::parse(&bytes).unwrap(), Ranking::default());
    }

    #[test]
    fn out_of_range_and_unknown_values_are_refused() {
        let base = json!({"schema_version": 1, "words_weight": 0.2, "shown_blocks": 8,
            "candidates_per_area": 200, "total_candidates": 400, "scan_entries": 10000,
            "scan_seconds": 6, "log_days": 90});
        assert!(Ranking::parse(base.to_string().as_bytes()).is_ok());
        for (key, bad) in [
            ("words_weight", json!(1.5)),
            ("total_candidates", json!(100)),
            ("other", json!(1)),
        ] {
            let mut value = base.clone();
            value[key] = bad;
            assert!(
                Ranking::parse(value.to_string().as_bytes()).is_err(),
                "{key}"
            );
        }
    }
}
