//! The host requests an ability plugin may make, always as the chatting user. A plugin
//! declares the names it uses in its `plugin.json`; the app accepts only declared names,
//! only from this offered set, and checks the caller's own permissions on each.
use serde_json::Value;
use std::fmt;

/// What the app does for one offered request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Kind {
    /// One fixed route of the app's user API: method and path template.
    Route(&'static str, &'static str),
    /// The caller's own skills on an agent they administer.
    Skills(SkillAction),
    /// The plugin's own ability settings for the current session.
    Settings,
    /// Subscribe the current session to another of the caller's sessions; the app
    /// re-authorizes the tool call and its Automation dependency first.
    Watch,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SkillAction {
    List,
    Read,
    Save,
    Remove,
}

/// Every request the app offers, by name.
pub const OFFERED: [(&str, Kind); 23] = [
    ("agents.list", Kind::Route("GET", "/models/agents")),
    (
        "agents.identity.read",
        Kind::Route("GET", "/models/agents/{agent_id}/identity"),
    ),
    (
        "agents.abilities.read",
        Kind::Route("GET", "/agents/{agent_id}/abilities"),
    ),
    ("agents.create", Kind::Route("POST", "/models/agents")),
    (
        "agents.identity.update",
        Kind::Route("PUT", "/models/agents/{agent_id}/identity"),
    ),
    (
        "agents.tool_mode.set",
        Kind::Route(
            "PUT",
            "/agents/{agent_id}/abilities/{ability_id}/tools/{tool}/mode",
        ),
    ),
    (
        "agents.ability_visibility.set",
        Kind::Route(
            "PUT",
            "/agents/{agent_id}/abilities/{ability_id}/visibility",
        ),
    ),
    (
        "agents.slot.write",
        Kind::Route("PUT", "/agents/{agent_id}/slots/{slot}"),
    ),
    (
        "agents.chat_config.read",
        Kind::Route("GET", "/agents/{agent_id}/chat-config/{document}"),
    ),
    (
        "agents.chat_config.patch",
        Kind::Route("PATCH", "/agents/{agent_id}/chat-config/{document}"),
    ),
    (
        "agents.chat_config.reset",
        Kind::Route("DELETE", "/agents/{agent_id}/chat-config/{document}"),
    ),
    ("skills.list", Kind::Skills(SkillAction::List)),
    ("skills.read", Kind::Skills(SkillAction::Read)),
    ("skills.save", Kind::Skills(SkillAction::Save)),
    ("skills.remove", Kind::Skills(SkillAction::Remove)),
    ("sessions.list", Kind::Route("GET", "/sessions")),
    ("sessions.create", Kind::Route("POST", "/sessions")),
    (
        "sessions.read",
        Kind::Route("GET", "/sessions/{session_id}"),
    ),
    (
        "sessions.update",
        Kind::Route("PATCH", "/sessions/{session_id}"),
    ),
    (
        "sessions.recycle",
        Kind::Route("DELETE", "/sessions/{session_id}"),
    ),
    (
        "sessions.restore",
        Kind::Route("POST", "/sessions/{session_id}/restore"),
    ),
    ("sessions.watch", Kind::Watch),
    ("ability.settings", Kind::Settings),
];

/// Why a declaration or a request was refused.
#[derive(Debug, Eq, PartialEq)]
pub enum RequestError {
    /// The name is not in the offered set.
    Unknown(String),
    /// The plugin did not declare this name.
    Undeclared(String),
    /// A route parameter is missing or would leave its path segment.
    Parameter(String),
    /// The plugin's declaration is malformed.
    Declaration(&'static str),
}

impl fmt::Display for RequestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unknown(name) => write!(formatter, "Unknown host request: {name}"),
            Self::Undeclared(name) => write!(formatter, "Undeclared host request: {name}"),
            Self::Parameter(name) => write!(formatter, "Invalid request parameter: {name}"),
            Self::Declaration(reason) => write!(formatter, "Invalid plugin declaration: {reason}"),
        }
    }
}

impl std::error::Error for RequestError {}

/// The offered request `name`.
#[must_use]
pub fn offered(name: &str) -> Option<Kind> {
    OFFERED
        .iter()
        .find(|(offered, _)| *offered == name)
        .map(|(_, kind)| *kind)
}

/// The requests a plugin declared, checked against the offered set.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Declaration(Vec<String>);

impl Declaration {
    /// Read `requests` from a plugin manifest; every name must be offered, once.
    pub fn parse(manifest: &Value) -> Result<Self, RequestError> {
        let names = manifest["requests"]
            .as_array()
            .ok_or(RequestError::Declaration("requests must be a list"))?;
        let mut declared = Vec::with_capacity(names.len());
        for name in names {
            let name = name
                .as_str()
                .ok_or(RequestError::Declaration("request names are strings"))?;
            if offered(name).is_none() {
                return Err(RequestError::Unknown(name.to_owned()));
            }
            if declared.iter().any(|seen| seen == name) {
                return Err(RequestError::Declaration("duplicate request"));
            }
            declared.push(name.to_owned());
        }
        Ok(Self(declared))
    }

    /// The kind of `name`, if the plugin declared it.
    pub fn kind(&self, name: &str) -> Result<Kind, RequestError> {
        if !self.0.iter().any(|declared| declared == name) {
            return Err(RequestError::Undeclared(name.to_owned()));
        }
        offered(name).ok_or_else(|| RequestError::Unknown(name.to_owned()))
    }

    #[must_use]
    pub fn names(&self) -> &[String] {
        &self.0
    }
}

/// Fill a route template's `{name}` segments from `params`. Each value must be an
/// identifier (a UUID for `session_id`), so it cannot leave its segment; unused
/// parameters are refused.
pub fn path(template: &str, params: &Value) -> Result<String, RequestError> {
    let empty = serde_json::Map::new();
    let params = match params {
        Value::Object(params) => params,
        Value::Null => &empty,
        _ => return Err(RequestError::Parameter("params".into())),
    };
    let mut used = 0;
    let mut path = String::with_capacity(template.len());
    for segment in template.split('/').skip(1) {
        path.push('/');
        let Some(name) = segment
            .strip_prefix('{')
            .and_then(|rest| rest.strip_suffix('}'))
        else {
            path.push_str(segment);
            continue;
        };
        let value = params
            .get(name)
            .and_then(Value::as_str)
            .filter(|value| {
                if name == "session_id" {
                    valid_uuid(value)
                } else {
                    valid_id(value)
                }
            })
            .ok_or_else(|| RequestError::Parameter(name.to_owned()))?;
        path.push_str(value);
        used += 1;
    }
    if used != params.len() {
        return Err(RequestError::Parameter("unexpected parameter".into()));
    }
    Ok(path)
}

/// 1–128 ASCII letters, digits, `_` or `-`.
#[must_use]
pub fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

/// A hyphenated UUID.
#[must_use]
pub fn valid_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use serde_json::json;

    #[test]
    fn declarations_name_only_offered_requests_once() {
        let declared =
            Declaration::parse(&json!({"requests":["agents.list","sessions.watch"]})).unwrap();
        assert_eq!(
            declared.kind("agents.list"),
            Ok(Kind::Route("GET", "/models/agents"))
        );
        assert_eq!(
            declared.kind("skills.list"),
            Err(RequestError::Undeclared("skills.list".into()))
        );
        assert_eq!(
            Declaration::parse(&json!({"requests":["accounts.delete"]})),
            Err(RequestError::Unknown("accounts.delete".into()))
        );
        assert!(Declaration::parse(&json!({"requests":["agents.list","agents.list"]})).is_err());
        assert!(Declaration::parse(&json!({})).is_err());
    }

    #[test]
    fn route_parameters_stay_inside_their_segment() {
        let template = "/agents/{agent_id}/abilities/{ability_id}/visibility";
        assert_eq!(
            path(
                template,
                &json!({"agent_id":"a-1","ability_id":"web_access"})
            )
            .unwrap(),
            "/agents/a-1/abilities/web_access/visibility"
        );
        for bad in ["../x", "a/b", "a?b=1", "a%2f", ""] {
            assert!(
                path(template, &json!({"agent_id":bad,"ability_id":"x"})).is_err(),
                "{bad}"
            );
        }
        assert!(path(template, &json!({"agent_id":"a"})).is_err(), "missing");
        assert!(path(
            template,
            &json!({"agent_id":"a","ability_id":"b","extra":"c"})
        )
        .is_err());
        assert!(path(
            "/sessions/{session_id}",
            &json!({"session_id":"not-a-uuid"})
        )
        .is_err());
        assert_eq!(path("/sessions", &Value::Null).unwrap(), "/sessions");
    }

    #[test]
    fn offered_names_are_unique() {
        let mut names: Vec<&str> = OFFERED.iter().map(|(name, _)| *name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), OFFERED.len());
    }
}
