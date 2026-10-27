//! Packaged gate descriptors define which module can run at each fixed Rust checkpoint.
//! Operators select modules in workflow JSON; a descriptor never grants tools or publication.
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;
use crate::workflow::{Checkpoint, Policy};

pub const MESSAGE_VISIBILITY: &str = "message_visibility_v1";
pub const CLOSING_STATUS: &str = "closing_status_v1";
/// Checkout folder of the integral gate configuration files.
pub const GATE_FOLDER: &str = "crates/modules/engine/gates";
pub const PUBLICATION_SIZE: &str = "publication_size_guard_v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Runtime { Rust, PythonGate }

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema_version: u32,
    pub id: String,
    pub checkpoint: Checkpoint,
    pub runtime: Runtime,
    pub config: Value,
}

#[derive(Debug, Clone)]
pub struct Selection {
    pub id: String,
    pub optional: bool,
    pub manifest: Manifest,
}

fn source(id: &str) -> Result<String> {
    match id {
        MESSAGE_VISIBILITY | CLOSING_STATUS => {
            // Live files beside this crate (`live-files.json`); an integral gate has no
            // packaged default, so a missing file fails closed.
            let bytes = webagent_configuration::live::read(&format!("{GATE_FOLDER}/{id}.json"), 64 * 1024,
                |bytes| std::str::from_utf8(bytes).map(|_| ()).map_err(Into::into))?;
            Ok(String::from_utf8(bytes)?)
        }
        PUBLICATION_SIZE => publication_source(webagent_configuration::prompts::default_source(None)?.as_deref()),
        _ => bail!("Unknown gate module"),
    }
}

/// A configured published descriptor is authoritative; an absent publication uses the immutable image copy.
fn publication_source(source:Option<&std::path::Path>)->Result<String> {
    let relative="plugins/gates/publication_size_guard_v1/gate.json";
    let path=match source {
        Some(root)=>root.join(relative),
        None=>{
            #[cfg(any(test,feature="test-support"))]
            {std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").join(relative)}
            #[cfg(not(any(test,feature="test-support")))]
            {std::path::PathBuf::from("/opt/v3").join(relative)}
        },
    };
    read_publication_descriptor(&path)
}
fn read_publication_descriptor(path:&std::path::Path)->Result<String> {
    use std::io::Read;
    const LIMIT:u64=64*1024;
    let info=std::fs::symlink_metadata(path)?;
    if !info.is_file()||info.file_type().is_symlink()||info.len()>LIMIT {bail!("Invalid publication gate descriptor file");}
    let mut bytes=Vec::new();std::fs::File::open(path)?.take(LIMIT+1).read_to_end(&mut bytes)?;
    if bytes.len()as u64>LIMIT {bail!("Publication gate descriptor exceeds limit");}
    Ok(String::from_utf8(bytes)?)
}

pub fn manifest(id: &str) -> Result<Manifest> {
    let item: Manifest = serde_json::from_str(&source(id)?)?;
    validate_manifest(id, &item)?;
    Ok(item)
}

pub fn validate_manifest(id: &str, item: &Manifest) -> Result<()> {
    if item.schema_version != 1 || item.id != id {
        bail!("Invalid gate manifest identity");
    }
    // Each built-in descriptor is bounded and tied to its implementation. A future
    // plugin adapter can add IDs here only after its effects and data access are reviewed.
    match id {
        MESSAGE_VISIBILITY => {
            if item.checkpoint != Checkpoint::ModelDispatch || item.runtime != Runtime::Rust {
                bail!("Invalid message-visibility gate binding");
            }
            let config: VisibilityConfig = serde_json::from_value(item.config.clone())?;
            if config.tool_name != "publish_main_message"
                || config.guidance.is_empty() || config.guidance.len() > 4096 {
                bail!("Invalid message-visibility gate config");
            }
        }
        CLOSING_STATUS => {
            if item.checkpoint != Checkpoint::Publication || item.runtime != Runtime::Rust {
                bail!("Invalid closing-status gate binding");
            }
            let config: ClosingConfig = serde_json::from_value(item.config.clone())?;
            let expected = ["needs_input", "complete", "error", "partial", "unverified"];
            if config.tool_name != "set_closer_status"
                || config.flags.iter().map(String::as_str).collect::<Vec<_>>() != expected
                || config.fallback_status != "unverified"
                || !(1..=240).contains(&config.notification_max_chars)
                || !(1..=50).contains(&config.notification_max_words)
                || config.guidance.is_empty() || config.guidance.len() > 4096
                || config.fallback_notification.is_empty()
                || config.fallback_notification.chars().count() > config.notification_max_chars
                || config.fallback_notification.split_whitespace().count() > config.notification_max_words {
                bail!("Invalid closing-status gate config");
            }
        }
        PUBLICATION_SIZE => {
            if item.checkpoint != Checkpoint::Publication || item.runtime != Runtime::PythonGate {
                bail!("Invalid publication-size gate binding");
            }
            let config: SizeConfig = serde_json::from_value(item.config.clone())?;
            if !(1..=262144).contains(&config.max_text_bytes) {
                bail!("Invalid publication-size gate config");
            }
        }
        _ => unreachable!(),
    }
    Ok(())
}

pub use self::controls::{ClosingConfig, VisibilityConfig};
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SizeConfig { pub max_text_bytes: usize }

pub fn visibility_config() -> Result<VisibilityConfig> {
    Ok(serde_json::from_value(manifest(MESSAGE_VISIBILITY)?.config)?)
}
pub fn closing_config() -> Result<ClosingConfig> {
    Ok(serde_json::from_value(manifest(CLOSING_STATUS)?.config)?)
}

pub fn parse_selection(checkpoint: Checkpoint, configured: &str) -> Result<Selection> {
    let (optional, id) = match configured.strip_prefix("optional:") {
        Some(id) => (true, id),
        None => (false, configured),
    };
    let item = manifest(id)?;
    if item.checkpoint != checkpoint || (optional && item.runtime != Runtime::PythonGate) {
        bail!("Gate module cannot run at this checkpoint or in optional mode");
    }
    Ok(Selection { id: id.to_owned(), optional, manifest: item })
}

/// Preserve operator order, but reject duplicate IDs even if one spelling is optional.
pub fn selected(policy: &Policy, checkpoint: Checkpoint) -> Result<Vec<Selection>> {
    let name = serde_json::to_value(checkpoint)?.as_str().unwrap().to_owned();
    let mut seen = BTreeSet::new();
    let mut result = Vec::new();
    if let Some(configured) = policy.modules.get(&name) {
        if configured.len() > 8 { bail!("Too many gate modules at checkpoint"); }
        for raw in configured {
            let selection = parse_selection(checkpoint, raw)?;
            if !seen.insert(selection.id.clone()) { bail!("Duplicate gate module"); }
            result.push(selection);
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn packaged_manifests_are_bounded_and_typed() {
        let visibility = visibility_config().unwrap();
        assert_eq!(visibility.tool_name, "publish_main_message");
        assert!(visibility.guidance.contains("publish_main_message"));
        let closer = closing_config().unwrap();
        assert_eq!(closer.fallback_status, "unverified");
        assert!(closer.notification_max_chars <= 240);
        assert_eq!(manifest(PUBLICATION_SIZE).unwrap().runtime, Runtime::PythonGate);
        assert!(closer.notification_max_words <= 50);
    }
    #[test]
    fn malformed_integral_and_plugin_configs_fail_closed() {
        let mut close=manifest(CLOSING_STATUS).unwrap();
        close.config["notification_max_words"]=json!(0);
        assert!(validate_manifest(CLOSING_STATUS,&close).is_err());
        close.config["notification_max_words"]=json!(5);
        close.config["fallback_status"]=json!("complete");
        assert!(validate_manifest(CLOSING_STATUS,&close).is_err());
        let mut visibility=manifest(MESSAGE_VISIBILITY).unwrap();
        visibility.config["guidance"]=json!("");
        assert!(validate_manifest(MESSAGE_VISIBILITY,&visibility).is_err());
        let mut size=manifest(PUBLICATION_SIZE).unwrap();
        size.config["max_text_bytes"]=json!(262145);
        assert!(validate_manifest(PUBLICATION_SIZE,&size).is_err());
        size.config["max_text_bytes"]=json!(8192);
        size.checkpoint=Checkpoint::ModelDispatch;
        assert!(validate_manifest(PUBLICATION_SIZE,&size).is_err());
    }
    #[test]
    fn selection_rejects_unknown_duplicates_and_wrong_checkpoint() {
        for modules in [
            json!({"publication":["unknown"]}),
            json!({"publication":[PUBLICATION_SIZE, format!("optional:{PUBLICATION_SIZE}")]}),
            json!({"model_dispatch":[CLOSING_STATUS]}),
            json!({"publication":[format!("optional:{CLOSING_STATUS}")]}),
        ] {
            assert!(Policy::resolve(Some(&json!({"modules":modules}))).is_err());
        }
        let policy=Policy::resolve(Some(&json!({"modules":{
            "model_dispatch":[MESSAGE_VISIBILITY],
            "publication":[CLOSING_STATUS,format!("optional:{PUBLICATION_SIZE}")]
        }}))).unwrap();
        assert_eq!(selected(&policy,Checkpoint::Publication).unwrap().len(),2);
    }
}

pub mod controls {
//! The loop calls gate behavior through these two integral modules.
pub use super::initial::{VisibilityConfig, AgentVisibility, PromotionMode, agent_visibility, working_schema, working_text};
pub use super::closing::{AgentClosing, ClosingConfig, agent_closing, closing_schema, closing_status, fallback_status, guidance as closing_guidance};

}
mod initial {
//! The initial gate owns bounded working updates visible before the final reply.
use anyhow::{anyhow, bail, Result};
use crate::ports::AgentMetadata;
use webagent_access::storage::RecordCodec;
use serde_json::{json, Value};
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisibilityConfig { pub tool_name: String, pub guidance: String }

/// The agent selects one publication path; the server still owns the manual protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PromotionMode { None, Manual, Auto }

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentVisibility { pub mode: PromotionMode, pub guidance: String }
impl Default for AgentVisibility {
    fn default() -> Self { Self { mode: PromotionMode::Manual, guidance: String::new() } }
}
impl AgentVisibility {
    pub fn validate(&self) -> Result<()> {
        if self.guidance.len() > 4096 { bail!("Additional guidance must be at most 4096 bytes"); }
        Ok(())
    }
    pub fn from_stored(value: &Value) -> Result<Self> {
        if value.is_null() { return Ok(Self::default()); }
        if value.get("mode").is_some() { return Ok(serde_json::from_value(value.clone())?); }
        // Existing agents used a boolean. The retired format field is intentionally
        // ignored: Ask/Plan/Auto prompts now supply call-scoped formatting.
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Legacy { enabled: bool, guidance: String, #[serde(default)] format: String }
        let legacy: Legacy = serde_json::from_value(value.clone())?;
        let _ = legacy.format;
        Ok(Self { mode: if legacy.enabled { PromotionMode::Manual } else { PromotionMode::None },
            guidance: legacy.guidance })
    }
}
pub async fn agent_visibility(store: &impl crate::ports::AgentMetadata, agent: &str) -> Result<AgentVisibility> {
    let view = store.for_agent(agent).await?;
    let blob: Vec<u8> = sqlx::query_scalar("SELECT metadata FROM agents WHERE id=?")
        .bind(agent).fetch_one(view.agent_pool()).await?;
    let metadata = view.unseal(&blob)?;
    let settings = AgentVisibility::from_stored(&metadata["message_visibility"])?;
    settings.validate()?;
    Ok(settings)
}

pub fn working_schema(config: &VisibilityConfig) -> Value {
    json!({"type":"function","name":config.tool_name,
        "description":"Publish a short user-visible working update while this run continues.",
        "parameters":{"type":"object","additionalProperties":false,
            "properties":{"text":{"type":"string","description":"Complete clean working update"}},
            "required":["text"]}})
}

pub fn working_text(args: &Value) -> Result<String> {
    let object=args.as_object().ok_or_else(||anyhow!("Working update must be an object"))?;
    if object.len()!=1 { bail!("Working update has unexpected fields"); }
    let text=object.get("text").and_then(Value::as_str)
        .ok_or_else(||anyhow!("Working update text is missing"))?;
    if text.trim().is_empty() || text.len()>16384 { bail!("Working update text is empty or too large"); }
    Ok(text.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn agent_visibility_is_additive_strict_and_byte_bounded() {
        let defaults=AgentVisibility::default();assert_eq!(defaults.mode,PromotionMode::Manual);assert!(defaults.guidance.is_empty());
        assert!(AgentVisibility{mode:PromotionMode::None,guidance:"x".repeat(4096)}.validate().is_ok());
        assert!(AgentVisibility{mode:PromotionMode::Auto,guidance:"é".repeat(2049)}.validate().is_err());
        assert_eq!(AgentVisibility::from_stored(&json!({"enabled":true,"guidance":"ok","format":"retired"})).unwrap().mode,PromotionMode::Manual);
        assert_eq!(AgentVisibility::from_stored(&json!({"enabled":false,"guidance":"ok"})).unwrap().mode,PromotionMode::None);
        assert_eq!(AgentVisibility::from_stored(&json!({"mode":"auto","guidance":"ok"})).unwrap().mode,PromotionMode::Auto);
        assert!(AgentVisibility::from_stored(&json!({"mode":"auto","guidance":"ok","format":"retired"})).is_err());
        assert!(AgentVisibility::from_stored(&json!({"enabled":true,"guidance":"ok","fixed_protocol":"override"})).is_err());
        assert!(AgentVisibility::from_stored(&json!({"mode":"both","guidance":"ok"})).is_err());
    }
    #[test]
    fn working_update_is_bounded_and_exact() {
        assert_eq!(working_text(&json!({"text":"Progress"})).unwrap(),"Progress");
        assert!(working_text(&json!({"text":" ","extra":1})).is_err());
        assert!(working_text(&json!({"text":"x".repeat(16385)})).is_err());
    }
}

}
mod closing {
//! The closing gate records status and a short notification intent before publication.
use anyhow::{anyhow, bail, Result};
use crate::ports::AgentMetadata;
use webagent_access::storage::RecordCodec;
use serde_json::{json, Value};
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClosingConfig {
    pub tool_name: String,
    pub guidance: String,
    pub flags: Vec<String>,
    pub notification_max_chars: usize,
    pub notification_max_words: usize,
    pub fallback_status: String,
    pub fallback_notification: String,
}

/// Agent preferences guide the private closing notification.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentClosing {
    pub notification_guidance: String,
}
impl AgentClosing {
    pub fn validate(&self) -> Result<()> {
        if self.notification_guidance.len() > 4096 {
            bail!("Notification guidance must be at most 4096 bytes");
        }
        Ok(())
    }
}

pub async fn agent_closing(store: &impl crate::ports::AgentMetadata, agent: &str) -> Result<AgentClosing> {
    let view = store.for_agent(agent).await?;
    let blob: Vec<u8> = sqlx::query_scalar("SELECT metadata FROM agents WHERE id=?")
        .bind(agent).fetch_one(view.agent_pool()).await?;
    let metadata = view.unseal(&blob)?;
    let settings = if metadata["closing_message"].is_null() { AgentClosing::default() }
        else { serde_json::from_value(metadata["closing_message"].clone())? };
    settings.validate()?;
    Ok(settings)
}

pub fn guidance(config: &ClosingConfig, settings: &AgentClosing) -> String {
    let mut text = config.guidance.clone();
    if !settings.notification_guidance.trim().is_empty() {
        text.push_str("\nFor the private closing notification only, follow this agent guidance:\n");
        text.push_str(&settings.notification_guidance);
    }
    text
}
pub fn closing_schema(config: &ClosingConfig) -> Value {
    json!({"type":"function","name":config.tool_name,
        "description":"Record the final reply status and a short notification intent before finishing.",
        "parameters":{"type":"object","additionalProperties":false,
            "properties":{"flag":{"type":"string","enum":config.flags},
                "notification":{"type":"string","description":"At most five words"}},
            "required":["flag","notification"]}})
}

pub fn closing_status(args: &Value, config: &ClosingConfig) -> Result<Value> {
    let object=args.as_object().ok_or_else(||anyhow!("Closing status must be an object"))?;
    if object.len()!=2 { bail!("Closing status has unexpected fields"); }
    let flag=object.get("flag").and_then(Value::as_str)
        .ok_or_else(||anyhow!("Closing flag is missing"))?;
    if !config.flags.iter().any(|item|item==flag) { bail!("Closing flag is unsupported"); }
    let notification=object.get("notification").and_then(Value::as_str)
        .ok_or_else(||anyhow!("Closing notification is missing"))?;
    if notification.trim().is_empty()
        || notification.chars().count()>config.notification_max_chars
        || notification.split_whitespace().count()>config.notification_max_words {
        bail!("Closing notification is empty or too long");
    }
    Ok(json!({"flag":flag,"notification":notification,"fallback":false}))
}

pub fn fallback_status(config: &ClosingConfig) -> Value {
    json!({"flag":config.fallback_status,
        "notification":config.fallback_notification,"fallback":true})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn closer_rejects_unknown_or_long_status() {
        let config=crate::gates::closing_config().unwrap();
        assert_eq!(closing_status(&json!({"flag":"complete","notification":"Done"}),&config)
            .unwrap()["flag"],"complete");
        assert!(closing_status(&json!({"flag":"unknown","notification":"Done"}),&config).is_err());
        assert!(closing_status(&json!({"flag":"complete","notification":"one two three four five six"}),&config).is_err());
        assert_eq!(fallback_status(&config)["flag"],"unverified");
    }
    #[test]
    fn agent_guidance_is_bounded_and_keeps_the_fixed_protocol() {
        let config=crate::gates::closing_config().unwrap();
        let settings=AgentClosing{notification_guidance:"Name the next step".into()};
        settings.validate().unwrap();
        let combined=guidance(&config,&settings);
        assert!(combined.starts_with(&config.guidance));
        assert!(combined.contains("Name the next step"));
        assert!(AgentClosing{notification_guidance:"x".repeat(4097)}.validate().is_err());
    }
}

}
pub mod protocol {
//! Speak to the optional Python checker over a small, versioned local message.
//! It receives reply metadata, not the reply text. Rust still makes the final
//! decision and is the only process that can publish an assistant message.
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{path::Path, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
    time::timeout,
};

pub const MODULE_ID: &str = "publication_size_guard_v1";
pub const SOCKET_PATH: &str = "/run/v3/gate/gate.sock";
const PROTOCOL_VERSION: u32 = 1;
const MAX_FRAME: usize = 16 * 1024;
const GATE_UID: u32 = 1102;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateOutcome {
    Pass,
    Reject { module_id: String, reason_code: String },
    Skipped { module_id: String, reason_code: String },
}

#[derive(Debug, Clone)]
pub struct PublicationGateInput {
    pub run_id: String,
    pub generation: String,
    pub policy_revision: String,
    pub input_digest: String,
    pub turn: u64,
    pub call_index: u64,
    pub text_bytes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ErrorPolicy { Mandatory, Optional }

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct Request<'a> {
    protocol_version: u32,
    request_id: String,
    module_id: &'a str,
    checkpoint: &'static str,
    run_id: &'a str,
    generation: &'a str,
    policy_revision: &'a str,
    input_digest: &'a str,
    turn: u64,
    call_index: u64,
    facts: Facts,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct Facts { text_bytes: usize }

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Response {
    protocol_version: u32,
    request_id: String,
    module_id: String,
    decision: Decision,
    reason_code: Option<String>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Decision { Pass, Reject }

/// Python modules inspect bounded facts only. Rust owns activation, error policy,
/// native checks, and the final chat write; a worker response cannot publish.
pub async fn evaluate_publication(
    socket: &Path,
    policy: &crate::workflow::Policy,
    input: &PublicationGateInput,
) -> Result<GateOutcome> {
    let selections = crate::gates::selected(
        policy, crate::workflow::Checkpoint::Publication)?;
    for selected in selections {
        if selected.manifest.runtime != crate::gates::Runtime::PythonGate {
            continue;
        }
        let error_policy = if selected.optional { ErrorPolicy::Optional }
            else { ErrorPolicy::Mandatory };
        match timeout(
            Duration::from_secs(1),
            exchange(socket, input, &selected.id, GATE_UID),
        ).await {
            Ok(Ok(GateOutcome::Pass)) => {},
            Ok(Ok(other)) => return Ok(other),
            Ok(Err(_)) | Err(_) => return Ok(match error_policy {
                ErrorPolicy::Mandatory => GateOutcome::Reject {
                    module_id: selected.id, reason_code: "module_unavailable".into(),
                },
                ErrorPolicy::Optional => GateOutcome::Skipped {
                    module_id: selected.id, reason_code: "module_unavailable".into(),
                },
            }),
        }
    }
    Ok(GateOutcome::Pass)
}

async fn exchange(
    socket: &Path,
    input: &PublicationGateInput,
    module_id: &str,
    expected_uid: u32,
) -> Result<GateOutcome> {
    let mut stream = UnixStream::connect(socket).await.context("gate unavailable")?;
    if stream.peer_cred()?.uid() != expected_uid {
        bail!("gate peer identity mismatch");
    }
    let identity = serde_json::to_vec(&(
        &input.run_id, &input.generation, &input.policy_revision,
        &input.input_digest, input.turn, input.call_index, module_id,
    ))?;
    let request_id = hex::encode(Sha256::digest(identity));
    let request = Request {
        protocol_version: PROTOCOL_VERSION,
        request_id: request_id.clone(),
        module_id,
        checkpoint: "publication",
        run_id: &input.run_id,
        generation: &input.generation,
        policy_revision: &input.policy_revision,
        input_digest: &input.input_digest,
        turn: input.turn,
        call_index: input.call_index,
        facts: Facts { text_bytes: input.text_bytes },
    };
    let mut encoded = serde_json::to_vec(&request)?;
    if encoded.len() >= MAX_FRAME {
        bail!("gate request exceeds limit");
    }
    encoded.push(b'\n');
    stream.write_all(&encoded).await?;
    let mut frame = Vec::new();
    BufReader::new(stream.take((MAX_FRAME + 1) as u64))
        .read_until(b'\n', &mut frame).await?;
    if frame.len() > MAX_FRAME || frame.last() != Some(&b'\n') {
        bail!("invalid gate frame");
    }
    let response: Response = serde_json::from_slice(&frame)?;
    if response.protocol_version != PROTOCOL_VERSION
        || response.request_id != request_id
        || response.module_id != module_id {
        bail!("gate response identity mismatch");
    }
    match (response.decision, response.reason_code) {
        (Decision::Pass, None) => Ok(GateOutcome::Pass),
        (Decision::Reject, Some(reason)) if reason == "publication_size_exceeded" =>
            Ok(GateOutcome::Reject { module_id: module_id.into(), reason_code: reason }),
        _ => bail!("invalid gate decision"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::os::unix::fs::MetadataExt;
    use tokio::{io::{AsyncBufReadExt, AsyncWriteExt, BufReader}, net::UnixListener};

    fn input() -> PublicationGateInput {
        PublicationGateInput {
            run_id: "run".into(), generation: "gen".into(),
            policy_revision: "policy".into(), input_digest: "digest".into(),
            turn: 1, call_index: 0, text_bytes: 12,
        }
    }

    async fn mock(response: impl FnOnce(&serde_json::Value) -> serde_json::Value + Send + 'static)
        -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gate.sock");
        let listener = UnixListener::bind(&path).unwrap();
        tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut line = String::new();
            let mut reader = BufReader::new(socket);
            reader.read_line(&mut line).await.unwrap();
            let request: serde_json::Value = serde_json::from_str(&line).unwrap();
            let body = response(&request);
            reader.get_mut().write_all(format!("{body}\n").as_bytes()).await.unwrap();
        });
        (dir, path)
    }

    #[tokio::test]
    async fn accepted_response_binds_request_identity() {
        let (_dir, path) = mock(|r| json!({"protocol_version":1,
            "request_id":r["request_id"],"module_id":MODULE_ID,"decision":"pass",
            "reason_code":null})).await;
        let peer_uid = std::fs::metadata(&path).unwrap().uid();
        assert_eq!(exchange(&path, &input(), MODULE_ID, peer_uid).await.unwrap(),
            GateOutcome::Pass);
    }

    #[tokio::test]
    async fn explicit_reject_is_never_skipped() {
        let (_dir, path) = mock(|r| json!({"protocol_version":1,
            "request_id":r["request_id"],"module_id":MODULE_ID,"decision":"reject",
            "reason_code":"publication_size_exceeded"})).await;
        let peer_uid = std::fs::metadata(&path).unwrap().uid();
        assert_eq!(exchange(&path, &input(), MODULE_ID, peer_uid).await.unwrap(),
            GateOutcome::Reject { module_id: MODULE_ID.into(), reason_code:"publication_size_exceeded".into() });
    }

    #[tokio::test]
    async fn unavailable_gate_obeys_configured_error_policy() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.sock");
        let mandatory = crate::workflow::Policy::resolve(Some(
            &json!({"modules":{"publication":[MODULE_ID]}}))).unwrap();
        let optional = crate::workflow::Policy::resolve(Some(
            &json!({"modules":{"publication":["optional:publication_size_guard_v1"]}}))).unwrap();
        assert_eq!(evaluate_publication(&missing, &mandatory, &input()).await.unwrap(),
            GateOutcome::Reject { module_id: MODULE_ID.into(), reason_code:"module_unavailable".into() });
        assert_eq!(evaluate_publication(&missing, &optional, &input()).await.unwrap(),
            GateOutcome::Skipped { module_id: MODULE_ID.into(), reason_code:"module_unavailable".into() });
    }

    #[tokio::test]
    async fn wrong_identity_or_peer_is_rejected() {
        let (_dir, path) = mock(|_| json!({"protocol_version":1,
            "request_id":"wrong","module_id":MODULE_ID,"decision":"pass",
            "reason_code":null})).await;
        let peer_uid = std::fs::metadata(&path).unwrap().uid();
        assert!(exchange(&path, &input(), crate::gates::PUBLICATION_SIZE, peer_uid).await.is_err());
        let (_dir, path) = mock(|r| json!({"protocol_version":1,
            "request_id":r["request_id"],"module_id":MODULE_ID,"decision":"pass",
            "reason_code":null})).await;
        assert!(exchange(&path, &input(), MODULE_ID, u32::MAX).await.is_err());
    }
}

}

#[cfg(test)] mod publication_descriptor_file_tests {
 use super::*;
 #[test] fn authoritative_file_is_bounded_regular_and_never_falls_back() {
  let fixture=tempfile::tempdir().unwrap();
  let path=fixture.path().join("plugins/gates/publication_size_guard_v1/gate.json");
  assert!(publication_source(Some(fixture.path())).is_err());
  std::fs::create_dir_all(path.parent().unwrap()).unwrap();
  let original=publication_source(None).unwrap();std::fs::write(&path,&original).unwrap();
  let item:Manifest=serde_json::from_str(&publication_source(Some(fixture.path())).unwrap()).unwrap();
  validate_manifest(PUBLICATION_SIZE,&item).unwrap();
  std::fs::write(&path,vec![b' ';64*1024+1]).unwrap();assert!(publication_source(Some(fixture.path())).is_err());
  std::fs::remove_file(&path).unwrap();
  std::os::unix::fs::symlink("missing-target",&path).unwrap();assert!(publication_source(Some(fixture.path())).is_err());
  std::fs::remove_file(&path).unwrap();std::fs::create_dir(&path).unwrap();assert!(publication_source(Some(fixture.path())).is_err());
 }
}
