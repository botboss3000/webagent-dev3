//! One-time rename of saved exposure modes: visible → loaded, discoverable → on_request.
//! Rewriting only old values makes it idempotent; parsers read only the new names.
use super::*;

/// Rename one saved mode value; true when it changed.
fn rename(value: &mut Value) -> bool {
    let new = match value.as_str() {
        Some("visible") => "loaded",
        Some("discoverable") => "on_request",
        _ => return false,
    };
    *value = json!(new);
    true
}

fn rename_map(map: &mut Value) -> bool {
    let mut changed = false;
    for value in map.as_object_mut().into_iter().flat_map(|m| m.values_mut()) {
        changed |= rename(value);
    }
    changed
}

/// Rename the mode maps and capability-level exposures held in agent metadata.
pub(crate) fn rename_agent_modes(metadata: &mut Value) -> bool {
    let mut changed = false;
    for key in ["ability_modes", "skill_modes", "tool_modes"] {
        changed |= rename_map(&mut metadata[key]);
    }
    if let Some(levels) = metadata
        .get_mut("capability_levels")
        .and_then(|levels| levels.get_mut("levels"))
        .and_then(Value::as_object_mut)
    {
        for level in levels.values_mut() {
            for key in ["ability_exposure", "tool_exposure", "skill_exposure"] {
                if let Some(map) = level.get_mut(key) {
                    changed |= rename_map(map);
                }
            }
        }
    }
    changed
}

impl Store {
    /// Rewrite this agent's saved modes to the current names in place.
    pub(crate) async fn migrate_mode_names(&self) -> Result<()> {
        use webagent_access::storage::RecordCodec;
        let mut tx = self.agent_pool.begin().await?;
        let blob: Option<Vec<u8>> = sqlx::query_scalar("SELECT metadata FROM agents WHERE id=?")
            .bind(&self.agent_id)
            .fetch_optional(&mut *tx)
            .await?;
        let Some(blob) = blob else {
            return Ok(());
        };
        let mut metadata = RecordCodec::unseal_agent_for(self, &self.agent_id, &blob)?;
        if rename_agent_modes(&mut metadata) {
            sqlx::query("UPDATE agents SET metadata=? WHERE id=?")
                .bind(RecordCodec::seal_agent_for(
                    self,
                    &self.agent_id,
                    &metadata,
                )?)
                .bind(&self.agent_id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_mode_names_are_renamed_once() {
        let mut metadata = json!({"owner_id":"admin",
            "ability_modes":{"codebase_admin":"visible","web_access":"discoverable"},
            "skill_modes":{"codebase_admin":"visible","codebase_admin:ui_design":"off"},
            "tool_modes":{"file_read":"discoverable","file_edit":"off"},
            "capability_levels":{"revision":2,"levels":{"member":{
                "ability_exposure":{"*":"discoverable"},"tool_exposure":{"file_edit":"visible"},
                "skill_exposure":{}}}}});
        assert!(rename_agent_modes(&mut metadata));
        assert_eq!(metadata["ability_modes"]["codebase_admin"], "loaded");
        assert_eq!(metadata["ability_modes"]["web_access"], "on_request");
        assert_eq!(metadata["skill_modes"]["codebase_admin"], "loaded");
        assert_eq!(metadata["skill_modes"]["codebase_admin:ui_design"], "off");
        assert_eq!(metadata["tool_modes"]["file_read"], "on_request");
        let member = &metadata["capability_levels"]["levels"]["member"];
        assert_eq!(member["ability_exposure"]["*"], "on_request");
        assert_eq!(member["tool_exposure"]["file_edit"], "loaded");
        let migrated = metadata.clone();
        assert!(!rename_agent_modes(&mut metadata));
        assert_eq!(metadata, migrated);
    }

    #[tokio::test]
    async fn saved_agent_modes_migrate_on_open() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        let blob: Vec<u8> =
            sqlx::query_scalar("SELECT metadata FROM agents WHERE id='shared_default'")
                .fetch_one(&store.agent_pool)
                .await
                .unwrap();
        let mut metadata = webagent_access::storage::RecordCodec::unseal_agent_for(
            &store,
            "shared_default",
            &blob,
        )
        .unwrap();
        metadata["ability_modes"] = json!({"codebase_admin":"visible"});
        metadata["skill_modes"] = json!({"codebase_admin":"discoverable"});
        metadata["tool_modes"] = json!({"file_read":"visible"});
        sqlx::query("UPDATE agents SET metadata=? WHERE id='shared_default'")
            .bind(
                webagent_access::storage::RecordCodec::seal_agent_for(
                    &store,
                    "shared_default",
                    &metadata,
                )
                .unwrap(),
            )
            .execute(&store.agent_pool)
            .await
            .unwrap();
        store.app_pool.close().await;
        store.pool.close().await;
        store.agent_pool.close().await;
        let reopened = Store::open(root.path()).await.unwrap();
        let abilities = reopened.agent_abilities().await.unwrap();
        assert_eq!(abilities["visibility"]["codebase_admin"], "loaded");
        assert_eq!(
            abilities["skill_visibility"]["codebase_admin"],
            "on_request"
        );
        assert_eq!(abilities["tool_modes"]["file_read"], "loaded");
    }
}
