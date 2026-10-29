//! A session's administrator notices: the repository-change notice the watcher records
//! and the build broker's retained-work reminders. Each is shown only while the caller's
//! access to the matching tool holds, checked before and after the slow read.
use crate::{trusted, AdministrationHost, SessionAccess};
use axum::http::StatusCode;
use serde_json::{json, Value};
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use webagent_access::Permission;
use webagent_administration::notices::{reviewed, valid_notice_id, valid_page};
use webagent_host_core::{bad, digest, ApiError};

/// The tools whose access decides which notices a caller sees.
const TOOLS: [(&str, &str); 2] = [
    ("git_tool", "git_control"),
    ("build_work", "codebase_admin"),
];

/// What a caller may see now, and the fingerprint to compare after I/O.
struct Visible {
    repository: bool,
    builds: bool,
    fingerprint: String,
}

async fn visible(host: &dyn AdministrationHost, sid: &str) -> Result<Visible, ApiError> {
    let access: SessionAccess = host
        .access(sid, &TOOLS)
        .await?
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "Session not found".into()))?;
    if !access.admitted {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "Agent access unavailable".into(),
        ));
    }
    let repository = access.settings["git_control"]["notify_repo_changes"] == true
        && access.permissions.first() == Some(&Permission::Auto);
    // Viewing an owned reminder is metadata access, never permission to run a build
    // tool. Off or revoked ability access still withdraws the reminder.
    let builds = access
        .permissions
        .get(1)
        .is_some_and(|p| *p != Permission::Deny);
    Ok(Visible {
        repository,
        builds,
        fingerprint: access.fingerprint,
    })
}

/// One retained-work request to the broker, bounded to 3 seconds.
async fn build_notice_broker(
    host: &dyn AdministrationHost,
    sid: &str,
    name: &str,
    arguments: Value,
) -> Result<Value, ApiError> {
    let user = host.store().current_user_id();
    let fingerprint = json!(digest(&json!(["build_leftover_notice", user, sid])));
    let request = host.broker(
        name,
        &arguments,
        sid,
        Some(trusted(user, sid, &fingerprint)),
        CancellationToken::new(),
    );
    let response = tokio::time::timeout(Duration::from_secs(3), request)
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
    let valid = if name == "v3_build_leftovers" {
        valid_page(&output)
    } else {
        reviewed(&output, arguments["notice_id"].as_str().unwrap_or(""))
    };
    if !valid {
        let message = if name == "v3_build_leftovers" {
            "Invalid build notice response"
        } else {
            "Invalid build notice receipt"
        };
        return Err(ApiError(StatusCode::SERVICE_UNAVAILABLE, message.into()));
    }
    Ok(output)
}

/// The session's current notices. A change of access during the reads withdraws them.
pub async fn read(host: &dyn AdministrationHost, sid: &str) -> Result<Value, ApiError> {
    let before = visible(host, sid).await?;
    let repository = if before.repository {
        host.store()
            .get(&format!("administrator_notice:{sid}"))
            .await?
    } else {
        None
    };
    let builds = if before.builds {
        Some(build_notice_broker(host, sid, "v3_build_leftovers", json!({})).await)
    } else {
        None
    };
    let after = visible(host, sid).await?;
    let unchanged = before.fingerprint == after.fingerprint;
    let mut result = json!({
        "notice": if unchanged && after.repository { repository } else { None },
        "build_notices": [], "build_notice_total": 0,
        "build_notices_unavailable": false
    });
    if unchanged && after.builds {
        match builds {
            Some(Ok(value)) => {
                result["build_notices"] = value["notices"].clone();
                result["build_notice_total"] = value["total"].clone();
            }
            Some(Err(_)) => result["build_notices_unavailable"] = json!(true),
            None => {}
        }
    }
    Ok(result)
}

/// Mark one retained-work notice reviewed. This acknowledges it; it deletes nothing and
/// resolves no uncertain effect.
pub async fn review(
    host: &dyn AdministrationHost,
    sid: &str,
    notice_id: &str,
) -> Result<Value, ApiError> {
    if !valid_notice_id(notice_id) {
        return Err(bad("Invalid build notice identifier"));
    }
    let before = visible(host, sid).await?;
    if !before.builds {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "Build notice access unavailable".into(),
        ));
    }
    let value = build_notice_broker(
        host,
        sid,
        "v3_build_leftover_review",
        json!({"notice_id":notice_id}),
    )
    .await?;
    let after = visible(host, sid).await?;
    if !after.builds || before.fingerprint != after.fingerprint {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "Build notice access changed; refresh".into(),
        ));
    }
    Ok(value)
}
