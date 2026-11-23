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
