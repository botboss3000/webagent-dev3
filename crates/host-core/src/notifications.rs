//! `StoreCore` entry points for metadata-only lifecycle notifications. Every module stages
//! notifications inside its own write transaction, so these methods live on the shared
//! handle; the rules and SQL are in `webagent_notifications`, and this adapter supplies the
//! handle's identity, pools and record codec.
use crate::StoreCore;
use anyhow::Result;
use serde_json::Value;
use sqlx::{Sqlite, Transaction};
use webagent_notifications::staging::Notice;
use webagent_notifications::{outbox, Source};

impl StoreCore {
    fn notification_source(&self) -> Source<'_> {
        Source {
            user_id: &self.user_id,
            agent_id: &self.agent_id,
        }
    }

    /// Stage a notification inside the lifecycle owner's transaction `tx`. Called only from
    /// that owner's transaction; no caller payload is copied.
    pub async fn stage_notification(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        event_key: &str,
        kind: &str,
        sid: &str,
        rid: Option<&str>,
        status: Option<&str>,
    ) -> Result<()> {
        let notice = Notice {
            event_key,
            kind,
            session_id: sid,
            run_id: rid,
            status,
        };
        outbox::stage(tx, &self.notification_source(), &notice, |raw| {
            self.unseal(raw)
        })
        .await
    }

    /// Stage the notification a persisted `run` checkpoint produces, if any, inside `tx`.
    pub async fn stage_run_notification(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        run: &Value,
    ) -> Result<()> {
        outbox::stage_run(tx, &self.notification_source(), run, |raw| self.unseal(raw)).await
    }

    /// Relay this handle's committed staging rows into the application outbox.
    pub async fn relay_notifications(&self) -> Result<usize> {
        outbox::relay(&self.app_pool, &self.pool, &self.user_id).await
    }

    /// Apply outbox retention as of `now`.
    pub async fn prune_notifications(&self, now: i64) -> Result<usize> {
        outbox::prune(&self.app_pool, now).await
    }

    /// One outbox page after `since`; see `webagent_notifications::outbox::page`.
    pub async fn notification_page(&self, since: i64, limit: i64) -> Result<Value> {
        outbox::page(&self.app_pool, since, limit).await
    }
}
