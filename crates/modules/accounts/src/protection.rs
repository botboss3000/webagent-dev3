use anyhow::{ensure, Result};
#[derive(Clone)]
pub struct BrowserLoginBridge {
    token_hash: [u8; 32],
    caller_id: String,
}
impl BrowserLoginBridge {
    pub fn configure(token: &str, caller_id: &str) -> Result<Option<Self>> {
        if token.is_empty() && caller_id.is_empty() {
            return Ok(None);
        }
        ensure!(
            !caller_id.is_empty()
                && caller_id.len() <= 128
                && !caller_id.chars().any(char::is_control),
            "V3_BROWSER_LOGIN_CALLER_ID must identify the trusted diagnostics administrator"
        );
        let token_hash = crate::message_api_token_hash(token)?.ok_or_else(|| {
            anyhow::anyhow!("V3_BROWSER_LOGIN_TOKEN is required with V3_BROWSER_LOGIN_CALLER_ID")
        })?;
        Ok(Some(Self {
            token_hash,
            caller_id: caller_id.into(),
        }))
    }
    /// The host separately requires live Access administrator authorization.
    pub fn authorized(
        &self,
        authorization: Option<&str>,
        caller_id: &str,
        has_origin: bool,
    ) -> bool {
        !has_origin
            && caller_id == self.caller_id
            && crate::message_api_authorized(Some(&self.token_hash), authorization)
    }
}
pub fn valid_origin(origin: Option<&str>, host: &str, secure: bool) -> bool {
    origin.is_none_or(|v| {
        v == format!("https://{host}") || (!secure && v == format!("http://{host}"))
    })
}
pub fn valid_csrf(method: &str, expected: &str, supplied: Option<&str>) -> bool {
    matches!(method, "GET" | "HEAD" | "OPTIONS") || supplied.unwrap_or("") == expected
}
pub fn session_cookie(token: &str, duration: u64, secure: bool) -> String {
    format!(
        "v3_session={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age={duration}{}",
        if secure { "; Secure" } else { "" }
    )
}
/// Parse the established cookie name without exposing session storage details to hosts.
pub fn session_token(headers: &axum::http::HeaderMap) -> &str {
    headers
        .get("cookie")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .split(';')
        .find_map(|v| v.trim().strip_prefix("v3_session="))
        .unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn origin_csrf_and_secure_cookie_contract() {
        assert!(valid_origin(None, "app.test", true));
        assert!(valid_origin(Some("https://app.test"), "app.test", true));
        assert!(!valid_origin(Some("http://app.test"), "app.test", true));
        assert!(!valid_origin(Some("https://other.test"), "app.test", true));
        assert!(valid_csrf("GET", "csrf", None));
        assert!(!valid_csrf("POST", "csrf", None));
        assert!(valid_csrf("POST", "csrf", Some("csrf")));
        assert!(session_cookie("token", 600, true).ends_with("; Secure"));
        assert!(session_cookie("token", 600, false).contains("HttpOnly; SameSite=Strict"));
    }
    #[test]
    fn bridge_requires_bound_caller_token_and_no_origin() {
        let token = "z".repeat(32);
        let bridge = BrowserLoginBridge::configure(&token, "operator")
            .unwrap()
            .unwrap();
        let auth = format!("Bearer {token}");
        assert!(bridge.authorized(Some(&auth), "operator", false));
        assert!(!bridge.authorized(Some(&auth), "other", false));
        assert!(!bridge.authorized(Some(&auth), "operator", true));
        assert!(!bridge.authorized(None, "operator", false));
        assert!(BrowserLoginBridge::configure("", "operator").is_err());
    }
    #[test]
    fn session_cookie_parser_keeps_first_exact_name_and_rejects_invalid_header_text() {
        use axum::http::{HeaderMap, HeaderValue};
        let mut headers = HeaderMap::new();
        assert_eq!(session_token(&headers), "");
        headers.insert(
            "cookie",
            HeaderValue::from_static(
                "other=value; v3_session_suffix=wrong; v3_session=first; v3_session=second",
            ),
        );
        assert_eq!(session_token(&headers), "first");
        headers.insert("cookie", HeaderValue::from_bytes(&[0xff]).unwrap());
        assert_eq!(session_token(&headers), "");
    }
}
