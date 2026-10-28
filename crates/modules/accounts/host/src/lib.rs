//! Accounts storage for the shared storage handle. Accounts owns the record SQL
//! (`webagent_accounts::Repository`); this crate binds it to `StoreCore`'s
//! application pool and codec. Root calls these methods on its `Store` through
//! `Deref` with this trait imported.
use anyhow::Result;
use serde_json::Value;
use std::future::Future;
use webagent_accounts::{RecordCodec, Repository};
use webagent_host_core::StoreCore;

/// The handle's codec as the Accounts record codec. A wrapper because the orphan
/// rule forbids implementing a foreign trait on the foreign `StoreCore` here.
#[derive(Clone, Copy)]
pub struct AccountsCodec<'a>(&'a StoreCore);

impl RecordCodec for AccountsCodec<'_> {
    fn seal(&self, value: &Value) -> Result<Vec<u8>> {
        self.0.seal(value)
    }
    fn unseal(&self, value: &[u8]) -> Result<Value> {
        self.0.unseal(value)
    }
}

/// Accounts record access for one storage handle. Futures are `Send` so routes
/// and background tasks can hold them across awaits.
pub trait AccountsStore {
    /// The Accounts repository over the application authority.
    fn accounts_repository(&self) -> Repository<'_, AccountsCodec<'_>>;
    /// Every account id, guests included, in id order.
    fn all_user_ids(&self) -> impl Future<Output = Result<Vec<String>>> + Send;
    /// Creates a guest account and returns its record.
    fn create_guest_account(&self) -> impl Future<Output = Result<Value>> + Send;
    /// Creates an ordinary account from an already hashed password.
    fn create_user_account<'a>(
        &'a self,
        username: &'a str,
        password_hash: &'a str,
    ) -> impl Future<Output = Result<Value>> + Send + 'a;
    /// Whether the built-in administrator account exists.
    fn has_admin_account(&self) -> impl Future<Output = Result<bool>> + Send;
    /// The account record for an id, if it exists.
    fn account_by_id<'a>(
        &'a self,
        id: &'a str,
    ) -> impl Future<Output = Result<Option<Value>>> + Send + 'a;
    /// The account record for a username, if it exists.
    fn account_by_username<'a>(
        &'a self,
        name: &'a str,
    ) -> impl Future<Output = Result<Option<Value>>> + Send + 'a;
    /// The profile record (display name, default agent, last sign-in) for an account id.
    fn user_profile<'a>(
        &'a self,
        id: &'a str,
    ) -> impl Future<Output = Result<Option<Value>>> + Send + 'a;
}

impl AccountsStore for StoreCore {
    fn accounts_repository(&self) -> Repository<'_, AccountsCodec<'_>> {
        Repository::new(&self.app_pool, AccountsCodec(self))
    }
    async fn all_user_ids(&self) -> Result<Vec<String>> {
        self.accounts_repository().all_user_ids().await
    }
    async fn create_guest_account(&self) -> Result<Value> {
        self.accounts_repository()
            .create_guest_account(webagent_access::initial_guest_access())
            .await
    }
    async fn create_user_account<'a>(
        &'a self,
        username: &'a str,
        password_hash: &'a str,
    ) -> Result<Value> {
        self.accounts_repository()
            .create_user_account(
                username,
                password_hash,
                webagent_access::initial_user_access(),
            )
            .await
    }
    async fn has_admin_account(&self) -> Result<bool> {
        self.accounts_repository().has_admin_account().await
    }
    async fn account_by_id<'a>(&'a self, id: &'a str) -> Result<Option<Value>> {
        self.accounts_repository().account_state(id).await
    }
    async fn account_by_username<'a>(&'a self, name: &'a str) -> Result<Option<Value>> {
        self.accounts_repository()
            .account_state_by_username(name)
            .await
    }
    async fn user_profile<'a>(&'a self, id: &'a str) -> Result<Option<Value>> {
        self.accounts_repository().user_profile(id).await
    }
}

#[cfg(test)]
mod tests;
