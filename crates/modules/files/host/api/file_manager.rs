//! Administrator File Manager admission. Browser identity and CSRF are checked
//! by the router; the isolated worker confines checkout paths to /code. Absolute
//! paths outside `/live-checkout` go to the root host file service (`host_files.py`),
//! which serves them only while Host Admin has granted this administrator access
//! (approved with the host approval passphrase; security spec FILES-1).
use super::*;
use axum::Extension;

/// The checkout's name on the File Manager page; every other absolute path is the host's.
const CHECKOUT: &str = "/live-checkout";
/// The page shows these refusals as a toast with the passphrase prompt and stays where it is.
#[cfg(unix)]
const NEEDS_GRANT: &str =
    "Host Admin must authorize browsing outside the checkout. The request is waiting on the Host Admin page.";
#[cfg(unix)]
const NEEDS_TERMINAL_GRANT: &str =
    "Host Admin must authorize terminal access. The request is waiting on the Host Admin page.";
/// Terminal actions; they all run in the root host file service (security spec FILES-3).
const TERMINAL: [&str; 6] = [
    "terminal_open",
    "terminal_read",
    "terminal_send",
    "terminal_resize",
    "terminal_list",
    "terminal_close",
];

pub(crate) async fn action(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Json(body): Json<Value>,
) -> Api {
    let action = body["action"].as_str().unwrap_or("");
    let git = matches!(
        action,
        "git_status"
            | "git_graph"
            | "git_commit_detail"
            | "git_branches"
            | "git_tree_status"
            | "git_line_stats"
            | "git_ignore"
            | "git_tool"
            | "git_commit"
            | "git_settings"
    );
    if !git
        && !TERMINAL.contains(&action)
        && !matches!(
            action,
            "tree" | "read" | "write" | "create" | "rename" | "delete"
        )
    {
        return Err(bad("Unsupported File Manager action"));
    }
    if TERMINAL.contains(&action) {
        let admin = identity["_user_id"].as_str().unwrap_or_default();
        return host_files(admin, action, &body).await;
    }
    if !git {
        let host = host_path(&body["path"]);
        if action == "rename" && host != host_path(&body["new_path"]) {
            return Err(bad(
                "Moving between the checkout and other folders is not supported",
            ));
        }
        if host {
            let admin = identity["_user_id"].as_str().unwrap_or_default();
            return host_files(admin, action, &body).await;
        }
    }
    // This admin route owns admission. The worker owns checkout confinement;
    // repository secrets are passed only for operations that need transport.
    let needs_transport = action == "git_settings"
        || action == "git_tool"
            && matches!(body["operation"].as_str(), Some("fetch" | "pull" | "push"));
    let trusted = if needs_transport {
        Some(json!({"repository": repository::transport_for_agent(&app,"shared_default").await?}))
    } else {
        None
    };
    let result = tools::execute_trusted(
        if git {
            "administrator_file_git"
        } else {
            "administrator_file_manager"
        },
        &body,
        "00000000-0000-0000-0000-000000000000",
        &app.tool_socket,
        trusted,
        CancellationToken::new(),
    )
    .await?;
    if result["uncertain"] == true {
        return Err(crate::conflict(
            "Git effect uncertain; inspect repository before retry",
        ));
    }
    if result["status"] != "success" {
        let detail = result["output"]
            .as_str()
            .unwrap_or("File Manager operation failed");
        return Err(bad(detail));
    }
    let mut output = result["output"].clone();
    // The checkout root offers its host parent, so the up button can leave the checkout.
    if action == "tree" && output["path"] == CHECKOUT && output["parent"].is_null() {
        if let Some(parent) = checkout_parent().await {
            output["parent"] = parent;
        }
    }
    Ok(Json(output))
}

/// The checkout's host parent folder, when the host file service runs.
#[cfg(unix)]
async fn checkout_parent() -> Option<Value> {
    let place = host_admin::call(
        "files/files.sock",
        "Host file service",
        &json!({"op": "locate"}),
    )
    .await
    .ok()?;
    (place["status"] == "success").then(|| place["output"]["parent"].clone())
}

#[cfg(not(unix))]
async fn checkout_parent() -> Option<Value> {
    None
}

/// Whether `path` names a host location rather than the checkout.
fn host_path(path: &Value) -> bool {
    path.as_str().is_some_and(|path| {
        path.starts_with('/') && path != CHECKOUT && !path.starts_with("/live-checkout/")
    })
}

/// One operation through the root host file service. Without a live grant it files the
/// grant request on Host Admin (once per administrator) and refuses with [`NEEDS_GRANT`].
#[cfg(not(unix))]
async fn host_files(_admin: &str, _action: &str, _body: &Value) -> Api {
    Err(bad(
        "Folders outside the checkout need the Linux host file service",
    ))
}

#[cfg(unix)]
async fn host_files(admin: &str, action: &str, body: &Value) -> Api {
    if admin.is_empty() {
        return Err(bad("Administrator identity required"));
    }
    let mut message = json!({"op": action, "admin": admin});
    for key in [
        "path",
        "new_path",
        "show_hidden",
        "content",
        "encoding",
        "expected_sha256",
        "kind",
        "id",
        "data",
        "offset",
        "wait",
        "cols",
        "rows",
        "cwd",
    ] {
        if !body[key].is_null() {
            message[key] = body[key].clone();
        }
    }
    // A terminal opened on a checkout folder starts in the checkout's host folder.
    if let Some(cwd) = message["cwd"].as_str() {
        if let Some(rest) = cwd.strip_prefix(CHECKOUT) {
            if rest.is_empty() || rest.starts_with('/') {
                let place = host_admin::call(
                    "files/files.sock",
                    "Host file service",
                    &json!({"op": "locate"}),
                )
                .await?;
                if let Some(checkout) = place["output"]["checkout"].as_str() {
                    message["cwd"] = json!(format!("{checkout}{rest}"));
                }
            }
        }
    }
    let reply = host_admin::call("files/files.sock", "Host file service", &message).await?;
    if reply["status"] == "success" {
        return Ok(Json(reply["output"].clone()));
    }
    if reply["code"] == "authorization_required" {
        let terminal = action.starts_with("terminal_");
        host_admin::request_grant(
            if terminal {
                "terminal_grant"
            } else {
                "files_grant"
            },
            admin,
        )
        .await?;
        return Err(bad(if terminal {
            NEEDS_TERMINAL_GRANT
        } else {
            NEEDS_GRANT
        }));
    }
    Err(bad(reply["output"]
        .as_str()
        .unwrap_or("File Manager operation failed")))
}

#[cfg(test)]
mod tests {
    use super::host_path;
    use serde_json::json;

    #[test]
    fn checkout_paths_stay_with_the_worker() {
        for path in [
            "/live-checkout",
            "/live-checkout/src/main.rs",
            "src/main.rs",
            "",
        ] {
            assert!(!host_path(&json!(path)), "{path}");
        }
        assert!(!host_path(&json!(null)));
        for path in ["/", "/home/ar", "/live-checkoutx", "/etc/hosts"] {
            assert!(host_path(&json!(path)), "{path}");
        }
    }
}
