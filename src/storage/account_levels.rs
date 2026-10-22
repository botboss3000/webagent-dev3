//! Preserve guest identity while assigning the installation's Anonymous ceiling.
use super::*;
impl Store {
    pub(crate) async fn reconcile_guest_account_levels(&self)->Result<()> {
        let marker="migration:guest_account_levels_v1";
        let mut tx=self.app_pool.begin().await?;
        sqlx::query("UPDATE docs SET value=value WHERE key=?").bind(marker).execute(&mut *tx).await?;
        let done:Option<Vec<u8>>=sqlx::query_scalar("SELECT value FROM docs WHERE key=?").bind(marker).fetch_optional(&mut *tx).await?;
        if done.is_some(){tx.commit().await?;return Ok(());}
        let rows=sqlx::query("SELECT user_id,value FROM user_accounts").fetch_all(&mut *tx).await?;
        for row in rows {
            let mut account=self.unseal(&row.get::<Vec<u8>,_>("value"))?;
            if account["role"]!="guest"||account["tier_id"]=="anonymous"{continue;}
            account["tier_id"]=json!("anonymous");
            account["access_revision"]=json!(account["access_revision"].as_u64().unwrap_or(0).checked_add(1).ok_or_else(||anyhow!("Account revision overflow"))?);
            sqlx::query("UPDATE user_accounts SET value=? WHERE user_id=?").bind(self.seal(&account)?).bind(row.get::<String,_>("user_id")).execute(&mut *tx).await?;
        }
        sqlx::query("INSERT INTO docs(key,value) VALUES(?,?)").bind(marker).bind(self.seal(&json!({"version":1}))?).execute(&mut *tx).await?;
        tx.commit().await?;Ok(())
    }
}
