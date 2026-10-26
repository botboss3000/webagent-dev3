//! Authenticated encrypted uploads; ownership and admission links stay in the user plane.
//! Files rules live in `webagent_files`; this root-bound adapter supplies App/Store, the session
//! gate, the Storage document/event primitives and the media lane used for image descriptions.
use super::*;
pub(crate) use webagent_files::uploads::{allowed_media, metadata, AttachmentLimits};
use webagent_files::uploads::{self as files, Rejected, UploadInput};
use webagent_files::{attachment_key, message_files_key, QUOTA_KEY};
/// Limits come from the session agent's effective chat.json on every admission; browsers cannot raise them.
pub(crate) async fn limits(app: &App, sid: &str) -> Result<AttachmentLimits, ApiError> {
    let agent = session_agent_id(app, sid).await
        .map_err(|_| ApiError(StatusCode::NOT_FOUND, "Session not found".into()))?;
    limits_for(app, &agent).await
}
pub(crate) async fn limits_for(app: &App, agent: &str) -> Result<AttachmentLimits, ApiError> {
    crate::chat_config::effective_chat(&app.store, &app.chat_ui_config_path, agent).await
        .and_then(|config| webagent_chat::configuration::parse_attachment_limits(&config))
        .map_err(|error| ApiError(StatusCode::SERVICE_UNAVAILABLE, error.to_string()))
}
pub(crate) async fn request_body(app: &App, sid: &str, request: Request) -> Result<Value, ApiError> {
    let media = request.headers().get("content-type").and_then(|v|v.to_str().ok())
        .unwrap_or("").split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    if media != "application/json" && !(media.starts_with("application/") && media.ends_with("+json")) {
        return Err(ApiError(StatusCode::UNSUPPORTED_MEDIA_TYPE,"Attachment upload requires JSON".into()));
    }
    let maximum = files::envelope_limit(limits(app, sid).await?);
    let bytes = axum::body::to_bytes(request.into_body(),maximum).await
        .map_err(|_| ApiError(StatusCode::PAYLOAD_TOO_LARGE,"Attachment request exceeds configured limit".into()))?;
    serde_json::from_slice(&bytes).map_err(|_| bad("Invalid attachment JSON"))
}
pub(crate) async fn upload(
    State(app): State<App>,
    Path(sid): Path<String>,
    Json(body): Json<Value>,
) -> Api {
    let limits = limits(&app, &sid).await?;
    let uploader = crate::shared_chat::actor(&app).to_owned();
    let aid = required(&body, "client_upload_id", 64)?;
    if !files::upload_id_ok(aid) {
        return Err(bad("Invalid upload identifier"));
    }
    let name = required(&body, "name", 255)?;
    let media = files::normalize_media(required(&body, "media_type", 128)?);
    if !files::name_ok(name) || !files::allowed_media(&media) {
        return Err(bad("Unsupported file name or type"));
    }
    let encoded = required(&body, "data", usize::MAX)?;
    let new = files::new_upload(limits, UploadInput {id:aid,session_id:&sid,uploader_id:&uploader,name,media_type:&media,data:encoded,created_at:now()})
        .map_err(|rejected| match rejected {
            Rejected::Invalid(message) => bad(message),
            Rejected::TooLarge(message) => ApiError(StatusCode::PAYLOAD_TOO_LARGE, message),
        })?;
    // Persist first, then describe on demand at admission. Upload retries never bill a model twice.
    let _guard = app.gate.lock().await;
    let session = app
        .store
        .get(&key("session", &sid))
        .await?
        .ok_or_else(|| bad("Session not found"))?;
    if session["status"] != "active" {
        return Err(ApiError(StatusCode::NOT_FOUND,"Session not found".into()));
    }
    if let Some(saved) = app.store.get(&attachment_key(aid)).await? {
        if !files::same_upload(&saved, &sid, &new.fingerprint, draft_owner(&app, &saved), &uploader) {
            return Err(conflict("Upload identifier reused"));
        }
        return Ok(Json(metadata(&saved)));
    }
    let quota = files::quota_with(app.store.get(QUOTA_KEY).await?.as_ref(), new.bytes)
        .ok_or_else(|| conflict("Attachment byte count exceeds machine accounting range"))?;
    let info = metadata(&new.record);
    app.store
        .atomic_docs_event(
            vec![
                (attachment_key(aid), new.record),
                (QUOTA_KEY.into(), quota),
            ],
            &sid,
            "attachment_uploaded",
            &info,
        )
        .await?;
    Ok(Json(info))
}
pub(crate) async fn upload_delete(
    State(app): State<App>,
    Path((sid, aid)): Path<(String, String)>,
) -> Api {
    let _guard = app.gate.lock().await;
    if let Some(record) = app.store.get(&attachment_key(&aid)).await? {
        if record["session_id"] != sid {
            return Err(ApiError(
                StatusCode::NOT_FOUND,
                "Attachment not found".into(),
            ));
        }
        if !record["run_id"].is_null() {
            return Err(conflict(
                "Sent attachments are preserved with their message",
            ));
        }
    }
    app.store.delete_pending_upload(&sid, &aid).await?;
    Ok(Json(json!({"deleted":true})))
}
pub(crate) async fn lookup(app: &App, sid: &str, aid: &str) -> Result<Value, ApiError> {
    if !files::upload_id_ok(aid) {
        return Err(ApiError(
            StatusCode::NOT_FOUND,
            "Attachment not found".into(),
        ));
    }
    app.store
        .get(&attachment_key(aid))
        .await?
        .filter(|v| v["session_id"] == sid)
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "Attachment not found".into()))
}
fn draft_owner<'a>(app: &'a App, record: &'a Value) -> &'a str {
    files::draft_owner(record, app.store.current_user_id())
}
fn require_draft_owner(app: &App, record: &Value) -> Result<(), ApiError> {
    if draft_owner(app, record) != crate::shared_chat::actor(app) {
        return Err(ApiError(StatusCode::NOT_FOUND, "Attachment not found".into()));
    }
    Ok(())
}
// Only the owning session can read this encrypted attachment record. The
// model-facing copy was already saved with the message at admission.
pub(crate) async fn attachment_get(State(app): State<App>, Path((sid, aid)): Path<(String, String)>) -> Api {
    if !files::upload_id_ok(&aid) {
        return Err(ApiError(StatusCode::NOT_FOUND, "Attachment not found".into()));
    }
    let record = app.store.get(&attachment_key(&aid)).await?
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "Attachment not found".into()))?;
    files::sent_view(&record, &sid).map(Json)
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND,"Attachment not found".into()))
}
pub(crate) async fn upload_content(
    State(app): State<App>,
    Path((sid, aid)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let record = lookup(&app, &sid, &aid).await?;
    if record["run_id"].is_null() { require_draft_owner(&app, &record)?; }
    let bytes = files::content_bytes(&record).map_err(|_| bad("Attachment bytes unavailable"))?;
    let media = record["media_type"]
        .as_str()
        .unwrap_or("application/octet-stream");
    let mut response = bytes.into_response();
    response.headers_mut().insert(
        "content-type",
        HeaderValue::from_str(media).map_err(|_| bad("Invalid media type"))?,
    );
    response.headers_mut().insert(
        "content-disposition",
        HeaderValue::from_static(if files::inline_media(media) { "inline" } else { "attachment" }),
    );
    response.headers_mut().insert(
        "content-security-policy",
        HeaderValue::from_static(files::CONTENT_SECURITY_POLICY),
    );
    Ok(response)
}
pub(crate) async fn prepare_uploads(
    app: &App,
    sid: &str,
    agent: &str,
    rid: &str,
    value: Option<&Value>,
    prepared: &mut attachments::Prepared,
) -> Result<(), ApiError> {
    if value.is_none() && prepared.metadata.is_empty() { return Ok(()); }
    // The session may still be pending admission, so its agent is supplied by the caller.
    let limits = limits_for(app, agent).await?;
    let empty = json!([]);
    let ids = value.unwrap_or(&empty).as_array().ok_or_else(|| bad("Attachment IDs must be an array"))?;
    files::check_message_limits(limits, ids.len(), prepared).map_err(|message| bad(&message))?;
    let mut seen = std::collections::HashSet::new();
    for id in ids {
        let id = id
            .as_str()
            .filter(|id| seen.insert(*id))
            .ok_or_else(|| bad("Invalid or duplicate attachment identifier"))?;
        let record = lookup(app, sid, id).await?;
        files::check_record_size(limits, &record).map_err(|message| bad(&message))?;
        if !record["run_id"].is_null() {
            return Err(conflict("Attachment is already sent"));
        }
        require_draft_owner(app, &record)?;
        prepared.claim_upload(id, record, rid);
    }
    Ok(())
}
pub(crate) async fn message_files(app: &App, messages: &mut [Value]) -> Result<()> {
    for message in messages.iter_mut().filter(|m| m["role"] == "user") {
        if let Some(rid) = message["run_id"].as_str() {
            if let Some(files) = app.store.get(&message_files_key(rid)).await? {
                message["attachments"] = files;
            }
        }
    }
    Ok(())
}

pub(crate) async fn upload_describe(
    State(app): State<App>,
    Path((sid, aid)): Path<(String, String)>,
) -> Api {
    let record = lookup(&app, &sid, &aid).await?;
    if record["preview"] != true {
        return Ok(Json(metadata(&record)));
    }
    if !record["run_id"].is_null() {
        return Err(conflict("Sent attachment is immutable"));
    }
    if !record["text"].as_str().unwrap_or("").is_empty() {
        return Ok(Json(metadata(&record)));
    }
    let run = match media_lane::lane_run(&app, &sid, model_selection::ModelRole::ImageIn).await {
        Ok(run) => run,
        Err(_) => {
            let run =
                media_lane::lane_run(&app, &sid, model_selection::ModelRole::Standard).await?;
            let catalog = app.store.model_catalog().await?;
            let entry = run["selection_plan"]["candidates"][0]["entry_id"]
                .as_str()
                .unwrap_or("");
            if !catalog
                .entries
                .get(entry)
                .is_some_and(|e| e.capabilities.contains("image_in"))
            {
                return Err(conflict(
                    "Assign an Image In model to describe attached images",
                ));
            }
            run
        }
    };
    let description=media_lane::lane_answer(&app,&run,files::describe_input(&record),files::DESCRIBE_INSTRUCTIONS).await?;
    let _guard = app.gate.lock().await;
    let mut current = lookup(&app, &sid, &aid).await?;
    if !current["run_id"].is_null() {
        return Err(conflict(
            "Attachment was sent before its description finished",
        ));
    }
    current["text"] = json!(description);
    current["described_by"] = run["selection_plan"]["candidates"][0]["entry_id"].clone();
    app.store.put(&attachment_key(&aid), &current).await?;
    Ok(Json(metadata(&current)))
}
#[cfg(test)]
mod shared_upload_tests {
    use super::*;
    use base64::{engine::general_purpose::STANDARD, Engine};
    #[tokio::test]
    async fn unsent_uploads_are_private_to_uploader_then_shared_when_sent() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let sid = "shared-upload";
        let aid = crate::uuid();
        app.store.put(&key("session",sid),&json!({"id":sid,"agent_id":"shared_default","status":"active"})).await.unwrap();
        let mut uploader = app.clone();
        uploader.chat_actor = Some("alice".into());
        let input = json!({"client_upload_id":aid,"name":"draft.txt","media_type":"text/plain","data":STANDARD.encode(b"private draft sentinel")});
        upload(State(uploader.clone()),Path(sid.into()),Json(input.clone())).await.unwrap();
        let mut record = lookup(&app,sid,&aid).await.unwrap();
        assert_eq!(record["uploader_id"],"alice");
        let content = upload_content(State(uploader.clone()),Path((sid.into(),aid.clone()))).await.unwrap();
        assert_eq!(axum::body::to_bytes(content.into_body(),1024).await.unwrap().as_ref(),b"private draft sentinel");
        for (actor,kind) in [("bob",crate::shared_chat::AccessKind::Participant),("staff",crate::shared_chat::AccessKind::Observer)] {
            let mut other = app.clone();
            other.chat_actor = Some(actor.into());
            other.chat_access = Some(crate::shared_chat::SessionAccess {actor_id:actor.into(),owner_id:"admin".into(),session_id:sid.into(),agent_id:"shared_default".into(),kind});
            assert_eq!(upload_content(State(other.clone()),Path((sid.into(),aid.clone()))).await.unwrap_err().0,StatusCode::NOT_FOUND);
            assert_eq!(upload(State(other.clone()),Path(sid.into()),Json(input.clone())).await.unwrap_err().0,StatusCode::CONFLICT);
            assert!(require_draft_owner(&other,&record).is_err());
            record["run_id"] = json!("published-message");
            app.store.put(&key("attachment",&aid),&record).await.unwrap();
            assert!(upload_content(State(other),Path((sid.into(),aid.clone()))).await.is_ok());
            record["run_id"] = Value::Null;
            app.store.put(&key("attachment",&aid),&record).await.unwrap();
        }
        record.as_object_mut().unwrap().remove("uploader_id");
        app.store.put(&key("attachment",&aid),&record).await.unwrap();
        assert!(upload_content(State(app.clone()),Path((sid.into(),aid.clone()))).await.is_ok());
        assert_eq!(upload_content(State(uploader),Path((sid.into(),aid))).await.unwrap_err().0,StatusCode::NOT_FOUND);
    }
}
