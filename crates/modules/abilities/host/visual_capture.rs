//! Offline capture receives only the caller's stored mini app, never an authenticated browser cookie.
use crate::*;
use base64::Engine as _;
use serde::Deserialize;
use sha2::Digest as _;
/// Tools this adapter executes; their schemas live in the ability's `tools/<tool>/tool.json`.
pub const TOOLS: &[&str] = &["screenshot_genui"];
pub fn schemas() -> Vec<Value> {
    crate::tools::folder_schemas(TOOLS)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    genui_id: String,
    expected_revision: u64,
    width: Option<u64>,
    height: Option<u64>,
    theme: Option<String>,
    #[serde(default)]
    click_selectors: Vec<String>,
}
fn result(file: &Value, sid: &str, id: &str, observation: Value, console: Value) -> Value {
    json!({"attachment":api::uploads::metadata(file),"url":format!("/api/v3/sessions/{sid}/attachments/{id}/content"),"observation":observation,"console":console,"source":"isolated_browser_capture"})
}
pub async fn execute(
    app: &App,
    sid: &str,
    rid: &str,
    generation: &str,
    pending: &str,
    args: &Value,
    cancel: CancellationToken,
) -> Result<Value> {
    super::core_authority::check(
        app,
        sid,
        rid,
        generation,
        pending,
        "screenshot_genui",
        args,
        &cancel,
    )
    .await?;
    if shared_chat::actor(app) != app.store.current_user_id() {
        return Err(anyhow!(
            "Use an owner-private conversation for mini app capture"
        ));
    }
    let q: Args = serde_json::from_value(args.clone())?;
    Uuid::parse_str(&q.genui_id)?;
    let width = q.width.unwrap_or(1280);
    let height = q.height.unwrap_or(800);
    let theme = q.theme.as_deref().unwrap_or("dark");
    if !(320..=1600).contains(&width)
        || !(240..=1200).contains(&height)
        || !matches!(theme, "dark" | "light")
        || q.click_selectors.len() > 10
        || q.click_selectors
            .iter()
            .any(|s| s.is_empty() || s.len() > 200)
    {
        return Err(anyhow!("Capture viewport or actions exceed bounds"));
    }
    let state_id = format!("core_capture_receipt:{rid}:{pending}");
    let signature = digest(args);
    let id = super::core_visualizer::stable_id(&json!([sid, rid, pending, &signature]));
    let mut receipt = app.store.module_state(&state_id).await?;
    if receipt["state"].is_object() && !receipt["state"].as_object().unwrap().is_empty() {
        if receipt["state"]["digest"] != signature {
            return Err(anyhow!("Capture invocation identity reused"));
        }
        if receipt["state"]["completed"] == true {
            return Ok(json!({"status":"success","output":receipt["state"]["result"]}));
        }
        if let Ok(file) = api::uploads::lookup(app, sid, &id).await {
            if file["uploader_id"] != shared_chat::actor(app)
                || file["sha256"] != receipt["state"]["sha256"]
            {
                return Err(anyhow!("Capture attachment identity conflict"));
            }
            let output = result(
                &file,
                sid,
                &id,
                receipt["state"]["observation"].clone(),
                receipt["state"]["console"].clone(),
            );
            receipt["state"]["completed"] = json!(true);
            receipt["state"]["result"] = output.clone();
            app.store
                .module_cas(
                    &state_id,
                    receipt["revision"].as_u64().unwrap_or(0),
                    &receipt,
                )
                .await?;
            return Ok(json!({"status":"success","output":output}));
        }
    }
    let record = super::core_visualizer::record(app, &q.genui_id).await?;
    if record["revision"] != q.expected_revision {
        return Err(anyhow!("Mini app revision changed; read it before capture"));
    }
    let html = super::core_visualizer::document(&record, theme)?;
    let capture = tools::execute_trusted(
        "core_visual_capture",
        &json!({"html":html,"width":width,"height":height,"click_selectors":q.click_selectors}),
        sid,
        &app.tool_socket,
        None,
        cancel.clone(),
    )
    .await?;
    if capture["status"] != "success" {
        return Ok(capture);
    }
    super::core_authority::check(
        app,
        sid,
        rid,
        generation,
        pending,
        "screenshot_genui",
        args,
        &cancel,
    )
    .await?;
    let encoded = capture["output"]["png_base64"]
        .as_str()
        .filter(|v| v.len() <= 1400000)
        .ok_or_else(|| anyhow!("Capture image unavailable"))?;
    let bytes = base64::engine::general_purpose::STANDARD.decode(encoded)?;
    if bytes.len() > 1024 * 1024
        || bytes.len() < 24
        || !bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        || u32::from_be_bytes(bytes[16..20].try_into()?) != width as u32
        || u32::from_be_bytes(bytes[20..24].try_into()?) != height as u32
    {
        return Err(anyhow!("Capture returned an invalid PNG or viewport"));
    }
    let sha = hex::encode(sha2::Sha256::digest(&bytes));
    let observation = capture["output"]["observation"]
        .as_str()
        .filter(|s| s.len() <= 16384)
        .map(|s| json!(s))
        .unwrap_or(Value::Null);
    let console = capture["output"]["console"].clone();
    if serde_json::to_vec(&console)?.len() > 16384 {
        return Err(anyhow!("Capture diagnostics exceed bounds"));
    }
    receipt["state"] = json!({"digest":signature,"sha256":sha,"attachment_id":id,"observation":observation,"console":console,"completed":false});
    app.store
        .module_cas(
            &state_id,
            receipt["revision"].as_u64().unwrap_or(0),
            &receipt,
        )
        .await?;
    let _=api::uploads::upload(State(app.clone()),Path(sid.to_owned()),Json(json!({"client_upload_id":id,"name":format!("visual-{}.png",q.genui_id),"media_type":"image/png","data":encoded}))).await.map_err(|e|anyhow!(e.1))?;
    let file = api::uploads::lookup(app, sid, &id)
        .await
        .map_err(|e| anyhow!(e.1))?;
    if file["uploader_id"] != shared_chat::actor(app) || file["sha256"] != sha {
        return Err(anyhow!("Capture attachment identity conflict"));
    }
    let output = result(&file, sid, &id, observation.clone(), console.clone());
    let mut receipt = app.store.module_state(&state_id).await?;
    receipt["state"]["completed"] = json!(true);
    receipt["state"]["result"] = output.clone();
    app.store
        .module_cas(
            &state_id,
            receipt["revision"].as_u64().unwrap_or(0),
            &receipt,
        )
        .await?;
    let _guard = app.gate.lock().await;
    super::core_authority::check(
        app,
        sid,
        rid,
        generation,
        pending,
        "screenshot_genui",
        args,
        &cancel,
    )
    .await?;
    let mut logs = app
        .store
        .module_state(&format!("core_capture:{}", q.genui_id))
        .await?;
    logs["state"] = json!({"revision":q.expected_revision,"width":width,"height":height,"theme":theme,"sha256":sha,"observation":observation,"console":console});
    app.store
        .module_cas(
            &format!("core_capture:{}", q.genui_id),
            logs["revision"].as_u64().unwrap_or(0),
            &logs,
        )
        .await?;
    Ok(json!({"status":"success","output":output}))
}
