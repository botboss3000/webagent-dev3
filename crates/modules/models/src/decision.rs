//! System One decision requests: questions with fixed options answered with probabilities,
//! no generated text. TypeSafe's Jev serves `POST {base_url}/v1/systemone`; self-hosted
//! servers with the same request shape (Laya, Kev) can stand in for it. The base URL comes
//! from the provider's file, the key from the caller's credential. Self-hosted models of the
//! `local` provider are asked through the host's local models relay instead
//! (`build-service/local_models_relay.py`), which holds their addresses and keys. Failures
//! carry a [`FailureKind`] so a caller can move on to its next model.
use crate::model_selection::FailureKind;
use crate::transport::typed_failure;
use anyhow::{bail, Context, Result};
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::time::Instant;
use std::{collections::BTreeMap, sync::Mutex, sync::OnceLock, time::Duration};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};

const MAX_REQUEST: usize = 64 * 1024;
const MAX_RESPONSE: usize = 512 * 1024;

/// One answered `choice` question.
#[derive(Clone, Debug, PartialEq)]
pub struct Choice {
    pub choice: String,
    pub confidence: f64,
}

/// One choice question: what to answer, and its options (option name to description, or null).
#[derive(Clone, Debug, PartialEq)]
pub struct Question {
    pub instructions: String,
    pub options: Map<String, Value>,
}

/// The request body: `state` is what the model judges, `questions` maps each question name
/// to its question. Every question carries its instructions; some servers (Laya) refuse one
/// without them.
pub fn request(model: &str, state: Value, questions: &BTreeMap<String, Question>) -> Value {
    let questions: Map<String, Value> = questions
        .iter()
        .map(|(name, question)| {
            (
                name.clone(),
                json!({"type":"choice","instructions":question.instructions,
                    "criteria":Value::Object(question.options.clone())}),
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

/// The local models relay's socket, in the runtime folder the app's containers mount.
fn relay_socket() -> PathBuf {
    webagent_configuration::paths::runtime().join("local-models/relay.sock")
}

/// One request to the relay at `socket` and its reply, within `limit`. The relay answers one
/// JSON line per request; a reply larger than an answer plus its envelope is refused.
async fn relay_at(socket: &Path, request: &Value, limit: Duration) -> Result<Value> {
    let unavailable =
        |detail: &str| typed_failure(FailureKind::Transient, false, detail.to_owned());
    let exchange = async {
        let mut stream = tokio::net::UnixStream::connect(socket)
            .await
            .map_err(|_| unavailable("Local models relay unavailable"))?;
        let mut line = serde_json::to_vec(request)?;
        line.push(b'\n');
        stream
            .write_all(&line)
            .await
            .map_err(|_| unavailable("Local models relay unavailable"))?;
        let mut reply = Vec::new();
        tokio::io::BufReader::new(stream)
            .take((MAX_RESPONSE + 4096) as u64)
            .read_until(b'\n', &mut reply)
            .await
            .map_err(|_| unavailable("Local models relay cut off"))?;
        serde_json::from_slice::<Value>(&reply).map_err(|_| {
            typed_failure(
                FailureKind::Protocol,
                true,
                "Invalid local models relay reply",
            )
        })
    };
    tokio::time::timeout(limit, exchange)
        .await
        .map_err(|_| unavailable("Local model timed out"))?
}

/// A relay failure reply as a typed failure, so the caller can move on to its next model.
fn relay_failure(reply: &Value) -> anyhow::Error {
    let detail = reply["error"].as_str().unwrap_or("Local model failed");
    match reply["kind"].as_str() {
        Some("rejected") => failure(reply["status"].as_u64().unwrap_or(0) as u16),
        Some("unknown_model") => {
            typed_failure(FailureKind::ModelUnavailable, false, detail.to_owned())
        }
        Some("invalid") => typed_failure(FailureKind::MalformedRequest, false, detail.to_owned()),
        Some("too_large") => typed_failure(FailureKind::Protocol, true, detail.to_owned()),
        _ => typed_failure(FailureKind::Transient, false, detail.to_owned()),
    }
}

async fn ask_local_at(
    socket: &Path,
    model: &str,
    body: &Value,
    limit: Duration,
) -> Result<BTreeMap<String, Choice>> {
    if serde_json::to_vec(body)?.len() > MAX_REQUEST {
        return Err(typed_failure(
            FailureKind::MalformedRequest,
            false,
            "Decision request exceeds 64 KiB",
        ));
    }
    let reply = relay_at(
        socket,
        &json!({"op":"decide","model":model,"body":body}),
        limit,
    )
    .await?;
    if reply["ok"] != true {
        return Err(relay_failure(&reply));
    }
    choices(body, &reply["answer"])
        .map_err(|error| typed_failure(FailureKind::Protocol, true, error.to_string()))
}

/// Ask the local model `model` (its id in the relay's registry) through the relay and return
/// its checked choices, within `limit`.
pub async fn ask_local(
    model: &str,
    body: &Value,
    limit: Duration,
) -> Result<BTreeMap<String, Choice>> {
    ask_local_at(&relay_socket(), model, body, limit).await
}

async fn local_models_at(socket: &Path) -> Result<Vec<Value>> {
    let reply = relay_at(socket, &json!({"op":"models"}), Duration::from_secs(10)).await?;
    if reply["ok"] != true {
        return Err(relay_failure(&reply));
    }
    reply["models"]
        .as_array()
        .cloned()
        .context("Local models relay sent no model list")
}

/// The models the relay serves: id, name, capabilities, api, max_input_tokens,
/// needs_instructions and whether each answers its health check (`up`).
pub async fn local_models() -> Result<Vec<Value>> {
    local_models_at(&relay_socket()).await
}

/// How many input tokens the local model `model` reads. The relay's list is kept for five
/// minutes, since listing it waits on every model's health check.
pub async fn local_input_limit(model: &str) -> Result<usize> {
    static LIST: Mutex<Option<(Instant, Vec<Value>)>> = Mutex::new(None);
    let cached = LIST
        .lock()
        .expect("Local model list lock")
        .as_ref()
        .filter(|(at, _)| at.elapsed() < Duration::from_secs(300))
        .map(|(_, models)| models.clone());
    let models = match cached {
        Some(models) => models,
        None => {
            let models = local_models().await?;
            *LIST.lock().expect("Local model list lock") = Some((Instant::now(), models.clone()));
            models
        }
    };
    models
        .iter()
        .find(|m| m["id"] == model)
        .and_then(|m| m["max_input_tokens"].as_u64())
        .map(|limit| limit as usize)
        .ok_or_else(|| {
            typed_failure(
                FailureKind::ModelUnavailable,
                false,
                "Local model not in the relay registry",
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body() -> Value {
        let mut questions = BTreeMap::new();
        let mut options = Map::new();
        options.insert("requested".into(), json!("asked for"));
        options.insert("unclear".into(), Value::Null);
        questions.insert(
            "asked".to_owned(),
            Question {
                instructions: "Did the user ask?".into(),
                options,
            },
        );
        request("jev-latest", json!({"x":1}), &questions)
    }

    #[test]
    fn requests_name_each_choice_question() {
        let body = body();
        assert_eq!(body["model"], "jev-latest");
        assert_eq!(body["questions"]["asked"]["type"], "choice");
        assert_eq!(
            body["questions"]["asked"]["instructions"],
            "Did the user ask?"
        );
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

    /// A fake relay on a temporary socket: answers each connection with the next reply and
    /// records the request lines it read.
    fn fake_relay(
        replies: Vec<Value>,
    ) -> (
        tempfile::TempDir,
        PathBuf,
        tokio::task::JoinHandle<Vec<Value>>,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("relay.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let task = tokio::spawn(async move {
            let mut seen = Vec::new();
            for reply in replies {
                let (stream, _) = listener.accept().await.unwrap();
                let (read, mut write) = stream.into_split();
                let mut line = String::new();
                tokio::io::BufReader::new(read)
                    .read_line(&mut line)
                    .await
                    .unwrap();
                seen.push(serde_json::from_str(&line).unwrap());
                write
                    .write_all(format!("{reply}\n").as_bytes())
                    .await
                    .unwrap();
            }
            seen
        });
        (dir, socket, task)
    }

    fn kind_of(error: &anyhow::Error) -> Option<FailureKind> {
        error
            .downcast_ref::<crate::transport::ProviderFailure>()
            .map(|f| f.kind)
    }

    #[tokio::test]
    async fn local_decisions_go_through_the_relay_and_are_checked() {
        let answer = json!({"answers":{"asked":{"choice":"requested","confidence":0.8}}});
        let (_dir, socket, task) = fake_relay(vec![
            json!({"ok":true,"answer":answer}),
            json!({"ok":true,"answer":{"answers":{"asked":{"choice":"maybe","confidence":0.8}}}}),
        ]);
        let limit = Duration::from_secs(5);
        let choices = ask_local_at(&socket, "laya", &body(), limit).await.unwrap();
        assert_eq!(choices["asked"].choice, "requested");
        let error = ask_local_at(&socket, "laya", &body(), limit)
            .await
            .unwrap_err();
        assert_eq!(kind_of(&error), Some(FailureKind::Protocol));
        let seen = task.await.unwrap();
        assert_eq!(seen[0]["op"], "decide");
        assert_eq!(seen[0]["model"], "laya");
        assert_eq!(seen[0]["body"], body());
    }

    #[tokio::test]
    async fn relay_failures_map_to_failover_kinds() {
        let cases = [
            (
                json!({"ok":false,"kind":"unavailable","error":"down"}),
                FailureKind::Transient,
            ),
            (
                json!({"ok":false,"kind":"timeout","error":"slow"}),
                FailureKind::Transient,
            ),
            (
                json!({"ok":false,"kind":"unknown_model","error":"x"}),
                FailureKind::ModelUnavailable,
            ),
            (
                json!({"ok":false,"kind":"rejected","status":401,"error":"x"}),
                FailureKind::InvalidCredentials,
            ),
            (
                json!({"ok":false,"kind":"rejected","status":422,"error":"x"}),
                FailureKind::MalformedRequest,
            ),
        ];
        let (_dir, socket, _task) =
            fake_relay(cases.iter().map(|(reply, _)| reply.clone()).collect());
        for (_, expected) in cases {
            let error = ask_local_at(&socket, "laya", &body(), Duration::from_secs(5))
                .await
                .unwrap_err();
            assert_eq!(kind_of(&error), Some(expected));
        }
    }

    #[tokio::test]
    async fn a_missing_relay_is_transient_and_models_are_listed() {
        let dir = tempfile::tempdir().unwrap();
        let error = ask_local_at(
            &dir.path().join("none.sock"),
            "laya",
            &body(),
            Duration::from_secs(1),
        )
        .await
        .unwrap_err();
        assert_eq!(kind_of(&error), Some(FailureKind::Transient));
        let listed = json!([{"id":"laya","max_input_tokens":512,"up":true}]);
        let (_dir, socket, task) = fake_relay(vec![json!({"ok":true,"models":listed})]);
        assert_eq!(
            Value::Array(local_models_at(&socket).await.unwrap()),
            listed
        );
        assert_eq!(task.await.unwrap()[0], json!({"op":"models"}));
    }
}
