//! Root composition of Accounts with Access: these Store methods write both owners'
//! records, so they stay here until Access has its own host crate.
//! Accounts-only storage lives in `webagent_accounts_host::AccountsStore`.
use crate::store::Store;
use anyhow::Result;
use webagent_accounts_host::AccountsStore;
impl Store {
    pub async fn init_accounts(&self) -> Result<()> {
        self.accounts_repository().initialize().await?;
        self.access_repository()
            .initialize_authorization_tables()
            .await?;
        self.accounts_repository().import_original_admin().await?;
        self.access_repository().migrate_access_v1().await
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
    pub(crate) async fn init_access_policy(&self) -> Result<()> {
        // The live policy (a checkout-relative path) always exists; fixtures may lack theirs.
        let path = self.access_policy_path();
        if path.is_relative() || path.exists() {
            self.read_access_policy()?;
            return Ok(());
        }
        let legacy = self.get("auth:registration_policy").await?;
        self.access_repository().init_access_policy(legacy.as_ref())
    }
    pub(crate) fn set_registration_policy(&self, registration: &str) -> Result<()> {
        self.access_repository()
            .set_registration_policy(registration)
    }
    pub(crate) fn set_registration_policy_revision(
        &self,
        registration: &str,
        expected: Option<u64>,
    ) -> Result<()> {
        self.access_repository()
            .set_registration_policy_revision(registration, expected)
    }
}
