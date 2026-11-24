//! The once-per-account widget trial start (plain `user_accounts.widget_trial_started`); the
//! rules that use it are Access's (`webagent_access::widget_trial`).
use crate::{RecordCodec, Repository};
use anyhow::{anyhow, Result};

impl<C: RecordCodec> Repository<'_, C> {
    /// When the account's widget trial started (Unix seconds); `None`: never, or no account.
    pub async fn widget_trial_started(&self, user_id: &str) -> Result<Option<u64>> {
        Ok(
            crate::account_rows::widget_trial_started(self.app_pool, user_id)
                .await?
                .and_then(|at| u64::try_from(at).ok()),
        )
    }

    /// Starts the account's widget trial at `at` unless it already started, and returns the
    /// stored start: the first start stays for good. Raises `auth_epoch` when it writes.
    pub async fn start_widget_trial(&self, user_id: &str, at: u64) -> Result<u64> {
        let at = i64::try_from(at).map_err(|_| anyhow!("Invalid trial start"))?;
        crate::account_rows::start_widget_trial(self.app_pool, user_id, at)
            .await?
            .and_then(|at| u64::try_from(at).ok())
            .ok_or_else(|| anyhow!("Account not found"))
    }
}
