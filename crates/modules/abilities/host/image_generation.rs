//! Image Out requests retain a durable started receipt before a potentially billable request.
use crate::{api, digest, model_selection, shared_chat, App, State};
use anyhow::{anyhow, Result};
use axum::extract::Path;
use axum::Json;
use base64::Engine as _;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::Digest as _;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
pub fn owns(name: &str) -> bool {
    name == "generate_image"
}
pub fn schemas() -> Vec<Value> {
    vec![
        json!({"type":"function","name":"generate_image","description":"Generate one image using this agent's assigned Image Out role. Returns an authenticated image URL. Interrupted requests are not silently repeated.",
 "parameters":{"type":"object","properties":{"prompt":{"type":"string","minLength":1,"maxLength":16384}},"required":["prompt"],"additionalProperties":false}}),
    ]
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    prompt: String,
}
fn attachment_id(input: &str) -> String {
    let h = digest(&json!(input));
    let bytes = hex::decode(&h[..32]).expect("a digest is lowercase hex");
    Uuid::from_slice(&bytes)
        .expect("32 hex characters are 16 bytes")
        .to_string()
}
/// A failure before any provider request: report it plainly instead of as an uncertain effect.
fn not_sent(message: &str) -> Value {
    json!({"status":"error","output":message})
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
        "generate_image",
        args,
        &cancel,
    )
    .await?;
    let q: Args = serde_json::from_value(args.clone())?;
    if q.prompt.trim().is_empty() || q.prompt.len() > 16384 {
        return Ok(not_sent("Image prompt must contain 1..16384 bytes"));
    }
    let receipt = format!(
        "receipt:core_image:{sid}:{}",
        digest(&json!([rid, pending]))
    );
    let id = attachment_id(&receipt);
    let hash = digest(args);
    if let Some(saved) = app.store.get(&receipt).await? {
        if saved["digest"] != hash {
            return Err(anyhow!("Image invocation reused with different input"));
        }
        if saved["state"] == "completed" {
            return Ok(json!({"status":"success","output":saved["result"]}));
        }
        if saved["state"] == "generated" {
            return publish(app, sid, &receipt, &id, &hash, &saved).await;
        }
        return Ok(
            json!({"status":"uncertain","uncertain":true,"output":{"receipt":receipt,"message":"Image request may have been submitted. Reconcile this receipt before another generation."}}),
        );
    }
    // Nothing has been submitted yet, so these failures are definite errors.
    let lane = match api::media_lane::lane_run(app, sid, model_selection::ModelRole::ImageOut).await
    {
        Ok(lane) => lane,
        Err(error) => return Ok(not_sent(&error.1.to_string())),
    };
    let (selection, tokens) = match api::media_lane::lane_selection(app, &lane).await {
        Ok(selected) => selected,
        Err(error) => return Ok(not_sent(&error.1.to_string())),
    };
    if !webagent_models::transport::image_transport(selection.provider) {
        return Ok(not_sent(
            "Assigned Image Out transport does not support image generation",
        ));
    }
    let gate = app.gate.lock().await;
    super::core_authority::check(
        app,
        sid,
        rid,
        generation,
        pending,
        "generate_image",
        args,
        &cancel,
    )
    .await?;
    let (current, _) = api::media_lane::lane_selection(app, &lane)
        .await
        .map_err(|e| anyhow!(e.1))?;
    if current != selection {
        return Err(anyhow!("Image model authority changed"));
    }
    if app.store.get(&receipt).await?.is_some() {
        return Err(anyhow!(
            "Image invocation already started; inspect its receipt"
        ));
    }
    app.store
        .atomic_docs_event(
            vec![(
                receipt.clone(),
                json!({"digest":hash,"state":"started","attachment_id":id}),
            )],
            sid,
            "core_image_started",
            &json!({"attachment_id":id}),
        )
        .await?;
    let mut request =
        Box::pin(
            app.provider
                .generate_image(&selection, &tokens, &q.prompt, sid, cancel.clone()),
        );
    let mut immediate = None;
    futures_util::future::poll_fn(|cx| {
        if let std::task::Poll::Ready(value) = std::future::Future::poll(request.as_mut(), cx) {
            immediate = Some(value);
        }
        std::task::Poll::Ready(())
    })
    .await;
    drop(gate);
    let generated = match immediate {
        Some(value) => value,
        None => request.await,
    };
    let image = match generated {
        Ok(image) => image,
        Err(_) => {
            return Ok(
                json!({"status":"uncertain","uncertain":true,"output":{"receipt":receipt,"message":"Image generation did not complete; do not replay a potentially billable request without reconciliation."}}),
            )
        }
    };
    let bytes = base64::engine::general_purpose::STANDARD.decode(&image.data)?;
    let sha = hex::encode(sha2::Sha256::digest(&bytes));
    let saved = json!({"digest":hash,"state":"generated","attachment_id":id,"sha256":sha,"media_type":image.media_type,"bytes":bytes.len(),"data":image.data});
    app.store.put(&receipt, &saved).await?;
    let output = publish(app, sid, &receipt, &id, &hash, &saved).await?;
    super::core_authority::check(
        app,
        sid,
        rid,
        generation,
        pending,
        "generate_image",
        args,
        &cancel,
    )
    .await?;
    Ok(output)
}
/// Recover attachment publication from saved bytes without making another billable provider request.
async fn publish(
    app: &App,
    sid: &str,
    receipt: &str,
    id: &str,
    hash: &str,
    saved: &Value,
) -> Result<Value> {
    let media = saved["media_type"]
        .as_str()
        .ok_or_else(|| anyhow!("Generated image type unavailable"))?;
    let bytes = base64::engine::general_purpose::STANDARD.decode(
        saved["data"]
            .as_str()
            .filter(|s| s.len() <= 24 * 1024 * 1024)
            .ok_or_else(|| anyhow!("Generated image recovery bytes unavailable"))?,
    )?;
    let sha = hex::encode(sha2::Sha256::digest(&bytes));
    if saved["sha256"] != sha {
        return Err(anyhow!("Generated image recovery digest conflict"));
    }
    if api::uploads::lookup(app, sid, id).await.is_err() {
        let filename = match media {
            "image/jpeg" => "generated.jpg",
            "image/webp" => "generated.webp",
            "image/gif" => "generated.gif",
            "image/png" => "generated.png",
            _ => return Err(anyhow!("Generated image type unsupported")),
        };
        let _ = api::uploads::upload(State(app.clone()),Path(sid.to_owned()),Json(json!({"client_upload_id":id,"name":filename,"media_type":media,"data":saved["data"]}))).await.map_err(|e|anyhow!(e.1))?;
    }
    let file = api::uploads::lookup(app, sid, id)
        .await
        .map_err(|e| anyhow!(e.1))?;
    if file["uploader_id"] != shared_chat::actor(app) || file["sha256"] != sha {
        return Err(anyhow!("Generated image attachment identity conflict"));
    }
    let result = json!({"attachment":api::uploads::metadata(&file),"url":format!("/api/v3/sessions/{sid}/attachments/{id}/content"),"role":"Image Out"});
    app.store
        .put(
            receipt,
            &json!({"digest":hash,"state":"completed","result":result}),
        )
        .await?;
    Ok(json!({"status":"success","output":result}))
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::uuid;
    #[tokio::test]
    async fn stored_image_recovery_publishes_once_and_rejects_preexisting_different_bytes() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let sid = crate::runtime_tests::conversation(&app).await;
        let bytes = b"\x89PNG\r\n\x1a\nfixture-image-recovery";
        let data = base64::engine::general_purpose::STANDARD.encode(bytes);
        let sha = hex::encode(sha2::Sha256::digest(bytes));
        let saved = json!({"data":data,"sha256":sha,"media_type":"image/png"});
        let id = uuid();
        let receipt = format!("receipt:core_image:{sid}:recovery");
        let first = publish(&app, &sid, &receipt, &id, "input", &saved)
            .await
            .unwrap();
        assert_eq!(first["status"], "success");
        assert!(app
            .store
            .get(&receipt)
            .await
            .unwrap()
            .unwrap()
            .get("data")
            .is_none());
        let second = publish(&app, &sid, &receipt, &id, "input", &saved)
            .await
            .unwrap();
        assert_eq!(first, second);
        let wrong = json!({"data":base64::engine::general_purpose::STANDARD.encode(b"different"),"sha256":hex::encode(sha2::Sha256::digest(b"different")),"media_type":"image/png"});
        assert!(publish(&app, &sid, &receipt, &id, "input", &wrong)
            .await
            .is_err());
    }
}
