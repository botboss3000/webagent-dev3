//! Preserve guest identity while assigning the installation's Anonymous ceiling.
use super::*;
impl Store {
    pub(crate) async fn reconcile_guest_account_levels(&self) -> Result<()> {
        let marker = "migration:guest_account_levels_v1";
        let mut tx = self.app_pool.begin().await?;
        sqlx::query("UPDATE docs SET value=value WHERE key=?")
            .bind(marker)
            .execute(&mut *tx)
            .await?;
        let done: Option<Vec<u8>> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
            .bind(marker)
            .fetch_optional(&mut *tx)
            .await?;
        if done.is_some() {
            tx.commit().await?;
            return Ok(());
        }
        let assignment = crate::access::initial_guest_access();
        self.accounts_repository()
            .initialize_tier_assignment_in(&mut tx, assignment.role, assignment.tier_id)
            .await?;
        sqlx::query("INSERT INTO docs(key,value) VALUES(?,?)")
            .bind(marker)
            .bind(self.seal(&json!({"version":1}))?)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }
    /// One test account per signed-in tier of the live policy (never administrator; the
    /// anonymous tier's test visitor is a fresh guest per sign-in). Idempotent; a tier whose
    /// reserved username a person already holds is left alone with a warning.
    pub(crate) async fn ensure_test_accounts(&self) -> Result<()> {
        let policy = self.access_repository().read_access_policy()?;
        let tiers: Vec<String> = policy["tiers"]
            .as_object()
            .map(|tiers| tiers.keys().cloned().collect())
            .unwrap_or_default();
        for tier in tiers {
            let role = crate::access::Role::for_tier(&tier);
            if role != crate::access::Role::User {
                continue;
            }
            match self
                .accounts_repository()
                .ensure_test_account(&tier, "user")
                .await
            {
                Ok(Some(_)) => {}
                Ok(None) => {
                    tracing::warn!(tier=%tier,"Test account username is held by another account; not created")
                }
                Err(error) => tracing::warn!(tier=%tier,error=%error,"Test account not created"),
            }
        }
        Ok(())
    }
}
