//! Administrator config edits preserve unrelated settings and fence stale saves.
use super::{bad, conflict, json, loop_runtime, required, Api, App, Json, Result, State, Value};
fn snapshot(app: &App) -> Result<Value> {
    Ok(serde_json::to_value(loop_runtime::Config::read(
        &app.runtime_path,
    )?)?)
}
pub(crate) async fn read(State(app): State<App>) -> Api {
    let config = snapshot(&app)?;
    Ok(Json(
        json!({"digest":crate::digest(&config),"config":config}),
    ))
}
pub(crate) async fn update(State(app): State<App>, Json(body): Json<Value>) -> Api {
    let expected = required(&body, "expected_digest", 64)?;
    let fields = body["settings"]
        .as_object()
        .ok_or_else(|| bad("Settings required"))?;
    if fields.is_empty()
        || fields.keys().any(|k| {
            !matches!(
                k.as_str(),
                "max_active_runs"
                    | "max_unresolved_runs"
                    | "poll_interval_ms"
                    | "recovery_interval_ms"
                    | "background_rescan_interval_ms"
                    | "max_parallel_calls"
            )
        })
    {
        return Err(bad("Unsupported runtime setting"));
    }
    let _guard = app.gate.lock().await;
    let mut value = snapshot(&app)?;
    if crate::digest(&value) != expected {
        return Err(conflict(
            "Runtime configuration changed; reload before saving",
        ));
    }
    for (key, input) in fields {
        let input = if key == "max_active_runs" && (input.is_null() || input == "") {
            json!(0)
        } else {
            input.clone()
        };
        if input.as_u64().is_none() {
            return Err(bad("Runtime settings must be whole numbers"));
        }
        let group = if key == "max_parallel_calls" {
            "tools"
        } else {
            "scheduler"
        };
        value[group][key] = input;
    }
    let config: loop_runtime::Config =
        serde_json::from_value(value).map_err(|_| bad("Invalid runtime settings"))?;
    config
        .validate()
        .map_err(|_| bad("Invalid runtime settings or limits"))?;
    crate::loop_runtime::save(&app, &config)
        .await
        .map_err(|_| conflict("Runtime configuration changed; reload before saving"))?;
    if fields.contains_key("background_rescan_interval_ms") {
        crate::wake::notify(&app).await;
    }
    let config = snapshot(&app)?;
    Ok(Json(
        json!({"digest":crate::digest(&config),"config":config,"restart_required":false,"adoption":"next_scheduler_tick"}),
    ))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn fenced_updates_preserve_other_settings_and_validate_unlimited() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let before = read(State(app.clone())).await.unwrap().0;
        let saved = update(
            State(app.clone()),
            Json(json!({"expected_digest":before["digest"],"settings":{"max_active_runs":5}})),
        )
        .await
        .unwrap()
        .0;
        let mut expected = before["config"].clone();
        expected["scheduler"]["max_active_runs"] = json!(5);
        assert_eq!(saved["config"], expected);
        assert!(update(
            State(app.clone()),
            Json(json!({"expected_digest":before["digest"],"settings":{"max_active_runs":0}}))
        )
        .await
        .is_err());
        for input in [json!(""), Value::Null, json!(0)] {
            let current = read(State(app.clone())).await.unwrap().0;
            let result=update(State(app.clone()),Json(json!({"expected_digest":current["digest"],"settings":{"max_active_runs":input}}))).await.unwrap().0;
            assert_eq!(result["config"]["scheduler"]["max_active_runs"], 0);
        }
        for settings in [
            json!({"max_active_runs":17}),
            json!({"max_active_runs":-1}),
            json!({"max_active_runs":1.5}),
            json!({"max_unresolved_runs":0}),
            json!({"other":1}),
        ] {
            let current = read(State(app.clone())).await.unwrap().0;
            assert!(update(
                State(app.clone()),
                Json(json!({"expected_digest":current["digest"],"settings":settings}))
            )
            .await
            .is_err());
            assert_eq!(read(State(app.clone())).await.unwrap().0, current);
        }
    }
}

#[cfg(test)]
mod background_rescan_tests {
    use super::*;
    /// Fenced interval edits preserve unrelated settings, reject invalid values and notify live dispatch.
    #[tokio::test]
    async fn configurable_interval_is_fenced_and_live() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        for interval in [1u64, 250, 5000, 15000, u64::MAX] {
            let before = read(State(app.clone())).await.unwrap().0;
            let hints = crate::wake::subscribe();
            let after = update(
                State(app.clone()),
                Json(json!({"expected_digest":before["digest"],
    "settings":{"background_rescan_interval_ms":interval}})),
            )
            .await
            .unwrap()
            .0;
            let mut expected = before["config"].clone();
            expected["scheduler"]["background_rescan_interval_ms"] = json!(interval);
            assert_eq!(after["config"], expected);
            assert!(hints.has_changed().unwrap());
            let stale = update(
                State(app.clone()),
                Json(json!({"expected_digest":before["digest"],
    "settings":{"background_rescan_interval_ms":interval.saturating_add(1)}})),
            )
            .await;
            assert!(stale.is_err());
            assert_eq!(
                read(State(app.clone())).await.unwrap().0["config"],
                after["config"]
            );
        }
        for value in [json!(0), json!(-1), json!(1.5), json!("250"), Value::Null] {
            let before = read(State(app.clone())).await.unwrap().0;
            assert!(update(
                State(app.clone()),
                Json(json!({"expected_digest":before["digest"],
    "settings":{"background_rescan_interval_ms":value}}))
            )
            .await
            .is_err());
            assert_eq!(read(State(app.clone())).await.unwrap().0, before);
        }
    }
}
