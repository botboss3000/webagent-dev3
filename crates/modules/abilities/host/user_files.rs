//! Native User Files adapter. The actor's private database owns files; no host path is accepted.
use crate::*;
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::Deserialize;
use webagent_abilities::user_files::{metadata, Repository};

pub fn owns(name: &str) -> bool {
    matches!(
        name,
        "save_file" | "list_user_files" | "read_user_file" | "attach_file"
    )
}
/// Tools this adapter executes; their schemas live in the ability's `tools/<tool>/tool.json`.
pub const TOOLS: &[&str] = &["save_file", "list_user_files", "read_user_file", "attach_file"];
pub fn schemas() -> Vec<Value> {
    crate::tools::folder_schemas(TOOLS)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Save {
    name: String,
    media_type: Option<String>,
    text: Option<String>,
    data: Option<String>,
    expected_revision: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Read {
    name: String,
    offset: Option<usize>,
    limit: Option<usize>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct List {
    offset: Option<u32>,
    limit: Option<u32>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Attach {
    name: String,
}
fn stable_uuid(input: &str) -> String {
    let hash = digest(&json!(input));
    let bytes = hex::decode(&hash[..32]).unwrap();
    Uuid::from_slice(&bytes).unwrap().to_string()
}
pub async fn execute(
    app: &App,
    sid: &str,
    rid: &str,
    generation: &str,
    pending: &str,
    name: &str,
    args: &Value,
    cancel: CancellationToken,
) -> Result<Value> {
    if cancel.is_cancelled() {
        return Err(anyhow!("File operation cancelled"));
    }
    let run = super::core_authority::check(app, sid, rid, generation, pending, name, args, &cancel)
        .await?;
    let actor = shared_chat::actor(app);
    if actor != app.store.current_user_id() {
        return Err(anyhow!(
            "Use an owner-private conversation for private saved files"
        ));
    }
    let user = app.store.for_user(actor).await?;
    let repo = Repository::new(&user.pool, &user);
    let output = match name {
        "save_file" => {
            let q: Save = serde_json::from_value(args.clone())?;
            webagent_abilities::user_files::name(&q.name)?;
            let bytes = match (q.text, q.data) {
                (Some(text), None) => text.into_bytes(),
                (None, Some(data)) => STANDARD
                    .decode(data)
                    .map_err(|_| anyhow!("Invalid file base64"))?,
                _ => return Err(anyhow!("Supply exactly one of text or data")),
            };
            let media = q.media_type.unwrap_or_else(|| "text/plain".into());
            if !api::uploads::allowed_media(&media) {
                return Err(anyhow!("Unsupported file media type"));
            }
            let limits = api::uploads::limits(app, sid).await.map_err(|e| anyhow!(e.1))?;
            if limits
                .max_file_size_mb
                .is_some_and(|n| bytes.len() > n.saturating_mul(1024 * 1024))
            {
                return Err(anyhow!("File exceeds configured attachment limit"));
            }
            let record = json!({"name":q.name,"media_type":media,"bytes":bytes.len(),"data":STANDARD.encode(&bytes),
                "sha256":hex::encode(Sha256::digest(&bytes))});
            repo.save(
                &record,
                q.expected_revision,
                &webagent_abilities::user_files::Invocation {
                    session_id: sid,
                    run_id: rid,
                    pending_id: pending,
                    generation: run["generation"]
                        .as_str()
                        .ok_or_else(|| anyhow!("Run generation unavailable"))?,
                    arguments: args,
                },
            )
            .await?
        }
        "list_user_files" => {
            let q: List = serde_json::from_value(args.clone())?;
            let offset = q.offset.unwrap_or(0);
            let limit = q.limit.unwrap_or(50);
            let files = repo.list(offset, limit).await?;
            let next = if files.len() == limit as usize {
                Some(offset.saturating_add(limit))
            } else {
                None
            };
            json!({"files":files,"next_offset":next})
        }
        "read_user_file" => {
            let q: Read = serde_json::from_value(args.clone())?;
            let file = repo
                .read(&q.name)
                .await?
                .ok_or_else(|| anyhow!("User file not found"))?;
            let bytes = STANDARD.decode(file["data"].as_str().unwrap_or(""))?;
            let start = q.offset.unwrap_or(0).min(bytes.len());
            let limit = q.limit.unwrap_or(16384);
            if !(1..=65536).contains(&limit) {
                return Err(anyhow!("Read limit must be 1..65536"));
            }
            let end = start.saturating_add(limit).min(bytes.len());
            let page = &bytes[start..end];
            json!({"file":metadata(&file),"offset":start,"data":STANDARD.encode(page),"text":std::str::from_utf8(page).ok(),
                "next_offset":if end<bytes.len(){Some(end)}else{None}})
        }
        "attach_file" => {
            let q: Attach = serde_json::from_value(args.clone())?;
            let file = repo
                .read(&q.name)
                .await?
                .ok_or_else(|| anyhow!("User file not found"))?;
            let id = stable_uuid(&format!("{sid}:{rid}:{pending}"));
            let info=api::uploads::upload(State(app.clone()),axum::extract::Path(sid.to_owned()),
                Json(json!({"client_upload_id":id,"name":file["name"],"media_type":file["media_type"],"data":file["data"]})))
                .await.map_err(|e|anyhow!(e.1))?.0;
            json!({"file":info,"url":format!("/api/v3/sessions/{sid}/attachments/{id}/content")})
        }
        _ => return Err(anyhow!("Unknown User Files tool")),
    };
    Ok(json!({"status":"success","output":output}))
}
