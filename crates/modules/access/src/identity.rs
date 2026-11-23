//! The request identity: who is calling and through which kind of credential.
//!
//! Built once per request by the host's authentication middleware from records it read (or
//! confirmed unchanged through the account's `auth_epoch`) during that same request. The rules
//! that depend only on the credential kind live here, so every route sees the same answer.
use crate::{Principal, Role};

/// How the caller proved who they are.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CredentialKind {
    /// A person's browser sign-in (cookie).
    Person,
    /// A sign-in issued to an agent's own browser (Browser Control).
    AgentBrowser,
    /// A chat-widget (embed) credential on another site.
    Widget,
    /// The administrator messaging bearer API.
    MessagingBearer,
    /// A notification event-reader bearer token.
    NotificationReader,
    /// A linked browser-extension device secret.
    BrowserDevice,
}

impl CredentialKind {
    /// Agent browsers and widgets act for a person but never with administrator power.
    pub fn restricted(self) -> bool {
        matches!(self, Self::AgentBrowser | Self::Widget)
    }
    /// Value of the sign-in record's `kind` field; a record without one is a person's sign-in
    /// unless its label says it was issued to an agent browser (records made before `kind`).
    pub fn of_sign_in(record: &serde_json::Value) -> Self {
        if record["kind"] == AGENT_BROWSER
            || (record["kind"].is_null() && record["user_agent"] == LEGACY_AGENT_BROWSER_LABEL)
        {
            Self::AgentBrowser
        } else {
            Self::Person
        }
    }
}

/// The sign-in record `kind` of an agent-browser sign-in.
pub const AGENT_BROWSER: &str = "agent_browser";
/// The label agent-browser sign-ins carried before records had a `kind`.
pub const LEGACY_AGENT_BROWSER_LABEL: &str = "Browser Control signed-in browser";

/// One request's caller.
#[derive(Clone, Debug)]
pub struct Identity {
    pub principal: Principal,
    pub username: String,
    pub generation: String,
    /// The account's `auth_epoch` this identity was built or confirmed at; `None` for
    /// credentials validated in full on every request.
    pub auth_epoch: Option<i64>,
    pub kind: CredentialKind,
    /// The sign-in or credential record this request presented.
    pub credential_key: String,
    /// That record's sealed bytes as read when this identity was built; a later check confirms
    /// the record is unchanged by comparing them, without decrypting. Empty when unused.
    pub credential_record: Vec<u8>,
    /// Widget and agent-browser credentials are bound to one agent.
    pub agent_id: Option<String>,
}

impl Identity {
    pub fn user_id(&self) -> &str {
        &self.principal.user_id
    }
    /// Administrator power: an active administrator account, never through a widget or an
    /// agent's browser.
    pub fn admin_power(&self) -> bool {
        self.principal.administrator() && !self.kind.restricted()
    }
    /// Whether this credential may change settings, permissions or approvals.
    pub fn may_change_settings(&self) -> bool {
        !self.kind.restricted()
    }
    /// The role handlers see: an administrator acting through a restricted credential is
    /// treated as an ordinary user, so no role check can hand it administrator power.
    pub fn effective_role(&self) -> Role {
        if self.principal.role == Role::Administrator && !self.admin_power() {
            Role::User
        } else {
            self.principal.role
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AccountStatus;
    use serde_json::json;

    fn identity(role: Role, kind: CredentialKind) -> Identity {
        Identity {
            principal: Principal {
                user_id: "u".into(),
                status: AccountStatus::Active,
                role,
                tier_id: "administrator".into(),
                access_revision: 1,
            },
            username: "u".into(),
            generation: "g".into(),
            auth_epoch: Some(0),
            kind,
            credential_key: "auth:x".into(),
            credential_record: Vec::new(),
            agent_id: None,
        }
    }

    #[test]
    fn only_people_and_the_messaging_bearer_carry_administrator_power() {
        use CredentialKind::*;
        for kind in [Person, MessagingBearer] {
            let admin = identity(Role::Administrator, kind);
            assert!(
                admin.admin_power() && admin.may_change_settings(),
                "{kind:?}"
            );
            assert_eq!(admin.effective_role(), Role::Administrator);
        }
        for kind in [AgentBrowser, Widget] {
            let admin = identity(Role::Administrator, kind);
            assert!(
                !admin.admin_power() && !admin.may_change_settings(),
                "{kind:?}"
            );
            assert_eq!(admin.effective_role(), Role::User);
        }
        assert!(!identity(Role::User, Person).admin_power());
        assert_eq!(identity(Role::Guest, Widget).effective_role(), Role::Guest);
    }

    #[test]
    fn sign_in_records_name_their_kind() {
        assert_eq!(
            CredentialKind::of_sign_in(&json!({"kind":"agent_browser"})),
            CredentialKind::AgentBrowser
        );
        assert_eq!(
            CredentialKind::of_sign_in(&json!({"user_agent":LEGACY_AGENT_BROWSER_LABEL})),
            CredentialKind::AgentBrowser
        );
        assert_eq!(
            CredentialKind::of_sign_in(&json!({"user_agent":"Firefox"})),
            CredentialKind::Person
        );
    }
}
