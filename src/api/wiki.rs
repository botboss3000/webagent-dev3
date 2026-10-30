//! The server Wiki over HTTP: the in-app Wiki page and chat reader (`/api/v3/wiki`) and
//! the public, crawlable pages (`/wiki`, `/wiki/<slug>`). Both call the same operations
//! as the Wiki tools, so visibility, revisions and the administrator-only write rule
//! cannot drift apart. Signed-in administrators see drafts; everyone else, including
//! embedded-chat visitors and the public pages, sees published articles only.
use super::*;
use axum::{extract::Path, extract::Query, response::Html, Extension};
use webagent_native_tools::{wiki, wiki_markdown};

fn failed(error: anyhow::Error) -> ApiError {
    let message = error.to_string();
    let status = if message.contains("not found") {
        StatusCode::NOT_FOUND
    } else if message.contains("changed") || message.contains("exists") {
        StatusCode::CONFLICT
    } else {
        StatusCode::BAD_REQUEST
    };
    ApiError(status, message)
}

/// Runs one Wiki operation for this caller; `pending_id` makes a retried write safe.
async fn run(
    app: &App,
    user: &str,
    administrator: bool,
    name: &str,
    args: &Value,
    pending_id: &str,
) -> std::result::Result<Value, ApiError> {
    let viewer = wiki::Viewer {
        administrator,
        staff: administrator,
        actor: user,
    };
    let result = wiki::operate(&app.store, &viewer, name, args, pending_id)
        .await
        .map_err(failed)?;
    if result["status"] == "denied" {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            result["output"].as_str().unwrap_or("Denied").to_owned(),
        ));
    }
    Ok(result["output"].clone())
}

async fn caller(app: &App, identity: &Value) -> std::result::Result<(String, bool), ApiError> {
    let user = identity["_user_id"]
        .as_str()
        .ok_or_else(|| ApiError(StatusCode::FORBIDDEN, "Sign in to use the Wiki".into()))?;
    let account = app.store.account_by_id(user).await?;
    let administrator = account.as_ref().is_some_and(|account| {
        crate::access::Principal::from_account(account)
            .is_ok_and(|p| p.active() && p.administrator())
    });
    Ok((user.to_owned(), administrator))
}

/// Maps a `[[target]]` (lowercased title or slug) to a slug the caller can read.
async fn resolver(
    app: &App,
    user: &str,
    administrator: bool,
) -> std::result::Result<impl Fn(&str) -> Option<String>, ApiError> {
    let listed = run(app, user, administrator, "wiki_list", &json!({}), "").await?;
    let index: Vec<(String, String)> = listed["articles"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| {
            let slug = a["slug"].as_str()?.to_owned();
            Some((a["title"].as_str()?.to_lowercase(), slug))
        })
        .collect();
    Ok(move |target: &str| {
        index
            .iter()
            .find(|(title, slug)| title == target || slug == target)
            .map(|(_, slug)| slug.clone())
    })
}

async fn handle(app: App, identity: Value, name: &str, args: Value) -> Api {
    let (user, administrator) = caller(&app, &identity).await?;
    let pending = args["client_request_id"].as_str().unwrap_or("").to_owned();
    if pending.is_empty() && !wiki::read_only(name) {
        return Err(bad("client_request_id is required"));
    }
    let mut output = run(&app, &user, administrator, name, &args, &pending).await?;
    // The reader shows the article rendered, with links resolved for this caller.
    if name == "wiki_get" {
        let resolve = resolver(&app, &user, administrator).await?;
        output["html"] = json!(wiki_markdown::render(
            output["body"].as_str().unwrap_or(""),
            &resolve
        ));
        output["url"] = json!(wiki::article_url(output["slug"].as_str().unwrap_or("")));
    }
    output["can_edit"] = json!(administrator);
    Ok(Json(output))
}

#[derive(Deserialize)]
pub(crate) struct SearchQuery {
    #[serde(default)]
    q: String,
    #[serde(default)]
    also: Option<String>,
    #[serde(default)]
    category: Option<String>,
}

pub(crate) async fn list(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Query(query): Query<SearchQuery>,
) -> Api {
    handle(
        app,
        identity,
        "wiki_list",
        json!({"category":query.category}),
    )
    .await
}
pub(crate) async fn search(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Query(query): Query<SearchQuery>,
) -> Api {
    // `also` is comma-separated alternate phrasings, as the search tool takes them.
    let also: Vec<&str> = query
        .also
        .as_deref()
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .collect();
    handle(
        app,
        identity,
        "wiki_search",
        json!({"query":query.q,"also":also,"limit":10}),
    )
    .await
}
pub(crate) async fn get(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path(slug): Path<String>,
) -> Api {
    handle(app, identity, "wiki_get", json!({"slug":slug})).await
}
pub(crate) async fn history(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path(slug): Path<String>,
) -> Api {
    handle(app, identity, "wiki_history", json!({"slug":slug})).await
}
pub(crate) async fn backlinks(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path(slug): Path<String>,
) -> Api {
    handle(app, identity, "wiki_backlinks", json!({"slug":slug})).await
}
pub(crate) async fn revision(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path(id): Path<String>,
) -> Api {
    handle(
        app,
        identity,
        "wiki_get_revision",
        json!({"revision_id":id}),
    )
    .await
}
pub(crate) async fn create(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Json(body): Json<Value>,
) -> Api {
    handle(app, identity, "wiki_create", body).await
}
/// `PATCH /articles/{slug}` with `action`: `update` (default), `status`, `restore` or `delete`.
pub(crate) async fn change(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path(slug): Path<String>,
    Json(mut body): Json<Value>,
) -> Api {
    let name = match body["action"].as_str().unwrap_or("update") {
        "update" => "wiki_update",
        "status" => "wiki_set_status",
        "restore" => "wiki_restore",
        "delete" => "wiki_delete",
        _ => return Err(bad("Unknown Wiki action")),
    };
    body["slug"] = json!(slug);
    handle(app, identity, name, body).await
}

// Public pages: published articles only, no scripts, styled by the shared design system.

const PAGE_CSS: &str = "*{box-sizing:border-box}body{margin:0;background:var(--bg-0);color:var(--fg-1);font-family:var(--font-sans,system-ui,sans-serif);line-height:1.65}\
a{color:var(--accent);text-decoration:none}a:hover{text-decoration:underline}.pw-wrap{max-width:760px;margin:0 auto;padding:24px 20px 80px}\
.pw-top{display:flex;justify-content:space-between;gap:16px;padding:8px 0 20px;margin-bottom:28px;border-bottom:var(--border-width) solid var(--border)}\
.pw-brand{font-weight:700;color:var(--fg-1)}.pw-cat{font-size:.76rem;font-weight:600;letter-spacing:.05em;text-transform:uppercase;color:var(--accent)}\
h1{font-size:2rem;line-height:1.2;margin:0 0 12px}.pw-meta,.pw-snip,.pw-empty{color:var(--fg-3);font-size:.9rem}\
article h2{font-size:1.4rem;margin:34px 0 12px;padding-bottom:6px;border-bottom:var(--border-width) solid var(--border-soft)}\
article code{font-family:var(--font-mono,ui-monospace,monospace);font-size:.88em;background:var(--bg-2);border-radius:var(--r-xs);padding:.12em .35em}\
article pre{background:var(--bg-2);border:var(--border-width) solid var(--border);border-radius:var(--r-md);padding:14px 16px;overflow:auto}\
article pre code{background:none;padding:0}article blockquote{margin:0 0 16px;padding:4px 16px;color:var(--fg-2);border-left:3px solid var(--accent-line)}\
article img{max-width:100%;height:auto}.wiki-link-missing{color:var(--fg-2)}.pw-tags{margin-top:30px;display:flex;flex-wrap:wrap;gap:8px}\
.pw-tag{font-size:.8rem;color:var(--fg-3);background:var(--bg-2);border-radius:var(--r-pill);padding:3px 11px}\
.pw-index{list-style:none;padding:0}.pw-index li{padding:14px 0;border-bottom:var(--border-width) solid var(--border-soft)}.pw-index a{font-weight:600}";

fn page(title: &str, description: &str, canonical: &str, body: &str) -> Html<String> {
    let origin = webagent_webapp::pages::public_origin();
    let (title, description) = (
        wiki_markdown::escape(title),
        wiki_markdown::escape(description),
    );
    Html(format!(
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"UTF-8\">\n<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
<title>{title}</title>\n<meta name=\"description\" content=\"{description}\">\n<link rel=\"canonical\" href=\"{origin}{canonical}\">\n\
<meta property=\"og:type\" content=\"article\">\n<meta property=\"og:title\" content=\"{title}\">\n<meta property=\"og:description\" content=\"{description}\">\n\
<link rel=\"icon\" href=\"/v3-ui/app-icon.svg\" type=\"image/svg+xml\">\n<link rel=\"stylesheet\" href=\"/ui/vendor/fonts/fonts.css\">\n\
<link rel=\"stylesheet\" href=\"/ui/shared/css/design-system.css\">\n<style>{PAGE_CSS}</style>\n</head>\n<body>\n<div class=\"pw-wrap\">\n\
<nav class=\"pw-top\"><a class=\"pw-brand\" href=\"/\">WebAgent</a><a href=\"/wiki\">Wiki</a></nav>\n{body}\n</div>\n</body>\n</html>\n"
    ))
}
fn not_published() -> Response {
    (
        StatusCode::NOT_FOUND,
        page("Article not found", "", "/wiki", "<h1>Article not found</h1>\n<p class=\"pw-empty\">This article does not exist or is not published.</p>"),
    )
        .into_response()
}

pub(crate) async fn public_index(State(app): State<App>) -> Response {
    let Ok(listed) = run(&app, "", false, "wiki_list", &json!({}), "").await else {
        return not_published();
    };
    let items: String = listed["articles"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|a| {
            format!(
                "<li><a href=\"{}\">{}</a><p class=\"pw-snip\">{}</p></li>",
                wiki_markdown::escape(&wiki::article_url(a["slug"].as_str().unwrap_or(""))),
                wiki_markdown::escape(a["title"].as_str().unwrap_or("")),
                wiki_markdown::escape(&wiki_markdown::description(
                    a["snippet"].as_str().unwrap_or(""),
                    160
                ))
            )
        })
        .collect();
    let body = if items.is_empty() {
        "<h1>Wiki</h1>\n<p class=\"pw-empty\">No published articles yet.</p>".to_owned()
    } else {
        format!("<h1>Wiki</h1>\n<ul class=\"pw-index\">{items}</ul>")
    };
    page(
        "Wiki · WebAgent",
        "Published WebAgent Wiki articles.",
        "/wiki",
        &body,
    )
    .into_response()
}

pub(crate) async fn public_article(State(app): State<App>, Path(slug): Path<String>) -> Response {
    let Ok(article) = run(&app, "", false, "wiki_get", &json!({"slug":slug}), "").await else {
        return not_published();
    };
    let Ok(resolve) = resolver(&app, "", false).await else {
        return not_published();
    };
    let body_text = article["body"].as_str().unwrap_or("");
    let category = article["category"]
        .as_str()
        .filter(|c| !c.is_empty())
        .map(|c| format!("<div class=\"pw-cat\">{}</div>\n", wiki_markdown::escape(c)))
        .unwrap_or_default();
    let updated = article["updated_at"]
        .as_str()
        .map(|at| {
            format!(
                "<p class=\"pw-meta\">Updated {}</p>\n",
                wiki_markdown::escape(at.get(..10).unwrap_or(at))
            )
        })
        .unwrap_or_default();
    let tags: String = article["tags"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(|t| {
            format!(
                "<span class=\"pw-tag\">#{}</span>",
                wiki_markdown::escape(t)
            )
        })
        .collect();
    let title = article["title"].as_str().unwrap_or(&slug).to_owned();
    let body = format!(
        "<article>\n{category}<h1>{}</h1>\n{updated}{}\n{}</article>",
        wiki_markdown::escape(&title),
        wiki_markdown::render(body_text, &resolve),
        if tags.is_empty() {
            String::new()
        } else {
            format!("<div class=\"pw-tags\">{tags}</div>\n")
        }
    );
    page(
        &format!("{title} · WebAgent Wiki"),
        &wiki_markdown::description(body_text, 160),
        &wiki::article_url(&slug),
        &body,
    )
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn public_pages_show_published_articles_and_only_administrators_write() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let draft = json!({"title":"Release pipeline","body":"Stages [[Missing]].","client_request_id":"w1"});
        let denied = run(&app, "visitor", false, "wiki_create", &draft, "w1").await;
        assert_eq!(denied.unwrap_err().0, StatusCode::FORBIDDEN);
        let created = run(&app, "admin", true, "wiki_create", &draft, "w1")
            .await
            .unwrap();
        assert_eq!(created["url"], "/wiki/release-pipeline");
        let page = |app: App| public_article(State(app), Path("release-pipeline".to_owned()));
        assert_eq!(page(app.clone()).await.status(), StatusCode::NOT_FOUND);
        let args = json!({"slug":"release-pipeline","status":"published","expected_revision":1});
        run(&app, "admin", true, "wiki_set_status", &args, "w2")
            .await
            .unwrap();
        let response = page(app.clone()).await;
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        let html = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(html.contains("<h1>Release pipeline</h1>"));
        assert!(html.contains("<span class=\"wiki-link-missing\">Missing</span>"));
    }
}
