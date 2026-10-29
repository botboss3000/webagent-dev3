//! Installation-owned startup service configuration, separate from agent ability grants.
//! Reads go to the encrypted application plane every time; a caller's store or cache
//! cannot route this policy.
use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use webagent_access::Principal;
use webagent_accounts_host::AccountsStore;
use webagent_host_core::{now, StoreCore};

/// The service a stopped-run resume setting belongs to.
pub const BOOT_ORPHAN_RESUME: &str = "boot_orphan_resume";
const CONFIG_KEY: &str = "service:boot_orphan_resume";

/// Whether startup resumes runs orphaned by a restart, and its revision.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceConfig {
    pub enabled: bool,
    pub revision: u64,
}

impl Default for ServiceConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            revision: 0,
        }
    }
}

fn decode(store: &StoreCore, blob: Option<Vec<u8>>) -> Result<ServiceConfig> {
    let Some(blob) = blob else {
        return Ok(ServiceConfig::default());
    };
    let config: ServiceConfig = serde_json::from_value(store.unseal(&blob)?)?;
    if config.revision == 0 {
        bail!("Invalid service configuration revision");
    }
    Ok(config)
}

/// The saved setting, or the default before the first save.
pub async fn boot_orphan_resume_config(store: &StoreCore) -> Result<ServiceConfig> {
    let blob: Option<Vec<u8>> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
        .bind(CONFIG_KEY)
        .fetch_optional(&store.app_pool)
        .await?;
    decode(store, blob)
}

/// Whether startup resumes orphaned runs. Only startup recovery reads this; uncertain-effect
/// classification and expired-lease recovery stay mandatory and independent.
pub async fn startup_resume_enabled(store: &StoreCore) -> Result<bool> {
    Ok(boot_orphan_resume_config(store).await?.enabled)
}

/// Save the setting if it is still at `expected_revision`. Only an installation
/// administrator may, and the change and its audit event commit together.
pub async fn save_boot_orphan_resume(
    store: &StoreCore,
    actor: &str,
    enabled: bool,
    expected_revision: u64,
) -> Result<ServiceConfig> {
    if !webagent_access::valid_id(actor) {
        bail!("Installation administrator required");
    }
    let mut tx = store.app_pool.begin().await?;
    // Even an absent first-start row takes SQLite's writer lock before the read, so
    // concurrent creators cannot both accept revision zero.
    sqlx::query("UPDATE docs SET value=value WHERE key=?")
        .bind(CONFIG_KEY)
        .execute(&mut *tx)
        .await?;
    let account = store
        .accounts_repository()
        .account_state_in(&mut tx, actor)
        .await?
        .ok_or_else(|| anyhow!("Installation administrator required"))?;
    let principal = Principal::from_account(&account)?;
    if principal.user_id != actor || !principal.administrator() {
        bail!("Installation administrator required");
    }
    let blob: Option<Vec<u8>> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
        .bind(CONFIG_KEY)
        .fetch_optional(&mut *tx)
        .await?;
    let previous = decode(store, blob)?;
    if previous.revision != expected_revision {
        bail!("Service revision changed");
    }
    let revision = expected_revision
        .checked_add(1)
        .ok_or_else(|| anyhow!("Service revision overflow"))?;
    let next = ServiceConfig { enabled, revision };
    sqlx::query(
        "INSERT INTO docs(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
    )
    .bind(CONFIG_KEY)
    .bind(store.seal(&serde_json::to_value(&next)?)?)
    .execute(&mut *tx)
    .await?;
    // The exact authenticated actor and the configuration commit together. No tool,
    // tenant administrator or metadata edit may appoint installation authority.
    let audit = json!({"service_id":BOOT_ORPHAN_RESUME,"actor_id":actor,
        "revision":next.revision,"enabled":enabled,"previous_enabled":previous.enabled,
        "changed_at":now(),"applies":"next_loop_startup"});
    sqlx::query(
        "INSERT INTO events(session_id,kind,payload) VALUES('','service_config_changed',?)",
    )
    .bind(store.seal(&audit)?)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(next)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use serde_json::Value;
    use std::path::Path;
    use webagent_host_core::test_support::store_core;

    /// A store with the installation administrator `admin` and one member.
    async fn store(root: &Path) -> (StoreCore, String) {
        let store = store_core(root, "admin").await;
        let accounts = store.accounts_repository();
        accounts.initialize().await.unwrap();
        let mut tx = store.app_pool.begin().await.unwrap();
        accounts
            .create_admin_in(
                &mut tx,
                "admin",
                "fixture-password-hash",
                "fixture-generation",
                webagent_access::initial_admin_access(),
            )
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let member = store
            .create_user_account("service-member", "long-fixture-password-hash")
            .await
            .unwrap();
        (store, member["user_id"].as_str().unwrap().to_owned())
    }

    async fn audits(store: &StoreCore) -> Vec<Value> {
        let rows: Vec<Vec<u8>> =
            sqlx::query_scalar("SELECT payload FROM events WHERE kind='service_config_changed'")
                .fetch_all(&store.app_pool)
                .await
                .unwrap();
        rows.iter().map(|row| store.unseal(row).unwrap()).collect()
    }

    #[tokio::test]
    async fn default_cas_and_audit_are_durable_and_installation_owned() {
        let root = tempfile::tempdir().unwrap();
        let (store, member) = store(root.path()).await;
        assert_eq!(
            boot_orphan_resume_config(&store).await.unwrap(),
            ServiceConfig::default()
        );
        assert!(startup_resume_enabled(&store).await.unwrap());
        assert!(save_boot_orphan_resume(&store, &member, false, 0)
            .await
            .is_err());
        assert_eq!(boot_orphan_resume_config(&store).await.unwrap().revision, 0);
        let saved = save_boot_orphan_resume(&store, "admin", false, 0)
            .await
            .unwrap();
        assert_eq!(
            saved,
            ServiceConfig {
                enabled: false,
                revision: 1
            }
        );
        assert_eq!(boot_orphan_resume_config(&store).await.unwrap(), saved);
        assert!(!startup_resume_enabled(&store).await.unwrap());
        assert!(save_boot_orphan_resume(&store, "admin", true, 0)
            .await
            .is_err());
        let audits = audits(&store).await;
        assert_eq!(audits.len(), 1);
        assert_eq!(audits[0]["actor_id"], "admin");
        assert_eq!(audits[0]["enabled"], false);
        assert_eq!(audits[0]["revision"], 1);
        assert_eq!(audits[0]["previous_enabled"], true);
        let user_rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM docs WHERE key=?")
            .bind(CONFIG_KEY)
            .fetch_one(&store.pool)
            .await
            .unwrap();
        assert_eq!(
            user_rows, 0,
            "the setting lives in the application plane only"
        );
        let reopened = store_core(root.path(), "admin").await;
        assert_eq!(boot_orphan_resume_config(&reopened).await.unwrap(), saved);
    }

    #[tokio::test]
    async fn concurrent_first_start_writes_have_one_winner_and_one_audit() {
        let root = tempfile::tempdir().unwrap();
        let (store, _) = store(root.path()).await;
        let (a, b) = tokio::join!(
            save_boot_orphan_resume(&store, "admin", false, 0),
            save_boot_orphan_resume(&store, "admin", true, 0)
        );
        assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
        let loser = a.err().or(b.err()).unwrap();
        assert_eq!(loser.to_string(), "Service revision changed");
        assert_eq!(boot_orphan_resume_config(&store).await.unwrap().revision, 1);
        assert_eq!(audits(&store).await.len(), 1);
    }

    #[tokio::test]
    async fn invalid_configuration_never_defaults_to_automatic_resume() {
        let root = tempfile::tempdir().unwrap();
        let (store, _) = store(root.path()).await;
        for config in [
            json!({"enabled":true,"revision":0}),
            json!({"enabled":"true","revision":1}),
            json!({"enabled":true,"revision":1,"unknown":"field"}),
        ] {
            sqlx::query("INSERT INTO docs(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value")
                .bind(CONFIG_KEY)
                .bind(store.seal(&config).unwrap())
                .execute(&store.app_pool)
                .await
                .unwrap();
            assert!(startup_resume_enabled(&store).await.is_err());
        }
    }
}
