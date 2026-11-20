//! Which WebAgent app pages agent browsers may use, from the live `browser-pages.json`.
//!
//! A Browser Control browser signed in to this app acts as the person chatting, so its
//! pages are limited: sign-in is off unless this file turns it on, and a signed-in
//! browser shows only the listed app pages. The API sends the rules with each call's
//! authority; the worker's browser refuses any other page before it opens and closes
//! the browser when a click reaches one.
use anyhow::{bail, Result};
use serde_json::{json, Map, Value};

/// Live page rules, relative to the checkout.
pub const LIVE_PAGES: &str = "config/browser/browser-pages.json";
const MAX_BYTES: u64 = 16 * 1024;
/// Most allowed page rules.
pub const MAX_RULES: usize = 50;
/// URL parameters a page rule may name: the app's own route parameters.
pub const RULE_PARAMS: [&str; 7] = [
    "tab",
    "agentView",
    "agentScope",
    "appView",
    "article",
    "path",
    "window",
];

/// Whether agent browsers may sign in to this app, and the pages they may then show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pages {
    /// Agent browsers may open this app signed in as the person chatting (each agent's
    /// own `sign_in_app` setting must also be on).
    pub sign_in_app: bool,
    /// The main-panel tab a bare `/app` opens on.
    pub landing_tab: String,
    /// The allowed pages: every listed parameter must match (`"*"` any value, `""` left
    /// out); parameters a rule does not name are not checked.
    pub app_pages: Vec<Map<String, Value>>,
}

impl Default for Pages {
    /// Used when the live file cannot be read: signed out, and only Session Metrics.
    fn default() -> Self {
        let mut metrics = Map::new();
        metrics.insert("tab".into(), json!("session-metrics"));
        Self {
            sign_in_app: false,
            landing_tab: "session-metrics".into(),
            app_pages: vec![metrics],
        }
    }
}

fn route_value(value: &str) -> bool {
    value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'*'))
}

impl Pages {
    /// Parses and bounds the live file; unknown keys are refused.
    ///
    /// # Errors
    /// Invalid JSON, an unknown key or an invalid rule.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let value: Value = serde_json::from_slice(bytes)?;
        let Some(object) = value.as_object() else {
            bail!("Browser pages must be an object");
        };
        const KEYS: [&str; 5] = [
            "_instructions",
            "schema_version",
            "sign_in_app",
            "landing_tab",
            "app_pages",
        ];
        if object.keys().any(|key| !KEYS.contains(&key.as_str())) {
            bail!("Unknown browser pages key");
        }
        if value["schema_version"] != 1 {
            bail!("schema_version must be 1");
        }
        let Some(sign_in_app) = value["sign_in_app"].as_bool() else {
            bail!("sign_in_app must be true or false");
        };
        let landing_tab = value["landing_tab"].as_str().unwrap_or("");
        if landing_tab.is_empty() || landing_tab.contains('*') || !route_value(landing_tab) {
            bail!("landing_tab must be a tab name");
        }
        let Some(rules) = value["app_pages"].as_array() else {
            bail!("app_pages must be a list");
        };
        if rules.len() > MAX_RULES {
            bail!("app_pages allows at most {MAX_RULES} pages");
        }
        let pages = Self {
            sign_in_app,
            landing_tab: landing_tab.to_owned(),
            app_pages: rules
                .iter()
                .map(|rule| {
                    let Some(rule) = rule.as_object().filter(|rule| !rule.is_empty()) else {
                        bail!("Each app page is an object of URL parameters");
                    };
                    for (name, value) in rule {
                        if !RULE_PARAMS.contains(&name.as_str())
                            || !value.as_str().is_some_and(route_value)
                        {
                            bail!("Invalid app page parameter {name}");
                        }
                    }
                    Ok(rule.clone())
                })
                .collect::<Result<_>>()?,
        };
        let landing = [("tab".to_owned(), pages.landing_tab.clone())];
        if !pages.allows(&landing) {
            bail!("landing_tab must be one of app_pages");
        }
        Ok(pages)
    }

    /// The live rules; the defaults when the file is missing or invalid.
    #[must_use]
    pub fn live() -> Self {
        webagent_configuration::live::parsed(LIVE_PAGES, MAX_BYTES, Self::parse).unwrap_or_default()
    }

    /// Whether an `/app` route with these query parameters is an allowed page.
    #[must_use]
    pub fn allows(&self, query: &[(String, String)]) -> bool {
        self.app_pages.iter().any(|rule| {
            rule.iter().all(|(name, wanted)| {
                let found = query
                    .iter()
                    .find(|(key, _)| key == name)
                    .map(|(_, value)| value.as_str());
                match wanted.as_str().unwrap_or("") {
                    "*" => true,
                    "" => found.is_none(),
                    wanted => found == Some(wanted),
                }
            })
        })
    }

    /// The rules a trusted authority carries ([`Pages::authority`]), checked as the file is.
    #[must_use]
    pub fn from_authority(value: &Value) -> Option<Self> {
        let rules = value.as_object().filter(|rules| rules.len() == 2)?;
        let file = json!({"schema_version": 1, "sign_in_app": true,
            "landing_tab": rules.get("landing_tab")?, "app_pages": rules.get("app_pages")?});
        Self::parse(&serde_json::to_vec(&file).ok()?).ok()
    }

    /// The rules as the worker's trusted authority carries them.
    #[must_use]
    pub fn authority(&self) -> Value {
        json!({"landing_tab": self.landing_tab, "app_pages": self.app_pages})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn shipped_file_parses_signed_out_and_blocks_settings_pages() {
        let bytes = include_bytes!("../../../../config/browser/browser-pages.json");
        let pages = Pages::parse(bytes).unwrap();
        assert!(!pages.sign_in_app);
        assert!(pages.allows(&query(&[("tab", "session-metrics"), ("session_id", "x")])));
        for blocked in [
            vec![
                ("tab", "agents"),
                ("agent", "shared_default"),
                ("agentView", "abilities"),
            ],
            vec![("tab", "agents"), ("agentView", "model")],
            vec![("tab", "agents"), ("agentView", "config")],
            vec![("tab", "agents"), ("agentView", "identity")],
            vec![("tab", "agents"), ("agentView", "members")],
            vec![("tab", "agents")],
            vec![("tab", "app"), ("appView", "settings")],
            vec![("tab", "app")],
            vec![("tab", "builds"), ("job_id", "x")],
            vec![("tab", "host-admin")],
            vec![("tab", "account")],
            vec![("tab", "notifications")],
            vec![],
        ] {
            assert!(!pages.allows(&query(&blocked)), "{blocked:?}");
        }
        assert_eq!(Pages::default().sign_in_app, pages.sign_in_app);
    }

    #[test]
    fn rules_match_every_named_parameter() {
        let pages = Pages::parse(
            br#"{"schema_version":1,"sign_in_app":true,"landing_tab":"wiki","app_pages":[
            {"tab":"wiki"},{"tab":"agents","agentView":"sessions","agentScope":"*"},
            {"tab":"app","appView":""}]}"#,
        )
        .unwrap();
        assert!(pages.allows(&query(&[("tab", "wiki"), ("article", "a")])));
        assert!(pages.allows(&query(&[
            ("tab", "agents"),
            ("agentView", "sessions"),
            ("agentScope", "sessions")
        ])));
        // "*" also matches a parameter left out; a different value never matches.
        assert!(pages.allows(&query(&[("tab", "agents"), ("agentView", "sessions")])));
        assert!(!pages.allows(&query(&[("tab", "agents"), ("agentView", "model")])));
        assert!(pages.allows(&query(&[("tab", "app")])));
        assert!(!pages.allows(&query(&[("tab", "app"), ("appView", "settings")])));
    }

    #[test]
    fn refuses_invalid_files() {
        let good = r#""schema_version":1,"sign_in_app":false,"landing_tab":"wiki""#;
        assert!(
            Pages::parse(format!(r#"{{{good},"app_pages":[{{"tab":"wiki"}}]}}"#).as_bytes())
                .is_ok()
        );
        for bad in [
            format!(r#"{{{good},"app_pages":[{{"tab":"wiki"}}],"extra":1}}"#),
            format!(r#"{{{good},"app_pages":[{{"session_id":"x"}},{{"tab":"wiki"}}]}}"#),
            format!(r#"{{{good},"app_pages":[{{"tab":"a b"}},{{"tab":"wiki"}}]}}"#),
            format!(r#"{{{good},"app_pages":[{{}}]}}"#),
            format!(r#"{{{good},"app_pages":[{{"tab":"builds"}}]}}"#),
            r#"{"schema_version":1,"sign_in_app":"yes","landing_tab":"wiki","app_pages":[{"tab":"wiki"}]}"#.to_owned(),
        ] {
            assert!(Pages::parse(bad.as_bytes()).is_err(), "{bad}");
        }
    }
}
