//! Existing presentation JSON is read on demand; shared-file siblings remain untouched.
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub fn pull_refresh_start_zone(settings: &Value) -> Result<f64> {
    let mut section = settings;
    for key in ["ui", "pull_to_refresh"] {
        let Some(value) = section.get(key) else {
            return Ok(50.0);
        };
        if !value.is_object() {
            return Err(anyhow!("Invalid pull-to-refresh settings"));
        }
        section = value;
    }
    match section.get("start_zone_px") {
        None => Ok(50.0),
        Some(value) => value
            .as_f64()
            .filter(|n| n.is_finite() && *n >= 0.0)
            .ok_or_else(|| anyhow!("Pull-to-refresh start_zone_px must be a nonnegative number")),
    }
}

pub fn validate_app_settings(settings: &Value) -> Result<()> {
    pull_refresh_start_zone(settings)?;
    if let Some(dev) = settings.get("development") {
        if !dev.is_object() || dev.get("live_ui_editing").is_some_and(|v| !v.is_boolean()) {
            return Err(anyhow!("Invalid development settings"));
        }
    }
    Ok(())
}
/// Match Storage's existing missing-file fallback without taking over encryption policy.
fn app_settings(root: &Path) -> Result<Value> {
    match std::fs::read(root.join("config/app-settings.json")) {
        Ok(bytes) => {
            let value: Value = serde_json::from_slice(&bytes)?;
            if !value.is_object() {
                return Err(anyhow!("app-settings.json must be an object"));
            }
            Ok(value)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(e) => Err(e.into()),
    }
}
pub fn live_root(state: &Path, ui: &Path) -> Option<PathBuf> {
    (app_settings(state).is_ok_and(|s| s["development"]["live_ui_editing"].as_bool() == Some(true))
        && ui.is_dir()
        && !ui.is_symlink())
    .then(|| ui.to_path_buf())
}
#[derive(Debug)]
pub enum Error {
    Http(axum::http::StatusCode, &'static str),
    Settings(anyhow::Error),
}
/// Preserve the UI API's bounded status and public error messages.
pub async fn ui_config(chat_path: &Path, state: &Path) -> Result<Value, Error> {
    use axum::http::StatusCode;
    let bytes = tokio::fs::read(chat_path).await.map_err(|_| {
        Error::Http(
            StatusCode::SERVICE_UNAVAILABLE,
            "Chat UI configuration unavailable",
        )
    })?;
    let chat_ui: Value = serde_json::from_slice(&bytes).map_err(|_| {
        Error::Http(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Chat UI configuration invalid",
        )
    })?;
    if !chat_ui["chat_common"].is_object() {
        return Err(Error::Http(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Chat UI configuration invalid",
        ));
    }
    let settings = app_settings(state).map_err(Error::Settings)?;
    let start_zone_px = pull_refresh_start_zone(&settings).map_err(Error::Settings)?;
    Ok(json!({"chat_ui":chat_ui,"app_ui":{"pull_to_refresh":{"start_zone_px":start_zone_px}}}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pull_refresh_defaults_and_validation_belong_to_webapp() {
        assert_eq!(pull_refresh_start_zone(&json!({})).unwrap(), 50.0);
        for settings in [
            json!({"ui":false}),
            json!({"ui":{"pull_to_refresh":[]}}),
            json!({"ui":{"pull_to_refresh":{"start_zone_px":-1}}}),
            json!({"ui":{"pull_to_refresh":{"start_zone_px":"200"}}}),
            json!({"ui":{"pull_to_refresh":{"start_zone_px":null}}}),
        ] {
            assert!(pull_refresh_start_zone(&settings).is_err());
            assert!(validate_app_settings(&settings).is_err());
        }
    }
    #[tokio::test]
    async fn configuration_reloads_rejects_invalid_and_never_projects_private_siblings() {
        let root = tempfile::tempdir().unwrap();
        let chat = root.path().join("chat.json");
        let config = root.path().join("config");
        std::fs::create_dir(&config).unwrap();
        let settings = config.join("app-settings.json");
        std::fs::write(
            &chat,
            r#"{"chat_common":{"execution_mode":{"default_mode":"ask"}},"chat_mobile":{}}"#,
        )
        .unwrap();
        for zone in [50.0, 0.0, 200.0, 12.5] {
            let value = json!({"ui":{"pull_to_refresh":{"start_zone_px":zone}},"admin_database_encryption":{"enabled":true},"private_marker":"private"});
            std::fs::write(&settings, value.to_string()).unwrap();
            let result = ui_config(&chat, root.path()).await.unwrap();
            assert_eq!(
                result["app_ui"],
                json!({"pull_to_refresh":{"start_zone_px":zone}})
            );
            assert_eq!(
                result["chat_ui"]["chat_common"]["execution_mode"]["default_mode"],
                "ask"
            );
            assert!(!result.to_string().contains("private"));
            assert_eq!(
                std::fs::read_to_string(&settings).unwrap(),
                value.to_string()
            );
        }
        for value in [
            json!({"ui":false}),
            json!({"ui":{"pull_to_refresh":{"start_zone_px":-1}}}),
            json!({"development":{"live_ui_editing":"yes"}}),
        ] {
            assert!(validate_app_settings(&value).is_err());
        }
        std::fs::write(&settings, "{").unwrap();
        assert!(ui_config(&chat, root.path()).await.is_err());
        std::fs::write(&settings, "{}").unwrap();
        assert_eq!(
            ui_config(&chat, root.path()).await.unwrap()["app_ui"]["pull_to_refresh"]
                ["start_zone_px"],
            50.0
        );
        std::fs::write(&chat, "{}").unwrap();
        assert!(matches!(
            ui_config(&chat, root.path()).await.unwrap_err(),
            Error::Http(axum::http::StatusCode::INTERNAL_SERVER_ERROR, _)
        ));
        std::fs::remove_file(&chat).unwrap();
        assert!(matches!(
            ui_config(&chat, root.path()).await.unwrap_err(),
            Error::Http(axum::http::StatusCode::SERVICE_UNAVAILABLE, _)
        ));
    }
}
