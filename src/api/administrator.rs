//! Session administrator routes: private artifacts and notices. The Administration
//! module (`webagent-administration-host`) owns the behavior; this adds HTTP framing.
use super::*;
use crate::tools::administrator::Host;

pub(crate) async fn artifact(
    axum::Extension(app): axum::Extension<App>,
    Path((sid, id)): Path<(String, String)>,
) -> std::result::Result<Response, ApiError> {
    let data = webagent_administration_host::artifacts::read(&Host(&app), &sid, &id).await?;
    let mut response = (
        [("content-type", "image/png"), ("cache-control", "no-store")],
        data,
    )
        .into_response();
    response.headers_mut().insert(
        "content-disposition",
        HeaderValue::from_static("inline; filename=administrator.png"),
    );
    Ok(response)
}

pub(crate) async fn notices(
    axum::Extension(app): axum::Extension<App>,
    Path(sid): Path<String>,
) -> Api {
    Ok(Json(
        webagent_administration_host::notices::read(&Host(&app), &sid).await?,
    ))
}

pub(crate) async fn review_notice(
    axum::Extension(app): axum::Extension<App>,
    Path((sid, id)): Path<(String, String)>,
) -> Api {
    Ok(Json(
        webagent_administration_host::notices::review(&Host(&app), &sid, &id).await?,
    ))
}

#[cfg(test)]
mod notice_tests {
    use super::*;
    use axum::body::Body;
    use tower::ServiceExt;
    #[tokio::test]
    async fn routes_require_auth_csrf_and_owned_session() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let token = uuid();
        app.store
            .put(
                &format!("auth:{}", digest(&json!(token))),
                &json!({"csrf":"expected","expires":now()+3600,"generation":"fixture-generation"}),
            )
            .await
            .unwrap();
        let mut account = app.store.account_by_id("admin").await.unwrap().unwrap();
        account["user_id"] = json!("other");
        account["username"] = json!("other");
        sqlx::query("INSERT INTO user_accounts(user_id,username,value) VALUES('other','other',?)")
            .bind(app.store.seal(&account).unwrap())
            .execute(&app.store.app_pool)
            .await
            .unwrap();
        let foreign = app.store.for_user("other").await.unwrap();
        let sid = uuid();
        foreign
            .put(&key("session", &sid), &json!({"agent_id":"shared_default"}))
            .await
            .unwrap();
        let path = format!("/api/v3/sessions/{sid}/administrator/notices");
        let request = |method: &str, cookie: bool, csrf: bool| {
            let uri = if method == "POST" {
                format!("{path}/{}/review", "a".repeat(64))
            } else {
                path.clone()
            };
            let mut builder = Request::builder().method(method).uri(uri);
            if cookie {
                builder = builder.header("cookie", format!("v3_session={token}"));
            }
            if csrf {
                builder = builder.header("x-csrf-token", "expected");
            }
            builder.body(Body::empty()).unwrap()
        };
        for (method, cookie, csrf, expected) in [
            ("GET", false, false, StatusCode::UNAUTHORIZED),
            ("POST", true, false, StatusCode::FORBIDDEN),
            ("GET", true, false, StatusCode::NOT_FOUND),
            ("POST", true, true, StatusCode::NOT_FOUND),
        ] {
            let response = crate::api::router(app.clone())
                .oneshot(request(method, cookie, csrf))
                .await
                .unwrap();
            assert_eq!(response.status(), expected);
        }
    }
}
