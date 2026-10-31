//! HTTP authority stays in the root; Diagnostics owns the bounded UI trace semantics.
use super::*;
use axum::{extract::DefaultBodyLimit, routing::post};
use package::ui_trace::Trace;
async fn configuration(Extension(app): Extension<crate::App>) -> crate::Api {
    Ok(Json(package::ui_trace::configuration(
        RECORDER.get(),
        &app.store,
    )))
}
async fn report(
    Extension(app): Extension<crate::App>,
    Extension(identity): Extension<Value>,
    Json(body): Json<Trace>,
) -> crate::Api {
    authorize(&app, &identity).await?;
    Ok(Json(
        package::ui_trace::report(RECORDER.get(), &app.store, body)
            .await
            .map_err(api_error)?,
    ))
}
pub(super) fn router() -> Router<crate::App> {
    Router::new()
        .route("/diagnostics/ui-trace/config", get(configuration))
        .route("/diagnostics/ui-trace/events", post(report))
        .layer(DefaultBodyLimit::max(96 * 1024))
        .layer(axum::middleware::map_response(no_store))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn trace_reports_retain_auth_and_csrf_and_reject_unbounded_bodies() {
        use axum::{
            body::Body,
            http::{Request, StatusCode},
        };
        use tower::ServiceExt;
        let (_root, app) = crate::runtime_tests::fixture().await;
        let token = uuid::Uuid::new_v4().to_string();
        app.store.put(&format!("auth:{}",crate::digest(&json!(token))),
            &json!({"csrf":"fixture","expires":crate::now()+3600,"generation":"fixture-generation"})).await.unwrap();
        let cookie = format!("v3_session={token}");
        let send = |csrf: Option<&str>, body: Value| {
            let mut r = Request::builder()
                .method("POST")
                .uri("/api/v3/diagnostics/ui-trace/events")
                .header("content-type", "application/json")
                .header("cookie", &cookie);
            if let Some(c) = csrf {
                r = r.header("x-csrf-token", c);
            }
            crate::api::router(app.clone()).oneshot(r.body(Body::from(body.to_string())).unwrap())
        };
        let body = json!({"trace_id":uuid::Uuid::new_v4().to_string(),"trigger":"composer_focus_lost","width":390,"height":844,
            "entries":[{"before_ms":0,"kind":"focusout","target":"textarea#chat-input"}]});
        assert_eq!(
            send(None, body.clone()).await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            send(Some("fixture"), body.clone()).await.unwrap().status(),
            StatusCode::OK
        );
        let mut invalid = body;
        invalid["entries"][0]["kind"] = json!("typed_text");
        assert_eq!(
            send(Some("fixture"), invalid).await.unwrap().status(),
            StatusCode::BAD_REQUEST
        );
    }
}
