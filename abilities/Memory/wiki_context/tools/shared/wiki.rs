//! The server Wiki: one shared Markdown knowledge base in the application plane.
//! Every permitted session reads it (visitors see published articles only); only app
//! administrators change it, whatever an agent's tool policy allows.
use crate::{Call, NativeHost};
use anyhow::{anyhow, bail, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
use webagent_abilities::tool_arguments::read;
use webagent_access::Relationship;
use webagent_host_core::{now, StoreCore};

#[path = "../wiki_backlinks/wiki_backlinks.rs"]
mod wiki_backlinks;
#[path = "../wiki_create/wiki_create.rs"]
mod wiki_create;
#[path = "../wiki_delete/wiki_delete.rs"]
mod wiki_delete;
#[path = "../wiki_get/wiki_get.rs"]
mod wiki_get;
#[path = "../wiki_get_revision/wiki_get_revision.rs"]
mod wiki_get_revision;
#[path = "../wiki_history/wiki_history.rs"]
mod wiki_history;
#[path = "../wiki_list/wiki_list.rs"]
mod wiki_list;
#[path = "../wiki_restore/wiki_restore.rs"]
mod wiki_restore;
#[path = "../wiki_search/wiki_search.rs"]
mod wiki_search;
#[path = "../wiki_set_status/wiki_set_status.rs"]
mod wiki_set_status;
#[path = "../wiki_update/wiki_update.rs"]
mod wiki_update;

const ARTICLES: &str = "wiki:article:";
const REVISIONS: &str = "wiki:revision:";
const BODY_MAX_BYTES: usize = 200 * 1024;
const ARTICLE_LIMIT: usize = 1000;

#[must_use]
pub fn owns(name: &str) -> bool {
    TOOLS.contains(&name)
}
/// Tools this module executes; their schemas live in the ability's `tools/<tool>/<tool>.tool.json`.
pub const TOOLS: &[&str] = &[
    "wiki_search",
    "wiki_list",
    "wiki_get",
    "wiki_create",
    "wiki_update",
    "wiki_set_status",
    "wiki_delete",
    "wiki_history",
    "wiki_get_revision",
    "wiki_restore",
    "wiki_backlinks",
];
/// Argument types of this family's tools, checked against their tool.json by the crate test.
pub const ARGUMENT_TYPES: &[webagent_abilities::tool_arguments::ArgumentType] = &[
    webagent_abilities::argument_type!("wiki_backlinks", ArticleArgs),
    webagent_abilities::argument_type!("wiki_create", wiki_create::CreateArgs),
    webagent_abilities::argument_type!("wiki_delete", wiki_delete::DeleteArgs),
    webagent_abilities::argument_type!("wiki_get", ArticleArgs),
    webagent_abilities::argument_type!("wiki_get_revision", wiki_get_revision::RevisionArgs),
    webagent_abilities::argument_type!("wiki_history", ArticleArgs),
    webagent_abilities::argument_type!("wiki_list", wiki_list::ListArgs),
    webagent_abilities::argument_type!("wiki_restore", wiki_restore::RestoreArgs),
    webagent_abilities::argument_type!("wiki_search", wiki_search::SearchArgs),
    webagent_abilities::argument_type!("wiki_set_status", wiki_set_status::StatusArgs),
    webagent_abilities::argument_type!("wiki_update", wiki_update::UpdateArgs),
];
/// The arguments of the tools that read one article: `wiki_get`, `wiki_history`, `wiki_backlinks`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ArticleArgs {
    slug: String,
}
/// Whether operation `name` only reads.
#[must_use]
pub fn read_only(name: &str) -> bool {
    READS.contains(&name)
}
/// Operations that change nothing; every other one needs an app administrator.
const READS: &[&str] = &[
    "wiki_search",
    "wiki_list",
    "wiki_get",
    "wiki_history",
    "wiki_get_revision",
    "wiki_backlinks",
];

fn valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= 80
        && slug
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}
fn slug_arg(slug: &str) -> Result<String> {
    Some(slug)
        .filter(|s| valid_slug(s))
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("Invalid slug; use one from wiki_search or wiki_list"))
}
/// The slug of the one article `args` names.
fn article_slug(name: &str, args: &Value) -> Result<String> {
    slug_arg(&read::<ArticleArgs>(name, args)?.slug)
}
fn title_arg(value: &str) -> Result<String> {
    Some(value.trim())
        .filter(|t| !t.is_empty() && t.len() <= 200 && !t.chars().any(char::is_control))
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("Title must be 1 to 200 characters"))
}
fn body_arg(body: &str) -> Result<String> {
    if body.len() > BODY_MAX_BYTES {
        bail!("Body must be at most 200 KiB");
    }
    Ok(body.to_owned())
}
/// A list of up to `max` trimmed, non-empty strings of up to `len` bytes.
fn list_arg(items: &[String], max: usize, len: usize, label: &str) -> Result<Value> {
    let items: Vec<&str> = items
        .iter()
        .map(|t| t.trim())
        .filter(|t| !t.is_empty())
        .collect();
    if items.len() > max || items.iter().any(|t| t.len() > len) {
        bail!("At most {max} {label} of up to {len} characters");
    }
    Ok(json!(items))
}
fn tags_arg(value: &[String]) -> Result<Value> {
    list_arg(value, 20, 40, "tags")
}
fn keywords_arg(value: &[String]) -> Result<Value> {
    list_arg(value, 30, 60, "keywords")
}
fn category_arg(value: &str) -> Result<String> {
    Some(value.trim())
        .filter(|c| c.len() <= 60)
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("Category must be up to 60 characters"))
}
fn status_arg(value: &str) -> Result<&'static str> {
    match value {
        "draft" => Ok("draft"),
        "published" => Ok("published"),
        _ => bail!("Status must be draft or published"),
    }
}
fn revision_id(slug: &str, revision: u64) -> String {
    format!("{slug}:{revision}")
}
/// The article's page: the public `/wiki/<slug>` page, or the in-app reader in a chat.
#[must_use]
pub fn article_url(slug: &str) -> String {
    format!("/wiki/{slug}")
}
fn summary(article: &Value) -> Value {
    let body = article["body"].as_str().unwrap_or("");
    json!({"slug":article["slug"],"url":article_url(article["slug"].as_str().unwrap_or("")),"title":article["title"],"tags":article["tags"],
        "category":article["category"],"status":article["status"],"revision":article["revision"],
        "updated_by":article["updated_by"],"updated_at":article["updated_at"],"snippet":excerpt(body, 0, 200)})
}
/// Up to `len` bytes of `text` from `at`, snapped to char boundaries.
fn excerpt(text: &str, at: usize, len: usize) -> &str {
    let mut start = at.min(text.len());
    while !text.is_char_boundary(start) {
        start -= 1;
    }
    let mut end = (start + len).min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[start..end]
}

/// Write `article` and its revision snapshot; the snapshot first, so a retried call
/// rewrites the same revision key instead of skipping history.
async fn save(store: &StoreCore, article: &Value) -> Result<()> {
    let slug = article["slug"].as_str().unwrap_or("");
    let revision = article["revision"].as_u64().unwrap_or(0);
    store
        .put(&format!("{REVISIONS}{slug}:{revision:06}"), article)
        .await?;
    store.put(&format!("{ARTICLES}{slug}"), article).await
}

/// Who is asking. Only administrators write; staff also see drafts and history.
pub struct Viewer<'a> {
    pub administrator: bool,
    pub staff: bool,
    pub actor: &'a str,
}

/// Runs one Wiki tool. Reads hide drafts from non-staff; writes need an app administrator.
pub async fn execute(
    host: &dyn NativeHost,
    call: Call<'_>,
    name: &str,
    args: &Value,
    cancel: CancellationToken,
) -> Result<Value> {
    let run = host.authorize(call, name, args, &cancel).await?;
    let agent = run["agent_id"]
        .as_str()
        .unwrap_or("shared_default")
        .to_owned();
    // The actor is the real participant, not a shared chat's owner.
    let access = host.session_access(call.sid, &agent).await?;
    let administrator = access.principal.administrator();
    let staff = administrator
        || matches!(
            access.relationship,
            Relationship::Owner | Relationship::Member
        );
    let viewer = Viewer {
        administrator,
        staff,
        actor: host.actor(),
    };
    // The application plane is shared by every caller's store view.
    operate(host.store(), &viewer, name, args, call.pending_id).await
}

/// One Wiki operation, named like its tool, for the tools and the Wiki API alike.
/// `pending_id` makes a retried write report its saved result instead of repeating it.
pub async fn operate(
    store: &StoreCore,
    viewer: &Viewer<'_>,
    name: &str,
    args: &Value,
    pending_id: &str,
) -> Result<Value> {
    if !owns(name) {
        bail!("Unknown Wiki tool");
    }
    if !READS.contains(&name) && !viewer.administrator {
        return Ok(
            json!({"status":"denied","output":"Only app administrators can change the server Wiki"}),
        );
    }
    let staff = viewer.staff || viewer.administrator;
    // shortcut: every call decodes all articles; fine to ~1000 articles, index if it grows.
    let all = store.list(ARTICLES).await?;
    let visible: Vec<&Value> = all
        .iter()
        .filter(|a| a["deleted"] != true && (staff || a["status"] == "published"))
        .collect();
    let stored = |slug: &str| all.iter().find(|a| a["slug"] == slug);
    let actor = viewer.actor;
    let output = match name {
        "wiki_search" => wiki_search::run(args, &visible)?,
        "wiki_list" => wiki_list::run(args, &visible)?,
        "wiki_get" => wiki_get::run(args, &visible)?,
        "wiki_backlinks" => wiki_backlinks::run(args, &visible)?,
        "wiki_history" | "wiki_get_revision" if !staff => {
            return Ok(json!({"status":"denied","output":"Wiki history needs staff access"}));
        }
        "wiki_history" => wiki_history::run(store, args, &all).await?,
        "wiki_get_revision" => wiki_get_revision::run(store, args).await?,
        _ => {
            // Writes. A retried call finds its own pending_id and reports the saved result.
            let write = Write::read(name, args)?;
            let slug = write.slug()?;
            let current = stored(&slug);
            if let Some(done) = current.filter(|a| a["pending_id"] == pending_id) {
                return Ok(json!({"status":"success","output":summary(done)}));
            }
            let live = current.filter(|a| a["deleted"] != true);
            let revision = current.and_then(|a| a["revision"].as_u64()).unwrap_or(0);
            if let Some(expected) = write.expected_revision() {
                if live.is_none() && name != "wiki_restore" {
                    bail!("Wiki article not found");
                }
                if expected != revision {
                    bail!("Wiki article changed; wiki_get it again before changing it");
                }
            }
            let stamp = json!({"revision":revision + 1,"updated_by":actor,"updated_at":now(),"pending_id":pending_id});
            let mut article = match &write {
                Write::Create(create) => {
                    wiki_create::article(create, &slug, live, visible.len(), actor)?
                }
                Write::Update(update) => wiki_update::article(update, live)?,
                Write::SetStatus(set) => wiki_set_status::article(set, live)?,
                Write::Restore(restore) => wiki_restore::article(store, restore, &slug).await?,
                // Delete keeps a tombstone so history survives and numbering continues.
                Write::Delete(_) => wiki_delete::article(&slug),
            };
            for (field, value) in stamp.as_object().into_iter().flatten() {
                article[field] = value.clone();
            }
            if name == "wiki_delete" {
                store.put(&format!("{ARTICLES}{slug}"), &article).await?;
                json!({"slug":slug,"deleted":true})
            } else {
                save(store, &article).await?;
                summary(&article)
            }
        }
    };
    Ok(json!({"status":"success","output":output}))
}

/// A write tool's checked arguments.
enum Write {
    Create(wiki_create::CreateArgs),
    Update(wiki_update::UpdateArgs),
    SetStatus(wiki_set_status::StatusArgs),
    Restore(wiki_restore::RestoreArgs),
    Delete(wiki_delete::DeleteArgs),
}

impl Write {
    fn read(name: &str, args: &Value) -> Result<Self> {
        Ok(match name {
            "wiki_create" => Self::Create(read(name, args)?),
            "wiki_update" => Self::Update(read(name, args)?),
            "wiki_set_status" => Self::SetStatus(read(name, args)?),
            "wiki_restore" => Self::Restore(read(name, args)?),
            "wiki_delete" => Self::Delete(read(name, args)?),
            _ => bail!("Unknown Wiki tool"),
        })
    }

    /// The article's slug: from the title for a create, else the one given.
    fn slug(&self) -> Result<String> {
        match self {
            Self::Create(create) => wiki_create::slug(&create.title),
            Self::Update(update) => slug_arg(&update.slug),
            Self::SetStatus(set) => slug_arg(&set.slug),
            Self::Restore(restore) => slug_arg(&restore.slug),
            Self::Delete(delete) => slug_arg(&delete.slug),
        }
    }

    /// The revision the caller last saw; a create has none.
    fn expected_revision(&self) -> Option<u64> {
        match self {
            Self::Create(_) => None,
            Self::Update(update) => Some(update.expected_revision),
            Self::SetStatus(set) => Some(set.expected_revision),
            Self::Restore(restore) => Some(restore.expected_revision),
            Self::Delete(delete) => Some(delete.expected_revision),
        }
    }
}

/// Visible article `slug`, if any.
fn find<'a>(visible: &[&'a Value], slug: &str) -> Option<&'a Value> {
    visible.iter().find(|a| a["slug"] == slug).copied()
}

#[cfg(test)]
#[path = "../../tests/wiki.rs"]
mod tests;
