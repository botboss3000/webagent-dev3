//! Administrator storage policy: Storage's live encryption switch, shared by API and loop.
use super::*;

fn snapshot(app: &App) -> Result<(Value, bool, String)> {
    let settings = crate::store::read_settings(&app.store.root)?;
    let enabled = crate::store::admin_encryption_enabled(&settings)?;
    let digest = crate::digest(&settings);
    Ok((settings, enabled, digest))
}

pub(crate) async fn read(State(app): State<App>) -> Api {
    let (_, enabled, digest) = snapshot(&app)?;
    Ok(Json(json!({"enabled":enabled,"digest":digest})))
}

fn switched(current: &Value, enabled: bool) -> Result<Value> {
    crate::store::admin_encryption_enabled(current)?;
    let mut settings = current.clone();
    if settings.get("admin_database_encryption").is_none() {
        settings["admin_database_encryption"] = json!({});
    }
    settings["admin_database_encryption"]["enabled"] = json!(enabled);
    Ok(settings)
}

pub(crate) async fn update(State(app): State<App>, Json(body): Json<Value>) -> Api {
    let enabled = body["enabled"]
        .as_bool()
        .ok_or_else(|| bad("Encryption enabled must be a boolean"))?;
    let expected = body["expected_digest"]
        .as_str()
        .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or_else(|| bad("Expected settings digest required"))?;
    let stale = || conflict("Storage settings changed; reload before saving");
    let _guard = app.gate.lock().await;
    let path = crate::store::settings_path(&app.store.root);
    if let Some(relative) = path.to_str().filter(|_| path.is_relative()) {
        // The broker replaces the live file only while it still holds the bytes read here.
        let bytes = crate::live_files::current(relative, 64 * 1024)?;
        let current = webagent_storage::settings::parse(&bytes)?;
        if crate::digest(&current) != expected {
            return Err(stale());
        }
        let body = crate::live_files::pretty(&switched(&current, enabled)?)?;
        crate::live_files::save(&app, relative, &body, &bytes)
            .await
            .map_err(|_| stale())?;
    } else {
        std::fs::create_dir_all(
            path.parent()
                .ok_or_else(|| anyhow!("Invalid storage settings path"))?,
        )
        .map_err(anyhow::Error::from)?;
        let saved = webagent_configuration::update_if_current(
            &path,
            &path.with_extension("json.publication.lock"),
            expected,
            Some(&json!({})),
            |current| switched(current, enabled),
        )?;
        if saved.is_none() {
            return Err(stale());
        }
    }
    // The next user-plane write in either process rereads this policy.
    let (_, enabled, digest) = snapshot(&app)?;
    Ok(Json(json!({"enabled":enabled,"digest":digest})))
}
