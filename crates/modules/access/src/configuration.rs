//! Access-policy JSON validation and page decisions; the host owns live reads and writes.
use anyhow::{anyhow, Result};
use serde_json::Value;

// Page audiences are a global ceiling; administrative APIs retain their role gate.
pub fn validate_pages(pages: &Value) -> Result<()> {
    let scopes = pages
        .as_object()
        .ok_or_else(|| anyhow!("Invalid page policy"))?;
    for (scope, entries) in scopes {
        if !matches!(scope.as_str(), "main" | "app") {
            return Err(anyhow!("Invalid page scope"));
        }
        let entries = entries
            .as_object()
            .ok_or_else(|| anyhow!("Invalid page entries"))?;
        if entries.len() > 128 {
            return Err(anyhow!("Too many page policies"));
        }
        for (id, audience) in entries {
            if !crate::valid_agent_id(id)
                || !matches!(audience.as_str(), Some("everyone" | "registered" | "admin"))
            {
                return Err(anyhow!("Invalid page audience"));
            }
        }
    }
    Ok(())
}
pub fn page_allowed(policy: &Value, scope: &str, id: &str, principal: &crate::Principal) -> bool {
    if !principal.active() {
        return false;
    }
    if principal.administrator() {
        return true;
    } // protected administration recovery
    if scope == "app" && matches!(id, "settings" | "users" | "user-policies" | "page-access") {
        return false;
    }
    match policy["pages"][scope][id].as_str().unwrap_or("everyone") {
        "everyone" => true,
        "registered" => principal.role != crate::Role::Guest,
        _ => false,
    }
}

pub fn validate_configuration(value: &Value) -> Result<()> {
    if webagent_accounts::RegistrationMode::from_policy(value).is_err() {
        return Err(anyhow!("Invalid access-policy.json registration policy"));
    }
    webagent_accounts::LoginProtection::from_policy(value)?;
    if value
        .get("revision")
        .is_some_and(|v| v.as_u64().is_none_or(|n| n == 0))
    {
        return Err(anyhow!("Invalid policy revision"));
    }
    if let Some(tiers) = value.get("tiers") {
        let map = tiers
            .as_object()
            .ok_or_else(|| anyhow!("Invalid policy tiers"))?;
        for (id, definition) in map {
            let tier: crate::Tier = serde_json::from_value(definition.clone())?;
            tier.validate()?;
            if tier.id != *id {
                return Err(anyhow!("Tier identity mismatch"));
            }
        }
    }
    if value.get("tier_order").is_some() {
        crate::account_ladder::order(value)?;
    }
    if let Some(pages) = value.get("pages") {
        validate_pages(pages)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn saved_policy_extensions_survive_validation() {
        let value = json!({"registration":"restricted", "revision":1,
            "extension":{"future":true}, "pages":{"main":{"chat":"registered"}}});
        validate_configuration(&value).unwrap();
        assert_eq!(value["extension"]["future"], true);
    }

    #[test]
    fn malformed_registration_revision_and_audiences_are_rejected() {
        for value in [
            json!(null),
            json!({"registration":"unknown"}),
            json!({"registration":"open","revision":0}),
            json!({"registration":"open","pages":{"unknown":{}}}),
            json!({"registration":"open","pages":{"main":{"chat":"unknown"}}}),
        ] {
            assert!(validate_configuration(&value).is_err());
        }
    }

    #[test]
    fn login_protection_values_are_bounded() {
        validate_configuration(&json!({"registration":"open",
            "login_protection":{"address":{"failures":10}}}))
        .unwrap();
        for section in [
            json!(null),
            json!({"address":{"failures":0}}),
            json!({"address":{"failures":1001}}),
            json!({"account":{"window_seconds":86401}}),
            json!({"address":{"block_seconds":120,"max_block_seconds":60}}),
            json!({"account":{"delay_seconds":-1}}),
            json!({"account":{"unknown":1}}),
        ] {
            let value = json!({"registration":"open","login_protection":section});
            assert!(validate_configuration(&value).is_err(), "{section}");
        }
    }

    #[test]
    fn tier_identity_must_match_its_policy_key() {
        let value = json!({"registration":"restricted", "tiers":{"basic":{
            "id":"other", "revision":1, "allowed_models":[], "tools":{},
            "max_sessions":1, "max_active_runs":1}}});
        assert!(validate_configuration(&value).is_err());
    }

    #[test]
    fn page_policy_keeps_account_and_administrator_fences() {
        let policy = json!({"pages":{"main":{"chat":"registered"}}});
        let mut principal = crate::Principal::from_account(&json!({"user_id":"person",
            "status":"active", "role":"guest", "tier_id":"basic", "access_revision":1}))
        .unwrap();
        assert!(!page_allowed(&policy, "main", "chat", &principal));
        principal.role = crate::Role::User;
        assert!(page_allowed(&policy, "main", "chat", &principal));
        assert!(!page_allowed(&policy, "app", "settings", &principal));
        principal.role = crate::Role::Administrator;
        assert!(page_allowed(&policy, "app", "settings", &principal));
        principal.status = crate::AccountStatus::Suspended;
        assert!(!page_allowed(&policy, "app", "settings", &principal));
    }
}

/// Main-page authorization resource for existing browser API routes.
pub fn main_page_for_request(path: &str) -> Option<&'static str> {
    if path.ends_with("/main-chat")
        || path.contains("/main-notes")
        || [
            "/api/v3/main-chat",
            "/main-chat",
            "/api/v3/main-notes",
            "/main-notes",
            "/api/v3/sessions",
            "/sessions",
            "/api/v3/runs",
            "/runs",
        ]
        .iter()
        .any(|prefix| path.starts_with(prefix))
        || path.ends_with("/user/status")
    {
        Some("chat")
    } else if path.starts_with("/api/v3/agents/") || path.starts_with("/agents/") {
        Some("agents")
    } else {
        None
    }
}
#[cfg(test)]
mod request_page_tests {
    #[test]
    fn browser_routes_keep_existing_authorization_resources() {
        for path in [
            "/api/v3/sessions/one",
            "/runs/one",
            "/api/v3/agents/one/main-chat",
            "/api/v3/main-notes/one",
            "/api/v3/user/status",
        ] {
            assert_eq!(super::main_page_for_request(path), Some("chat"));
        }
        assert_eq!(
            super::main_page_for_request("/api/v3/agents/one"),
            Some("agents")
        );
        assert_eq!(super::main_page_for_request("/api/v3/auth/me"), None);
    }
}
