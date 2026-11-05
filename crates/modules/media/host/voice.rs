//! Composer dictation with a shared local recognizer, and agent voice output.
//! Capture, dictation sequencing and speech formatting live in `webagent_media`; this root-bound
//! adapter supplies the HTTP handlers, App/Store, the session gate and model lanes.
//! Dictation runs on one shared Parakeet recognizer (`webagent_media::recognizer`, this binary
//! started with `--voice-service`) and voice output on local Kokoro speech (`speak.py`); the
//! `voice` stage of the Dockerfile packages their libraries and models.
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::Path;
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use futures_util::stream;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdout, Command};
use tokio::sync::{mpsc, oneshot, watch, Mutex, OwnedSemaphorePermit, Semaphore};
use webagent_media::capture::{self, Capture, Chunk};
use webagent_media::speech::{sample_text, speech_text};

use super::media_lane;
use crate::{
    bad, conflict, model_selection, required, session_agent_id, uuid, Api, ApiError, App, State,
};

/// Drop this caller's dictation for `sid`; a result still in flight can no longer land in it.
pub(crate) async fn quiesce(app: &App, sid: &str) {
    dictations().lock().await.remove(&dictation_key(app, sid));
}
/// One caller's dictation in one conversation; neither id can be read as part of the other.
fn dictation_key(app: &App, sid: &str) -> String {
    format!("{}\0{sid}", app.store.current_user_id())
}
/// One composer dictation: frame sequencing and its stream in the shared recognizer. Dropping
/// an unfinished one cancels that stream and frees its slot.
struct Dictation {
    capture: Capture,
    requests: mpsc::UnboundedSender<String>,
    routes: Routes,
    /// The latest transcript so far, written by the service reader.
    text: watch::Receiver<String>,
    /// Receives the final transcript; taken by the first finish.
    finished: Option<oneshot::Receiver<String>>,
    used: Instant,
    /// The final transcript, kept so a retried finish replays it.
    done: Option<String>,
    /// Held while the dictation is open; released at finish.
    slot: Option<OwnedSemaphorePermit>,
}

impl Drop for Dictation {
    fn drop(&mut self) {
        if let Ok(mut routes) = self.routes.lock() {
            routes.remove(&self.capture.id);
        }
        if self.done.is_none() {
            send(
                &self.requests,
                json!({"op": "cancel", "id": self.capture.id}),
            );
        }
    }
}

type Dictations = HashMap<String, Arc<Mutex<Dictation>>>;

fn dictations() -> &'static Mutex<Dictations> {
    static ALL: OnceLock<Mutex<Dictations>> = OnceLock::new();
    ALL.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The shared recognizer: this binary in its `--voice-service` role.
#[cfg(not(test))]
fn recognizer() -> std::io::Result<Command> {
    let mut command = Command::new(std::env::current_exe()?);
    command.arg("--voice-service");
    Ok(command)
}
/// Stands in for the service: ignores audio and answers each `finish` with one transcript.
#[cfg(test)]
fn recognizer() -> std::io::Result<Command> {
    let mut command = Command::new("/bin/sh");
    command.args([
        "-c",
        r#"while read -r line; do case "$line" in *'"op":"finish"'*) id=${line#*'"id":"'}; echo "{\"id\":\"${id%%\"*}\",\"final\":\"Hello there.\"}";; esac; done"#,
    ]);
    Ok(command)
}
/// Dictations open at once; the shared service decodes their phrases one at a time (about
/// 0.3 s each on two threads), so this bounds how long a phrase can wait.
const MAX_DICTATIONS: usize = 4;
/// A dictation with no frames for this long was abandoned; dropping it cancels its stream.
const DICTATION_IDLE: Duration = Duration::from_secs(90);
const DICTATION_STOPPED: &str = "Dictation stopped; start it again";

fn dictation_slots() -> Arc<Semaphore> {
    static SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    SLOTS
        .get_or_init(|| Arc::new(Semaphore::new(MAX_DICTATIONS)))
        .clone()
}

/// Transcript updates and the final result of one open dictation, filled by the service reader.
struct Route {
    text: watch::Sender<String>,
    done: Option<oneshot::Sender<String>>,
}
type Routes = Arc<std::sync::Mutex<HashMap<String, Route>>>;

/// The running recognizer service. Requests go through `requests` to the task that owns its
/// stdin, so frames from every dictation reach it in the order they were accepted.
struct Service {
    requests: mpsc::UnboundedSender<String>,
    routes: Routes,
    child: Child,
}

/// Queue one request line for the service; false once its input has closed.
fn send(requests: &mpsc::UnboundedSender<String>, request: Value) -> bool {
    requests.send(format!("{request}\n")).is_ok()
}

/// The running service, started when missing or exited. The models load in a few seconds;
/// requests queue in its input meanwhile. Replacing an exited service drops its dictations, so
/// their clients restart them with the whole recording.
async fn running_service() -> std::io::Result<(mpsc::UnboundedSender<String>, Routes)> {
    static SERVICE: OnceLock<Mutex<Option<Service>>> = OnceLock::new();
    let mut slot = SERVICE.get_or_init(|| Mutex::new(None)).lock().await;
    if let Some(service) = slot.as_mut() {
        if matches!(service.child.try_wait(), Ok(None)) && !service.requests.is_closed() {
            return Ok((service.requests.clone(), service.routes.clone()));
        }
    }
    let mut child = recognizer()?
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let missing = || std::io::Error::other("recognizer pipes unavailable");
    let mut stdin = child.stdin.take().ok_or_else(missing)?;
    let stdout = child.stdout.take().ok_or_else(missing)?;
    let (requests, mut queue) = mpsc::unbounded_channel::<String>();
    tokio::spawn(async move {
        while let Some(line) = queue.recv().await {
            if stdin.write_all(line.as_bytes()).await.is_err() {
                break;
            }
        }
    });
    let routes = Routes::default();
    tokio::spawn(read_replies(stdout, routes.clone()));
    *slot = Some(Service {
        requests: requests.clone(),
        routes: routes.clone(),
        child,
    });
    Ok((requests, routes))
}

/// Route each service reply to its dictation until the service closes its output, then drop
/// every route so waiting finishes fail instead of hanging.
async fn read_replies(stdout: ChildStdout, routes: Routes) {
    let mut lines = BufReader::new(stdout).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        route_reply(&routes, &line);
    }
    if let Ok(mut routes) = routes.lock() {
        routes.clear();
    }
}

/// Apply one reply: `text` is the transcript so far, `final` completes the dictation, and
/// `error` ends it. Replies for unknown or closed dictations are ignored.
fn route_reply(routes: &Routes, line: &str) {
    let Ok(reply) = serde_json::from_str::<Value>(line) else {
        return;
    };
    let (Some(id), Ok(mut routes)) = (reply["id"].as_str(), routes.lock()) else {
        return;
    };
    let Some(route) = routes.get_mut(id) else {
        return;
    };
    if let Some(text) = reply["text"].as_str() {
        route.text.send_replace(text.to_owned());
    }
    if let Some(text) = reply["final"].as_str() {
        route.text.send_replace(text.to_owned());
        if let Some(done) = route.done.take() {
            let _ = done.send(text.to_owned());
        }
        routes.remove(id);
    } else if reply.get("error").is_some() {
        routes.remove(id);
    }
}

fn dictation_unavailable() -> ApiError {
    ApiError(
        StatusCode::SERVICE_UNAVAILABLE,
        "Dictation is unavailable".into(),
    )
}

fn dictation_stopped() -> ApiError {
    ApiError(StatusCode::SERVICE_UNAVAILABLE, DICTATION_STOPPED.into())
}

/// Composer dictation: `start` opens a capture in the shared recognizer, each `chunk` streams a
/// PCM frame and returns the transcript so far (it grows phrase by phrase at pauses), `finish`
/// returns the final transcript and `cancel` discards it. Nothing is stored; the client types
/// the text into the composer.
pub(crate) async fn voice_audio(
    State(app): State<App>,
    Path(sid): Path<String>,
    Json(body): Json<Value>,
) -> Api {
    session_agent_id(&app, &sid).await?;
    let op = required(&body, "op", 16)?;
    let k = dictation_key(&app, &sid);
    if op == "start" {
        let mut all = dictations().lock().await;
        // A new start replaces this caller's previous dictation in the conversation.
        all.remove(&k);
        let now = Instant::now();
        let mut stale = vec![];
        for (key, entry) in all.iter() {
            if entry
                .try_lock()
                .is_ok_and(|d| now.duration_since(d.used) > DICTATION_IDLE)
            {
                stale.push(key.clone());
            }
        }
        for key in stale {
            all.remove(&key);
        }
        let slot = dictation_slots().try_acquire_owned().map_err(|_| {
            ApiError(
                StatusCode::SERVICE_UNAVAILABLE,
                "Dictation is busy; try again shortly".into(),
            )
        })?;
        let (requests, routes) = running_service()
            .await
            .map_err(|_| dictation_unavailable())?;
        let id = uuid();
        let (sender, text) = watch::channel(String::new());
        let (done, finished) = oneshot::channel();
        routes.lock().map_err(|_| dictation_unavailable())?.insert(
            id.clone(),
            Route {
                text: sender,
                done: Some(done),
            },
        );
        let dictation = Dictation {
            capture: Capture::new(id.clone()),
            requests,
            routes,
            text,
            finished: Some(finished),
            used: now,
            done: None,
            slot: Some(slot),
        };
        if !send(&dictation.requests, json!({"op": "open", "id": id})) {
            return Err(dictation_unavailable());
        }
        all.insert(k, Arc::new(Mutex::new(dictation)));
        return Ok(Json(
            json!({"capture_id": id, "sample_rate": capture::SAMPLE_RATE, "ephemeral": true}),
        ));
    }
    let id = required(&body, "capture_id", 64)?;
    let entry = dictations().lock().await.get(&k).cloned();
    if op == "cancel" {
        if let Some(entry) = entry {
            if entry.lock().await.capture.id == id {
                dictations().lock().await.remove(&k);
            }
        }
        return Ok(Json(json!({"cancelled": true, "ephemeral": true})));
    }
    let entry = entry.ok_or_else(|| conflict("Dictation expired; start it again"))?;
    let mut d = entry.lock().await;
    if d.capture.id != id {
        return Err(conflict("Dictation expired; start it again"));
    }
    if op == "chunk" {
        let data = required(&body, "data", 128 * 1024)?;
        let bytes = STANDARD
            .decode(data)
            .map_err(|_| bad("Invalid PCM frame"))?;
        if !capture::valid_frame(&bytes) {
            return Err(bad("Invalid PCM frame bounds"));
        }
        let sequence = body["seq"]
            .as_u64()
            .ok_or_else(|| bad("PCM sequence required"))?;
        if d.done.is_some() {
            return Err(conflict("Dictation already finished"));
        }
        let replayed = match d.capture.accept(sequence, &bytes) {
            Ok(Chunk::Replayed { .. }) => true,
            Ok(Chunk::Accepted) => false,
            Err(_) => return Err(bad("PCM sequence or dictation length limit exceeded")),
        };
        if !replayed {
            // The validated base64 frame goes to the service as received.
            if !send(&d.requests, json!({"op": "audio", "id": id, "pcm": data})) {
                drop(d);
                dictations().lock().await.remove(&k);
                return Err(dictation_stopped());
            }
            d.used = Instant::now();
        }
        let text = d.text.borrow().clone();
        return Ok(Json(json!({
            "seq": sequence,
            "replayed": replayed,
            "text": text,
            "ephemeral": true,
        })));
    }
    if op != "finish" {
        return Err(bad("Unknown voice audio operation"));
    }
    if let Some(text) = &d.done {
        return Ok(Json(json!({
            "text": text,
            "capture_id": id,
            "ephemeral": true,
            "replayed": true,
        })));
    }
    // The service decodes the last phrase after the frames queued before this request.
    let finished = d.finished.take().ok_or_else(dictation_unavailable)?;
    let text = if send(&d.requests, json!({"op": "finish", "id": id})) {
        tokio::time::timeout(Duration::from_secs(15), finished)
            .await
            .ok()
            .and_then(Result::ok)
    } else {
        None
    };
    let Some(text) = text else {
        drop(d);
        dictations().lock().await.remove(&k);
        return Err(dictation_stopped());
    };
    d.slot = None;
    d.done = Some(text.clone());
    Ok(Json(
        json!({"text": text, "capture_id": id, "ephemeral": true}),
    ))
}

pub(crate) async fn voice_capabilities(State(app): State<App>, Path(sid): Path<String>) -> Api {
    let voice = media_lane::lane_run(&app, &sid, model_selection::ModelRole::Voice).await;
    let configured = if let Ok(run) = voice {
        media_lane::lane_selection(&app, &run).await.is_ok()
            && run["selection_plan"]["candidates"][0]["transport"] != "openai_subscription"
    } else {
        false
    };
    let tts_available =
        std::env::var("V3_VOICE_TTS_API_KEY").is_ok_and(|key| !key.trim().is_empty());
    let agent = session_agent_id(&app, &sid).await?;
    let voice_output = crate::agents_api::voice_output(&app.store, &agent).await?;
    Ok(Json(json!({
        "voice_configured": configured,
        "streaming": true,
        "tts_available": tts_available,
        "browser_fallback": true,
        "voice_output": voice_output.to_value(),
    })))
}

/// The local Kokoro speaker; its packaging is the `voice` stage of the Dockerfile.
const SPEAKER: &str = "/opt/voice/venv/bin/python";
const SPEAKER_ARGS: [&str; 2] = ["-I", "/opt/voice/speak.py"];
/// Larger than any MP3 chunk the speaker writes for a bounded reply.
const MAX_CHUNK_BYTES: u32 = 16 << 20;
const SPEECH_FAILED: &str = "Voice reply stopped; the reply is still on screen";

/// Kokoro uses every core, so one synthesis runs at a time and later ones queue.
fn speech_slot() -> Arc<Semaphore> {
    static SLOT: OnceLock<Arc<Semaphore>> = OnceLock::new();
    SLOT.get_or_init(|| Arc::new(Semaphore::new(1))).clone()
}

fn speech_unavailable() -> ApiError {
    ApiError(
        StatusCode::SERVICE_UNAVAILABLE,
        "Voice output is unavailable".into(),
    )
}

/// A running speaker; dropping it (for example when the client disconnects) kills the process
/// and frees the speech slot.
struct Speaker {
    _slot: OwnedSemaphorePermit,
    child: Child,
    stdout: ChildStdout,
}

/// Wait up to two minutes for the speech slot, then start the speaker on `text` in `voice`.
/// Callers have bounded the text and validated the voice.
async fn start_speaker(text: &str, voice: &str) -> Result<Speaker, ApiError> {
    let slot = tokio::time::timeout(Duration::from_secs(120), speech_slot().acquire_owned())
        .await
        .map_err(|_| {
            ApiError(
                StatusCode::SERVICE_UNAVAILABLE,
                "Voice is busy; try again shortly".into(),
            )
        })?
        .map_err(anyhow::Error::from)?;
    let mut child = Command::new(SPEAKER)
        .args(SPEAKER_ARGS)
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| speech_unavailable())?;
    let mut stdin = child.stdin.take().ok_or_else(speech_unavailable)?;
    let request = json!({"text": text, "voice": voice}).to_string();
    stdin
        .write_all(request.as_bytes())
        .await
        .map_err(|_| speech_unavailable())?;
    drop(stdin);
    let stdout = child.stdout.take().ok_or_else(speech_unavailable)?;
    Ok(Speaker {
        _slot: slot,
        child,
        stdout,
    })
}

/// The next length-prefixed MP3 chunk, or `None` once the speaker has closed its output.
async fn next_chunk(out: &mut (impl AsyncRead + Unpin)) -> std::io::Result<Option<Vec<u8>>> {
    let length = match out.read_u32().await {
        Ok(length) => length,
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error),
    };
    if length == 0 || length > MAX_CHUNK_BYTES {
        return Err(std::io::ErrorKind::InvalidData.into());
    }
    let mut chunk = vec![0; length as usize];
    out.read_exact(&mut chunk).await?;
    Ok(Some(chunk))
}

/// NDJSON lines: first the speaker's plan, `{"plan": {"chunks": [characters per chunk]}}`, then
/// one `{"audio": <base64 MP3>}` per chunk as it is synthesized, or a final `{"error": ...}` when
/// the speaker fails. A clean exit ends the stream.
fn speech_lines(speaker: Speaker) -> Body {
    Body::from_stream(stream::unfold(Some((speaker, true)), |state| async move {
        let (mut speaker, first) = state?;
        let line = match next_chunk(&mut speaker.stdout).await {
            Ok(Some(plan)) if first => match serde_json::from_slice::<Value>(&plan) {
                Ok(plan) => json!({"plan": plan}),
                Err(_) => json!({"error": SPEECH_FAILED}),
            },
            Ok(Some(mp3)) => json!({"audio": STANDARD.encode(mp3)}),
            Ok(None) if speaker.child.wait().await.is_ok_and(|s| s.success()) => return None,
            Ok(None) | Err(_) => json!({"error": SPEECH_FAILED}),
        };
        let more = line.get("error").is_none().then_some((speaker, false));
        Some((Ok::<_, std::io::Error>(format!("{line}\n")), more))
    }))
}

/// Speak a reply in the session agent's voice when the agent has voice output on. Streams
/// [`speech_lines`] so the client can start playing after the first sentence.
pub(crate) async fn voice_reply(
    State(app): State<App>,
    Path(sid): Path<String>,
    Json(body): Json<Value>,
) -> Result<Response, ApiError> {
    let text = speech_text(required(&body, "text", 8192)?);
    if text.is_empty() {
        return Err(bad("Speech text is empty"));
    }
    if app.shutoff().await? {
        return Err(conflict("Global shutoff is active"));
    }
    let agent = session_agent_id(&app, &sid).await?;
    let output = crate::agents_api::voice_output(&app.store, &agent).await?;
    if !output.enabled {
        return Err(conflict("Voice output is off for this agent"));
    }
    let speaker = start_speaker(&text, &output.voice).await?;
    Ok((
        [
            (CONTENT_TYPE, "application/x-ndjson"),
            (CACHE_CONTROL, "no-store"),
        ],
        speech_lines(speaker),
    )
        .into_response())
}

/// A short MP3 introduction in `voice` for the Identity voice picker, cached after first use.
pub(crate) async fn voice_sample(
    State(app): State<App>,
    Path(voice): Path<String>,
) -> Result<Response, ApiError> {
    static SAMPLES: OnceLock<Mutex<HashMap<String, Vec<u8>>>> = OnceLock::new();
    let text = sample_text(&voice)
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "Unknown voice".into()))?;
    let samples = SAMPLES.get_or_init(Default::default);
    let cached = samples.lock().await.get(&voice).cloned();
    let mp3 = match cached {
        Some(mp3) => mp3,
        None => {
            if app.shutoff().await? {
                return Err(conflict("Global shutoff is active"));
            }
            let mut speaker = start_speaker(&text, &voice).await?;
            // The first frame is the plan; a sample is one short chunk.
            next_chunk(&mut speaker.stdout)
                .await
                .map_err(|_| speech_unavailable())?;
            let mut mp3 = Vec::new();
            while let Some(chunk) = next_chunk(&mut speaker.stdout)
                .await
                .map_err(|_| speech_unavailable())?
            {
                mp3.extend(chunk);
            }
            if !speaker.child.wait().await.is_ok_and(|s| s.success()) {
                return Err(speech_unavailable());
            }
            samples.lock().await.insert(voice, mp3.clone());
            mp3
        }
    };
    Ok((
        [
            (CONTENT_TYPE, "audio/mpeg"),
            (CACHE_CONTROL, "private, max-age=86400"),
        ],
        mp3,
    )
        .into_response())
}

#[cfg(test)]
mod voice_output_tests {
    // Test code may unwrap (the standard's `allow-unwrap-in-tests`).
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn service_replies_reach_only_their_dictation() {
        let routes = Routes::default();
        let (text, seen) = watch::channel(String::new());
        let (done, mut finished) = oneshot::channel();
        routes.lock().unwrap().insert(
            "a".into(),
            Route {
                text,
                done: Some(done),
            },
        );
        route_reply(&routes, r#"{"id":"b","text":"Not mine."}"#);
        route_reply(&routes, "not json");
        assert_eq!(*seen.borrow(), "");
        route_reply(&routes, r#"{"id":"a","text":"Hello."}"#);
        assert_eq!(*seen.borrow(), "Hello.");
        assert!(finished.try_recv().is_err());
        route_reply(&routes, r#"{"id":"a","final":"Hello there."}"#);
        assert_eq!(finished.try_recv().unwrap(), "Hello there.");
        assert!(routes.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn speaker_chunks_are_length_prefixed_and_bounded() {
        let mut frames: &[u8] = &[0, 0, 0, 2, 7, 8, 0, 0, 0, 1, 9];
        assert_eq!(next_chunk(&mut frames).await.unwrap(), Some(vec![7, 8]));
        assert_eq!(next_chunk(&mut frames).await.unwrap(), Some(vec![9]));
        assert_eq!(next_chunk(&mut frames).await.unwrap(), None);
        for bad in [&[0u8, 0, 0, 0][..], &[2, 0, 0, 0], &[0, 0, 0, 5, 1]] {
            let mut frames = bad;
            assert!(next_chunk(&mut frames).await.is_err());
        }
    }

    #[tokio::test]
    async fn voice_reply_follows_the_agent_setting() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let sid = crate::create_session(
            State(app.clone()),
            Json(json!({"client_request_id":uuid(),"agent_id":"shared_default"})),
        )
        .await
        .unwrap()
        .0["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let Json(capabilities) = voice_capabilities(State(app.clone()), Path(sid.clone()))
            .await
            .unwrap();
        assert_eq!(capabilities["voice_output"]["enabled"], false);
        let reply = |text: &str| {
            voice_reply(
                State(app.clone()),
                Path(sid.clone()),
                Json(json!({"text": text})),
            )
        };
        assert_eq!(reply("Hello.").await.err().unwrap().0, StatusCode::CONFLICT);
        assert_eq!(reply("   ").await.err().unwrap().0, StatusCode::BAD_REQUEST);
        assert_eq!(
            voice_sample(State(app.clone()), Path("../speak".into()))
                .await
                .err()
                .unwrap()
                .0,
            StatusCode::NOT_FOUND
        );
    }
}
