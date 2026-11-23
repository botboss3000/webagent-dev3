//! Administrator alerts: security events counted against the live `admin-alerts.json` and
//! raised as content-free rows the administrator Notifications page lists.
//!
//! Counts live in process memory per event kind; only a raised alert is written.
use anyhow::{bail, Result};
use serde_json::{json, Value};
use sqlx::{Row, SqlitePool};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, OnceLock};

/// Live alert configuration, relative to the checkout.
pub const LIVE_CONFIG: &str = "config/notifications/admin-alerts.json";
const MAX_BYTES: u64 = 16 * 1024;
/// Event kinds the host reports.
/// `host_approval` is one host action waiting on the Host Admin page (raised per request).
/// The `supply_chain_*` kinds come from the dependency scan watch (administration module):
/// new high or critical findings with a fix, a failed or unreadable scan, a stale scan.
pub const KINDS: [&str; 7] = [
    "login_failed",
    "login_blocked",
    "rate_limited",
    "host_approval",
    "supply_chain_fixable",
    "supply_chain_failed",
    "supply_chain_stale",
];
/// Delivery channels; only `in_app` is implemented.
pub const CHANNELS: [&str; 4] = ["in_app", "ntfy", "email", "twilio"];
const SEVERITIES: [&str; 3] = ["info", "warning", "critical"];

/// When one event kind raises an alert.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    pub notify: bool,
    pub severity: String,
    pub threshold: u32,
    pub window_seconds: u64,
}

/// When the dependency scan watch reads the scan summary and when it calls a scan stale.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SupplyChain {
    /// A scan older than this many days is stale (the scanner rescans at least weekly).
    pub stale_days: u64,
    /// No scanner run recorded for this many hours is stale (its timer runs every 15 min).
    pub missed_check_hours: u64,
    /// Seconds between the watch's reads of the summary.
    pub check_seconds: u64,
}

/// The parsed live configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub retention_days: u64,
    pub rules: BTreeMap<String, Rule>,
    pub channels: BTreeMap<String, bool>,
    pub supply_chain: SupplyChain,
}

fn object<'a>(value: &'a Value, what: &str) -> Result<&'a serde_json::Map<String, Value>> {
    value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("{what} must be an object"))
}

fn bounded(value: &Value, key: &str, min: u64, max: u64) -> Result<u64> {
    match value.get(key).and_then(Value::as_u64) {
        Some(number) if (min..=max).contains(&number) => Ok(number),
        _ => bail!("{key} must be an integer from {min} to {max}"),
    }
}

impl Config {
    /// Parses and bounds the live file; unknown keys, kinds and channels are refused.
    ///
    /// # Errors
    /// Invalid JSON, an unknown key, a value out of range or an unavailable channel enabled.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let value: Value = serde_json::from_slice(bytes)?;
        let top = object(&value, "Alert configuration")?;
        const KEYS: [&str; 6] = [
            "_instructions",
            "schema_version",
            "retention_days",
            "events",
            "channels",
            "supply_chain",
        ];
        if top.keys().any(|key| !KEYS.contains(&key.as_str())) {
            bail!("Unknown alert setting");
        }
        if value["schema_version"] != 1 {
            bail!("schema_version must be 1");
        }
        let retention_days = bounded(&value, "retention_days", 1, 365)?;
        let mut rules = BTreeMap::new();
        for (kind, rule) in object(&value["events"], "events")? {
            if !KINDS.contains(&kind.as_str()) {
                bail!("Unknown alert event {kind}");
            }
            let fields = object(rule, kind)?;
            const RULE_KEYS: [&str; 4] = ["notify", "severity", "threshold", "window_seconds"];
            if fields.keys().any(|key| !RULE_KEYS.contains(&key.as_str())) {
                bail!("Unknown setting in {kind}");
            }
            let Some(notify) = rule["notify"].as_bool() else {
                bail!("{kind}.notify must be true or false");
            };
            let severity = rule["severity"].as_str().unwrap_or_default();
            if !SEVERITIES.contains(&severity) {
                bail!("{kind}.severity must be info, warning or critical");
            }
            rules.insert(
                kind.clone(),
                Rule {
                    notify,
                    severity: severity.to_owned(),
                    threshold: u32::try_from(bounded(rule, "threshold", 1, 10_000)?)?,
                    window_seconds: bounded(rule, "window_seconds", 60, 86_400)?,
                },
            );
        }
        let mut channels = BTreeMap::new();
        for (name, channel) in object(&value["channels"], "channels")? {
            if !CHANNELS.contains(&name.as_str()) {
                bail!("Unknown alert channel {name}");
            }
            if object(channel, name)?.keys().any(|key| key != "enabled") {
                bail!("Unknown setting in channel {name}");
            }
            let Some(enabled) = channel["enabled"].as_bool() else {
                bail!("{name}.enabled must be true or false");
            };
            if enabled && name != "in_app" {
                bail!("The {name} channel is not available yet");
            }
            channels.insert(name.clone(), enabled);
        }
        let scan = object(&value["supply_chain"], "supply_chain")?;
        const SCAN_KEYS: [&str; 3] = ["stale_days", "missed_check_hours", "check_seconds"];
        if scan.keys().any(|key| !SCAN_KEYS.contains(&key.as_str())) {
            bail!("Unknown setting in supply_chain");
        }
        let supply = &value["supply_chain"];
        let supply_chain = SupplyChain {
            stale_days: bounded(supply, "stale_days", 1, 90)?,
            missed_check_hours: bounded(supply, "missed_check_hours", 1, 720)?,
            check_seconds: bounded(supply, "check_seconds", 60, 86_400)?,
        };
        Ok(Self {
            retention_days,
            rules,
            channels,
            supply_chain,
        })
    }

    /// The live configuration; the shipped file's values when it is missing or invalid.
    #[must_use]
    pub fn live() -> Self {
        webagent_configuration::live::parsed(LIVE_CONFIG, MAX_BYTES, Self::parse)
            .unwrap_or_else(|_| Self::shipped())
    }

    fn shipped() -> Self {
        Self::parse(include_bytes!(
            "../../../../config/notifications/admin-alerts.json"
        ))
        .expect("shipped alert file is valid")
    }

    /// The configuration as the Notifications page shows it.
    #[must_use]
    pub fn view(&self) -> Value {
        let events: BTreeMap<_, _> = self
            .rules
            .iter()
            .map(|(kind, rule)| {
                (
                    kind.clone(),
                    json!({"notify":rule.notify,"severity":rule.severity,
                    "threshold":rule.threshold,"window_seconds":rule.window_seconds}),
                )
            })
            .collect();
        let channels: BTreeMap<_, _> = CHANNELS
            .iter()
            .map(|name| {
                (
                    *name,
                    json!({"enabled":self.channels.get(*name).copied().unwrap_or(false),
                    "available":*name == "in_app"}),
                )
            })
            .collect();
        json!({"retention_days":self.retention_days,"events":events,"channels":channels,
        "supply_chain":{"stale_days":self.supply_chain.stale_days,
        "missed_check_hours":self.supply_chain.missed_check_hours,
        "check_seconds":self.supply_chain.check_seconds}})
    }
}

/// An alert to record: `count` events of `kind` within its window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Raised {
    pub kind: String,
    pub severity: String,
    pub count: u32,
    pub window_seconds: u64,
}

impl Raised {
    /// The page's one-line message; it names no account or address.
    #[must_use]
    pub fn message(&self) -> String {
        let minutes = self.window_seconds.div_ceil(60);
        match self.kind.as_str() {
            "login_failed" => format!("{} failed sign-ins within {minutes} min", self.count),
            "login_blocked" => format!(
                "Sign-in blocked after repeated failures ({} within {minutes} min)",
                self.count
            ),
            "rate_limited" => format!(
                "{} requests refused by rate limits within {minutes} min",
                self.count
            ),
            "host_approval" => {
                "A host action is waiting for approval on the Host Admin page".into()
            }
            "supply_chain_fixable" => format!(
                "{} new high or critical {} with a fix available in the deployed release; see the Dashboard's dependency scan card",
                self.count,
                if self.count == 1 { "finding" } else { "findings" }
            ),
            "supply_chain_failed" => {
                "The dependency scan failed or its report could not be read; see the Dashboard".into()
            }
            "supply_chain_stale" => format!(
                "The dependency scan has not run for more than {} days, or its scanner stopped checking; see the Dashboard",
                self.count
            ),
            other => format!("{} {other} events within {minutes} min", self.count),
        }
    }
}

/// Per-kind fixed windows: the start, the count and whether this window already alerted.
#[derive(Default)]
pub struct Counter(HashMap<String, (u64, u32, bool)>);

impl Counter {
    /// Count one `kind` event at `now`; the alert to raise when it reaches the threshold.
    pub fn hit(&mut self, config: &Config, kind: &str, now: u64) -> Option<Raised> {
        let rule = config.rules.get(kind).filter(|rule| rule.notify)?;
        if config.channels.get("in_app") != Some(&true) {
            return None;
        }
        let entry = self.0.entry(kind.to_owned()).or_insert((now, 0, false));
        if now.saturating_sub(entry.0) >= rule.window_seconds {
            *entry = (now, 0, false);
        }
        entry.1 = entry.1.saturating_add(1);
        if entry.2 || entry.1 < rule.threshold {
            return None;
        }
        entry.2 = true;
        Some(Raised {
            kind: kind.to_owned(),
            severity: rule.severity.clone(),
            count: entry.1,
            window_seconds: rule.window_seconds,
        })
    }
}

/// Count one event against the live configuration in the process-wide counter.
pub fn hit(kind: &str, now: u64) -> Option<Raised> {
    static COUNTER: OnceLock<Mutex<Counter>> = OnceLock::new();
    let config = Config::live();
    COUNTER
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .hit(&config, kind, now)
}

/// One alert for a single event of `kind` (no counting), when the kind and in-app delivery are on.
#[must_use]
pub fn single(config: &Config, kind: &str) -> Option<Raised> {
    let rule = config.rules.get(kind).filter(|rule| rule.notify)?;
    Some(Raised {
        kind: kind.to_owned(),
        severity: rule.severity.clone(),
        count: 1,
        window_seconds: rule.window_seconds,
    })
}

/// Deliver one raised alert through every enabled channel of the live configuration; the
/// single place a new channel (ntfy, email, SMS) is added. Only `in_app` exists, and the
/// configuration refuses enabling the others, so today this records the alert for the
/// Notifications page.
///
/// # Errors
/// The in-app record failed.
pub async fn deliver(pool: &SqlitePool, alert: &Raised, now: i64) -> Result<()> {
    for (channel, enabled) in Config::live().channels {
        if !enabled {
            continue;
        }
        match channel.as_str() {
            "in_app" => record(pool, alert, now).await?,
            // Config::parse refuses enabling a channel that has no delivery here yet.
            _ => {}
        }
    }
    Ok(())
}

/// Create the alert table in the application pool.
pub async fn schema(pool: &SqlitePool) -> Result<()> {
    sqlx::raw_sql(
        "CREATE TABLE IF NOT EXISTS admin_alerts(id INTEGER PRIMARY KEY AUTOINCREMENT,
        kind TEXT NOT NULL,severity TEXT NOT NULL,message TEXT NOT NULL CHECK(length(message)<=512),
        count INTEGER NOT NULL,created_at INTEGER NOT NULL,read_at INTEGER);
        CREATE INDEX IF NOT EXISTS admin_alerts_created ON admin_alerts(created_at);
        CREATE TABLE IF NOT EXISTS admin_alert_memory(key TEXT PRIMARY KEY CHECK(length(key)<=64),
        value TEXT NOT NULL CHECK(length(value)<=16384),updated_at INTEGER NOT NULL);",
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// What a watch already alerted on under `key` (one row per watch, replaced on each change, so it
/// never grows): fingerprints and state words only, no content.
pub async fn memory(pool: &SqlitePool, key: &str) -> Result<Option<Value>> {
    let row: Option<String> =
        sqlx::query_scalar("SELECT value FROM admin_alert_memory WHERE key=?")
            .bind(key)
            .fetch_optional(pool)
            .await?;
    Ok(row.and_then(|text| serde_json::from_str(&text).ok()))
}

/// Replace the watch memory under `key`.
pub async fn remember(pool: &SqlitePool, key: &str, value: &Value, now: i64) -> Result<()> {
    sqlx::query(
        "INSERT INTO admin_alert_memory(key,value,updated_at) VALUES(?,?,?)
        ON CONFLICT(key) DO UPDATE SET value=excluded.value,updated_at=excluded.updated_at",
    )
    .bind(key)
    .bind(value.to_string())
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}

/// Store one raised alert.
pub async fn record(pool: &SqlitePool, alert: &Raised, now: i64) -> Result<()> {
    sqlx::query(
        "INSERT INTO admin_alerts(kind,severity,message,count,created_at) VALUES(?,?,?,?,?)",
    )
    .bind(&alert.kind)
    .bind(&alert.severity)
    .bind(alert.message())
    .bind(i64::from(alert.count))
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}

/// The newest `limit` alerts and the unread count.
pub async fn list(pool: &SqlitePool, limit: i64) -> Result<Value> {
    let rows = sqlx::query(
        "SELECT id,kind,severity,message,count,created_at,read_at FROM admin_alerts
        ORDER BY id DESC LIMIT ?",
    )
    .bind(limit)
    .fetch_all(pool)
    .await?;
    let alerts: Vec<Value> = rows
        .iter()
        .map(|row| {
            json!({"id":row.get::<i64,_>("id"),"kind":row.get::<String,_>("kind"),
            "severity":row.get::<String,_>("severity"),"message":row.get::<String,_>("message"),
            "count":row.get::<i64,_>("count"),"created_at":row.get::<i64,_>("created_at"),
            "read_at":row.get::<Option<i64>,_>("read_at")})
        })
        .collect();
    let unread: i64 = sqlx::query_scalar("SELECT count(*) FROM admin_alerts WHERE read_at IS NULL")
        .fetch_one(pool)
        .await?;
    Ok(json!({"alerts":alerts,"unread":unread}))
}

/// Mark one alert, or every unread alert when `id` is `None`, as read.
pub async fn mark_read(pool: &SqlitePool, id: Option<i64>, now: i64) -> Result<u64> {
    let result = match id {
        Some(id) => {
            sqlx::query("UPDATE admin_alerts SET read_at=? WHERE id=? AND read_at IS NULL")
                .bind(now)
                .bind(id)
                .execute(pool)
                .await?
        }
        None => {
            sqlx::query("UPDATE admin_alerts SET read_at=? WHERE read_at IS NULL")
                .bind(now)
                .execute(pool)
                .await?
        }
    };
    Ok(result.rows_affected())
}

/// Delete alerts older than `retention_days`.
pub async fn prune(pool: &SqlitePool, now: i64, retention_days: u64) -> Result<u64> {
    let cutoff = now - i64::try_from(retention_days * 86_400)?;
    Ok(sqlx::query("DELETE FROM admin_alerts WHERE created_at<?")
        .bind(cutoff)
        .execute(pool)
        .await?
        .rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Config {
        Config::shipped()
    }

    #[test]
    fn shipped_file_parses() {
        let config = config();
        assert_eq!(config.retention_days, 30);
        assert_eq!(config.rules.len(), KINDS.len());
        assert_eq!(config.channels["in_app"], true);
        assert_eq!(config.view()["channels"]["ntfy"]["available"], false);
    }

    #[test]
    fn unavailable_channel_and_unknown_keys_are_refused() {
        let mut value: Value = serde_json::from_slice(include_bytes!(
            "../../../../config/notifications/admin-alerts.json"
        ))
        .unwrap();
        value["channels"]["ntfy"]["enabled"] = json!(true);
        assert!(Config::parse(&serde_json::to_vec(&value).unwrap()).is_err());
        let mut value: Value = serde_json::from_slice(include_bytes!(
            "../../../../config/notifications/admin-alerts.json"
        ))
        .unwrap();
        value["events"]["unknown"] = json!({});
        assert!(Config::parse(&serde_json::to_vec(&value).unwrap()).is_err());
        let mut value: Value = serde_json::from_slice(include_bytes!(
            "../../../../config/notifications/admin-alerts.json"
        ))
        .unwrap();
        value["events"]["login_failed"]["threshold"] = json!(0);
        assert!(Config::parse(&serde_json::to_vec(&value).unwrap()).is_err());
    }

    #[test]
    fn alerts_once_per_window_at_threshold() {
        let config = config();
        let mut counter = Counter::default();
        for _ in 0..4 {
            assert_eq!(counter.hit(&config, "login_failed", 100), None);
        }
        let raised = counter.hit(&config, "login_failed", 100).unwrap();
        assert_eq!((raised.count, raised.severity.as_str()), (5, "warning"));
        assert_eq!(raised.message(), "5 failed sign-ins within 10 min");
        assert_eq!(counter.hit(&config, "login_failed", 200), None);
        // A new window starts counting again.
        for _ in 0..4 {
            assert_eq!(counter.hit(&config, "login_failed", 700), None);
        }
        assert!(counter.hit(&config, "login_failed", 701).is_some());
    }

    #[test]
    fn host_approval_alerts_once_per_request() {
        let config = config();
        let first = single(&config, "host_approval").unwrap();
        assert_eq!((first.count, first.severity.as_str()), (1, "warning"));
        assert!(single(&config, "host_approval").is_some());
        assert!(first.message().contains("Host Admin"));
        assert_eq!(single(&config, "not_a_kind"), None);
    }

    #[test]
    fn disabled_kind_never_alerts() {
        let mut config = config();
        config.rules.get_mut("login_blocked").unwrap().notify = false;
        assert_eq!(Counter::default().hit(&config, "login_blocked", 1), None);
        assert_eq!(Counter::default().hit(&config, "not_a_kind", 1), None);
    }

    #[test]
    fn supply_chain_settings_are_bounded_and_messages_name_the_dashboard() {
        let config = config();
        assert_eq!(config.supply_chain.stale_days, 8);
        let mut value: Value = serde_json::from_slice(include_bytes!(
            "../../../../config/notifications/admin-alerts.json"
        ))
        .unwrap();
        value["supply_chain"]["stale_days"] = json!(0);
        assert!(Config::parse(&serde_json::to_vec(&value).unwrap()).is_err());
        value["supply_chain"] =
            json!({"stale_days":8,"missed_check_hours":24,"check_seconds":900,"x":1});
        assert!(Config::parse(&serde_json::to_vec(&value).unwrap()).is_err());
        let mut fixable = single(&config, "supply_chain_fixable").unwrap();
        fixable.count = 2;
        assert!(fixable
            .message()
            .starts_with("2 new high or critical findings"));
        assert!(single(&config, "supply_chain_failed")
            .unwrap()
            .message()
            .contains("failed"));
    }

    #[tokio::test]
    async fn memory_is_one_replaced_row_per_key() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        schema(&pool).await.unwrap();
        assert_eq!(memory(&pool, "supply_chain").await.unwrap(), None);
        remember(&pool, "supply_chain", &json!({"a":1}), 1)
            .await
            .unwrap();
        remember(&pool, "supply_chain", &json!({"a":2}), 2)
            .await
            .unwrap();
        assert_eq!(
            memory(&pool, "supply_chain").await.unwrap(),
            Some(json!({"a":2}))
        );
        let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM admin_alert_memory")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rows, 1);
    }

    #[tokio::test]
    async fn records_lists_marks_and_prunes() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        schema(&pool).await.unwrap();
        let mut counter = Counter::default();
        let raised = counter.hit(&config(), "login_blocked", 1).unwrap();
        record(&pool, &raised, 1_000).await.unwrap();
        record(&pool, &raised, 5_000_000).await.unwrap();
        let page = list(&pool, 50).await.unwrap();
        assert_eq!(page["unread"], 2);
        let newest = page["alerts"][0]["id"].as_i64().unwrap();
        assert_eq!(mark_read(&pool, Some(newest), 6).await.unwrap(), 1);
        assert_eq!(list(&pool, 50).await.unwrap()["unread"], 1);
        assert_eq!(mark_read(&pool, None, 7).await.unwrap(), 1);
        assert_eq!(prune(&pool, 5_000_000, 30).await.unwrap(), 1);
        assert_eq!(
            list(&pool, 50).await.unwrap()["alerts"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }
}
