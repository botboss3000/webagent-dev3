//! The mini app view route; the tools live in `webagent_native_tools::visualizer`.
//! Caller-owned mini apps render only in opaque sandbox frames; private vault data is never injected.
use crate::*;
use axum::{response::Html, Extension};
use webagent_native_tools::visualizer;
pub async fn view(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Response, ApiError> {
    let owner = identity["_user_id"]
        .as_str()
        .ok_or_else(|| bad("Caller required"))?;
    let store = app.store.for_user(owner).await?;
    let record = visualizer::record(&store, &id)
        .await
        .map_err(|_| ApiError(StatusCode::NOT_FOUND, "Mini app not found".into()))?;
    let body = visualizer::document(
        &record,
        query.get("theme").map(String::as_str).unwrap_or("dark"),
    )?;
    let mut response = Html(body).into_response();
    response
        .headers_mut()
        .insert("x-dns-prefetch-control", HeaderValue::from_static("off"));
    response.headers_mut().insert(
        "content-security-policy",
        HeaderValue::from_static(visualizer::CSP),
    );
    response.headers_mut().insert("permissions-policy",HeaderValue::from_static("camera=(),microphone=(),geolocation=(),display-capture=(),clipboard-read=(),clipboard-write=(),payment=(),usb=()"));
    Ok(response)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn mini_app_views_cannot_read_another_users_record() {
        let (_root, app) = crate::runtime_tests::fixture_uninitialized().await;
        let a = app
            .store
            .create_user_account("visual-first", "fixture-password-hash-placeholder")
            .await
            .unwrap();
        let b = app
            .store
            .create_user_account("visual-second", "fixture-password-hash-placeholder")
            .await
            .unwrap();
        let id = uuid();
        let store = app
            .store
            .for_user(a["user_id"].as_str().unwrap())
            .await
            .unwrap();
        store
            .put(
                &format!("core_visual:{id}"),
                &json!({"id":id,"revision":1,"artifact":{"html":"<h1>Private</h1>","data":{}}}),
            )
            .await
            .unwrap();
        assert!(view(
            State(app.clone()),
            Extension(json!({"_user_id":a["user_id"]})),
            Path(id.clone()),
            Query(HashMap::new())
        )
        .await
        .is_ok());
        assert!(view(
            State(app),
            Extension(json!({"_user_id":b["user_id"]})),
            Path(id),
            Query(HashMap::new())
        )
        .await
        .is_err());
    }
}
