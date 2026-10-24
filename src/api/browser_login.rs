//! Trusted diagnostics browser-login route delegates credential and session behavior to Accounts.
pub(crate) use crate::account_authentication::diagnostics_browser_login as login;
pub(crate) use webagent_accounts::BrowserLoginBridge as Bridge;
