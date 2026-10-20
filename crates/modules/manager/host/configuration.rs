//! Host-owned encrypted settings and live installation prompt storage.
use anyhow::{bail, Result};
pub use webagent_manager::configuration::IncomingMessage;
pub async fn incoming(store: &crate::store::Store, agent: &str) -> Result<IncomingMessage> {
    Ok(snapshot(store, agent).await?.0)
}
pub async fn snapshot(store: &crate::store::Store, agent: &str) -> Result<(IncomingMessage, u64)> {
    let view = store.for_agent(agent).await?;
    let blob: Vec<u8> = sqlx::query_scalar("SELECT metadata FROM agents WHERE id=?")
        .bind(agent)
        .fetch_one(&view.agent_pool)
        .await?;
    let metadata = view.unseal(&blob)?;
    let value = &metadata["manager_incoming_message"];
    let mut settings: IncomingMessage = if value.is_null() {
        IncomingMessage::default()
    } else {
        serde_json::from_value(value.clone())?
    };
    if let Some(prompt) = read_prompt(&store.root, agent)? {
        settings.criteria = prompt;
    } else if !settings.criteria.is_empty() {
        // Preserve existing administrator criteria during the storage transition.
        save_prompt_inner(&store.root, agent, &settings.criteria, false)?;
        settings.criteria = read_prompt(&store.root, agent)?.unwrap_or_default();
    }
    settings.validate()?;
    Ok((
        settings,
        metadata["manager_incoming_message_revision"]
            .as_u64()
            .unwrap_or(0),
    ))
}

fn read_prompt(root: &std::path::Path, agent: &str) -> Result<Option<String>> {
    let path = root.join("config/prompts.json");
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let _=bytes;
    let prompts=crate::live_assets::resolved_prompts(root)?;
    match prompts
        .get("manager_incoming_message")
        .and_then(|entries| entries.get(agent))
    {
        None => Ok(None),
        Some(serde_json::Value::String(prompt)) => Ok(Some(prompt.clone())),
        Some(_) => bail!("Invalid manager prompt"),
    }
}
pub(crate) fn save_prompt(root: &std::path::Path, agent: &str, prompt: &str) -> Result<()> {
    save_prompt_inner(root, agent, prompt, true)
}
fn save_prompt_inner(
    root: &std::path::Path,
    agent: &str,
    prompt: &str,
    replace: bool,
) -> Result<()> {
    use fs2::FileExt;
    use std::io::Write;
    let directory = root.join("config");
    std::fs::create_dir_all(&directory)?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(directory.join(".prompts.lock"))?;
    lock.lock_exclusive()?;
    let path = directory.join("prompts.json");
    let original = match std::fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let mut prompts: serde_json::Value = serde_json::from_slice(
        original
            .as_deref()
            .unwrap_or(include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/config/prompts.json"))),
    )?;
    let object = prompts
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("Invalid prompts file"))?;
    let entries = object
        .entry("manager_incoming_message")
        .or_insert_with(|| serde_json::json!({}));
    let entries = entries
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("Invalid manager prompts"))?;
    if !replace && entries.contains_key(agent) {
        return Ok(());
    }
    entries.insert(
        agent.to_owned(),
        serde_json::Value::String(prompt.to_owned()),
    );
    let mut temporary = tempfile::NamedTempFile::new_in(&directory)?;
    if let Ok(metadata) = std::fs::metadata(&path) {
        temporary
            .as_file()
            .set_permissions(metadata.permissions())?;
    }
    serde_json::to_writer_pretty(&mut temporary, &prompts)?;
    temporary.write_all(b"\n")?;
    temporary.as_file().sync_all()?;
    // Cooperating API saves use the lock; detect unrelated direct configuration edits too.
    let current = match std::fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    if current != original {
        bail!("Prompts changed; retry saving");
    }
    temporary.persist(&path).map_err(|error| error.error)?;
    std::fs::File::open(directory)?.sync_all()?;
    Ok(())
}
#[cfg(test)]
mod prompt_file_tests {
    use super::*;
    #[test]
    fn prompts_are_agent_scoped_and_preserve_existing_fields() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("config")).unwrap();
        let path = root.path().join("config/prompts.json");
        std::fs::write(&path, r#"{"base_instructions":"keep me","other":{"x":1}}"#).unwrap();
        save_prompt(root.path(), "one", "Clothing only").unwrap();
        save_prompt(root.path(), "two", "Hardware only").unwrap();
        assert_eq!(
            read_prompt(root.path(), "one").unwrap().as_deref(),
            Some("Clothing only")
        );
        assert_eq!(
            read_prompt(root.path(), "two").unwrap().as_deref(),
            Some("Hardware only")
        );
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(value["base_instructions"], "keep me");
        assert_eq!(value["other"]["x"], 1);
    }
}
