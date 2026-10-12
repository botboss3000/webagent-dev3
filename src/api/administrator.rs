//! Private artifacts are read through authenticated session ownership only.
use super::*;
pub(crate) async fn artifact(axum::Extension(app):axum::Extension<App>,Path((sid,id)):Path<(String,String)>)->std::result::Result<Response,ApiError> {
    uuid::Uuid::parse_str(&id).map_err(|_|ApiError(StatusCode::BAD_REQUEST,"Invalid artifact identifier".into()))?;
    let (before,trusted)=artifact_authority(&app,&sid).await?;
    let output=tools::execute_trusted("administrator_artifact_get",&json!({"artifact_id":id}),&sid,
        &app.tool_socket,Some(trusted),CancellationToken::new()).await?;
    // Worker I/O is not an authorization lease. A concurrent restriction or
    // credential replacement withdraws the bytes before publication.
    let (after,_)=artifact_authority(&app,&sid).await?;
    if before!=after {return Err(artifact_denied());}
    let encoded=output["png_base64"].as_str().filter(|v|v.len()<=1400000)
        .ok_or_else(||ApiError(StatusCode::NOT_FOUND,"Artifact unavailable".into()))?;
    use base64::Engine;
    let data=base64::engine::general_purpose::STANDARD.decode(encoded)
        .map_err(|_|ApiError(StatusCode::INTERNAL_SERVER_ERROR,"Invalid artifact".into()))?;
    if data.len()>1024*1024 || !data.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err(ApiError(StatusCode::INTERNAL_SERVER_ERROR,"Invalid artifact".into()));
    }
    let mut response=( [("content-type","image/png"),("cache-control","no-store")],data).into_response();
    response.headers_mut().insert("content-disposition",HeaderValue::from_static("inline; filename=administrator.png"));
    Ok(response)
}

fn artifact_denied()->ApiError {
    ApiError(StatusCode::FORBIDDEN,"Artifact access unavailable under current policy".into())
}
async fn artifact_authority(app:&App,sid:&str)->std::result::Result<(String,Value),ApiError> {
    let session=app.store.get(&key("session",sid)).await?
        .ok_or_else(||ApiError(StatusCode::NOT_FOUND,"Session not found".into()))?;
    if session["status"] != "active" { return Err(ApiError(StatusCode::NOT_FOUND,"Session not found".into())); }
    let agent=session["agent_id"].as_str().unwrap_or("shared_default");
    let mut scoped=app.clone();scoped.store=app.store.for_agent(agent).await?;
    let (policy,revision,visibility,state)=ability_controls(&scoped,sid).await?;
    let mode=session_mode(&scoped,sid).await?;
    let access=scoped.store.live_access(scoped.store.current_user_id(),agent).await?;
    if !access.admitted() || live_tool_decision(&scoped,&access,"dev_browser_action",
        &policy,&visibility,&state,&json!(["diagnostics"]),mode["mode"]=="read_only").permission==access::Permission::Deny {
        return Err(artifact_denied());
    }
    let trusted=app.store.get(&format!("administrator_browser:{sid}")).await?
        .ok_or_else(||ApiError(StatusCode::NOT_FOUND,"Artifact not found".into()))?;
    let account=app.store.account_by_id(app.store.current_user_id()).await?
        .ok_or_else(artifact_denied)?;
    if trusted["session_id"]!=sid || trusted["user_id"]!=app.store.current_user_id()
        || trusted["agent_id"]!=agent || trusted["generation"]!=account["generation"]
        || trusted["expires_at"].as_u64().unwrap_or(0)<=now() {
        return Err(artifact_denied());
    }
    let fingerprint=digest(&json!([app.store.current_user_id(),sid,agent,policy,revision,
        visibility,state,mode,access.context_fingerprint(),trusted,app.abilities.live_version()]));
    Ok((fingerprint,trusted))
}


pub(crate) async fn notices(
    axum::Extension(app): axum::Extension<App>,
    Path(sid): Path<String>,
) -> Api {
    let (repo_allowed, build_allowed, before) = notice_authority(&app, &sid).await?;
    let repository = if repo_allowed {
        app.store
            .get(&format!("administrator_notice:{sid}"))
            .await?
    } else {
        None
    };
    let builds = if build_allowed {
        Some(build_notice_broker(&app, &sid, "v3_build_leftovers", json!({})).await)
    } else {
        None
    };
    let (repo_current, build_current, after) = notice_authority(&app, &sid).await?;
    let unchanged = before == after;
    let mut result = json!({
        "notice": if unchanged && repo_current { repository } else { None },
        "build_notices": [], "build_notice_total": 0,
        "build_notices_unavailable": false
    });
    if unchanged && build_current {
        match builds {
            Some(Ok(value)) => {
                result["build_notices"] = value["notices"].clone();
                result["build_notice_total"] = value["total"].clone();
            }
            Some(Err(_)) => result["build_notices_unavailable"] = json!(true),
            None => {}
        }
    }
    Ok(Json(result))
}

async fn notice_authority(
    app: &App,
    sid: &str,
) -> std::result::Result<(bool, bool, String), ApiError> {
    let session = app
        .store
        .get(&key("session", sid))
        .await?
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "Session not found".into()))?;
    let agent = session["agent_id"].as_str().unwrap_or("shared_default");
    let mut scoped = app.clone();
    scoped.store = app.store.for_agent(agent).await?;
    let (policy, revision, visibility, state) = ability_controls(&scoped, sid).await?;
    let mode = session_mode(&scoped, sid).await?;
    let access = scoped
        .store
        .live_access(scoped.store.current_user_id(), agent)
        .await?;
    if !access.admitted() {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "Agent access unavailable".into(),
        ));
    }
    let repository = visibility["settings"]["git_control"]["notify_repo_changes"] == true
        && live_tool_decision(
            &scoped,
            &access,
            "git_tool",
            &policy,
            &visibility,
            &state,
            &json!(["git_control"]),
            mode["mode"] == "read_only",
        )
        .permission
            == access::Permission::Auto;
    // Viewing an owned reminder is metadata access, never permission to run a
    // build tool. Off/revoked ability access still withdraws the reminder.
    let builds = live_tool_decision(
        &scoped,
        &access,
        "build_work",
        &policy,
        &visibility,
        &state,
        &json!(["codebase_admin"]),
        mode["mode"] == "read_only",
    )
    .permission
        != access::Permission::Deny;
    let fingerprint = digest(&json!([
        app.store.current_user_id(),
        sid,
        agent,
        policy,
        revision,
        visibility,
        state,
        mode,
        access.context_fingerprint()
    ]));
    Ok((repository, builds, fingerprint))
}

async fn build_notice_broker(
    app: &App,
    sid: &str,
    name: &str,
    arguments: Value,
) -> std::result::Result<Value, ApiError> {
    let trusted = json!({
        "user_id": app.store.current_user_id(), "session_id": sid,
        "lease_until": now() + 30,
        "access_fingerprint": digest(&json!(["build_leftover_notice", app.store.current_user_id(), sid]))
    });
    let response = tokio::time::timeout(
        Duration::from_secs(3),
        tools::execute_trusted(
            name,
            &arguments,
            sid,
            &crate::tools::administrator::build_socket(&app),
            Some(trusted),
            CancellationToken::new(),
        ),
    )
    .await
    .map_err(|_| {
        ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "Build notices unavailable".into(),
        )
    })??;
    if response["status"] != "success" {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "Build notice changed or unavailable; refresh".into(),
        ));
    }
    let output = response["output"].clone();
    if name == "v3_build_leftovers" {
        validate_build_notices(&output)?;
    } else if output["status"] != "reviewed" || output["notice_id"] != arguments["notice_id"] {
        return Err(ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "Invalid build notice receipt".into(),
        ));
    }
    Ok(output)
}

fn validate_build_notices(output: &Value) -> std::result::Result<(), ApiError> {
    let invalid = || {
        ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "Invalid build notice response".into(),
        )
    };
    let rows = output["notices"]
        .as_array()
        .filter(|v| v.len() <= 20)
        .ok_or_else(invalid)?;
    output["total"]
        .as_u64()
        .filter(|v| *v >= rows.len() as u64 && *v <= 1000)
        .ok_or_else(invalid)?;
    for row in rows {
        if !row["id"].as_str().is_some_and(valid_notice_id)
            || !row["task_id"].as_str().is_some_and(|v| {
                (8..=80).contains(&v.len())
                    && v.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            })
            || !matches!(
                row["status"].as_str(),
                Some("stale_open" | "failed" | "interrupted" | "uncertain" | "stale")
            )
            || !row["notification"]
                .as_str()
                .is_some_and(|v| !v.is_empty() && v.len() <= 512)
            || row["observed_at"].as_u64().is_none()
            || row["since"].as_u64().is_none()
            || (!row["retained_bytes"].is_null() && row["retained_bytes"].as_u64().is_none())
        {
            return Err(invalid());
        }
    }
    Ok(())
}

fn valid_notice_id(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub(crate) async fn review_notice(
    axum::Extension(app): axum::Extension<App>,
    Path((sid, id)): Path<(String, String)>,
) -> Api {
    if !valid_notice_id(&id) {
        return Err(bad("Invalid build notice identifier"));
    }
    let (_, allowed, before) = notice_authority(&app, &sid).await?;
    if !allowed {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "Build notice access unavailable".into(),
        ));
    }
    let value = build_notice_broker(
        &app,
        &sid,
        "v3_build_leftover_review",
        json!({"notice_id":id}),
    )
    .await?;
    let (_, current, after) = notice_authority(&app, &sid).await?;
    if !current || before != after {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "Build notice access changed; refresh".into(),
        ));
    }
    Ok(Json(value))
}

#[cfg(test)]
mod notice_tests {
    use super::*;
    use axum::body::Body;
    use tower::ServiceExt;
    #[test]
    fn projection_rejects_bad_revision_and_negative_space() {
        let row = json!({"id":"a".repeat(64),"task_id":"retained-task","status":"uncertain",
            "notification":"Investigate retained work.","since":1,"observed_at":2,"retained_bytes":null});
        let mut output = json!({"notices":[row],"total":1});
        assert!(validate_build_notices(&output).is_ok());
        output["notices"][0]["id"] = json!("../foreign");
        assert!(validate_build_notices(&output).is_err());
        output["notices"][0]["id"] = json!("a".repeat(64));
        output["notices"][0]["retained_bytes"] = json!(-1);
        assert!(validate_build_notices(&output).is_err());
        output["notices"][0]["retained_bytes"] = json!(0);
        output["total"] = json!(0);
        assert!(validate_build_notices(&output).is_err());
    }
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
