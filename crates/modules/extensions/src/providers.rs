//! External providers an ability plugin declares in `plugin.json` (`providers`) and
//! reaches only through the `web.provider_request` host request. Declarations are
//! checked once, when the plugin loads: an HTTPS origin with nothing after the host,
//! fixed endpoint methods and paths, and a key injected only into a declared header (so
//! it never appears in a URL or an error). Neither the plugin nor a model argument can
//! choose the host, the path or the credential.
use crate::requests::{valid_id, Declaration, RequestError};
use serde_json::Value;
use std::collections::BTreeMap;

/// The host request that uses these declarations.
pub const REQUEST: &str = "web.provider_request";
const MAX_PROVIDERS: usize = 16;
const MAX_ENDPOINTS: usize = 8;
const MAX_QUERY_PAIRS: usize = 32;
const MAX_QUERY_VALUE: usize = 2048;
const MAX_JSON_BODY: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Method {
    Get,
    Post,
}

/// The vault credential the app adds to a provider's requests, and where.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Credential {
    pub name: String,
    pub header: String,
    pub prefix: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Endpoint {
    pub method: Method,
    pub path: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Provider {
    pub origin: String,
    pub credential: Option<Credential>,
    pub endpoints: BTreeMap<String, Endpoint>,
}

/// A plugin's validated provider table; empty for plugins that declare none.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Providers(BTreeMap<String, Provider>);

impl Providers {
    /// Read `providers` from a plugin manifest. A table is required exactly when the
    /// plugin declares the `web.provider_request` request.
    pub fn parse(manifest: &Value, declared: &Declaration) -> Result<Self, RequestError> {
        let uses = declared.names().iter().any(|name| name == REQUEST);
        let Some(table) = manifest.get("providers") else {
            return if uses {
                Err(RequestError::Declaration(
                    "providers are required for web.provider_request",
                ))
            } else {
                Ok(Self::default())
            };
        };
        if !uses {
            return Err(RequestError::Declaration(
                "providers need the web.provider_request request",
            ));
        }
        let table = table
            .as_object()
            .filter(|table| !table.is_empty() && table.len() <= MAX_PROVIDERS)
            .ok_or(RequestError::Declaration(
                "providers must be 1 to 16 entries",
            ))?;
        let mut providers = BTreeMap::new();
        for (name, value) in table {
            if !valid_id(name) {
                return Err(RequestError::Declaration("invalid provider name"));
            }
            providers.insert(name.clone(), provider(value)?);
        }
        Ok(Self(providers))
    }

    /// The declared provider and endpoint, if both exist.
    #[must_use]
    pub fn endpoint(&self, provider: &str, endpoint: &str) -> Option<(&Provider, &Endpoint)> {
        let provider = self.0.get(provider)?;
        Some((provider, provider.endpoints.get(endpoint)?))
    }
}

fn provider(value: &Value) -> Result<Provider, RequestError> {
    let fields = value
        .as_object()
        .ok_or(RequestError::Declaration("a provider is an object"))?;
    if fields
        .keys()
        .any(|key| !matches!(key.as_str(), "origin" | "credential" | "endpoints"))
    {
        return Err(RequestError::Declaration("unknown provider field"));
    }
    let origin = fields
        .get("origin")
        .and_then(Value::as_str)
        .filter(|origin| valid_origin(origin))
        .ok_or(RequestError::Declaration(
            "provider origin must be https://host",
        ))?
        .to_owned();
    let credential = match fields.get("credential") {
        None => None,
        Some(value) => Some(credential(value)?),
    };
    let table = fields
        .get("endpoints")
        .and_then(Value::as_object)
        .filter(|table| !table.is_empty() && table.len() <= MAX_ENDPOINTS)
        .ok_or(RequestError::Declaration(
            "endpoints must be 1 to 8 entries",
        ))?;
    let mut endpoints = BTreeMap::new();
    for (name, value) in table {
        let fields = value.as_object().filter(|fields| {
            fields.len() == 2 && fields.contains_key("method") && fields.contains_key("path")
        });
        let method = match fields.and_then(|fields| fields["method"].as_str()) {
            Some("GET") => Method::Get,
            Some("POST") => Method::Post,
            _ => {
                return Err(RequestError::Declaration(
                    "endpoint method must be GET or POST",
                ))
            }
        };
        let path = fields
            .and_then(|fields| fields["path"].as_str())
            .filter(|path| valid_path(path))
            .ok_or(RequestError::Declaration("invalid endpoint path"))?;
        if !valid_id(name) {
            return Err(RequestError::Declaration("invalid endpoint name"));
        }
        endpoints.insert(
            name.clone(),
            Endpoint {
                method,
                path: path.to_owned(),
            },
        );
    }
    Ok(Provider {
        origin,
        credential,
        endpoints,
    })
}

fn credential(value: &Value) -> Result<Credential, RequestError> {
    let fields = value
        .as_object()
        .filter(|fields| {
            fields.len() == 3
                && ["name", "header", "prefix"]
                    .iter()
                    .all(|key| fields.get(*key).is_some_and(Value::is_string))
        })
        .ok_or(RequestError::Declaration(
            "credential needs name, header and prefix",
        ))?;
    let text = |key: &str| fields[key].as_str().unwrap_or_default().to_owned();
    let (name, header, prefix) = (text("name"), text("header"), text("prefix"));
    let token = |value: &str| {
        !value.is_empty()
            && value.len() <= 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    };
    if !token(&name)
        || !token(&header)
        || prefix.len() > 32
        || !prefix.bytes().all(|byte| (b' '..=b'~').contains(&byte))
    {
        return Err(RequestError::Declaration("invalid credential"));
    }
    Ok(Credential {
        name,
        header,
        prefix,
    })
}

/// `https://` and a lowercase DNS name of at least two labels; no port, path or query.
#[must_use]
pub fn valid_origin(origin: &str) -> bool {
    let Some(host) = origin.strip_prefix("https://") else {
        return false;
    };
    host.len() <= 253
        && host.split('.').count() >= 2
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        })
}

/// An absolute path of plain segments: no `.` or `..`, empty inner segment, query or escape.
#[must_use]
pub fn valid_path(path: &str) -> bool {
    path.starts_with('/')
        && path.len() <= 256
        && !path.contains("//")
        && path
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'_' | b'-'))
        && !path
            .split('/')
            .any(|segment| segment == "." || segment == "..")
}

/// One validated `web.provider_request` from a plugin.
#[derive(Debug, PartialEq)]
pub struct ProviderCall {
    pub provider: String,
    pub endpoint: String,
    /// Query parameters the app encodes onto the endpoint's fixed path.
    pub query: Vec<(String, String)>,
    /// A JSON body, for POST endpoints only.
    pub json: Option<Value>,
    /// The plugin's monthly cap for this provider; 0 means none.
    pub monthly_cap: u64,
}

impl ProviderCall {
    /// `params` name the provider and endpoint; `body` may carry `query`, `json` and
    /// `monthly_cap`, and nothing else.
    pub fn parse(params: &Value, body: &Value) -> Result<Self, RequestError> {
        let parameter = |key: &str| {
            params[key]
                .as_str()
                .filter(|value| valid_id(value))
                .map(str::to_owned)
                .ok_or_else(|| RequestError::Parameter(key.to_owned()))
        };
        if params.as_object().is_none_or(|params| params.len() != 2) {
            return Err(RequestError::Parameter("params".into()));
        }
        let (provider, endpoint) = (parameter("provider")?, parameter("endpoint")?);
        let fields = body
            .as_object()
            .ok_or_else(|| RequestError::Parameter("body".into()))?;
        if let Some(key) = fields
            .keys()
            .find(|key| !matches!(key.as_str(), "query" | "json" | "monthly_cap"))
        {
            return Err(RequestError::Parameter(key.clone()));
        }
        let mut query = Vec::new();
        if let Some(pairs) = fields.get("query") {
            let pairs = pairs
                .as_object()
                .filter(|pairs| pairs.len() <= MAX_QUERY_PAIRS)
                .ok_or_else(|| RequestError::Parameter("query".into()))?;
            for (key, value) in pairs {
                let value = value
                    .as_str()
                    .filter(|value| {
                        value.len() <= MAX_QUERY_VALUE && !value.chars().any(char::is_control)
                    })
                    .ok_or_else(|| RequestError::Parameter(key.clone()))?;
                if !valid_id(key) {
                    return Err(RequestError::Parameter(key.clone()));
                }
                query.push((key.clone(), value.to_owned()));
            }
        }
        let json = match fields.get("json") {
            None | Some(Value::Null) => None,
            Some(value @ Value::Object(_)) if value.to_string().len() <= MAX_JSON_BODY => {
                Some(value.clone())
            }
            Some(_) => return Err(RequestError::Parameter("json".into())),
        };
        let monthly_cap = match fields.get("monthly_cap") {
            None => 0,
            Some(value) => value
                .as_u64()
                .ok_or_else(|| RequestError::Parameter("monthly_cap".into()))?,
        };
        Ok(Self {
            provider,
            endpoint,
            query,
            json,
            monthly_cap,
        })
    }
}

/// `YYYY-MM` (UTC) of a Unix time, the period of monthly usage counters.
#[must_use]
pub fn month(unix_seconds: u64) -> String {
    // Days to civil date (proleptic Gregorian), after Howard Hinnant's algorithm.
    let shifted = unix_seconds / 86_400 + 719_468;
    let era = shifted / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + u64::from(month <= 2);
    format!("{year:04}-{month:02}")
}

/// The caller's usage-counter key for one plugin provider in one month. `module_state:`
/// keys live in the caller's own user database and are always sealed, so counters are
/// per user with the same isolation as vault keys.
#[must_use]
pub fn usage_key(ability: &str, provider: &str, month: &str) -> String {
    format!("module_state:extension_usage:{ability}:{provider}:{month}")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use serde_json::json;

    fn manifest(providers: Value) -> Value {
        json!({"requests": ["ability.settings", REQUEST], "providers": providers})
    }

    fn parse(manifest: &Value) -> Result<Providers, RequestError> {
        Providers::parse(manifest, &Declaration::parse(manifest).unwrap())
    }

    fn table() -> Value {
        json!({
            "tavily": {"origin": "https://api.tavily.com",
                "credential": {"name": "tavily_api_key", "header": "Authorization", "prefix": "Bearer "},
                "endpoints": {"search": {"method": "POST", "path": "/search"}}},
            "duckduckgo": {"origin": "https://html.duckduckgo.com",
                "endpoints": {"search": {"method": "GET", "path": "/html/"}}}
        })
    }

    #[test]
    fn declared_providers_are_checked_once_at_load() {
        let providers = parse(&manifest(table())).unwrap();
        let (tavily, search) = providers.endpoint("tavily", "search").unwrap();
        assert_eq!(
            (tavily.origin.as_str(), search.method),
            ("https://api.tavily.com", Method::Post)
        );
        assert_eq!(tavily.credential.as_ref().unwrap().header, "Authorization");
        assert!(providers.endpoint("duckduckgo", "extract").is_none());
        assert!(providers.endpoint("brave", "search").is_none());
        // No table and no request is fine; either one alone is refused.
        let plain = json!({"requests": ["ability.settings"]});
        assert_eq!(
            Providers::parse(&plain, &Declaration::parse(&plain).unwrap()),
            Ok(Providers::default())
        );
        let table_only = json!({"requests": ["ability.settings"], "providers": table()});
        assert!(Providers::parse(&table_only, &Declaration::parse(&table_only).unwrap()).is_err());
        assert!(parse(&json!({"requests": [REQUEST]})).is_err());
    }

    #[test]
    fn origins_paths_and_credentials_cannot_widen() {
        let bad_origins = [
            "http://api.tavily.com",
            "https://api.tavily.com/v1",
            "https://api.tavily.com:8443",
            "https://API.tavily.com",
            "https://localhost",
            "https://user@api.tavily.com",
            "https://-a.com",
        ];
        for origin in bad_origins {
            let mut providers = table();
            providers["tavily"]["origin"] = json!(origin);
            assert!(parse(&manifest(providers)).is_err(), "{origin}");
        }
        for path in [
            "search", "/a/../b", "/a/./b", "/a//b", "/a?b=1", "/a%2e", "",
        ] {
            let mut providers = table();
            providers["tavily"]["endpoints"]["search"]["path"] = json!(path);
            assert!(parse(&manifest(providers)).is_err(), "{path}");
        }
        let mut providers = table();
        providers["tavily"]["endpoints"]["search"]["method"] = json!("PUT");
        assert!(parse(&manifest(providers)).is_err());
        for (key, value) in [
            ("header", "X Token"),
            ("prefix", "Bearer\r\n"),
            ("name", "../key"),
        ] {
            let mut providers = table();
            providers["tavily"]["credential"][key] = json!(value);
            assert!(parse(&manifest(providers)).is_err(), "{key}");
        }
        let mut providers = table();
        providers["tavily"]["query_key"] = json!("api_key");
        assert!(parse(&manifest(providers)).is_err(), "unknown field");
    }

    #[test]
    fn provider_calls_are_bounded() {
        let params = json!({"provider": "tavily", "endpoint": "search"});
        let call = ProviderCall::parse(
            &params,
            &json!({"query": {"q": "rust", "count": "5"}, "json": {"query": "rust"}, "monthly_cap": 1000}),
        )
        .unwrap();
        let mut query = call.query.clone();
        query.sort();
        assert_eq!(
            query,
            [("count".into(), "5".into()), ("q".into(), "rust".into())]
        );
        assert_eq!((call.monthly_cap, call.json.is_some()), (1000, true));
        assert_eq!(
            ProviderCall::parse(&params, &json!({}))
                .unwrap()
                .monthly_cap,
            0
        );
        for body in [
            json!({"url": "https://x"}),
            json!({"query": {"q": "a\nb"}}),
            json!({"query": {"q": 1}}),
            json!({"query": {"a b": "x"}}),
            json!({"json": "text"}),
            json!({"monthly_cap": -1}),
            json!(null),
        ] {
            assert!(ProviderCall::parse(&params, &body).is_err(), "{body}");
        }
        for params in [
            json!({"provider": "tavily"}),
            json!({"provider": "../x", "endpoint": "search"}),
            json!({"provider": "tavily", "endpoint": "search", "origin": "https://x"}),
        ] {
            assert!(
                ProviderCall::parse(&params, &json!({})).is_err(),
                "{params}"
            );
        }
    }

    #[test]
    fn months_are_utc_calendar_months() {
        assert_eq!(month(0), "1970-01");
        assert_eq!(month(951_782_400), "2000-02"); // 2000-02-29, a leap day
        assert_eq!(month(1_709_251_199), "2024-02"); // 2024-02-29 23:59:59
        assert_eq!(month(1_709_251_200), "2024-03");
        assert_eq!(month(1_793_232_036), "2026-10");
        assert_eq!(
            usage_key("web_search", "tavily", "2026-10"),
            "module_state:extension_usage:web_search:tavily:2026-10"
        );
    }
}
