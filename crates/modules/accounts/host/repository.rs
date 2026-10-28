//! Root supplies Storage resources and tenant routing; Accounts owns record SQL.
use crate::{access, Result, Store, Value};
impl webagent_accounts::RecordCodec for Store {
    fn seal(&self, value: &Value) -> Result<Vec<u8>> {
        Store::seal(self, value)
    }
    fn unseal(&self, value: &[u8]) -> Result<Value> {
        Store::unseal(self, value)
    }
}
impl Store {
    pub(crate) fn accounts_repository(&self) -> webagent_accounts::Repository<'_, Self> {
        webagent_accounts::Repository::new(&self.app_pool, self)
    }
    pub async fn all_user_ids(&self) -> Result<Vec<String>> {
        self.accounts_repository().all_user_ids().await
    }
    pub async fn create_guest_account(&self) -> Result<Value> {
        self.accounts_repository()
            .create_guest_account(access::initial_guest_access())
            .await
    }
    pub async fn create_user_account(&self, username: &str, password_hash: &str) -> Result<Value> {
        self.accounts_repository()
            .create_user_account(username, password_hash, access::initial_user_access())
            .await
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
