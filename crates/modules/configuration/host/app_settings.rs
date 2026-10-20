//! Administrator storage policy. The data-root file is shared by API and loop.
use super::*;

fn snapshot(app: &App) -> Result<(Value, bool, String)> {
    let settings = crate::store::read_app_settings(&app.store.root)?;
    let enabled = crate::store::admin_encryption_enabled(&settings)?;
    let digest = crate::digest(&settings);
    Ok((settings, enabled, digest))
}

pub(crate) async fn read(State(app): State<App>) -> Api {
    let (_, enabled, digest) = snapshot(&app)?;
    Ok(Json(json!({"enabled":enabled,"digest":digest})))
}

pub(crate) async fn update(State(app): State<App>, Json(body): Json<Value>) -> Api {
    let enabled = body["enabled"].as_bool()
        .ok_or_else(|| bad("Encryption enabled must be a boolean"))?;
    let expected = body["expected_digest"].as_str()
        .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or_else(|| bad("Expected settings digest required"))?;
    let _guard = app.gate.lock().await;
    let path = app.store.root.join("config/app-settings.json");
    std::fs::create_dir_all(path.parent().ok_or_else(||anyhow!("Invalid app settings path"))?).map_err(anyhow::Error::from)?;
    let saved=webagent_configuration::update_if_current(
        &path,&path.with_extension("json.publication.lock"),expected,Some(&json!({})),|current| {
            crate::store::admin_encryption_enabled(current)?;
            let mut settings=current.clone();
            if settings.get("admin_database_encryption").is_none() {
                settings["admin_database_encryption"] = json!({});
            }
            settings["admin_database_encryption"]["enabled"] = json!(enabled);
            Ok(settings)
        })?;
    if saved.is_none() {return Err(conflict("App settings changed; reload before saving"));}
    // The next user-plane write in either process rereads this policy.
    let (_, enabled, digest) = snapshot(&app)?;
    Ok(Json(json!({"enabled":enabled,"digest":digest})))
}
