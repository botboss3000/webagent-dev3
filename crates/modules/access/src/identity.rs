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
    /// A test account's sign-in, made by Diagnostics `test_login` for one administrator run:
    /// a person of that tier inside its own account, never administrator power. The only
    /// way a test account is signed in.
    TestPersona,
}

impl CredentialKind {
    /// The name handlers see in the identity document's `_credential` field.
    pub fn name(self) -> &'static str {
        match self {
            Self::Person => "person",
            Self::AgentBrowser => AGENT_BROWSER,
            Self::Widget => "widget",
            Self::MessagingBearer => "messaging_bearer",
            Self::NotificationReader => "notification_reader",
            Self::BrowserDevice => "browser_device",
            Self::TestPersona => TEST_PERSONA,
        }
    }
    /// Whether an identity document (the handlers' JSON form) came from a widget or an agent
    /// browser. Fails closed: any widget binding counts.
    pub fn restricted_document(identity: &serde_json::Value) -> bool {
        !identity["_embed_agent"].is_null()
            || identity["_credential"] == AGENT_BROWSER
            || identity["_credential"] == "widget"
    }
    /// The refusal for a request this credential may never make, checked before any handler:
    /// an agent's browser is read-only (BROWSER-4).
    pub fn refuses_method(self, method: &str) -> Option<&'static str> {
        (self == Self::AgentBrowser && !matches!(method, "GET" | "HEAD" | "OPTIONS"))
            .then_some("This sign-in is read-only")
    }
    /// Agent browsers and widgets act for a person but never with administrator power.
    pub fn restricted(self) -> bool {
        matches!(self, Self::AgentBrowser | Self::Widget)
    }
    /// Value of the sign-in record's `kind` field; a record without one is a person's sign-in
    /// unless its label says it was issued to an agent browser (records made before `kind`).
    pub fn of_sign_in(record: &serde_json::Value) -> Self {
        if record["kind"] == TEST_PERSONA {
            return Self::TestPersona;
        }
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
pub const AGENT_BROWSER: &str = webagent_accounts::AGENT_BROWSER_KIND;
/// The sign-in record `kind` of a test-persona sign-in.
pub const TEST_PERSONA: &str = webagent_accounts::TEST_PERSONA_KIND;
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
        self.principal.administrator()
            && !self.kind.restricted()
            && self.kind != CredentialKind::TestPersona
            && !self.principal.test_account
    }
    /// Fail closed: a test account is reachable only through a test-persona sign-in, and a
    /// test-persona sign-in only ever names a test account.
    pub fn kind_matches_account(&self) -> bool {
        self.principal.test_account == (self.kind == CredentialKind::TestPersona)
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
                test_account: false,
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
    fn a_test_persona_is_a_person_of_its_tier_and_matches_only_test_accounts() {
        let mut persona = identity(Role::User, CredentialKind::TestPersona);
        persona.principal.test_account = true;
        assert!(persona.may_change_settings() && persona.kind_matches_account());
        assert!(!persona.admin_power());
        assert_eq!(persona.kind.refuses_method("POST"), None);
        assert_eq!(
            CredentialKind::of_sign_in(&json!({"kind": TEST_PERSONA})),
            CredentialKind::TestPersona
        );
        assert_eq!(persona.kind.name(), "test_persona");
        // Refused: the kind on a real account, a test account through any other kind, and
        // administrator power even if a record claimed the role.
        let mut real = identity(Role::User, CredentialKind::TestPersona);
        assert!(!real.kind_matches_account());
        real.kind = CredentialKind::Person;
        assert!(real.kind_matches_account());
        let mut test_person = identity(Role::User, CredentialKind::Person);
        test_person.principal.test_account = true;
        assert!(!test_person.kind_matches_account());
        let mut claimed = identity(Role::Administrator, CredentialKind::TestPersona);
        claimed.principal.test_account = true;
        assert!(!claimed.admin_power());
        assert_eq!(claimed.effective_role(), Role::User);
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
    fn agent_browsers_are_read_only_and_restricted_documents_are_recognised() {
        use CredentialKind::*;
        for method in ["POST", "PUT", "PATCH", "DELETE"] {
            assert!(AgentBrowser.refuses_method(method).is_some(), "{method}");
            for kind in [Person, Widget, MessagingBearer] {
                assert!(kind.refuses_method(method).is_none(), "{kind:?} {method}");
            }
        }
        assert!(AgentBrowser.refuses_method("GET").is_none());
        for document in [
            json!({"_embed_agent":"a"}),
            json!({"_credential":"agent_browser"}),
            json!({"_credential":"widget"}),
        ] {
            assert!(CredentialKind::restricted_document(&document), "{document}");
        }
        assert!(!CredentialKind::restricted_document(
            &json!({"_credential":"person"})
        ));
        assert!(!CredentialKind::restricted_document(
            &json!({"_user_id":"admin"})
        ));
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
