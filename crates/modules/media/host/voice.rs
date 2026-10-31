//! Parallel voice conversation: transient recordings/questions, explicit durable work admission.
//! Lane, capture, routing and speech rules live in `webagent_media`; this root-bound adapter
//! supplies the HTTP handlers, App/Store, the session gate, model lanes and run commands.
use axum::extract::Path;
use axum::http::StatusCode;
use axum::Json;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde_json::{json, Value};
use webagent_media::capture::{self, Capture, Chunk};
use webagent_media::lanes::{self, Completed, Lane, LaneError, Lanes};
use webagent_media::speech::speech_text;
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
/// Drop this caller's lane for `sid`; a result still in flight can no longer land in it.
pub(crate) async fn quiesce(app: &App, sid: &str) {
    lanes::registry().lock().await.remove(&lane_key(app, sid));
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
async fn transcribe(app: &App, run: &Value, bytes: &[u8], mime: &str) -> Result<String, ApiError> {
    let (selection, tokens) = media_lane::lane_selection(app, run).await?;
    let cancel = app.stop.child_token();
    let text = app
        .session_operation(
            run["session_id"].as_str().unwrap_or(""),
            cancel.clone(),
            app.provider
                .transcribe(&selection, &tokens, bytes, mime, cancel),
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
                "Recording could not be transcribed by the configured Voice model".into(),
            )
        })?;
    media_lane::lane_selection(app, run).await?;
    Ok(text)
}
pub(crate) async fn voice_audio(
    State(app): State<App>,
    Path(sid): Path<String>,
    Json(body): Json<Value>,
) -> Api {
    session_agent_id(&app, &sid).await?;
    let op = required(&body, "op", 16)?;
    let k = lane_key(&app, &sid);
    if op == "start" {
        let run = media_lane::lane_run(&app, &sid, model_selection::ModelRole::Voice).await?;
        media_lane::lane_selection(&app, &run).await?;
        if run["selection_plan"]["candidates"][0]["transport"] == "openai_subscription" {
            return Err(conflict(
                "Voice recording requires an audio-capable API model connection",
            ));
        }
        let mut all = lanes::registry().lock().await;
        let lane = get_lane(&mut all, &k)?;
        if lane.busy || lane.capture.is_some() {
            return Err(conflict("A voice turn is already active"));
        }
        let id = uuid();
        lane.completed = None;
        lane.capture = Some(Capture::new(id.clone(), run));
        return Ok(Json(
            json!({"capture_id": id, "sample_rate": capture::SAMPLE_RATE, "ephemeral": true}),
        ));
    }
    let id = required(&body, "capture_id", 64)?;
    if op == "cancel" {
        let mut all = lanes::registry().lock().await;
        let lane = get_lane(&mut all, &k)?;
        if lane.capture.as_ref().is_some_and(|c| c.id == id) {
            lane.capture = None;
        }
        return Ok(Json(json!({"cancelled": true, "ephemeral": true})));
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
        let mut all = lanes::registry().lock().await;
        let lane = get_lane(&mut all, &k)?;
        let c = lane
            .capture
            .as_mut()
            .filter(|c| c.id == id)
            .ok_or_else(|| conflict("Recording expired; start a new voice turn"))?;
        let (speaking, end, partial) = match c.accept(sequence, bytes) {
            Ok(Chunk::Replayed { seq }) => {
                return Ok(Json(
                    json!({"seq": seq, "replayed": true, "ephemeral": true}),
                ));
            }
            Ok(Chunk::Accepted {
                speaking,
                turn_end,
                partial,
            }) => (speaking, turn_end, partial.map(|wav| (c.run.clone(), wav))),
            Err(_) => return Err(bad("PCM sequence or recording limit exceeded")),
        };
        drop(all);
        let partial_text = if let Some((run, bytes)) = partial {
            match transcribe(&app, &run, &bytes, "audio/wav").await {
                Ok(text) => Some(text),
                Err(error) if error.0 == StatusCode::NOT_FOUND => return Err(error),
                Err(_) => None,
            }
        } else {
            None
        };
        return Ok(Json(json!({
            "seq": sequence,
            "speaking": speaking,
            "turn_end": end,
            "partial": partial_text,
            "ephemeral": true,
        })));
    }
    if !matches!(op, "finish" | "recording") {
        return Err(bad("Unknown voice audio operation"));
    }
    let mut all = lanes::registry().lock().await;
    let lane = get_lane(&mut all, &k)?;
    if let Some(done) = &lane.completed {
        if done.capture_id == id {
            return Ok(Json(json!({
                "text": done.text,
                "capture_id": id,
                "ephemeral": true,
                "replayed": true,
            })));
        }
    }
    if lane.busy {
        return Err(conflict("Voice is still transcribing"));
    }
    let Some(c) = lane.capture.take_if(|c| c.id == id) else {
        return Err(conflict("Recording expired; start a new voice turn"));
    };
    if op == "finish" && !c.speaking() {
        lane.completed = Some(Completed {
            capture_id: id.to_owned(),
            text: String::new(),
        });
        return Ok(Json(
            json!({"text": "", "capture_id": id, "ephemeral": true}),
        ));
    }
    lane.busy = true;
    let generation = lane.generation.clone();
    let recording = if op == "recording" {
        let data = body["data"].as_str().unwrap_or("");
        if data.len() > 14 * 1024 * 1024 {
            lane.busy = false;
            return Err(bad("Recording exceeds 10 MB"));
        }
        let bytes = STANDARD.decode(data).map_err(|_| {
            lane.busy = false;
            bad("Invalid recording")
        })?;
        (
            bytes,
            body["media_type"]
                .as_str()
                .unwrap_or("")
                .split(';')
                .next()
                .unwrap_or("")
                .to_owned(),
        )
    } else {
        (c.wav(), "audio/wav".to_owned())
    };
    drop(all);
    let result = transcribe(&app, &c.run, &recording.0, &recording.1).await;
    let mut all = lanes::registry().lock().await;
    let lane = existing_lane(&mut all, &k, &generation)?;
    lane.busy = false;
    match result {
        Ok(text) => {
            lane.completed = Some(Completed {
                capture_id: id.to_owned(),
                text: text.clone(),
            });
            Ok(Json(
                json!({"text": text, "capture_id": id, "ephemeral": true}),
            ))
        }
        Err(error) => Err(error),
    }
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
    Ok(Json(json!({
        "voice_configured": configured,
        "streaming": true,
        "tts_available": tts_available,
        "browser_fallback": true,
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
