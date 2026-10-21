use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use sha2::Digest;
use std::path::Path;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub version: u32,
    pub enabled: bool,
    pub capture_scope: String,
    pub categories: Categories,
    pub assembly_successes: bool,
    pub retention: Retention,
    pub full_context: FullContext,
    #[serde(default)]
    pub background_issues: BackgroundIssues,
}
/// Background capture is independently selectable; toast visibility never disables logging.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackgroundIssues {
    pub enabled: bool,
    pub show_toasts: bool,
}
impl Default for BackgroundIssues {
    fn default() -> Self {
        Self {
            enabled: true,
            show_toasts: true,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Categories {
    pub provider: bool,
    pub context_assembly: bool,
    #[serde(default = "category_default")]
    pub run: bool,
    #[serde(default = "category_default")]
    pub tool: bool,
    #[serde(default = "category_default")]
    pub recovery: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Retention {
    pub hours: u64,
    pub max_rows: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FullContext {
    pub enabled: bool,
    pub sessions: Vec<Session>,
    pub retention_hours: u64,
    pub max_snapshot_bytes: usize,
    pub max_retained_bytes: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Session {
    pub session_id: String,
    pub expires_at: String,
}
fn category_default() -> bool {
    true
}
impl Config {
    pub fn disabled() -> Self {
        // Fail-closed state, not installation defaults or a last-good configuration.
        Self { version:1, enabled:false, capture_scope:"fixed_admin".into(),
            categories:Categories {provider:true,context_assembly:true,run:true,tool:true,recovery:true},
            assembly_successes:false, retention:Retention {hours:168,max_rows:200000},
            full_context:FullContext {enabled:false,sessions:Vec::new(),retention_hours:24,max_snapshot_bytes:16777216,max_retained_bytes:268435456},
            background_issues:BackgroundIssues::default() }
    }
    pub fn read(path: &Path) -> Result<(Self, String)> {
        use std::io::Read;
        let mut raw = Vec::new();
        std::fs::File::open(path)?
            .take(65537)
            .read_to_end(&mut raw)?;
        if raw.len() > 65536 {
            bail!("Diagnostics configuration too large");
        }
        let config: Self = serde_json::from_slice(&raw)?;
        config.validate()?;
        Ok((
            config,
            hex::encode(sha2::Sha256::digest(serde_json::to_vec(&raw)?)),
        ))
    }
    pub fn validate(&self) -> Result<()> {
        if self.version != 1
            || self.capture_scope != "fixed_admin"
            || self.retention.hours == 0
            || self.retention.hours > 87600
            || self.retention.max_rows == 0
            || self.retention.max_rows > 2000000
            || self.full_context.retention_hours == 0
            || self.full_context.retention_hours > 87600
            || self.full_context.max_snapshot_bytes == 0
            || self.full_context.max_snapshot_bytes > 64 * 1024 * 1024
            || self.full_context.max_retained_bytes == 0
            || self.full_context.max_retained_bytes > i64::MAX as u64
            || self.full_context.sessions.len() > 128
        {
            bail!("Invalid diagnostics configuration");
        }
        let mut seen = std::collections::HashSet::new();
        for entry in &self.full_context.sessions {
            if uuid::Uuid::parse_str(&entry.session_id).is_err()
                || !seen.insert(&entry.session_id)
                || expiry(&entry.expires_at).is_none()
            {
                bail!("Invalid diagnostics session window");
            }
        }
        Ok(())
    }
    pub fn active(&self) -> bool {
        self.enabled && std::env::var("V3_DIAGNOSTICS_ENABLED").as_deref() != Ok("false")
    }
    pub fn selected(&self, sid: &str, now: u64) -> bool {
        self.active()
            && self.categories.context_assembly
            && self.full_context.enabled
            && self
                .full_context
                .sessions
                .iter()
                .any(|s| s.session_id == sid && expiry(&s.expires_at).is_some_and(|t| t > now))
    }
}
// Fixed RFC3339 UTC seconds; no additional clock/parser dependency.
pub fn expiry(text: &str) -> Option<u64> {
    let b = text.as_bytes();
    if b.len() != 20
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || b[19] != b'Z'
    {
        return None;
    }
    let parse = |a, b| text.get(a..b)?.parse::<i64>().ok();
    let y = parse(0, 4)?;
    let m = parse(5, 7)?;
    let d = parse(8, 10)?;
    let h = parse(11, 13)?;
    let min = parse(14, 16)?;
    let s = parse(17, 19)?;
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if !(1970..=9999).contains(&y)
        || !(1..=12).contains(&m)
        || d < 1
        || d > days[(m - 1) as usize]
        || !(0..24).contains(&h)
        || !(0..60).contains(&min)
        || !(0..60).contains(&s)
    {
        return None;
    }
    let yy = y - i64::from(m <= 2);
    let era = yy / 400;
    let yo = yy - era * 400;
    let mp = m + if m > 2 { -3 } else { 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    Some(
        ((era * 146097 + yo * 365 + yo / 4 - yo / 100 + doy - 719468) * 86400
            + h * 3600
            + min * 60
            + s) as u64,
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scope_defaults_and_windows_are_strict() {
        let mut c = Config::disabled();
        assert!(!c.active());
        c.validate().unwrap();
        c.capture_scope = "all".into();
        assert!(c.validate().is_err());
        assert_eq!(expiry("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(expiry("2026-10-09T00:00:00Z"), Some(1791504000));
        assert!(expiry("2026-02-29T00:00:00Z").is_none());
        assert!(expiry("2026-10-09T00:00:00+01:00").is_none());
    }
}
