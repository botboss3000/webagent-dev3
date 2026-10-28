//! Per-agent chat configuration overlays: agent-plane storage, effective documents for
//! enforcement and UI serving, and the agent-manager API. Chat owns the merge rules.
use crate::{
    admin_agent_view, bad, conflict, json, now, Api, ApiError, App, Deserialize, Json, Path,
    PathBuf, Result, State, StatusCode, Store, Value,
};
use webagent_chat::agent_config::{self as overlays, Change, SaveError};
use webagent_chat::configuration::{
    parse_chat_preferences, read_chat_document, ChatModePreferences,
};

/// Read one agent's stored overlay record through that agent's database.
async fn record(store: &Store, agent: &str, document: &str) -> Result<Value> {
    let view = store.for_agent(agent).await?;
    overlays::read(&view.agent_pool, &view, agent, document).await
}

/// Effective chat.json for an agent: the installation base with that agent's valid overlay.
pub(crate) async fn effective_chat(
    store: &Store,
    base: &std::path::Path,
    agent: &str,
) -> Result<Value> {
    let base = read_chat_document(base).await?;
    Ok(overlays::effective(
        "chat",
        &base,
        &record(store, agent, "chat").await?["overlay"],
    ))
}

/// Execution-mode preferences (default mode, Auto confirmation) for sessions of this agent.
pub(crate) async fn preferences(app: &App, agent: &str) -> Result<ChatModePreferences> {
    parse_chat_preferences(&effective_chat(&app.store, &app.chat_ui_config_path, agent).await?)
}

/// Replace the UI response's `chat_ui` with the agent's effective document and add its dropdown overlay.
pub(crate) async fn apply_to_ui_config(
    store: &Store,
    agent: &str,
    value: &mut Value,
) -> Result<()> {
    let chat = record(store, agent, "chat").await?;
    let dropdown = record(store, agent, "session_dropdown").await?;
    value["chat_ui"] = overlays::effective("chat", &value["chat_ui"], &chat["overlay"]);
    // The browser merges this overlay over the published component JSON.
    value["components"] = json!({"session_dropdown":
        overlays::effective("session_dropdown", &json!({}), &dropdown["overlay"])});
    Ok(())
}

/// The published session-dropdown component JSON, from the same UI root the app serves.
fn session_dropdown_base(_state: &std::path::Path) -> Value {
    let ui = webagent_webapp::configuration::live_root(
        std::path::Path::new(webagent_webapp::configuration::LIVE_SETTINGS),
        std::path::Path::new("/live-checkout/ui"),
    )
    .unwrap_or_else(|| {
        PathBuf::from(std::env::var("V3_UI_ROOT").unwrap_or_else(|_| "/opt/v3/ui".into()))
    });
    std::fs::read(ui.join("chat/controls/session-dropdown/session-dropdown.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}))
}

async fn base(app: &App, document: &str) -> Result<Value> {
    if document == "chat" {
        read_chat_document(&app.chat_ui_config_path).await
    } else {
        Ok(session_dropdown_base(&app.store.root))
    }
}

/// Same authority as the agent model page; denial and unknown documents look like absence.
async fn managed(app: &App, agent: &str, document: &str) -> std::result::Result<Store, ApiError> {
    let allowed = overlays::known_document(document)
        && app
            .store
            .agent_authority(app.store.current_user_id(), agent)
            .await
            .is_ok_and(|r| r["can_manage"] == true);
    if !allowed {
        return Err(ApiError(
            StatusCode::NOT_FOUND,
            "Agent chat configuration not found".into(),
        ));
    }
    Ok(admin_agent_view(app, agent).await?.store)
}

fn response(agent: &str, document: &str, record: &Value, base: &Value) -> Value {
    json!({"agent_id":agent,"document":document,"overlay":record["overlay"],"revision":record["revision"],
        "effective":overlays::effective(document, base, &record["overlay"])})
}

pub(crate) async fn get(
    State(app): State<App>,
    Path((agent, document)): Path<(String, String)>,
) -> Api {
    let store = managed(&app, &agent, &document).await?;
    let record = overlays::read(&store.agent_pool, &store, &agent, &document).await?;
    Ok(Json(response(
        &agent,
        &document,
        &record,
        &base(&app, &document).await?,
    )))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PatchBody {
    patch: Value,
    expected_revision: i64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ResetBody {
    expected_revision: i64,
}

pub(crate) async fn patch(
    State(app): State<App>,
    Path((agent, document)): Path<(String, String)>,
    Json(body): Json<PatchBody>,
) -> Api {
    save(
        &app,
        &agent,
        &document,
        Change::Patch(&body.patch),
        body.expected_revision,
    )
    .await
}

pub(crate) async fn reset(
    State(app): State<App>,
    Path((agent, document)): Path<(String, String)>,
    Json(body): Json<ResetBody>,
) -> Api {
    save(
        &app,
        &agent,
        &document,
        Change::Reset,
        body.expected_revision,
    )
    .await
}

async fn save(app: &App, agent: &str, document: &str, change: Change<'_>, expected: i64) -> Api {
    let store = managed(app, agent, document).await?;
    let base = base(app, document).await?;
    let record = overlays::save(
        &store.agent_pool,
        &store,
        agent,
        document,
        change,
        expected,
        &base,
        app.store.current_user_id(),
        now() as i64,
    )
    .await
    .map_err(|error| match error {
        SaveError::Conflict => conflict("Agent chat configuration changed; reload before saving"),
        SaveError::Invalid(message) => bad(&message),
        SaveError::Storage(error) => error.into(),
    })?;
    Ok(Json(response(agent, document, &record, &base)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn agent_api_requires_management_and_overlays_reach_enforcement_and_ui() {
        let (root, mut app) = crate::runtime_tests::fixture().await;
        app.chat_ui_config_path = root.path().join("host-chat.json");
        std::fs::write(
            &app.chat_ui_config_path,
            json!({"chat_common":{"execution_mode":{"confirm_auto_mode":true}},
            "chat_limits":{"attachments":{"max_file_size_mb":10}}})
            .to_string(),
        )
        .unwrap();
        let path = |document: &str| Path(("shared_default".to_owned(), document.to_owned()));
        let body = |patch: Value, revision: i64| {
            Json(PatchBody {
                patch,
                expected_revision: revision,
            })
        };
        let saved = patch(
            State(app.clone()),
            path("chat"),
            body(
                json!({"chat_common":{"execution_mode":{"confirm_auto_mode":false}},
            "chat_limits":{"attachments":{"max_file_size_mb":50,"max_per_message":2}}}),
                0,
            ),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(
            (
                saved["revision"].clone(),
                saved["effective"]["chat_limits"]["attachments"].clone()
            ),
            (json!(1), json!({"max_file_size_mb":10,"max_per_message":2}))
        );
        assert!(
            !preferences(&app, "shared_default")
                .await
                .unwrap()
                .confirm_auto_mode
        );
        let limits = webagent_chat::configuration::parse_attachment_limits(
            &effective_chat(&app.store, &app.chat_ui_config_path, "shared_default")
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            (limits.max_file_size_mb, limits.max_per_message),
            (Some(10), Some(2))
        );
        let stale = patch(State(app.clone()), path("chat"), body(json!({"x":1}), 0))
            .await
            .unwrap_err();
        assert_eq!(stale.0, StatusCode::CONFLICT);
        let invalid = patch(
            State(app.clone()),
            path("session_dropdown"),
            body(json!({"other":{"x":1}}), 0),
        )
        .await
        .unwrap_err();
        assert_eq!(invalid.0, StatusCode::BAD_REQUEST);
        let _ = patch(
            State(app.clone()),
            path("session_dropdown"),
            body(json!({"mobile":{"footer":{"enabled":false}}}), 0),
        )
        .await
        .unwrap();
        let mut ui = json!({"chat_ui":{"chat_common":{}}});
        apply_to_ui_config(&app.store, "shared_default", &mut ui)
            .await
            .unwrap();
        assert_eq!(
            ui["chat_ui"]["chat_common"]["execution_mode"]["confirm_auto_mode"],
            false
        );
        assert_eq!(
            ui["components"]["session_dropdown"],
            json!({"mobile":{"footer":{"enabled":false}}})
        );
        let cleared = reset(
            State(app.clone()),
            path("chat"),
            Json(ResetBody {
                expected_revision: 1,
            }),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(
            (cleared["overlay"].clone(), cleared["revision"].clone()),
            (json!({}), json!(2))
        );
        assert!(
            preferences(&app, "shared_default")
                .await
                .unwrap()
                .confirm_auto_mode
        );
        assert_eq!(
            get(State(app.clone()), path("unknown"))
                .await
                .unwrap_err()
                .0,
            StatusCode::NOT_FOUND
        );
        // A caller without agent administration sees absence, not the overlay.
        let guest = app.store.create_guest_account().await.unwrap();
        let mut outsider = app.clone();
        outsider.store = app
            .store
            .for_user(guest["user_id"].as_str().unwrap())
            .await
            .unwrap();
        assert_eq!(
            get(State(outsider.clone()), path("chat"))
                .await
                .unwrap_err()
                .0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            patch(State(outsider), path("chat"), body(json!({"x":1}), 2))
                .await
                .unwrap_err()
                .0,
            StatusCode::NOT_FOUND
        );
    }
}
