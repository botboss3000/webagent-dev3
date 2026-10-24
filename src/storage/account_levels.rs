//! Preserve guest identity while assigning the installation's Anonymous ceiling.
use super::*;
impl Store {
    pub(crate) async fn reconcile_guest_account_levels(&self)->Result<()> {
        let marker="migration:guest_account_levels_v1";
        let mut tx=self.app_pool.begin().await?;
        sqlx::query("UPDATE docs SET value=value WHERE key=?").bind(marker).execute(&mut *tx).await?;
        let done:Option<Vec<u8>>=sqlx::query_scalar("SELECT value FROM docs WHERE key=?").bind(marker).fetch_optional(&mut *tx).await?;
        if done.is_some(){tx.commit().await?;return Ok(());}
        let assignment=crate::access::initial_guest_access();
        self.accounts_repository().initialize_tier_assignment_in(&mut tx,assignment.role,assignment.tier_id).await?;
        sqlx::query("INSERT INTO docs(key,value) VALUES(?,?)").bind(marker).bind(self.seal(&json!({"version":1}))?).execute(&mut *tx).await?;
        tx.commit().await?;Ok(())
    }
}
