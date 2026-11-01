//! Composer dictation with a local recognizer, and parallel voice conversation: transient
//! questions and explicit durable work admission.
//! Lane, capture, routing and speech rules live in `webagent_media`; this root-bound adapter
//! supplies the HTTP handlers, App/Store, the session gate, model lanes and run commands.
//! Dictation runs local Kroko recognition and agent voice output local Kokoro speech (`/opt/voice`,
//! see `crates/modules/media/voice`).
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
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{watch, Mutex, OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinHandle;
use webagent_media::capture::{self, Capture, Chunk};
use webagent_media::lanes::{self, Lane, LaneError, Lanes};
use webagent_media::speech::{sample_text, speech_text};
use webagent_media::utterance;

use super::chat::{command, send};
use super::media_lane;
use crate::{
    bad, conflict, model_selection, now, required, session_agent_id, session_mode, status,
    terminal, uuid, Api, ApiError, App, State,
};

/// Public status and message for a lane rejection.
fn lane_error(error: LaneError) -> ApiError {
    match error {
        LaneError::Busy => conflict(error.message()),
        LaneError::Gone => ApiError(StatusCode::NOT_FOUND, error.message().into()),
    }
}
/// Drop this caller's lane and dictation for `sid`; a result still in flight can no longer land
/// in it.
pub(crate) async fn quiesce(app: &App, sid: &str) {
    lanes::registry().lock().await.remove(&lane_key(app, sid));
    dictations().lock().await.remove(&lane_key(app, sid));
}
fn lane_key(app: &App, sid: &str) -> String {
    lanes::lane_key(app.store.current_user_id(), sid)
}
fn get_lane<'a>(all: &'a mut Lanes, key: &str) -> Result<&'a mut Lane, ApiError> {
    lanes::get_lane(all, key, now()).map_err(lane_error)
}
fn existing_lane<'a>(
    all: &'a mut Lanes,
    key: &str,
    generation: &str,
) -> Result<&'a mut Lane, ApiError> {
    lanes::existing_lane(all, key, generation).map_err(lane_error)
}
/// One composer dictation: frame sequencing and its local recognizer. Dropping it kills the
/// recognizer and frees its slot.
struct Dictation {
    capture: Capture,
    listener: Option<Listener>,
    /// The latest transcript so far, written by the listener's output reader.
    text: watch::Receiver<String>,
    reader: Option<JoinHandle<()>>,
    used: Instant,
    /// The final transcript, kept so a retried finish replays it.
    done: Option<String>,
    /// Held while the recognizer runs; released at finish.
    slot: Option<OwnedSemaphorePermit>,
}

/// A recognizer process waiting for or receiving audio.
struct Listener {
    child: Child,
    stdin: ChildStdin,
    /// Taken by the transcript reader when the dictation starts.
    stdout: Option<ChildStdout>,
}

type Dictations = HashMap<String, Arc<Mutex<Dictation>>>;

fn dictations() -> &'static Mutex<Dictations> {
    static ALL: OnceLock<Mutex<Dictations>> = OnceLock::new();
    ALL.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The local Kroko recognizer; its packaging is the `voice` stage of the Dockerfile.
#[cfg(not(test))]
const LISTENER: [&str; 3] = ["/opt/voice/venv/bin/python", "-I", "/opt/voice/listen.py"];
/// Stands in for the recognizer: reads the whole dictation, then prints one transcript line.
#[cfg(test)]
const LISTENER: [&str; 3] = [
    "/bin/sh",
    "-c",
    r#"cat >/dev/null; echo '{"text":"Hello there."}'"#,
];
/// Recognizers decoding at once; each uses one core, and builds share the host.
const MAX_DICTATIONS: usize = 2;
/// A dictation with no frames for this long was abandoned; its recognizer has exited.
const DICTATION_IDLE: Duration = Duration::from_secs(90);
const DICTATION_STOPPED: &str = "Dictation stopped; start it again";

fn dictation_slots() -> Arc<Semaphore> {
    static SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    SLOTS
        .get_or_init(|| Arc::new(Semaphore::new(MAX_DICTATIONS)))
        .clone()
}

fn spawn_listener() -> std::io::Result<Listener> {
    let mut child = Command::new(LISTENER[0])
        .args(&LISTENER[1..])
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let missing = || std::io::Error::other("listener pipes unavailable");
    let stdin = child.stdin.take().ok_or_else(missing)?;
    let stdout = child.stdout.take().ok_or_else(missing)?;
    Ok(Listener {
        child,
        stdin,
        stdout: Some(stdout),
    })
}

/// A recognizer for a new dictation. The model takes seconds to load, so one spare is kept
/// loaded and waiting; it is handed out and replaced in the background.
async fn take_listener() -> std::io::Result<Listener> {
    static SPARE: OnceLock<Mutex<Option<Listener>>> = OnceLock::new();
    let spare = SPARE.get_or_init(|| Mutex::new(None));
    let warm = spare.lock().await.take();
    let listener = match warm {
        Some(mut l) => match l.child.try_wait() {
            Ok(None) => l,
            _ => spawn_listener()?,
        },
        None => spawn_listener()?,
    };
    tokio::spawn(async move {
        let mut slot = spare.lock().await;
        if slot.is_none() {
            *slot = spawn_listener().ok();
        }
    });
    Ok(listener)
}

/// Publish each transcript line the recognizer writes until it closes its output.
fn read_transcripts(stdout: ChildStdout, text: watch::Sender<String>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if let Some(latest) = serde_json::from_str::<Value>(&line)
                .ok()
                .and_then(|v| v["text"].as_str().map(str::to_owned))
            {
                text.send_replace(latest);
            }
        }
    })
}

fn dictation_unavailable() -> ApiError {
    ApiError(
        StatusCode::SERVICE_UNAVAILABLE,
        "Dictation is unavailable".into(),
    )
}

/// Composer dictation: `start` opens a capture with a local recognizer, each `chunk` streams a
/// PCM frame and returns the transcript so far, `finish` returns the final transcript and
/// `cancel` discards it. Nothing is stored; the client types the text into the composer.
pub(crate) async fn voice_audio(
    State(app): State<App>,
    Path(sid): Path<String>,
    Json(body): Json<Value>,
) -> Api {
    session_agent_id(&app, &sid).await?;
    let op = required(&body, "op", 16)?;
    let k = lane_key(&app, &sid);
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
        let mut listener = take_listener().await.map_err(|_| dictation_unavailable())?;
        let (sender, text) = watch::channel(String::new());
        let stdout = listener.stdout.take().ok_or_else(dictation_unavailable)?;
        let reader = read_transcripts(stdout, sender);
        let id = uuid();
        all.insert(
            k,
            Arc::new(Mutex::new(Dictation {
                capture: Capture::new(id.clone()),
                listener: Some(listener),
                text,
                reader: Some(reader),
                used: now,
                done: None,
                slot: Some(slot),
            })),
        );
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
            let listener = d.listener.as_mut().ok_or_else(dictation_unavailable)?;
            if listener.stdin.write_all(&bytes).await.is_err() {
                drop(d);
                dictations().lock().await.remove(&k);
                return Err(ApiError(
                    StatusCode::SERVICE_UNAVAILABLE,
                    DICTATION_STOPPED.into(),
                ));
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
    // Closing the recognizer's input flushes the model; it writes the final line and exits.
    let mut listener = d.listener.take().ok_or_else(dictation_unavailable)?;
    drop(listener.stdin);
    let flushed = match d.reader.take() {
        Some(reader) => tokio::time::timeout(Duration::from_secs(15), reader)
            .await
            .is_ok_and(|joined| joined.is_ok()),
        None => false,
    };
    let _ = listener.child.start_kill();
    d.slot = None;
    if !flushed {
        drop(d);
        dictations().lock().await.remove(&k);
        return Err(ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            DICTATION_STOPPED.into(),
        ));
    }
    let text = d.text.borrow().clone();
    d.done = Some(text.clone());
    Ok(Json(
        json!({"text": text, "capture_id": id, "ephemeral": true}),
    ))
}
async fn voice_target(app: &App, sid: &str) -> Result<Option<Value>, ApiError> {
    let view = app
        .store
        .active_runs()
        .await?
        .into_iter()
        .find(|r| r["session_id"] == sid && !terminal(status(r)));
    if let Some(view) = view {
        let id = view["id"].as_str().expect("an active run view has an id");
        Ok(Some(app.run(id).await?))
    } else {
        Ok(None)
    }
}
/// The target run's id and revision, or nulls when no run is active.
fn target_fields(target: Option<&Value>) -> (Value, Value) {
    (
        target.map(|r| r["id"].clone()).unwrap_or(Value::Null),
        target
            .map(|r| r["run_revision"].clone())
            .unwrap_or(Value::Null),
    )
}
/// A work proposal for `command`; it needs confirmation only when it would replace active work.
fn work_proposal(id: &str, command: &str, target: Option<&Value>, mode: &Value) -> Value {
    let (target_run_id, target_revision) = target_fields(target);
    json!({
        "id": id,
        "state": if target.is_some() { "pending" } else { "ready" },
        "command": command,
        "target_run_id": target_run_id,
        "target_revision": target_revision,
        "mode_revision": mode["revision"],
        "expires": now() + 120,
    })
}
pub(crate) async fn voice_route(
    State(app): State<App>,
    Path(sid): Path<String>,
    Json(body): Json<Value>,
) -> Api {
    let utterance = required(&body, "utterance", 8192)?.trim().to_owned();
    let text = utterance::words(&utterance);
    let _guard = app.gate.lock().await;
    let target = voice_target(&app, &sid).await?;
    let mode = session_mode(&app, &sid).await?;
    let k = lane_key(&app, &sid);
    let mut all = lanes::registry().lock().await;
    let lane = get_lane(&mut all, &k)?;
    if lane.busy {
        return Err(conflict("Voice is still answering"));
    }
    // "Cancel" both rejects a pending proposal and stops a run; with no proposal waiting it
    // is a run control.
    let awaiting = lane.proposal.as_ref().is_some_and(|p| {
        p["expires"].as_u64().unwrap_or(0) > now()
            && p["state"] == "pending"
            && body["proposal_id"] == p["id"]
    });
    if utterance::yes(&text)
        || (utterance::no(&text) && (awaiting || utterance::control(&text).is_none()))
    {
        let proposal =
            lane.proposal.as_mut().filter(|_| awaiting).ok_or_else(|| {
                conflict("No matching pending voice proposal; nothing was changed")
            })?;
        if utterance::no(&text) {
            proposal["state"] = json!("rejected");
            return Ok(Json(json!({
                "action": "noop",
                "answer": "The current work is unchanged.",
                "proposal": proposal,
                "ephemeral": true,
            })));
        }
        let (target_run_id, target_revision) = target_fields(target.as_ref());
        if proposal["target_run_id"] != target_run_id
            || proposal["target_revision"] != target_revision
            || proposal["mode_revision"] != mode["revision"]
        {
            lane.proposal = None;
            return Err(conflict(
                "Run or mode changed; repeat the voice request before confirming",
            ));
        }
        proposal["state"] = json!("ready");
        return Ok(Json(json!({
            "action": "dispatch_work",
            "ticket": proposal["id"],
            "command": proposal["command"],
            "ephemeral": true,
        })));
    }
    if let Some(action) = utterance::control(&text) {
        let Some(run) = target else {
            return Ok(Json(json!({
                "action": "noop",
                "answer": "There is no active or paused run to control.",
                "ephemeral": true,
            })));
        };
        let ticket = uuid();
        lane.proposal = Some(json!({
            "id": ticket,
            "state": "ready",
            "control": action.as_str(),
            "target_run_id": run["id"],
            "target_revision": run["run_revision"],
            "mode_revision": mode["revision"],
            "expires": now() + 120,
        }));
        return Ok(Json(json!({
            "action": "interrupt",
            "ticket": ticket,
            "control_action": action.as_str(),
            "ephemeral": true,
        })));
    }
    if utterance::status_query(&text) {
        let answer = target
            .as_ref()
            .map(|r| format!("The run is {}.", status(r)))
            .unwrap_or_else(|| "There is no active run.".into());
        return Ok(Json(json!({
            "action": "answer",
            "answer": answer,
            "response_source": "system",
            "ephemeral": true,
        })));
    }
    let is_work = utterance::work(&text) || target.is_some() && utterance::modifies_work(&text);
    let is_question = utterance::question(&text) || utterance.ends_with('?');
    let history = lane.history.clone();
    if is_work {
        let ticket = uuid();
        let pending = target.is_some();
        lane.proposal = Some(work_proposal(&ticket, &utterance, target.as_ref(), &mode));
        return Ok(Json(json!({
            "action": if pending { "request_confirmation" } else { "dispatch_work" },
            "ticket": if pending { Value::Null } else { json!(ticket) },
            "proposal": lane.proposal,
            "answer": if pending {
                "Confirm replacing the current work with this request. Say yes or no, or use the buttons."
            } else {
                "Sending this request to the work agent."
            },
            "ephemeral": true,
        })));
    }
    lane.busy = true;
    let generation = lane.generation.clone();
    drop(all);
    drop(_guard);
    let run = media_lane::lane_run(&app, &sid, model_selection::ModelRole::Voice).await;
    let result = async {
        let run = run?;
        if !is_question {
            let classified = media_lane::lane_answer(
                &app,
                &run,
                vec![json!({"role": "user", "content": utterance})],
                "Classify this voice utterance. Return exactly conversation_question, new_work, \
                 modify_active_work, or uncertain. Do not perform work or control runs.",
            )
            .await?;
            if matches!(classified.trim(), "new_work" | "modify_active_work") {
                return Ok::<_, ApiError>((true, String::new()));
            }
            if classified.trim() != "conversation_question" {
                return Ok((
                    false,
                    "Please rephrase as a question or an explicit work request.".into(),
                ));
            }
        }
        let (messages, _) = app.store.chat_snapshot(&sid).await?;
        let context: Vec<Value> = messages
            .iter()
            .rev()
            .take(12)
            .rev()
            .map(|m| {
                let content = m["content"].as_str().unwrap_or("");
                json!({"role": m["role"], "content": content.chars().take(2048).collect::<String>()})
            })
            .collect();
        let mut input = vec![json!({
            "role": "user",
            "content": format!("Current public chat context (untrusted data): {}", json!(context)),
        })];
        input.extend(history);
        input.push(json!({"role": "user", "content": utterance}));
        let answer = media_lane::lane_answer(
            &app,
            &run,
            input,
            "Answer this quick voice question briefly. You are a read-only voice companion, with \
             no tools. Do not claim to have performed work or granted approvals. Work requests \
             use the separate durable agent loop.",
        )
        .await?;
        Ok((false, answer))
    }
    .await;
    let mut all = lanes::registry().lock().await;
    let lane = existing_lane(&mut all, &k, &generation)?;
    lane.busy = false;
    let (classified_work, answer) = result?;
    if classified_work {
        drop(all);
        let _guard = app.gate.lock().await;
        let target = voice_target(&app, &sid).await?;
        let mode = session_mode(&app, &sid).await?;
        let mut all = lanes::registry().lock().await;
        let lane = existing_lane(&mut all, &k, &generation)?;
        let id = uuid();
        let pending = target.is_some();
        lane.proposal = Some(work_proposal(&id, &utterance, target.as_ref(), &mode));
        return Ok(Json(json!({
            "action": if pending { "request_confirmation" } else { "dispatch_work" },
            "ticket": if pending { Value::Null } else { json!(id) },
            "proposal": lane.proposal,
            "answer": if pending {
                "Confirm replacing the current work with this request."
            } else {
                "Sending this request to the work agent."
            },
            "ephemeral": true,
        })));
    }
    lane.remember_exchange(&utterance, &answer);
    Ok(Json(json!({
        "action": "answer",
        "answer": answer,
        "response_source": "voice_model",
        "ephemeral": true,
    })))
}
pub(crate) async fn voice_control(
    State(app): State<App>,
    Path(sid): Path<String>,
    Json(body): Json<Value>,
) -> Api {
    let id = required(&body, "ticket", 64)?.to_owned();
    let receipt_key = format!("receipt:voice:{sid}:{id}");
    if let Some(saved) = app.store.get(&receipt_key).await? {
        return Ok(Json(saved));
    }
    if let Some(saved) = app
        .store
        .get(&format!("receipt:message:{sid}:voice-{id}"))
        .await?
    {
        return Ok(Json(json!({
            "message": "Sent to the work agent.",
            "run_id": saved["result"]["run_id"],
            "ephemeral": false,
            "replayed": true,
        })));
    }
    let k = lane_key(&app, &sid);
    let proposal = {
        let mut all = lanes::registry().lock().await;
        let lane = get_lane(&mut all, &k)?;
        let p = lane
            .proposal
            .as_ref()
            .filter(|p| {
                p["id"] == id && p["state"] == "ready" && p["expires"].as_u64().unwrap_or(0) > now()
            })
            .ok_or_else(|| conflict("Voice action expired or requires confirmation"))?
            .clone();
        if lane.busy {
            return Err(conflict("Voice action already executing"));
        }
        lane.busy = true;
        p
    };
    let result = async {
        let _guard = app.gate.lock().await;
        let target = voice_target(&app, &sid).await?;
        let expected = proposal["target_run_id"].clone();
        let (target_run_id, target_revision) = target_fields(target.as_ref());
        if expected != target_run_id
            || proposal["target_revision"] != target_revision
            || proposal["mode_revision"] != session_mode(&app, &sid).await?["revision"]
        {
            return Err(conflict("Run or mode changed; repeat the voice request"));
        }
        drop(_guard);
        let action = proposal["control"].as_str();
        if let Some(rid) = expected.as_str() {
            let Json(result) = command(
                State(app.clone()),
                Path((rid.to_owned(), action.unwrap_or("cancel").to_owned())),
                Json(json!({
                    "client_request_id": format!("voice-{id}"),
                    "expected_run_revision": proposal["target_revision"],
                })),
            )
            .await?;
            if result["uncertain"] == true {
                return Err(conflict(
                    "Action outcome is uncertain; review recovery in chat before continuing",
                ));
            }
            if let Some(action) = action {
                return Ok(Json(json!({
                    "message": if action == "resume" { "Run resumed." } else { "Run cancelled." },
                    "run": result,
                    "ephemeral": true,
                })));
            }
        }
        let (_, Json(sent)) = send(
            State(app.clone()),
            Path(sid.clone()),
            Json(json!({"text": proposal["command"], "client_message_id": format!("voice-{id}")})),
        )
        .await?;
        Ok(Json(json!({
            "message": "Sent to the work agent.",
            "run_id": sent["run_id"],
            "ephemeral": false,
        })))
    }
    .await;
    let mut all = lanes::registry().lock().await;
    let lane = get_lane(&mut all, &k)?;
    lane.busy = false;
    if let Ok(Json(value)) = &result {
        if let Some(p) = lane.proposal.as_mut() {
            p["state"] = json!("accepted");
        }
        drop(all);
        app.store.put(&receipt_key, value).await?;
    }
    result
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
pub(crate) async fn voice_speech(
    State(app): State<App>,
    Path(sid): Path<String>,
    Json(body): Json<Value>,
) -> Api {
    let text = speech_text(required(&body, "text", 8192)?);
    if text.is_empty() {
        return Err(bad("Speech text is empty"));
    }
    if app.shutoff().await? {
        return Err(conflict("Global shutoff is active"));
    }
    // Optional installation-owned speech service; no v2 keys or client URLs/models.
    let key = std::env::var("V3_VOICE_TTS_API_KEY")
        .ok()
        .filter(|key| !key.trim().is_empty())
        .ok_or_else(|| {
            ApiError(
                StatusCode::SERVICE_UNAVAILABLE,
                "Server speech is unavailable; browser speech can be used".into(),
            )
        })?;
    let model = std::env::var("V3_VOICE_TTS_MODEL").unwrap_or_else(|_| "gpt-4o-mini-tts".into());
    session_agent_id(&app, &sid).await?;
    let cancel = app.stop.child_token();
    let bytes = app
        .session_operation(
            &sid,
            cancel.clone(),
            app.provider.speak(&key, &model, &text, cancel),
        )
        .await
        .map_err(|error| {
            if error
                .downcast_ref::<crate::store::session::ArchivedSession>()
                .is_some()
            {
                return ApiError::from(error);
            }
            ApiError(
                StatusCode::BAD_GATEWAY,
                "Server speech did not complete; browser speech can be used".into(),
            )
        })?;
    let agent = session_agent_id(&app, &sid).await?;
    if !app
        .store
        .live_access(app.store.current_user_id(), &agent)
        .await?
        .admitted()
    {
        return Err(conflict("Voice access changed"));
    }
    Ok(Json(json!({
        "data": STANDARD.encode(bytes),
        "media_type": "audio/mpeg",
        "ephemeral": true,
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
