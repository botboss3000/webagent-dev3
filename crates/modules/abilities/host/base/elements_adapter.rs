//! Saved UI elements for the app's pages (storage in
//! `webagent_native_tools::sandbox::elements`): the agent's Artifacts page lists and
//! deletes them, and an open chat loads one into its Artifacts card. Agents save them with
//! the approval-gated `sandbox_save`.
//! Every route works only on the caller's own elements with an agent they may use; a shared
//! element is deleted only by someone who manages the agent.
use crate::*;
use axum::Extension;
use webagent_native_tools::sandbox::elements::{self, Config, Scope};

fn caller(identity: &Value) -> std::result::Result<String, ApiError> {
    identity["_user_id"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| bad("Caller required"))
}

fn settings() -> std::result::Result<Config, ApiError> {
    Config::read().map_err(|error| ApiError(StatusCode::SERVICE_UNAVAILABLE, error.to_string()))
}

/// GET: the caller's elements and the shared ones for one agent.
pub async fn page_read(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path(agent): Path<String>,
) -> Api {
    super::core_scripts::page_store(&app, &identity, &agent).await?;
    let user = caller(&identity)?;
    let mut list =
        elements::list(&app.store.root, &agent, &user).map_err(|e| bad(&e.to_string()))?;
    let rights = app
        .store
        .agent_authority(&user, &agent)
        .await
        .unwrap_or_default();
    list["agent_id"] = json!(agent);
    list["can_manage"] = json!(rights["can_manage"] == true);
    list["max_elements"] = json!(settings()?.max_elements);
    Ok(Json(list))
}

/// DELETE: remove one of the caller's elements, or a shared element
/// when the caller manages the agent.
pub async fn page_delete(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path((agent, scope, name)): Path<(String, String, String)>,
) -> Api {
    super::core_scripts::page_store(&app, &identity, &agent).await?;
    let user = caller(&identity)?;
    let scope = Scope::parse(&scope).map_err(|e| bad(&e.to_string()))?;
    if scope == Scope::Shared
        && app
            .store
            .agent_authority(&user, &agent)
            .await
            .unwrap_or_default()["can_manage"]
            != true
    {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "Only someone who manages this agent can delete a shared element".into(),
        ));
    }
    let _guard = app.gate.lock().await;
    elements::delete(&app.store.root, &agent, &user, scope, &name)
        .map(|()| Json(json!({"status":"deleted"})))
        .map_err(|error| ApiError(StatusCode::NOT_FOUND, error.to_string()))
}

/// GET: one element for a card in the caller's own conversation `sid`: its HTML, its
/// description and what the frame's bridge may do. The element comes from the session's
/// agent, for the caller.
pub async fn session_element(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path((sid, scope, name)): Path<(String, String, String)>,
) -> Api {
    let user = caller(&identity)?;
    // The caller's own store finds only sessions the caller owns.
    let session = app
        .store
        .get(&key("session", &sid))
        .await?
        .filter(|session| session["status"] != "deleted")
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "Session not found".into()))?;
    let agent = session["agent_id"].as_str().unwrap_or("shared_default");
    let scope = Scope::parse(&scope).map_err(|e| bad(&e.to_string()))?;
    let (meta, html) = elements::read(&app.store.root, agent, &user, scope, &name)
        .map_err(|error| ApiError(StatusCode::NOT_FOUND, error.to_string()))?;
    Ok(Json(
        json!({"element":meta,"html":html,"bridge":settings()?.bridge()}),
    ))
}

#[derive(Deserialize)]
pub struct FrameQuery {
    card: String,
}

/// The policy of an element's frame document: an opaque origin (`sandbox allow-scripts`),
/// so it has no cookies and no same-origin access to the app; inline style and script only;
/// no network; framed only by the app itself.
const FRAME_POLICY: &str =
    "sandbox allow-scripts; default-src 'none'; script-src 'unsafe-inline'; \
    style-src 'unsafe-inline'; img-src data: blob:; font-src data:; media-src data: blob:; \
    connect-src 'none'; form-action 'none'; base-uri 'none'; frame-ancestors 'self'";

/// GET: the frame document of card `card` in the caller's conversation `sid`: the bridge
/// (`abilities/Core/sandbox/element-bridge.js`), the card's stored params and title, then the
/// element's HTML. Params come from the stored artifact, never from the request.
pub async fn session_frame(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path((sid, scope, name)): Path<(String, String, String)>,
    axum::extract::Query(query): axum::extract::Query<FrameQuery>,
) -> std::result::Result<Response, ApiError> {
    let Json(loaded) = session_element(
        State(app.clone()),
        Extension(identity),
        Path((sid.clone(), scope.clone(), name.clone())),
    )
    .await?;
    let card = app
        .store
        .session_artifacts(&sid)
        .await?
        .into_iter()
        .find(|row| {
            row["id"] == query.card.as_str()
                && row["kind"] == "ui_element"
                && row["element"]["name"] == name.as_str()
                && row["element"]["scope"] == scope.as_str()
        })
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "Card not found".into()))?;
    let bridge_path = webagent_abilities::tool_definitions::root()
        .map(|root| root.join("Core/sandbox/element-bridge.js"))
        .ok_or_else(|| {
            ApiError(
                StatusCode::SERVICE_UNAVAILABLE,
                "Element bridge unavailable".into(),
            )
        })?;
    let bridge = tokio::fs::read_to_string(&bridge_path).await.map_err(|_| {
        ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "Element bridge unavailable".into(),
        )
    })?;
    let data = json!({"params":card["params"],"card":{"title":card["title"],
        "description":card["sections"][0]["value"]}});
    // Escaped so no value can close the script element.
    let data = data.to_string().replace('<', "\\u003c");
    let document = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <script>window.__webagent={data};</script><script>{bridge}</script></head><body>{}</body></html>",
        loaded["html"].as_str().unwrap_or_default()
    );
    Ok((
        [
            (axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (axum::http::header::CONTENT_SECURITY_POLICY, FRAME_POLICY),
            (axum::http::header::CACHE_CONTROL, "no-store"),
            (axum::http::header::REFERRER_POLICY, "no-referrer"),
        ],
        document,
    )
        .into_response())
}
