//! Composer dictation and agent voice replies on the local voice service.
//! Capture, speech formatting, the service itself and its live settings (`voice.json`) live in
//! `webagent_media`; this root-bound adapter supplies the HTTP handlers, App/Store, the session
//! gate and model lanes. The service is this binary started with `--voice-service ROLE`: one
//! process holds Parakeet for every dictation and another Kokoro for replies, each started on
//! first use and stopped after its idle time so the model's memory is returned. The `voice`
//! stage of the Dockerfile packages their libraries and models.
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
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
use futures_util::{stream, StreamExt};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdout, Command};
use tokio::sync::{mpsc, oneshot, watch, Mutex, OwnedMutexGuard};
use webagent_media::capture::{self, Capture, Chunk};
use webagent_media::settings::VoiceSettings;
use webagent_media::speech::{sample_text, speech_chunks, speech_text};

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
/// One composer dictation: frame sequencing and its stream in the dictation service. Dropping
/// an unfinished one cancels that stream and frees its slot.
struct Dictation {
    capture: Capture,
    link: Link,
    /// The latest transcript so far, written by the service reader.
    text: watch::Receiver<String>,
    /// Receives the final transcript; taken by the first finish.
    finished: Option<oneshot::Receiver<String>>,
    used: Instant,
    /// The final transcript, kept so a retried finish replays it.
    done: Option<String>,
    /// Held while the dictation is open; released at finish.
    slot: Option<Slot>,
}

impl Drop for Dictation {
    fn drop(&mut self) {
        if let Ok(mut routes) = self.link.routes.lock() {
            routes.remove(&self.capture.id);
        }
        if self.done.is_none() {
            self.link
                .send(json!({"op": "cancel", "id": self.capture.id}));
        }
    }
}

type Dictations = HashMap<String, Arc<Mutex<Dictation>>>;

fn dictations() -> &'static Mutex<Dictations> {
    static ALL: OnceLock<Mutex<Dictations>> = OnceLock::new();
    ALL.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Drop dictations with no audio for `idle`; dropping one cancels its stream.
fn drop_stale(all: &mut Dictations, idle: Duration) {
    let now = Instant::now();
    all.retain(|_, entry| {
        !entry
            .try_lock()
            .is_ok_and(|d| now.duration_since(d.used) > idle)
    });
}

/// Dictations open now, across all users.
static OPEN_DICTATIONS: AtomicUsize = AtomicUsize::new(0);

/// One open dictation's place under `dictation.max_concurrent`; dropping it frees the place.
struct Slot;

impl Slot {
    fn take(max: usize) -> Option<Self> {
        OPEN_DICTATIONS
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |open| {
                (open < max).then_some(open + 1)
            })
            .ok()
            .map(|_| Slot)
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        OPEN_DICTATIONS.fetch_sub(1, Ordering::SeqCst);
    }
}

/// The two voice service processes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Dictation,
    Speech,
}

impl Role {
    fn name(self) -> &'static str {
        match self {
            Role::Dictation => "dictation",
            Role::Speech => "speech",
        }
    }

    fn idle_unload_minutes(self, settings: &VoiceSettings) -> u32 {
        match self {
            Role::Dictation => settings.dictation.idle_unload_minutes,
            Role::Speech => settings.speech.idle_unload_minutes,
        }
    }
}

/// The voice service in `role`: this binary with `--voice-service`, given the live settings.
#[cfg(not(test))]
fn service_command(role: Role, settings: &str) -> std::io::Result<Command> {
    let mut command = Command::new(std::env::current_exe()?);
    command.args(["--voice-service", role.name(), settings]);
    Ok(command)
}
/// Stands in for the service: ignores audio and answers each `finish` with one transcript.
#[cfg(test)]
fn service_command(_role: Role, _settings: &str) -> std::io::Result<Command> {
    let mut command = Command::new("/bin/sh");
    command.args([
        "-c",
        r#"while read -r line; do case "$line" in *'"op":"finish"'*) id=${line#*'"id":"'}; echo "{\"id\":\"${id%%\"*}\",\"final\":\"Hello there.\"}";; esac; done"#,
    ]);
    Ok(command)
}
const DICTATION_STOPPED: &str = "Dictation stopped; start it again";

/// Where a service reply goes: a dictation's transcript, or a reply's stream of audio.
enum Route {
    Dictation {
        text: watch::Sender<String>,
        done: Option<oneshot::Sender<String>>,
    },
    Speech(mpsc::UnboundedSender<Value>),
}
type Routes = Arc<std::sync::Mutex<HashMap<String, Route>>>;

/// Milliseconds since the first use of the voice service, for idle times.
fn clock() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// A running service as its users hold it. Requests go through `requests` to the task that owns
/// its stdin, so they reach it in the order they were accepted.
#[derive(Clone)]
struct Link {
    requests: mpsc::UnboundedSender<String>,
    routes: Routes,
    /// [`clock`] at the last request.
    used: Arc<AtomicU64>,
}

impl Link {
    /// Queue one request line for the service; false once its input has closed.
    fn send(&self, request: Value) -> bool {
        self.used.store(clock(), Ordering::Relaxed);
        self.requests.send(format!("{request}\n")).is_ok()
    }

    /// True when nothing is open and the last request is more than `minutes` old (never for 0).
    fn unload_due(&self, minutes: u32, now: u64) -> bool {
        minutes > 0
            && self.routes.lock().is_ok_and(|routes| routes.is_empty())
            && now.saturating_sub(self.used.load(Ordering::Relaxed)) > u64::from(minutes) * 60_000
    }
}

struct Service {
    link: Link,
    child: Child,
}

fn service_slot(role: Role) -> &'static Mutex<Option<Service>> {
    static DICTATION: OnceLock<Mutex<Option<Service>>> = OnceLock::new();
    static SPEECH: OnceLock<Mutex<Option<Service>>> = OnceLock::new();
    match role {
        Role::Dictation => &DICTATION,
        Role::Speech => &SPEECH,
    }
    .get_or_init(|| Mutex::new(None))
}

/// The running service in `role`, started when missing or exited. The model loads in a few
/// seconds; requests queue in its input meanwhile. Replacing an exited service drops its open
/// work, so dictation clients restart with the whole recording and replies report an error.
async fn running_service(role: Role) -> std::io::Result<Link> {
    let mut slot = service_slot(role).lock().await;
    if let Some(service) = slot.as_mut() {
        if matches!(service.child.try_wait(), Ok(None)) && !service.link.requests.is_closed() {
            return Ok(service.link.clone());
        }
    }
    let settings = serde_json::to_string(&VoiceSettings::live())?;
    let mut child = service_command(role, &settings)?
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let missing = || std::io::Error::other("voice service pipes unavailable");
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
    let link = Link {
        requests,
        routes,
        used: Arc::new(AtomicU64::new(clock())),
    };
    *slot = Some(Service {
        link: link.clone(),
        child,
    });
    start_reaper();
    Ok(link)
}

/// Once, start the task that every 30 seconds drops abandoned dictations and stops each idle
/// service; stopping the process returns all of its model's memory.
fn start_reaper() {
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        tokio::spawn(async {
            loop {
                tokio::time::sleep(Duration::from_secs(30)).await;
                let settings = VoiceSettings::live();
                let idle = Duration::from_secs(settings.dictation.idle_capture_seconds);
                drop_stale(&mut *dictations().lock().await, idle);
                for role in [Role::Dictation, Role::Speech] {
                    let mut slot = service_slot(role).lock().await;
                    let minutes = role.idle_unload_minutes(&settings);
                    if slot
                        .as_ref()
                        .is_some_and(|service| service.link.unload_due(minutes, clock()))
                    {
                        *slot = None;
                    }
                }
            }
        });
    });
}

/// Route each service reply until the service closes its output, then drop every route so
/// waiting dictations and replies fail instead of hanging.
async fn read_replies(stdout: ChildStdout, routes: Routes) {
    let mut lines = BufReader::new(stdout).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        route_reply(&routes, &line);
    }
    if let Ok(mut routes) = routes.lock() {
        routes.clear();
    }
}

/// Apply one reply. For a dictation, `text` is the transcript so far, `final` completes it and
/// `error` ends it; a reply's lines are passed on until its `end` or `error`. Replies for unknown
/// or closed work are ignored.
fn route_reply(routes: &Routes, line: &str) {
    let Ok(reply) = serde_json::from_str::<Value>(line) else {
        return;
    };
    let (Some(id), Ok(mut routes)) = (reply["id"].as_str().map(str::to_owned), routes.lock())
    else {
        return;
    };
    let closes = match routes.get_mut(&id) {
        None => return,
        Some(Route::Speech(lines)) => {
            let last = reply.get("end").is_some() || reply.get("error").is_some();
            lines.send(reply).is_err() || last
        }
        Some(Route::Dictation { text, done }) => {
            if let Some(partial) = reply["text"].as_str() {
                text.send_replace(partial.to_owned());
            }
            if let Some(last) = reply["final"].as_str() {
                text.send_replace(last.to_owned());
                if let Some(done) = done.take() {
                    let _ = done.send(last.to_owned());
                }
                true
            } else {
                reply.get("error").is_some()
            }
        }
    };
    if closes {
        routes.remove(&id);
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

/// Composer dictation: `start` opens a capture in the dictation service, each `chunk` streams
/// a PCM frame and returns the transcript so far (it grows phrase by phrase at pauses),
/// `finish` returns the final transcript and `cancel` discards it. Nothing is stored; the
/// client types the text into the composer.
pub(crate) async fn voice_audio(
    State(app): State<App>,
    Path(sid): Path<String>,
    Json(body): Json<Value>,
) -> Api {
    session_agent_id(&app, &sid).await?;
    let op = required(&body, "op", 16)?;
    let k = dictation_key(&app, &sid);
    let settings = VoiceSettings::live().dictation;
    if op == "start" {
        let mut all = dictations().lock().await;
        // A new start replaces this caller's previous dictation in the conversation.
        all.remove(&k);
        drop_stale(&mut all, Duration::from_secs(settings.idle_capture_seconds));
        let slot = Slot::take(settings.max_concurrent).ok_or_else(|| {
            ApiError(
                StatusCode::SERVICE_UNAVAILABLE,
                "Dictation is busy; try again shortly".into(),
            )
        })?;
        let link = running_service(Role::Dictation)
            .await
            .map_err(|_| dictation_unavailable())?;
        let id = uuid();
        let (sender, text) = watch::channel(String::new());
        let (done, finished) = oneshot::channel();
        link.routes
            .lock()
            .map_err(|_| dictation_unavailable())?
            .insert(
                id.clone(),
                Route::Dictation {
                    text: sender,
                    done: Some(done),
                },
            );
        let max_bytes = capture::recording_limit(settings.max_recording_seconds);
        let dictation = Dictation {
            capture: Capture::new(id.clone(), max_bytes),
            link,
            text,
            finished: Some(finished),
            used: Instant::now(),
            done: None,
            slot: Some(slot),
        };
        if !dictation.link.send(json!({"op": "open", "id": id})) {
            return Err(dictation_unavailable());
        }
        all.insert(k, Arc::new(Mutex::new(dictation)));
        return Ok(Json(json!({
            "capture_id": id,
            "sample_rate": capture::SAMPLE_RATE,
            "max_seconds": settings.max_recording_seconds,
            "ephemeral": true,
        })));
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
            if !d.link.send(json!({"op": "audio", "id": id, "pcm": data})) {
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
    let wait = Duration::from_secs(settings.finish_timeout_seconds);
    let text = if d.link.send(json!({"op": "finish", "id": id})) {
        tokio::time::timeout(wait, finished)
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

const SPEECH_FAILED: &str = "Voice reply stopped; the reply is still on screen";

/// Replies are synthesized one at a time (Kokoro uses all of its threads); later ones queue.
fn speech_turn() -> Arc<Mutex<()>> {
    static TURN: OnceLock<Arc<Mutex<()>>> = OnceLock::new();
    TURN.get_or_init(|| Arc::new(Mutex::new(()))).clone()
}

fn speech_unavailable() -> ApiError {
    ApiError(
        StatusCode::SERVICE_UNAVAILABLE,
        "Voice output is unavailable".into(),
    )
}

/// One reply being synthesized by the speech service. Dropping it before its end (for example
/// when the client disconnects) cancels the rest and frees the turn.
struct Speaker {
    _turn: OwnedMutexGuard<()>,
    id: String,
    link: Link,
    lines: mpsc::UnboundedReceiver<Value>,
    ended: bool,
}

impl Drop for Speaker {
    fn drop(&mut self) {
        if let Ok(mut routes) = self.link.routes.lock() {
            routes.remove(&self.id);
        }
        if !self.ended {
            self.link.send(json!({"op": "cancel", "id": self.id}));
        }
    }
}

impl Speaker {
    /// The next chunk as a base64 WAV, `Ok(None)` at the end, or `Err` when synthesis failed or
    /// the service stopped.
    async fn next(&mut self) -> Result<Option<String>, ()> {
        let line = self.lines.recv().await;
        if let Some(audio) = line.as_ref().and_then(|line| line["audio"].as_str()) {
            return Ok(Some(audio.to_owned()));
        }
        self.ended = true;
        match line {
            Some(line) if line.get("end").is_some() => Ok(None),
            _ => Err(()),
        }
    }
}

/// Wait for the speech turn (up to `speech.queue_wait_seconds`), then send `chunks` to the
/// speech service in `voice`. Callers have prepared the text and validated the voice.
async fn start_speaker(chunks: &[String], voice: &str) -> Result<Speaker, ApiError> {
    let wait = Duration::from_secs(VoiceSettings::live().speech.queue_wait_seconds);
    let turn = tokio::time::timeout(wait, speech_turn().lock_owned())
        .await
        .map_err(|_| {
            ApiError(
                StatusCode::SERVICE_UNAVAILABLE,
                "Voice is busy; try again shortly".into(),
            )
        })?;
    let link = running_service(Role::Speech)
        .await
        .map_err(|_| speech_unavailable())?;
    let id = uuid();
    let (sender, lines) = mpsc::unbounded_channel();
    link.routes
        .lock()
        .map_err(|_| speech_unavailable())?
        .insert(id.clone(), Route::Speech(sender));
    let speaker = Speaker {
        _turn: turn,
        id: id.clone(),
        link,
        lines,
        ended: false,
    };
    let request = json!({"op": "speak", "id": id, "voice": voice, "chunks": chunks});
    if !speaker.link.send(request) {
        return Err(speech_unavailable());
    }
    Ok(speaker)
}

/// NDJSON lines: first the plan, `{"plan": {"chunks": [characters per chunk]}}`, then one
/// `{"audio": <base64 WAV>}` per chunk as it is synthesized, or a final `{"error": ...}` when
/// synthesis fails. The end of the reply ends the stream.
fn speech_lines(plan: Vec<usize>, speaker: Speaker) -> Body {
    let plan = json!({"plan": {"chunks": plan}});
    let first = stream::once(async move { Ok::<_, std::io::Error>(format!("{plan}\n")) });
    let audio = stream::unfold(Some(speaker), |state| async move {
        let mut speaker = state?;
        let line = match speaker.next().await {
            Ok(Some(audio)) => json!({"audio": audio}),
            Ok(None) => return None,
            Err(()) => json!({"error": SPEECH_FAILED}),
        };
        let more = line.get("error").is_none().then_some(speaker);
        Some((Ok(format!("{line}\n")), more))
    });
    Body::from_stream(first.chain(audio))
}

/// Speak a reply in the session agent's voice when the agent has voice output on. Streams
/// [`speech_lines`] so the client can start playing after the first sentence.
pub(crate) async fn voice_reply(
    State(app): State<App>,
    Path(sid): Path<String>,
    Json(body): Json<Value>,
) -> Result<Response, ApiError> {
    let max_chars = VoiceSettings::live().speech.max_chars;
    let text = speech_text(required(&body, "text", 8192)?, max_chars);
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
    let chunks = speech_chunks(&text);
    let plan = chunks.iter().map(|chunk| chunk.chars().count()).collect();
    let speaker = start_speaker(&chunks, &output.voice).await?;
    Ok((
        [
            (CONTENT_TYPE, "application/x-ndjson"),
            (CACHE_CONTROL, "no-store"),
        ],
        speech_lines(plan, speaker),
    )
        .into_response())
}

/// A short WAV introduction in `voice` for the Identity voice picker, cached after first use.
pub(crate) async fn voice_sample(
    State(app): State<App>,
    Path(voice): Path<String>,
) -> Result<Response, ApiError> {
    static SAMPLES: OnceLock<Mutex<HashMap<String, Vec<u8>>>> = OnceLock::new();
    let text = sample_text(&voice)
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "Unknown voice".into()))?;
    let samples = SAMPLES.get_or_init(Default::default);
    let cached = samples.lock().await.get(&voice).cloned();
    let wav = match cached {
        Some(wav) => wav,
        None => {
            if app.shutoff().await? {
                return Err(conflict("Global shutoff is active"));
            }
            // A sample is one short chunk.
            let mut speaker = start_speaker(&[text], &voice).await?;
            let audio = speaker
                .next()
                .await
                .ok()
                .flatten()
                .ok_or_else(speech_unavailable)?;
            let wav = STANDARD.decode(audio).map_err(|_| speech_unavailable())?;
            samples.lock().await.insert(voice, wav.clone());
            wav
        }
    };
    Ok((
        [
            (CONTENT_TYPE, "audio/wav"),
            (CACHE_CONTROL, "private, max-age=86400"),
        ],
        wav,
    )
        .into_response())
}

#[cfg(test)]
mod voice_output_tests {
    // Test code may unwrap (the standard's `allow-unwrap-in-tests`).
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn service_replies_reach_only_their_work() {
        let routes = Routes::default();
        let (text, seen) = watch::channel(String::new());
        let (done, mut finished) = oneshot::channel();
        let (lines, mut reply) = mpsc::unbounded_channel();
        routes.lock().unwrap().insert(
            "a".into(),
            Route::Dictation {
                text,
                done: Some(done),
            },
        );
        routes
            .lock()
            .unwrap()
            .insert("s".into(), Route::Speech(lines));
        route_reply(&routes, r#"{"id":"b","text":"Not mine."}"#);
        route_reply(&routes, "not json");
        assert_eq!(*seen.borrow(), "");
        route_reply(&routes, r#"{"id":"a","text":"Hello."}"#);
        assert_eq!(*seen.borrow(), "Hello.");
        assert!(finished.try_recv().is_err());
        route_reply(&routes, r#"{"id":"a","final":"Hello there."}"#);
        assert_eq!(finished.try_recv().unwrap(), "Hello there.");
        route_reply(&routes, r#"{"id":"s","audio":"UklGRg=="}"#);
        assert_eq!(reply.try_recv().unwrap()["audio"], "UklGRg==");
        route_reply(&routes, r#"{"id":"s","end":true}"#);
        assert_eq!(reply.try_recv().unwrap()["end"], true);
        assert!(routes.lock().unwrap().is_empty());
    }

    #[test]
    fn idle_services_unload_only_when_nothing_is_open() {
        let (requests, _queue) = mpsc::unbounded_channel();
        let link = Link {
            requests,
            routes: Routes::default(),
            used: Arc::new(AtomicU64::new(1_000)),
        };
        let ten_minutes = 10 * 60_000;
        assert!(!link.unload_due(10, 1_000 + ten_minutes));
        assert!(link.unload_due(10, 1_001 + ten_minutes));
        assert!(!link.unload_due(0, u64::MAX));
        let (lines, _reply) = mpsc::unbounded_channel();
        link.routes
            .lock()
            .unwrap()
            .insert("s".into(), Route::Speech(lines));
        assert!(!link.unload_due(10, 1_001 + ten_minutes));
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
