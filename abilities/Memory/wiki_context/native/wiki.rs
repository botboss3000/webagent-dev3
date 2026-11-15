//! The server Wiki: one shared Markdown knowledge base in the application plane.
//! Every permitted session reads it (visitors see published articles only); only app
//! administrators change it, whatever an agent's tool policy allows.
use crate::{Call, NativeHost};
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
use webagent_access::Relationship;
use webagent_host_core::{digest, now, StoreCore};

const ARTICLES: &str = "wiki:article:";
const REVISIONS: &str = "wiki:revision:";
const BODY_MAX_BYTES: usize = 200 * 1024;
const ARTICLE_LIMIT: usize = 1000;

#[must_use]
pub fn owns(name: &str) -> bool {
    TOOLS.contains(&name)
}
/// Tools this module executes; their schemas live in the ability's `tools/<tool>/tool.json`.
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
fn slug_arg(args: &Value) -> Result<String> {
    args["slug"]
        .as_str()
        .filter(|s| valid_slug(s))
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("Invalid slug; use one from wiki_search or wiki_list"))
}
/// Lowercase ASCII words of `title` joined by hyphens.
fn slugify(title: &str) -> String {
    let words: Vec<String> = title
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_lowercase)
        .collect();
    let mut slug = words.join("-");
    slug.truncate(80);
    slug.trim_end_matches('-').to_owned()
}
/// `args[key]` unless absent or null.
fn given<'a>(args: &'a Value, key: &str) -> Option<&'a Value> {
    args.get(key).filter(|v| !v.is_null())
}
fn title_arg(value: &Value) -> Result<String> {
    value
        .as_str()
        .map(str::trim)
        .filter(|t| !t.is_empty() && t.len() <= 200 && !t.chars().any(char::is_control))
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("Title must be 1 to 200 characters"))
}
fn body_arg(value: &Value) -> Result<String> {
    let body = value.as_str().ok_or_else(|| anyhow!("Body must be text"))?;
    if body.len() > BODY_MAX_BYTES {
        bail!("Body must be at most 200 KiB");
    }
    Ok(body.to_owned())
}
/// A list of up to `max` trimmed, non-empty strings of up to `len` bytes.
fn list_arg(value: &Value, max: usize, len: usize, label: &str) -> Result<Value> {
    let items = value
        .as_array()
        .ok_or_else(|| anyhow!("{label} must be a list"))?;
    let items: Vec<&str> = items
        .iter()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .collect();
    if items.len() > max || items.iter().any(|t| t.len() > len) {
        bail!("At most {max} {label} of up to {len} characters");
    }
    Ok(json!(items))
}
fn tags_arg(value: &Value) -> Result<Value> {
    list_arg(value, 20, 40, "tags")
}
fn keywords_arg(value: &Value) -> Result<Value> {
    list_arg(value, 30, 60, "keywords")
}
fn category_arg(value: &Value) -> Result<String> {
    value
        .as_str()
        .map(str::trim)
        .filter(|c| c.len() <= 60)
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("Category must be up to 60 characters"))
}
fn status_arg(value: &Value) -> Result<&'static str> {
    match value.as_str() {
        Some("draft") => Ok("draft"),
        Some("published") => Ok("published"),
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
/// Score by distinct words of the query and its alternate terms: a title hit counts three,
/// a tag or keyword two, the body one, so an article matching several phrasings ranks first.
fn search(articles: &[&Value], query: &str, limit: usize) -> Vec<Value> {
    let mut words: Vec<String> = query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 1)
        .map(str::to_lowercase)
        .collect();
    words.sort();
    words.dedup();
    let mut hits: Vec<(usize, Value)> = articles
        .iter()
        .filter_map(|article| {
            let title = article["title"].as_str().unwrap_or("").to_lowercase();
            let tags = format!("{} {}", article["tags"], article["keywords"]).to_lowercase();
            let body = article["body"].as_str().unwrap_or("");
            let lower = body.to_lowercase();
            let score: usize = words
                .iter()
                .map(|w| {
                    3 * usize::from(title.contains(w.as_str()))
                        + 2 * usize::from(tags.contains(w.as_str()))
                        + usize::from(lower.contains(w.as_str()))
                })
                .sum();
            if score == 0 {
                return None;
            }
            // Lowercasing can shift byte offsets for some scripts; excerpt() snaps them.
            let at = words
                .iter()
                .filter_map(|w| lower.find(w.as_str()))
                .min()
                .unwrap_or(0);
            let mut hit = summary(article);
            hit["snippet"] = json!(excerpt(body, at.saturating_sub(200), 600));
            hit["score"] = json!(score);
            Some((score, hit))
        })
        .collect();
    hits.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
    hits.into_iter().take(limit).map(|(_, hit)| hit).collect()
}
/// Whether `body` links `[[slug]]` or `[[title]]`, ignoring case.
fn links_to(body: &str, slug: &str, title: &str) -> bool {
    let body = body.to_lowercase();
    [slug, title]
        .iter()
        .any(|target| body.contains(&format!("[[{}]]", target.to_lowercase())))
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
    let find = |slug: &str| visible.iter().find(|a| a["slug"] == slug).copied();
    let stored = |slug: &str| all.iter().find(|a| a["slug"] == slug);
    let actor = viewer.actor;
    let output = match name {
        "wiki_search" => {
            let limit = args["limit"].as_u64().unwrap_or(5).clamp(1, 10) as usize;
            let also = given(args, "also")
                .map(|v| list_arg(v, 8, 100, "alternate terms"))
                .transpose()?;
            // Alternate phrasings join the query; the caller supplies synonyms, not a model call.
            let query = also
                .iter()
                .flat_map(|v| v.as_array().into_iter().flatten())
                .fold(
                    args["query"].as_str().unwrap_or("").to_owned(),
                    |mut query, term| {
                        query.push(' ');
                        query.push_str(term.as_str().unwrap_or(""));
                        query
                    },
                );
            json!({"results":search(&visible, &query, limit)})
        }
        "wiki_list" => {
            let category = args["category"].as_str().filter(|c| !c.is_empty());
            let mut items: Vec<&Value> = visible
                .iter()
                .filter(|a| category.is_none_or(|c| a["category"] == c))
                .copied()
                .collect();
            items.sort_by(|a, b| b["updated_at"].as_str().cmp(&a["updated_at"].as_str()));
            json!({"articles":items.iter().take(100).map(|a| summary(a)).collect::<Vec<_>>(),"total":items.len()})
        }
        "wiki_get" => {
            let slug = slug_arg(args)?;
            let article = find(&slug).ok_or_else(|| anyhow!("Wiki article not found"))?;
            let mut article = article.clone();
            if let Some(map) = article.as_object_mut() {
                map.remove("pending_id");
            }
            article
        }
        "wiki_backlinks" => {
            let slug = slug_arg(args)?;
            let target = find(&slug).ok_or_else(|| anyhow!("Wiki article not found"))?;
            let title = target["title"].as_str().unwrap_or("");
            json!({"slug":slug,"linked_from":visible.iter()
                .filter(|a| a["slug"] != slug.as_str() && links_to(a["body"].as_str().unwrap_or(""), &slug, title))
                .map(|a| json!({"slug":a["slug"],"title":a["title"]})).collect::<Vec<_>>()})
        }
        "wiki_history" | "wiki_get_revision" if !staff => {
            return Ok(json!({"status":"denied","output":"Wiki history needs staff access"}));
        }
        "wiki_history" => {
            let slug = slug_arg(args)?;
            let mut revisions = store.list(&format!("{REVISIONS}{slug}:")).await?;
            if revisions.is_empty() {
                bail!("Wiki article not found");
            }
            revisions.sort_by_key(|r| std::cmp::Reverse(r["revision"].as_u64()));
            let current = stored(&slug);
            json!({"slug":slug,"current_revision":current.map(|a| &a["revision"]),
                "deleted":current.is_some_and(|a| a["deleted"] == true),"revisions":revisions.iter().take(50).map(|r| {
                let mut item = summary(r);
                item["revision_id"] = json!(revision_id(&slug, r["revision"].as_u64().unwrap_or(0)));
                item
            }).collect::<Vec<_>>()})
        }
        "wiki_get_revision" => {
            let (slug, revision) = args["revision_id"]
                .as_str()
                .and_then(|id| id.rsplit_once(':'))
                .and_then(|(slug, n)| Some((slug, n.parse::<u64>().ok()?)))
                .filter(|(slug, _)| valid_slug(slug))
                .ok_or_else(|| anyhow!("Invalid revision ID; use one from wiki_history"))?;
            let mut snapshot = store
                .get(&format!("{REVISIONS}{slug}:{revision:06}"))
                .await?
                .ok_or_else(|| anyhow!("Wiki revision not found"))?;
            if let Some(map) = snapshot.as_object_mut() {
                map.remove("pending_id");
            }
            snapshot["revision_id"] = json!(revision_id(slug, revision));
            snapshot
        }
        _ => {
            // Writes. A retried call finds its own pending_id and reports the saved result.
            let slug = match name {
                "wiki_create" => {
                    let title = title_arg(&args["title"])?;
                    let slug = slugify(&title);
                    // Titles without ASCII words still get a stable slug.
                    if slug.is_empty() {
                        digest(&json!(title))[..12].to_owned()
                    } else {
                        slug
                    }
                }
                _ => slug_arg(args)?,
            };
            let current = stored(&slug);
            if let Some(done) = current.filter(|a| a["pending_id"] == pending_id) {
                return Ok(json!({"status":"success","output":summary(done)}));
            }
            let live = current.filter(|a| a["deleted"] != true);
            let revision = current.and_then(|a| a["revision"].as_u64()).unwrap_or(0);
            if name != "wiki_create" {
                if live.is_none() && name != "wiki_restore" {
                    bail!("Wiki article not found");
                }
                if args["expected_revision"].as_u64() != Some(revision) {
                    bail!("Wiki article changed; wiki_get it again before changing it");
                }
            }
            let stamp = json!({"revision":revision + 1,"updated_by":actor,"updated_at":now(),"pending_id":pending_id});
            let mut article = match name {
                "wiki_create" => {
                    if live.is_some() {
                        bail!("A Wiki article with this title exists: {slug}; use wiki_update");
                    }
                    if visible.len() >= ARTICLE_LIMIT {
                        bail!("Wiki article limit (1000) reached");
                    }
                    let tags = given(args, "tags").map(tags_arg).transpose()?;
                    let keywords = given(args, "keywords").map(keywords_arg).transpose()?;
                    let category = given(args, "category").map(category_arg).transpose()?;
                    let status = given(args, "status").map(status_arg).transpose()?;
                    json!({"slug":slug,"title":title_arg(&args["title"])?,"body":body_arg(&args["body"])?,
                        "tags":tags.unwrap_or_else(|| json!([])),"keywords":keywords.unwrap_or_else(|| json!([])),"category":category.unwrap_or_default(),
                        "status":status.unwrap_or("draft"),"created_by":actor,"created_at":now()})
                }
                "wiki_update" => {
                    let mut article = live.cloned().unwrap_or_default();
                    for (field, value) in [
                        (
                            "title",
                            given(args, "title")
                                .map(title_arg)
                                .transpose()?
                                .map(Value::from),
                        ),
                        (
                            "body",
                            given(args, "body")
                                .map(body_arg)
                                .transpose()?
                                .map(Value::from),
                        ),
                        ("tags", given(args, "tags").map(tags_arg).transpose()?),
                        (
                            "keywords",
                            given(args, "keywords").map(keywords_arg).transpose()?,
                        ),
                        (
                            "category",
                            given(args, "category")
                                .map(category_arg)
                                .transpose()?
                                .map(Value::from),
                        ),
                        (
                            "status",
                            given(args, "status")
                                .map(status_arg)
                                .transpose()?
                                .map(Value::from),
                        ),
                    ] {
                        if let Some(value) = value {
                            article[field] = value;
                        }
                    }
                    article
                }
                "wiki_set_status" => {
                    let mut article = live.cloned().unwrap_or_default();
                    article["status"] = json!(status_arg(&args["status"])?);
                    article
                }
                "wiki_restore" => {
                    let wanted = args["revision_id"]
                        .as_str()
                        .and_then(|id| id.strip_prefix(&format!("{slug}:")))
                        .and_then(|n| n.parse::<u64>().ok())
                        .ok_or_else(|| anyhow!("Invalid revision ID for this article"))?;
                    let mut old = store
                        .get(&format!("{REVISIONS}{slug}:{wanted:06}"))
                        .await?
                        .ok_or_else(|| anyhow!("Wiki revision not found"))?;
                    if let Some(map) = old.as_object_mut() {
                        map.remove("deleted");
                    }
                    old
                }
                // Delete keeps a tombstone so history survives and numbering continues.
                "wiki_delete" => json!({"slug":slug,"deleted":true}),
                _ => bail!("Unknown Wiki tool"),
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn slugs_are_bounded_ascii_words() {
        assert_eq!(
            slugify("  Café Opening Hours (2026)! "),
            "caf-opening-hours-2026"
        );
        assert_eq!(slugify("!!!"), "");
        assert!(valid_slug("opening-hours"));
        assert!(!valid_slug("../x"));
        assert!(!valid_slug("Upper"));
        assert!(slugify(&"word ".repeat(40)).len() <= 80);
    }
    #[test]
    fn search_weights_title_over_body_and_snaps_excerpts() {
        let a = json!({"slug":"refunds","title":"Refund policy","tags":["billing"],"body":"Ünïcode refunds within 30 days."});
        let b =
            json!({"slug":"menu","title":"Menu","tags":[],"body":"Ask about refund on drinks."});
        let hits = search(&[&b, &a], "refund", 5);
        assert_eq!(hits[0]["slug"], "refunds");
        assert_eq!(hits.len(), 2);
        assert!(search(&[&a], "parking", 5).is_empty());
        let c = json!({"slug":"returns","title":"Returns","tags":[],"keywords":["money back"],"body":"Bring a receipt."});
        assert_eq!(search(&[&c, &b], "money", 5)[0]["slug"], "returns");
    }
    #[test]
    fn backlinks_match_slug_or_title() {
        assert!(links_to(
            "See [[Refund Policy]].",
            "refunds",
            "Refund policy"
        ));
        assert!(links_to("See [[refunds]].", "refunds", "Refund policy"));
        assert!(!links_to("See refunds.", "refunds", "Refund policy"));
    }
    #[test]
    fn arguments_are_validated() {
        assert!(status_arg(&json!("public")).is_err());
        assert!(tags_arg(&json!(vec!["a"; 21])).is_err());
        assert!(keywords_arg(&json!(["x".repeat(61)])).is_err());
        assert!(title_arg(&json!("")).is_err());
        assert!(body_arg(&json!("x".repeat(BODY_MAX_BYTES + 1))).is_err());
    }
}
