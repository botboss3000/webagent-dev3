//! HTTP admission, administration, and routing. Rust checks authority before a run is queued.
pub(crate) mod shared_chat;
use super::*;
mod auth;
pub(crate) mod embed;
mod admin;
pub(crate) mod sessions;
pub(crate) mod conversation_history;
pub(crate) use conversation_history::*;
mod promotions;
pub(crate) mod chat;
pub(crate) mod followup_queue;
pub(crate) mod session_messaging;
pub(crate) mod direct_messages;
mod chat_controls;
pub(crate) mod media_lane;
pub(crate) mod uploads;
mod voice;
#[cfg(test)]mod composer_tests;
#[cfg(test)]mod session_deletion_tests;
mod ability_admin;
mod agent_members;
mod service_admin;
mod administrator;
#[path = "../../crates/modules/models/host/api/platform_catalog.rs"]
mod model_catalog;
#[path = "../../crates/modules/models/host/api/account_catalog.rs"]
mod account_models;
#[path = "../../crates/modules/webapp/host/agent_pages.rs"]
mod agent_pages;
#[path = "../../crates/modules/webapp/host/app_pages.rs"]
mod app_pages;
mod file_manager;
mod build_dashboard;
#[path = "../../crates/modules/configuration/host/app_settings.rs"]
mod app_settings;
#[path = "../../crates/modules/configuration/host/api.rs"]
pub(crate) mod live_configuration;
#[path = "../../crates/modules/system/host/api.rs"]
mod system;
mod loop_settings;
mod engine_components;
pub(crate) mod notifications;
pub(crate) mod repository;
pub(crate) use auth::*;
pub(crate) use admin::*;
pub(crate) use sessions::*;
pub(crate) use promotions::*;
pub(crate) use chat::*;
pub(crate) use chat_controls::*;
pub(crate) use voice::*;
pub(crate) use uploads::{upload,upload_delete,upload_content,upload_describe};
// CONTEXT-CONFIG AUTHORITY: one validated data-root JSON serves seeded defaults
// and live updates. Model boundaries reread it; existing runs keep their history.
async fn context_config_get(State(app): State<App>) -> Api {
    let config=crate::context_compaction::Config::read(&app.compaction_config_path)?;
    let value=serde_json::to_value(config).map_err(anyhow::Error::from)?;
    Ok(Json(json!({"config":value,"digest":crate::digest(&value)})))
}
async fn context_config_put(State(app): State<App>, Json(body): Json<Value>) -> Api {
    let expected=body["expected_digest"].as_str()
        .filter(|s|s.len()==64 && s.bytes().all(|b|b.is_ascii_hexdigit()))
        .ok_or_else(||bad("Expected context configuration digest required"))?;
    let config:crate::context_compaction::Config=serde_json::from_value(body["config"].clone())
        .map_err(|_|bad("Invalid context configuration"))?;
    config.validate().map_err(|_|bad("Invalid context configuration"))?;
    let _guard=app.gate.lock().await;
    let current=crate::context_compaction::Config::read(&app.compaction_config_path)?;
    if crate::digest(&serde_json::to_value(current).map_err(anyhow::Error::from)?)!=expected {
        return Err(conflict("Context configuration changed; reload before saving"));
    }
    config.persist(&app.compaction_config_path)?;
    let value=serde_json::to_value(config).map_err(anyhow::Error::from)?;
    Ok(Json(json!({"config":value,"digest":crate::digest(&value)})))
}

pub(super) fn router(app: App) -> Router {
    let event_aliases=Router::new()
        .route("/admin/events",get(notifications::read))
        .route("/admin/events/stream",get(notifications::stream))
        .route_layer(middleware::from_fn_with_state(app.clone(),admin_auth))
        .route_layer(middleware::from_fn_with_state(app.clone(),auth));
    let messaging = user_api::messaging_router()
        .route_layer(middleware::from_fn_with_state(app.clone(), message_api_auth));
    let admin_api = Router::new()
        .merge(diagnostics::router())
        .nest("/admin/system", system::router())
        .route("/admin/event-readers",get(notifications::readers).post(notifications::issue_reader))
        .route("/admin/event-readers/{id}",axum::routing::delete(notifications::revoke_reader))
        .route("/admin/events",get(notifications::read))
        .route("/admin/events/stream",get(notifications::stream))
        .route("/admin/modules",get(crate::modules::admin_list))
        .route("/admin/modules/{id}",post(crate::modules::lifecycle))
        .route("/status", get(overview))
        .route("/settings", put(settings))
        .route("/admin/configuration",get(live_configuration::list))
        .route("/admin/configuration/{id}",get(live_configuration::get_config).put(live_configuration::put_config))
        .route("/admin/publication/status",get(live_configuration::publication))
        .route("/admin/app-settings/encryption", get(app_settings::read).put(app_settings::update))
        .route("/admin/engine-components",get(engine_components::read))
        .route("/admin/engine-components/{id}",put(engine_components::update))
        .route("/admin/loop-runtime", get(loop_settings::read).put(loop_settings::update))
        .route("/admin/context-compaction", get(context_config_get).put(context_config_put))
        .nest("/admin/access", phase3_api::router())
        .nest("/admin/services", service_admin::router())
        .nest("/admin/models/rosters", model_catalog::router())
        .route("/admin/access/registration", get(registration::access_mode).put(registration::set_mode))
        .route("/workflow/policy", get(workflow_policy_get).put(workflow_policy_set))
        .route("/agents/{id}/model", get(agent_model).put(agent_model_set))
        .route("/policy", get(policy_get).put(policy_set))
        .route("/abilities", get(ability_catalog))
        .route("/provider/login", post(provider_start))
        .route("/provider/logout", post(provider_logout))
        .route("/control", put(control))
        .route("/file-manager", post(file_manager::action))
        .route("/admin/builds", get(build_dashboard::list))
        .route("/admin/builds/scheduler", post(build_dashboard::scheduler))
        .route("/admin/builds/{job_id}/result", get(build_dashboard::result))
        .route("/admin/builds/{job_id}/cancel", post(build_dashboard::cancel))
        .route("/admin/builds/leftovers/{id}/review", post(build_dashboard::review_leftover))
        .route_layer(middleware::from_fn_with_state(app.clone(), admin_auth));
    // Model handlers enforce tenant authority themselves; all retain browser auth and CSRF.
    let api = Router::new()
        .merge(diagnostics::background_router())
        .route("/modules/{id}/assets/{asset}",get(crate::ui_pages::module_asset))
        .route("/modules/{id}/status",get(crate::modules::status_api))
        .route("/modules/{id}/call",post(crate::modules::call_api))
        .route("/sessions/{sid}/administrator/artifacts/{id}",get(administrator::artifact))
        .route("/sessions/{sid}/administrator/notices",get(administrator::notices))
        .route("/sessions/{sid}/administrator/notices/{id}/review",post(administrator::review_notice))
        .nest("/models", model_selection_api::router())
        .route("/models/me/catalog",get(account_models::read).put(account_models::save))
        .route("/models/me/catalog/default",put(account_models::set_default))
        .route("/models/agents/{id}/adoptions",post(account_models::adopt))
        .route("/agents/{id}/context-composition",get(ability_admin::composition))
        .route("/agents/{id}/slots",get(ability_admin::slots))
        .route("/agents/{id}/slots/{name}",get(ability_admin::slot).put(ability_admin::slot_put).delete(ability_admin::slot_delete))
        .route("/agents/{id}/mcp",get(crate::mcp::read))
        .route("/agents/{id}/mcp/search",post(crate::mcp::search))
        .route("/agents/{id}/mcp/inspect",post(crate::mcp::inspect))
        .route("/agents/{id}/mcp/connections/{connection}",put(crate::mcp::put).delete(crate::mcp::remove))
        .route("/agents/{id}/repository-connections",get(repository::read))
        .route("/agents/{id}/repository-connections/{connection}",put(repository::put))
        .route("/agents/{id}/abilities",get(ability_admin::read))
        .route("/agents/{id}/abilities/{ability}/settings",put(ability_admin::settings_set))
        .route("/agents/{id}/abilities/effective",get(ability_admin::preview))
        .route("/agents/{id}/ability-members",get(ability_admin::members))
        .route("/agents/{id}/ability-permissions",put(ability_admin::permissions_set))
        .route("/agents/{id}/capability-levels/{level}",put(ability_admin::level_set))
        .route("/agents/{id}/members/{user}/capability-level",put(ability_admin::member_set))
        .route("/agents/{id}/abilities/{ability}/visibility",put(ability_admin::visibility_set))
        .route("/agents/{id}/abilities/{ability}/skill-visibility",put(ability_admin::skill_set))
        .route("/agents/{id}/abilities/{ability}/skill-content",get(ability_admin::skill_content))
        .route("/agents/{id}/abilities/{ability}/skills/{skill}/content",get(ability_admin::skill_named_content))
        .route("/agents/{id}/abilities/{ability}/skills/{skill}/visibility",put(ability_admin::skill_named_set))
        .route("/agents/{id}/abilities/{ability}/tools/{tool}/skill-content",get(ability_admin::tool_skill_content))
        .route("/agents/{id}/abilities/{ability}/tools/{tool}/mode",put(ability_admin::tool_set))
        .route("/agents/{id}/abilities/{ability}/tools/{tool}/execution",put(ability_admin::execution_set))
        .route("/agents/{id}/abilities/{ability}/tools/{tool}/result-deduplication",put(ability_admin::result_deduplication_set))
        .route("/user/agent-page-favorites",get(agent_pages::read).put(agent_pages::save))
        .route("/user/app-page-favorites",get(app_pages::read).put(app_pages::save))
        .route("/agents/{id}/embed",get(embed::config_get).put(embed::config_put))
        .route("/embed/challenges/{id}",get(embed::challenge_info))
        .route("/embed/authorize",post(embed::authorize))
        .route("/embed/credentials/{id}",axum::routing::delete(embed::revoke))
        .route("/core/vault/{sid}/{name}",post(tools::core_vault::save).layer(DefaultBodyLimit::max(16384)))
        .route("/core/visuals/{id}/view",get(tools::core_visualizer::view))
        .route("/core/webhooks/{id}/setup",get(tools::core_webhooks::setup))
        .route("/auth/me", get(me))
        .route("/auth/ui-config", get(chat_ui_config))
        .route("/auth/profile", put(update_profile))
        .route("/auth/session-policy", get(session_policy).put(update_session_policy))
        .route("/auth/sessions", get(list_auth_sessions))
        .route("/auth/sessions/{id}", axum::routing::delete(revoke_auth_session))
        .route("/auth/change-password", post(change_password))
        .route("/auth/logout", post(logout))
        .merge(user_api::router())
        .merge(agent_members::router())
        .merge(self::shared_chat::router())
        .merge(admin_api)
        .route_layer(middleware::from_fn_with_state(app.clone(), auth));
    let ui = std::env::var("V3_UI_ROOT").unwrap_or_else(|_| "/opt/v3/ui".into());
    let shared = std::env::var("V3_SHARED_UI_ROOT").unwrap_or_else(|_| "/opt/v3/shared-ui".into());
    Router::new().merge(event_aliases).route("/api/v3/pages/catalog",get(ui_pages::catalog))
        .route("/embed/{id}",get(embed::page))
        .route("/api/v3/embed/{id}/descriptor",get(embed::descriptor))
        .route("/api/v3/embed/{id}/challenge",post(embed::challenge))
        .route("/api/v3/embed/{id}/guest",post(embed::guest))
        .route("/api/v3/embed/redeem",post(embed::redeem))
        .route("/api/v3/core/webhook-receive/{id}",post(tools::core_webhooks::receive).layer(DefaultBodyLimit::max(16384)))
        .route("/api/v3/auth/login",post(login))
        .route("/api/v3/auth/register",post(registration::register))
        .route("/api/v3/auth/guest-login",post(registration::guest_login))
        .route("/api/v3/auth/access-mode",get(registration::access_mode))
        .route("/api/v3/diagnostics/browser-login",post(browser_login::login))
        .route("/api/v3/auth/setup-status",get(setup_status))
        .route("/api/v3/auth/setup-admin",post(setup_admin))
        .nest("/api/v3",api)
        .nest("/api/v3/admin/messaging",messaging)
        .route("/health/live",get(||async{Json(json!({"status":"alive"}))}))
        .route("/health/ready",get(|State(app):State<App>|async move{
            let healthy=app.ready.load(Ordering::Acquire)
                && sqlx::query("SELECT 1").execute(&app.store.app_pool).await.is_ok()
                && sqlx::query("SELECT 1").execute(&app.store.pool).await.is_ok()
                && sqlx::query("SELECT 1").execute(&app.store.agent_pool).await.is_ok()
                && app.store.loop_fresh(10).await.unwrap_or(false);
            (if healthy{StatusCode::OK}else{StatusCode::SERVICE_UNAVAILABLE},Json(json!({"ready":healthy,"catalog_version":app.abilities.version()})))
        }))
        .merge(webagent_webapp::pages::routes(std::path::Path::new(&ui)))
        .nest_service("/ui",ServeDir::new(shared))
        .layer(middleware::from_fn_with_state(app.clone(), ui_pages::live_ui))
        .layer(DefaultBodyLimit::max(2*1024*1024))
        .layer(middleware::from_fn(diagnostics::http_errors))
        .layer(middleware::from_fn(|request:Request,next:Next|async move{
            let public_widget_module=request.uri().path()=="/v3-ui/chat/widget/window.js";
            let mut response=next.run(request).await;
            if public_widget_module {response.headers_mut().insert("access-control-allow-origin",HeaderValue::from_static("*"));}
            response.headers_mut().insert("cache-control",HeaderValue::from_static("no-store"));
            response.headers_mut().insert("x-content-type-options",HeaderValue::from_static("nosniff"));
            response.headers_mut().insert("referrer-policy",HeaderValue::from_static("same-origin"));
            if !response.headers().contains_key("content-security-policy") {response.headers_mut().insert("content-security-policy",HeaderValue::from_static("default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; media-src 'self' data: blob:; connect-src 'self'; frame-ancestors 'none'; base-uri 'self'; form-action 'self'"));}
            response
        })).with_state(app)
}

// Native Core tools arrive after persisted tool-policy/approval checks. Domain handlers still own tenant and revision admission.
pub(crate) fn core_tools_router()->Router<App> {
    Router::new().nest("/models",model_selection_api::router())
        .route("/agents/{id}/abilities",get(ability_admin::read))
        .route("/agents/{id}/abilities/{ability}/visibility",put(ability_admin::visibility_set))
        .route("/agents/{id}/abilities/{ability}/tools/{tool}/mode",put(ability_admin::tool_set))
        .route("/agents/{id}/slots/{name}",get(ability_admin::slot).put(ability_admin::slot_put))
        .merge(user_api::router())
}
