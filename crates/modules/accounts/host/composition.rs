//! Application composition borrows Storage resources; Accounts and Access own their records.
use crate::store::Store;
use anyhow::Result;
use serde_json::Value;
impl Store {
    pub async fn init_accounts(&self) -> Result<()> {
        self.accounts_repository().initialize().await?;
        self.access_repository()
            .initialize_authorization_tables()
            .await?;
        self.accounts_repository().import_original_admin().await?;
        self.access_repository().migrate_access_v1().await
    }
    pub async fn has_admin_account(&self) -> Result<bool> {
        self.accounts_repository().has_admin_account().await
    }
    pub async fn account_by_id(&self, id: &str) -> Result<Option<Value>> {
        self.accounts_repository().account_state(id).await
    }
    pub async fn account_by_username(&self, name: &str) -> Result<Option<Value>> {
        self.accounts_repository()
            .account_state_by_username(name)
            .await
    }
    pub async fn user_profile(&self, id: &str) -> Result<Option<Value>> {
        self.accounts_repository().user_profile(id).await
    }
    pub async fn create_admin_account(
        &self,
        name: &str,
        hash: &str,
        generation: &str,
    ) -> Result<()> {
        let mut tx = self.app_pool.begin().await?;
        self.accounts_repository()
            .create_admin_in(
                &mut tx,
                name,
                hash,
                generation,
                crate::access::initial_admin_access(),
            )
            .await?;
        self.access_repository()
            .bootstrap_admin_membership_in(&mut tx)
            .await?;
        tx.commit().await?;
        self.access_repository().migrate_access_v1().await
    }
}
