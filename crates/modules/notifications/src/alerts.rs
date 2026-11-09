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
pub const LIVE_CONFIG: &str = "crates/modules/notifications/admin-alerts.json";
const MAX_BYTES: u64 = 16 * 1024;
/// Event kinds the host reports.
pub const KINDS: [&str; 3] = ["login_failed", "login_blocked", "rate_limited"];
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

/// The parsed live configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub retention_days: u64,
    pub rules: BTreeMap<String, Rule>,
    pub channels: BTreeMap<String, bool>,
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
        const KEYS: [&str; 5] = [
            "_instructions",
            "schema_version",
            "retention_days",
            "events",
            "channels",
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
        Ok(Self {
            retention_days,
            rules,
            channels,
        })
    }

    /// The live configuration; the shipped file's values when it is missing or invalid.
    #[must_use]
    pub fn live() -> Self {
        webagent_configuration::live::parsed(LIVE_CONFIG, MAX_BYTES, Self::parse)
            .unwrap_or_else(|_| Self::shipped())
    }

    fn shipped() -> Self {
        Self::parse(include_bytes!("../admin-alerts.json")).expect("shipped alert file is valid")
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
        json!({"retention_days":self.retention_days,"events":events,"channels":channels})
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

/// Create the alert table in the application pool.
pub async fn schema(pool: &SqlitePool) -> Result<()> {
    sqlx::raw_sql(
        "CREATE TABLE IF NOT EXISTS admin_alerts(id INTEGER PRIMARY KEY AUTOINCREMENT,
        kind TEXT NOT NULL,severity TEXT NOT NULL,message TEXT NOT NULL CHECK(length(message)<=512),
        count INTEGER NOT NULL,created_at INTEGER NOT NULL,read_at INTEGER);
        CREATE INDEX IF NOT EXISTS admin_alerts_created ON admin_alerts(created_at);",
    )
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
        let mut value: Value =
            serde_json::from_slice(include_bytes!("../admin-alerts.json")).unwrap();
        value["channels"]["ntfy"]["enabled"] = json!(true);
        assert!(Config::parse(&serde_json::to_vec(&value).unwrap()).is_err());
        let mut value: Value =
            serde_json::from_slice(include_bytes!("../admin-alerts.json")).unwrap();
        value["events"]["unknown"] = json!({});
        assert!(Config::parse(&serde_json::to_vec(&value).unwrap()).is_err());
        let mut value: Value =
            serde_json::from_slice(include_bytes!("../admin-alerts.json")).unwrap();
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
    fn disabled_kind_never_alerts() {
        let mut config = config();
        config.rules.get_mut("login_blocked").unwrap().notify = false;
        assert_eq!(Counter::default().hit(&config, "login_blocked", 1), None);
        assert_eq!(Counter::default().hit(&config, "not_a_kind", 1), None);
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
