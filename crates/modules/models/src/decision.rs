//! System One decision requests: questions with fixed options answered with probabilities,
//! no generated text. TypeSafe's Jev serves `POST {base_url}/v1/systemone`; self-hosted
//! servers with the same request shape (Laya, Kev) can stand in for it. The base URL comes
//! from the provider's file, the key from the caller's credential. Failures carry a
//! [`FailureKind`] so a caller can move on to its next model.
use crate::model_selection::FailureKind;
use crate::transport::typed_failure;
use anyhow::{bail, Context, Result};
use serde_json::{json, Map, Value};
use std::{collections::BTreeMap, sync::OnceLock, time::Duration};

const MAX_REQUEST: usize = 64 * 1024;
const MAX_RESPONSE: usize = 512 * 1024;

/// One answered `choice` question.
#[derive(Clone, Debug, PartialEq)]
pub struct Choice {
    pub choice: String,
    pub confidence: f64,
}

/// The request body: `state` is what the model judges, `questions` maps each question name
/// to its options (option name to description, or null).
pub fn request(
    model: &str,
    state: Value,
    questions: &BTreeMap<String, Map<String, Value>>,
) -> Value {
    let questions: Map<String, Value> = questions
        .iter()
        .map(|(name, options)| {
            (
                name.clone(),
                json!({"type":"choice","criteria":Value::Object(options.clone())}),
            )
        })
        .collect();
    json!({"model":model,"state":state,"questions":questions})
}

/// The endpoint for a provider's `base_url`: HTTPS, or plain HTTP only to this machine or a
/// Tailscale address (a self-hosted model on the private network).
pub fn endpoint(base_url: &str) -> Result<String> {
    let base = base_url.trim_end_matches('/');
    let host = base
        .strip_prefix("https://")
        .or_else(|| {
            base.strip_prefix("http://")
                .filter(|rest| private_host(rest))
        })
        .context("Decision provider address must be HTTPS or a private HTTP address")?;
    if host.is_empty() || host.contains(['?', '#', '@']) || base.len() > 200 {
        bail!("Invalid decision provider address");
    }
    Ok(format!("{base}/v1/systemone"))
}

fn private_host(rest: &str) -> bool {
    let host = rest.split(['/', ':']).next().unwrap_or_default();
    if host == "localhost" || host == "127.0.0.1" {
        return true;
    }
    let octets: Vec<u8> = host
        .split('.')
        .filter_map(|part| part.parse().ok())
        .collect();
    // 100.64.0.0/10: the Tailscale range.
    octets.len() == 4 && octets[0] == 100 && (64..128).contains(&octets[1])
}

/// The choice answers of `answer`, checked against the questions of `body`: every question
/// answered with one of its options and a confidence between 0 and 1.
pub fn choices(body: &Value, answer: &Value) -> Result<BTreeMap<String, Choice>> {
    let questions = body["questions"].as_object().context("Questions missing")?;
    let answers = answer["answers"]
        .as_object()
        .context("Decision answers missing")?;
    let mut found = BTreeMap::new();
    for (name, question) in questions {
        let value = answers.get(name).context("Decision answer missing")?;
        let choice = value["choice"]
            .as_str()
            .context("Malformed choice answer")?;
        let confidence = value["confidence"]
            .as_f64()
            .context("Malformed choice answer")?;
        if !question["criteria"]
            .as_object()
            .is_some_and(|options| options.contains_key(choice))
            || !(0.0..=1.0).contains(&confidence)
        {
            bail!("Decision answer outside the offered options");
        }
        found.insert(
            name.clone(),
            Choice {
                choice: choice.to_owned(),
                confidence,
            },
        );
    }
    Ok(found)
}

fn client() -> Result<&'static reqwest::Client> {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    if let Some(client) = CLIENT.get() {
        return Ok(client);
    }
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .build()?;
    let _ = CLIENT.set(client);
    Ok(CLIENT.get().expect("Decision client initialized"))
}

fn failure(status: u16) -> anyhow::Error {
    let kind = match status {
        401 | 403 => FailureKind::InvalidCredentials,
        402 | 429 => FailureKind::QuotaExhausted,
        404 => FailureKind::ModelUnavailable,
        400 | 413 | 422 => FailureKind::MalformedRequest,
        _ => FailureKind::Transient,
    };
    typed_failure(kind, false, format!("Decision provider HTTP {status}"))
}

/// Send `body` to the System One endpoint of `base_url` with bearer `key` (when given) and
/// return its checked choices, within `limit`.
pub async fn ask(
    base_url: &str,
    key: Option<&str>,
    body: &Value,
    limit: Duration,
) -> Result<BTreeMap<String, Choice>> {
    let url = endpoint(base_url)?;
    let bytes = serde_json::to_vec(body)?;
    if bytes.len() > MAX_REQUEST {
        return Err(typed_failure(
            FailureKind::MalformedRequest,
            false,
            "Decision request exceeds 64 KiB",
        ));
    }
    let mut request = client()?
        .post(url)
        .header("content-type", "application/json")
        .timeout(limit)
        .body(bytes);
    if let Some(key) = key.filter(|key| !key.is_empty()) {
        request = request.bearer_auth(key);
    }
    let mut response = request.send().await.map_err(|error| {
        typed_failure(
            FailureKind::Transient,
            false,
            if error.is_timeout() {
                "Decision provider timed out"
            } else {
                "Decision provider unavailable"
            },
        )
    })?;
    if response.status() != reqwest::StatusCode::OK {
        return Err(failure(response.status().as_u16()));
    }
    let mut raw = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| typed_failure(FailureKind::Transient, true, "Decision response cut off"))?
    {
        if raw.len() + chunk.len() > MAX_RESPONSE {
            return Err(typed_failure(
                FailureKind::Protocol,
                true,
                "Decision response exceeds limit",
            ));
        }
        raw.extend_from_slice(&chunk);
    }
    let answer: Value = serde_json::from_slice(&raw).map_err(|_| {
        typed_failure(
            FailureKind::Protocol,
            true,
            "Invalid decision response JSON",
        )
    })?;
    choices(body, &answer)
        .map_err(|error| typed_failure(FailureKind::Protocol, true, error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body() -> Value {
        let mut questions = BTreeMap::new();
        let mut options = Map::new();
        options.insert("requested".into(), json!("asked for"));
        options.insert("unclear".into(), Value::Null);
        questions.insert("asked".to_owned(), options);
        request("jev-latest", json!({"x":1}), &questions)
    }

    #[test]
    fn requests_name_each_choice_question() {
        let body = body();
        assert_eq!(body["model"], "jev-latest");
        assert_eq!(body["questions"]["asked"]["type"], "choice");
        assert_eq!(
            body["questions"]["asked"]["criteria"]["unclear"],
            Value::Null
        );
    }

    #[test]
    fn answers_must_pick_an_offered_option_with_a_bounded_confidence() {
        let body = body();
        let ok =
            json!({"answers":{"asked":{"type":"choice","choice":"requested","confidence":0.93}}});
        assert_eq!(
            choices(&body, &ok).unwrap()["asked"],
            Choice {
                choice: "requested".into(),
                confidence: 0.93
            }
        );
        for bad in [
            json!({"answers":{}}),
            json!({"answers":{"asked":{"choice":"yes","confidence":0.9}}}),
            json!({"answers":{"asked":{"choice":"requested","confidence":1.5}}}),
            json!({"answers":{"asked":{"choice":"requested"}}}),
        ] {
            assert!(choices(&body, &bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn endpoints_need_https_or_a_private_address() {
        assert_eq!(
            endpoint("https://api.typesafe.ai/").unwrap(),
            "https://api.typesafe.ai/v1/systemone"
        );
        assert!(endpoint("http://100.126.37.96:8000").is_ok());
        assert!(endpoint("http://localhost:8000").is_ok());
        for bad in [
            "http://example.com",
            "ftp://x",
            "https://",
            "https://u@x",
            "http://100.200.1.1",
        ] {
            assert!(endpoint(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn http_statuses_map_to_failover_kinds() {
        let kind = |status| {
            failure(status)
                .downcast_ref::<crate::transport::ProviderFailure>()
                .map(|f| f.kind)
        };
        assert_eq!(kind(401), Some(FailureKind::InvalidCredentials));
        assert_eq!(kind(429), Some(FailureKind::QuotaExhausted));
        assert_eq!(kind(503), Some(FailureKind::Transient));
    }
}
