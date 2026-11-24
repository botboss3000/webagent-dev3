//! Installation-administrator controls fence stale edits without granting module authority.
use super::{bad, conflict, Api, App, Deserialize, Json, Path, State};
use serde_json::{json, Value};
/// Read installed generations, per-component availability and content-free affected-work counts.
pub(crate) async fn read(State(app): State<App>) -> Api {
    let catalog = crate::engine_components::catalog()?;
    let controls = crate::engine_components::controls(&app.store.root)?;
    let mut affected = 0usize;
    let mut per_component = std::collections::BTreeMap::<String, usize>::new();
    let mut active_runs = 0usize;
    let mut active_tasks = 0usize;
    // Paused and approval-waiting runs leave an account out of the periodic scan, so count
    // over every account (an administrator page read); an account nobody wrote since its
    // last count is not opened again.
    for user in crate::agent_loop::every_user_id(&app).await? {
        let Some(counts) = account_counts(&app, &user).await? else {
            continue;
        };
        active_runs += counts.runs;
        active_tasks += counts.tasks;
        for component in counts.paused.iter().filter(|id| {
            crate::engine_components::working_component(id)
                || catalog.entries.contains_key(*id)
                || catalog.withdrawn.contains(*id)
        }) {
            affected += 1;
            *per_component.entry(component.clone()).or_default() += 1;
        }
    }
    let mut entries = Vec::new();
    for descriptor in catalog.entries.values() {
        let (version, generation) = crate::engine_components::execution_identity(descriptor);
        entries.push(json!({"id":descriptor.id,"group":descriptor.group,"interface_version":descriptor.interface_version,
   "implementation_version":version,"generation":generation,
   "installed_implementation_version":descriptor.implementation_version,"installed_generation":descriptor.artifact_sha256,
   "mode":if cfg!(feature="live-context"){"live"}else{"linked"},"enabled":!controls.disabled.contains(&descriptor.id),
   "available":crate::engine_components::component_available(&app.store.root,&descriptor.id),"runtime_requirements":descriptor.runtime_requirements,
   "build_dependencies":descriptor.build.source_inputs,"affected_work":per_component.get(&descriptor.id).copied().unwrap_or(0)}));
    }
    for id in &catalog.withdrawn {
        entries.push(json!({"id":id,"group":"withdrawn","mode":if cfg!(feature="live-context"){"live"}else{"linked"},"available":false,"enabled":!controls.disabled.contains(id),"installed":false,"withdrawn":true,"affected_work":per_component.get(id).copied().unwrap_or(0)}));
    }
    // Keep controls and affected counts visible even after a required package is removed.
    for id in crate::engine_components::WORKING_COMPONENTS {
        if !catalog.entries.contains_key(id) && !catalog.withdrawn.contains(id) {
            entries.push(json!({"id":id,"group":"engine-loop","available":false,
    "enabled":!controls.disabled.iter().any(|disabled|disabled==id),"installed":false,
    "affected_work":per_component.get(id).copied().unwrap_or(0)}));
        }
    }
    let adoption = |role: &str| {
        std::fs::read(
            app.store
                .root
                .join("publication")
                .join(format!("engine-{role}.json")),
        )
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
    };
    Ok(Json(
        json!({"namespace":"engine","revision":controls.revision,"catalog_revision":catalog.revision,"components":entries,
 "affected_work":affected,"active_work":{"runs":active_runs,"tasks":active_tasks},"adoption":{"api":adoption("api"),"loop":adoption("loop")}}),
    ))
}
/// One account's share of the page: active runs and background tasks, and the component
/// each component-paused run waits for (filtered against the catalog when the page is built).
#[derive(Clone, Default)]
struct Counts {
    runs: usize,
    tasks: usize,
    paused: Vec<String>,
}

/// Counts per (state root, account), with a hash of the account's database and log marks
/// taken before they were read. Every write changes a mark, so an equal hash means the counts
/// still hold.
type Kept = std::collections::HashMap<(std::path::PathBuf, String), (u64, Counts)>;
static KEPT: std::sync::LazyLock<std::sync::Mutex<Kept>> =
    std::sync::LazyLock::new(Default::default);

/// Accounts opened for a count, per state root (tests read their own root's number).
#[cfg(test)]
static OPENED: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<std::path::PathBuf, usize>>,
> = std::sync::LazyLock::new(Default::default);

/// One account's counts: kept ones while its files are unchanged (no database access, and
/// with the change watch no file read either), else read from the account. `None`: the
/// account could not be opened (skipped, as before).
async fn account_counts(app: &App, user: &str) -> anyhow::Result<Option<Counts>> {
    let marks = {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        app.store.write_marks(user).hash(&mut hasher);
        hasher.finish()
    };
    let key = (app.store.root.clone(), user.to_owned());
    let kept = KEPT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&key)
        .filter(|(seen, _)| *seen == marks)
        .map(|(_, counts)| counts.clone());
    if kept.is_some() {
        return Ok(kept);
    }
    let Some(user) = crate::agent_loop::user_app(app, user).await else {
        return Ok(None);
    };
    #[cfg(test)]
    {
        *OPENED
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(app.store.root.clone())
            .or_default() += 1;
    }
    let mut counts = Counts::default();
    for task in user.store.background_scan(None).await? {
        if matches!(task["status"].as_str(), Some("queued" | "running")) {
            counts.tasks += 1;
        }
    }
    for run in user.store.active_runs().await? {
        if matches!(
            run["status"].as_str(),
            Some("queued" | "running" | "waiting_approval" | "waiting_external")
        ) {
            counts.runs += 1;
        }
        if run["status"] == "paused" {
            let id = run["id"].as_str().unwrap_or("");
            let full = user.run(id).await?;
            if let Some(component) = full["component_pause"]["component"].as_str() {
                counts.paused.push(component.to_owned());
            }
        }
    }
    KEPT.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(key, (marks, counts.clone()));
    Ok(Some(counts))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Update {
    pub expected_revision: u64,
    pub expected_catalog_revision: String,
    pub enabled: bool,
}
/// The owning API serializes controls; packages remain exclusively broker-managed.
pub(crate) async fn update(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(body): Json<Update>,
) -> Api {
    let installed = crate::engine_components::catalog()?;
    if !installed.entries.contains_key(&id)
        && !installed.withdrawn.contains(&id)
        && !crate::engine_components::working_component(&id)
    {
        return Err(bad("Unknown engine component"));
    }
    let _guard = app.gate.lock().await;
    let catalog = crate::engine_components::catalog()?;
    let mut current = crate::engine_components::controls(&app.store.root)?;
    if current.revision != body.expected_revision
        || catalog.revision != body.expected_catalog_revision
    {
        return Err(conflict("Engine components changed; reload before saving"));
    }
    if body.enabled && !catalog.entries.contains_key(&id) {
        return Err(bad(
            "Install a compatible component through the build broker first",
        ));
    }
    current.revision = current
        .revision
        .checked_add(1)
        .ok_or_else(|| bad("Component revision exhausted"))?;
    current.disabled.retain(|v| v != &id);
    if !body.enabled {
        current.disabled.push(id);
    }
    let root = app.store.root.clone();
    tokio::task::spawn_blocking(move || crate::engine_components::save_controls(&root, &current))
        .await
        .map_err(anyhow::Error::from)??;
    drop(_guard);
    read(State(app)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    /// Reject stale control revisions and verify disabling and restoring a registered package.
    async fn disable_fences_stale_saves_and_keeps_removed_package_controls() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let before = read(State(app.clone())).await.unwrap().0;
        let body = Update {
            expected_revision: before["revision"].as_u64().unwrap(),
            expected_catalog_revision: before["catalog_revision"].as_str().unwrap().into(),
            enabled: false,
        };
        let after = update(
            State(app.clone()),
            Path(crate::engine_components::CONTEXT.into()),
            Json(body),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(
            after["components"]
                .as_array()
                .unwrap()
                .iter()
                .find(|item| item["id"] == crate::engine_components::CONTEXT)
                .unwrap()["available"],
            false
        );
        assert_eq!(after["revision"], 1);
        let stale = Update {
            expected_revision: 0,
            expected_catalog_revision: before["catalog_revision"].as_str().unwrap().into(),
            enabled: true,
        };
        assert!(update(
            State(app.clone()),
            Path(crate::engine_components::CONTEXT.into()),
            Json(stale)
        )
        .await
        .is_err());
        let enabled = Update {
            expected_revision: 1,
            expected_catalog_revision: before["catalog_revision"].as_str().unwrap().into(),
            enabled: true,
        };
        let restored = update(
            State(app.clone()),
            Path(crate::engine_components::CONTEXT.into()),
            Json(enabled),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(
            restored["components"]
                .as_array()
                .unwrap()
                .iter()
                .find(|item| item["id"] == crate::engine_components::CONTEXT)
                .unwrap()["available"],
            true
        );
    }
    /// A second read opens no account nobody wrote since the first, with the same numbers; a
    /// write to the account (a run paused for a component, a run finished) is counted at once.
    #[tokio::test]
    async fn an_unwritten_account_is_not_opened_again_and_a_written_one_is_counted() {
        let (_root, app) = Box::pin(crate::runtime_tests::fixture()).await;
        let opened = || {
            OPENED
                .lock()
                .unwrap()
                .get(&app.store.root)
                .copied()
                .unwrap_or(0)
        };
        let sid = crate::runtime_tests::conversation(&app).await;
        // One chat per run: a second message to a chat joins its running run.
        let active = crate::runtime_tests::queued(&app, &sid).await;
        let other_chat = crate::runtime_tests::conversation(&app).await;
        let paused = crate::runtime_tests::queued(&app, &other_chat).await;
        let mut run = app.run(&paused).await.unwrap();
        run["component_pause"] = json!({"component":crate::engine_components::ASSETS,"version":1});
        crate::engine_components::pause(&app, &mut run)
            .await
            .unwrap();
        let first = read(State(app.clone())).await.unwrap().0;
        assert_eq!(first["affected_work"], 1);
        assert_eq!(first["active_work"]["runs"], 1);
        let accounts = opened();
        assert!(accounts >= 1, "the first read opens the accounts");
        let second = read(State(app.clone())).await.unwrap().0;
        assert_eq!(second, first);
        assert_eq!(opened(), accounts, "nothing written: nothing opened again");
        // The queued run ends and a second run is paused for a component.
        let mut finished = app.run(&active).await.unwrap();
        finished["status"] = json!("completed");
        app.save_override(&mut finished).await.unwrap();
        let third_chat = crate::runtime_tests::conversation(&app).await;
        let other = crate::runtime_tests::queued(&app, &third_chat).await;
        let mut run = app.run(&other).await.unwrap();
        run["component_pause"] =
            json!({"component":crate::engine_components::CAPABILITY,"version":1});
        crate::engine_components::pause(&app, &mut run)
            .await
            .unwrap();
        let third = read(State(app.clone())).await.unwrap().0;
        assert_eq!(third["affected_work"], 2);
        assert_eq!(third["active_work"]["runs"], 0);
        assert_eq!(
            opened(),
            accounts + 1,
            "only the written account is opened again"
        );
    }
    /// All newly registered modules retain independent controls and paused-work counts.
    #[tokio::test]
    async fn new_component_controls_and_counts_are_projected_independently() {
        let (_root, app) = Box::pin(crate::runtime_tests::fixture()).await;
        let sid = crate::runtime_tests::conversation(&app).await;
        let run_id = crate::runtime_tests::queued(&app, &sid).await;
        let mut run = app.run(&run_id).await.unwrap();
        for id in [
            crate::engine_components::ASSETS,
            crate::engine_components::CAPABILITY,
            crate::engine_components::PROJECTION,
        ] {
            run["component_pause"] = json!({"component":id,"version":1});
            crate::engine_components::pause(&app, &mut run)
                .await
                .unwrap();
            let before = read(State(app.clone())).await.unwrap().0;
            assert_eq!(before["affected_work"], 1);
            let entry = before["components"]
                .as_array()
                .unwrap()
                .iter()
                .find(|entry| entry["id"] == id)
                .unwrap();
            assert_eq!(entry["affected_work"], 1);
            let disabled = update(
                State(app.clone()),
                Path(id.into()),
                Json(Update {
                    expected_revision: before["revision"].as_u64().unwrap(),
                    expected_catalog_revision: before["catalog_revision"].as_str().unwrap().into(),
                    enabled: false,
                }),
            )
            .await
            .unwrap()
            .0;
            let entry = disabled["components"]
                .as_array()
                .unwrap()
                .iter()
                .find(|entry| entry["id"] == id)
                .unwrap();
            assert_eq!(entry["available"], false);
            assert_eq!(entry["enabled"], false);
            assert_eq!(entry["affected_work"], 1);
            for other in [
                crate::engine_components::ASSETS,
                crate::engine_components::CAPABILITY,
                crate::engine_components::PROJECTION,
            ] {
                if other != id {
                    assert_eq!(
                        disabled["components"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .find(|entry| entry["id"] == other)
                            .unwrap()["enabled"],
                        true
                    );
                }
            }
            let restored = update(
                State(app.clone()),
                Path(id.into()),
                Json(Update {
                    expected_revision: disabled["revision"].as_u64().unwrap(),
                    expected_catalog_revision: disabled["catalog_revision"]
                        .as_str()
                        .unwrap()
                        .into(),
                    enabled: true,
                }),
            )
            .await
            .unwrap()
            .0;
            assert_eq!(
                restored["components"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|entry| entry["id"] == id)
                    .unwrap()["available"],
                true
            );
        }
    }
}
