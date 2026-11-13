//! Administrator review log: run-health records kept in one shared database
//! (`diagnostics/agent-review.db`) so reviewing an issue never reads a user's
//! database. The record is the bounded, content-free form the run view carries:
//! counts, failure kinds, tool names and message templates only.
//!
//! The log is evidence, not authority. A failed write never fails the run, and no
//! run, session or account read ever depends on it.
use super::*;
use std::time::{SystemTime, UNIX_EPOCH};

/// Longest kept review history, in seconds (one year).
const REVIEW_RETENTION_SECONDS: i64 = 365 * 24 * 60 * 60;

/// The tool names a release's changed paths own: a plugin tool folder names its tool, and a
/// Rust module that declares a `TOOLS` constant owns every name in it.
async fn tools_in_paths(paths: &[String]) -> std::collections::BTreeSet<String> {
    let mut tools = std::collections::BTreeSet::new();
    for path in paths {
        let clean = path.strip_prefix("v3/").unwrap_or(path);
        if let Some(rest) = clean.split("/tools/").nth(1) {
            if let Some(name) = rest.split('/').next() {
                if !name.is_empty() && name.len() <= 40 {
                    tools.insert(name.to_owned());
                }
            }
        }
        if clean.ends_with(".rs") {
            let Ok(text) =
                tokio::fs::read_to_string(webagent_configuration::paths::checkout().join(&clean))
                    .await
            else {
                continue;
            };
            let Some(at) = text.find("const TOOLS") else {
                continue;
            };
            let window = &text[at..text.len().min(at.saturating_add(600))];
            for part in window.split('"').skip(1).step_by(2) {
                let name = part.trim();
                if !name.is_empty()
                    && name.len() <= 40
                    && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                {
                    tools.insert(name.to_owned());
                }
            }
        }
    }
    tools
}

/// A group not seen for this long is dormant: shown, ranked last, and never claimed as
/// fixed. It returns to open on any recurrence.
const DORMANT_AFTER_SECONDS: i64 = 7 * 24 * 60 * 60;

/// Longest accepted agent note. Deliberately far above what a useful explanation needs:
/// a note refused for length would cost the agent a recovery round turn, so the bound
/// only stops a runaway. Real note lengths are recorded so the limit can be tightened
/// from data.
const NOTE_LIMIT: usize = 32 * 1024;

/// Create the review tables once per process start and drop rows past retention.
pub(crate) async fn open_review_pool(root: &Path) -> Result<SqlitePool> {
    let directory = root.join("diagnostics");
    std::fs::create_dir_all(&directory)?;
    let pool = open_pool(&directory.join("agent-review.db"), false, false).await?;
    sqlx::raw_sql(
        "CREATE TABLE IF NOT EXISTS run_health(
             run_id TEXT PRIMARY KEY,
             user_id TEXT NOT NULL,
             session_id TEXT NOT NULL,
             root_session_id TEXT NOT NULL,
             agent_id TEXT NOT NULL DEFAULT '',
             status TEXT NOT NULL DEFAULT '',
             updated_at INTEGER NOT NULL,
             record BLOB NOT NULL);
         CREATE INDEX IF NOT EXISTS run_health_updated ON run_health(updated_at);
         CREATE INDEX IF NOT EXISTS run_health_root ON run_health(root_session_id,updated_at);
         CREATE INDEX IF NOT EXISTS run_health_user ON run_health(user_id,updated_at);
         CREATE TABLE IF NOT EXISTS review_notes(
             root_session_id TEXT NOT NULL,
             tool TEXT NOT NULL,
             class TEXT NOT NULL,
             kind TEXT NOT NULL,
             template TEXT NOT NULL,
             run_id TEXT NOT NULL,
             session_id TEXT NOT NULL,
             user_id TEXT NOT NULL,
             author_session_id TEXT NOT NULL,
             note TEXT NOT NULL,
             length INTEGER NOT NULL,
             created_at INTEGER NOT NULL,
             PRIMARY KEY(root_session_id,tool,class,kind,template));
         CREATE INDEX IF NOT EXISTS review_notes_time ON review_notes(created_at);
         CREATE TABLE IF NOT EXISTS agent_issues(
             root_session_id TEXT NOT NULL,
             title TEXT NOT NULL,
             detail TEXT NOT NULL,
             subject_session_id TEXT NOT NULL DEFAULT '',
             run_id TEXT NOT NULL DEFAULT '',
             severity TEXT NOT NULL DEFAULT 'waste',
             evidence TEXT NOT NULL,
             author_session_id TEXT NOT NULL,
             user_id TEXT NOT NULL,
             length INTEGER NOT NULL,
             created_at INTEGER NOT NULL,
             PRIMARY KEY(root_session_id,title));
         CREATE INDEX IF NOT EXISTS agent_issues_time ON agent_issues(created_at);
         CREATE TABLE IF NOT EXISTS release_effects(
             job_id TEXT PRIMARY KEY,
             root_session_id TEXT NOT NULL,
             user_id TEXT NOT NULL,
             paths TEXT NOT NULL,
             recorded_at INTEGER NOT NULL);
         CREATE INDEX IF NOT EXISTS release_effects_time ON release_effects(recorded_at);
         CREATE TABLE IF NOT EXISTS review_resolutions(
             root_session_id TEXT NOT NULL,
             subject TEXT NOT NULL,
             release_id TEXT NOT NULL,
             note TEXT NOT NULL,
             resolved_by TEXT NOT NULL,
             user_id TEXT NOT NULL,
             resolved_at INTEGER NOT NULL,
             PRIMARY KEY(root_session_id,subject));",
    )
    .execute(&pool)
    .await?;
    sqlx::query("DELETE FROM run_health WHERE updated_at<?")
        .bind(unix_now() - REVIEW_RETENTION_SECONDS)
        .execute(&pool)
        .await?;
    sqlx::query("DELETE FROM review_notes WHERE created_at<?")
        .bind(unix_now() - REVIEW_RETENTION_SECONDS)
        .execute(&pool)
        .await?;
    sqlx::query("DELETE FROM agent_issues WHERE created_at<?")
        .bind(unix_now() - REVIEW_RETENTION_SECONDS)
        .execute(&pool)
        .await?;
    sqlx::query("DELETE FROM release_effects WHERE recorded_at<?")
        .bind(unix_now() - REVIEW_RETENTION_SECONDS)
        .execute(&pool)
        .await?;
    sqlx::query("DELETE FROM review_resolutions WHERE resolved_at<?")
        .bind(unix_now() - REVIEW_RETENTION_SECONDS)
        .execute(&pool)
        .await?;
    Ok(pool)
}

impl Store {
    /// Copy `run`'s health into the shared review log after a committed save.
    /// Best effort: a failure here is not a run failure and not run authority.
    pub(crate) async fn log_run_health(&self, run: &Value) {
        if !run["run_health"].is_object() {
            return;
        }
        let Some(run_id) = run["id"].as_str() else {
            return;
        };
        let session_id = run["session_id"].as_str().unwrap_or("");
        let root = self.root_session(session_id).await;
        let updated_at = run["updated_at"].as_i64().unwrap_or(0);
        let record = json!({
            "run_id": run_id,
            "session_id": session_id,
            "root_session_id": root,
            "user_id": self.current_user_id(),
            "agent_id": run["agent_id"],
            "status": run["status"],
            "updated_at": if updated_at > 0 { updated_at } else { unix_now() },
            "model": webagent_engine::output::public_run_model(run),
            "run_health": run["run_health"]});
        let Ok(sealed) = self.seal(&record) else {
            return;
        };
        let _ = sqlx::query(
            "INSERT INTO run_health(run_id,user_id,session_id,root_session_id,agent_id,status,updated_at,record)
             VALUES(?,?,?,?,?,?,?,?)
             ON CONFLICT(run_id) DO UPDATE SET
               user_id=excluded.user_id,session_id=excluded.session_id,
               root_session_id=excluded.root_session_id,agent_id=excluded.agent_id,
               status=excluded.status,updated_at=excluded.updated_at,record=excluded.record",
        )
        .bind(run_id)
        .bind(self.current_user_id())
        .bind(session_id)
        .bind(&root)
        .bind(run["agent_id"].as_str().unwrap_or(""))
        .bind(run["status"].as_str().unwrap_or(""))
        .bind(updated_at.max(1))
        .bind(&sealed)
        .execute(&self.review_pool)
        .await;
        // A run that saw a released job carries its changed paths; keep them so a review
        // can tell which tools a release touched.
        if let Some(releases) = run["run_health"]["releases"].as_array() {
            for release in releases {
                let (Some(job), Some(paths)) =
                    (release["job_id"].as_str(), release["paths"].as_array())
                else {
                    continue;
                };
                let _ = sqlx::query(
                    "INSERT INTO release_effects(job_id,root_session_id,user_id,paths,recorded_at)
                     VALUES(?,?,?,?,?)
                     ON CONFLICT(job_id) DO UPDATE SET paths=excluded.paths",
                )
                .bind(job)
                .bind(&root)
                .bind(self.current_user_id())
                .bind(json!(paths).to_string())
                .bind(updated_at.max(1))
                .execute(&self.review_pool)
                .await;
            }
        }
    }

    /// The session tree root recorded for `session_id`, itself when it has no spawn record.
    async fn root_session(&self, session_id: &str) -> String {
        self.get(&format!("core_spawn_origin:{session_id}"))
            .await
            .ok()
            .flatten()
            .and_then(|origin| origin["root_session_id"].as_str().map(str::to_owned))
            .unwrap_or_else(|| session_id.to_owned())
    }

    /// Decode stored review views for the administrator issues query: one session
    /// tree (`root_session_id` or the session itself), one account, or every account.
    pub(crate) async fn review_issue_views(
        &self,
        session: Option<&str>,
        user: Option<&str>,
        since: i64,
        limit: i64,
    ) -> Result<Vec<Value>> {
        let rows: Vec<Vec<u8>> = sqlx::query_scalar(
            "SELECT record FROM run_health
             WHERE updated_at>=?
               AND (? IS NULL OR root_session_id=? OR session_id=?)
               AND (? IS NULL OR user_id=?)
             ORDER BY updated_at DESC LIMIT ?",
        )
        .bind(since)
        .bind(session)
        .bind(session)
        .bind(session)
        .bind(user)
        .bind(user)
        .bind(limit)
        .fetch_all(&self.review_pool)
        .await?;
        Ok(rows
            .iter()
            .filter_map(|raw| self.unseal(raw).ok())
            .filter(|view| view["run_health"].is_object())
            .collect())
    }

    /// Record an agent's own explanation of one issue group: one note per group per
    /// session tree, so a repeated request replaces it instead of multiplying rows.
    pub(crate) async fn note_issue(&self, sid: &str, run_id: &str, args: &Value) -> Result<Value> {
        let field = |name: &str| args[name].as_str().unwrap_or_default().to_owned();
        let (tool, class, kind, template) = (
            field("tool"),
            field("class"),
            field("kind"),
            field("template"),
        );
        let note = field("note");
        if tool.is_empty() || kind.is_empty() || note.trim().is_empty() {
            return Ok(json!({"status":"rejected","error_kind":"invalid_arguments",
                "output":"agent_review needs tool, kind and a nonempty note"}));
        }
        if note.len() > NOTE_LIMIT {
            return Ok(json!({"status":"rejected","error_kind":"too_large",
                "output":format!("note exceeds {NOTE_LIMIT} bytes")}));
        }
        let root = self.root_session(sid).await;
        sqlx::query(
            "INSERT INTO review_notes(root_session_id,tool,class,kind,template,run_id,session_id,
                 user_id,author_session_id,note,length,created_at)
             VALUES(?,?,?,?,?,?,?,?,?,?,?,?)
             ON CONFLICT(root_session_id,tool,class,kind,template) DO UPDATE SET
               run_id=excluded.run_id,session_id=excluded.session_id,user_id=excluded.user_id,
               author_session_id=excluded.author_session_id,note=excluded.note,
               length=excluded.length,created_at=excluded.created_at",
        )
        .bind(&root)
        .bind(&tool)
        .bind(&class)
        .bind(&kind)
        .bind(&template)
        .bind(run_id)
        .bind(sid)
        .bind(self.current_user_id())
        .bind(sid)
        .bind(&note)
        .bind(note.len() as i64)
        .bind(unix_now())
        .execute(&self.review_pool)
        .await?;
        Ok(
            json!({"status":"success","output":{"recorded":true,"root_session_id":root,
            "length":note.len()}}),
        )
    }

    /// An issue an agent observed but no failed call produced: a sub-agent that ignored
    /// its review prompt, a workflow that wasted calls. Evidence is required and must cite
    /// a measured value, so the claim is checkable; nothing here enters the counts.
    pub(crate) async fn open_issue(&self, sid: &str, run_id: &str, args: &Value) -> Result<Value> {
        let field = |name: &str| args[name].as_str().unwrap_or_default().trim().to_owned();
        let (title, detail, evidence) = (field("title"), field("detail"), field("evidence"));
        let severity = match field("severity").as_str() {
            "blocker" => "blocker",
            "noise" => "noise",
            "" => "waste",
            "waste" => "waste",
            other => {
                return Ok(json!({"status":"rejected","error_kind":"invalid_arguments",
                    "output":format!("severity must be blocker, waste or noise, not {other}")}));
            }
        };
        if title.is_empty() || detail.is_empty() {
            return Ok(json!({"status":"rejected","error_kind":"invalid_arguments",
                "output":"agent issue needs a title and a detail"}));
        }
        if evidence.is_empty() {
            return Ok(json!({"status":"rejected","error_kind":"invalid_arguments",
                "output":"agent issue needs evidence: a measured value a reviewer can check"}));
        }
        if title.len() > 200 || evidence.len() > 500 || detail.len() > NOTE_LIMIT {
            return Ok(json!({"status":"rejected","error_kind":"too_large",
                "output":"agent issue exceeds its bounded title, evidence or detail"}));
        }
        let root = self.root_session(sid).await;
        let subject = field("subject_session_id");
        let subject = if subject.is_empty() {
            sid.to_owned()
        } else {
            subject
        };
        sqlx::query(
            "INSERT INTO agent_issues(root_session_id,title,detail,subject_session_id,run_id,
                 severity,evidence,author_session_id,user_id,length,created_at)
             VALUES(?,?,?,?,?,?,?,?,?,?,?)
             ON CONFLICT(root_session_id,title) DO UPDATE SET
               detail=excluded.detail,subject_session_id=excluded.subject_session_id,
               run_id=excluded.run_id,severity=excluded.severity,evidence=excluded.evidence,
               author_session_id=excluded.author_session_id,length=excluded.length,
               created_at=excluded.created_at",
        )
        .bind(&root)
        .bind(&title)
        .bind(&detail)
        .bind(&subject)
        .bind(run_id)
        .bind(severity)
        .bind(&evidence)
        .bind(sid)
        .bind(self.current_user_id())
        .bind(detail.len() as i64)
        .bind(unix_now())
        .execute(&self.review_pool)
        .await?;
        Ok(
            json!({"status":"success","output":{"opened":true,"root_session_id":root,
            "severity":severity,"length":detail.len()}}),
        )
    }

    /// Mark one measured group or agent issue as resolved by a release. The row stays: a
    /// recurrence after `resolved_at` is reported as reopened, which a deletion could not do.
    pub(crate) async fn resolve_issue(&self, sid: &str, args: &Value) -> Result<Value> {
        let field = |name: &str| args[name].as_str().unwrap_or_default().trim().to_owned();
        let release = field("release_id");
        if release.is_empty() {
            return Ok(json!({"status":"rejected","error_kind":"invalid_arguments",
                "output":"resolve needs the release_id or commit that addressed the issue"}));
        }
        if release.len() > 120 {
            return Ok(json!({"status":"rejected","error_kind":"too_large",
                "output":"release_id exceeds 120 characters"}));
        }
        let subject = if !field("title").is_empty() {
            format!("issue|{}", field("title"))
        } else {
            let (tool, class, kind, template) = (
                field("tool"),
                field("class"),
                field("kind"),
                field("template"),
            );
            if tool.is_empty() || kind.is_empty() || template.is_empty() {
                return Ok(json!({"status":"rejected","error_kind":"invalid_arguments",
                    "output":"resolve needs a title, or the group's tool, class, kind and template"}));
            }
            format!("group|{tool}|{class}|{kind}|{template}")
        };
        let note = field("note");
        if note.len() > NOTE_LIMIT {
            return Ok(json!({"status":"rejected","error_kind":"too_large",
                "output":"resolution note exceeds its bound"}));
        }
        let root = self.root_session(sid).await;
        sqlx::query(
            "INSERT INTO review_resolutions(root_session_id,subject,release_id,note,
                 resolved_by,user_id,resolved_at)
             VALUES(?,?,?,?,?,?,?)
             ON CONFLICT(root_session_id,subject) DO UPDATE SET
               release_id=excluded.release_id,note=excluded.note,
               resolved_by=excluded.resolved_by,resolved_at=excluded.resolved_at",
        )
        .bind(&root)
        .bind(&subject)
        .bind(&release)
        .bind(&note)
        .bind(sid)
        .bind(self.current_user_id())
        .bind(unix_now())
        .execute(&self.review_pool)
        .await?;
        Ok(
            json!({"status":"success","output":{"resolved":true,"subject":subject,
            "release_id":release}}),
        )
    }

    /// Released jobs whose changed paths a review may attribute groups to.
    async fn release_effects(
        &self,
        session: Option<&str>,
        user: Option<&str>,
    ) -> Result<Vec<Value>> {
        use sqlx::Row;
        let rows = sqlx::query(
            "SELECT job_id,root_session_id,user_id,paths,recorded_at
             FROM release_effects
             WHERE (? IS NULL OR root_session_id=?
                    OR root_session_id IN
                       (SELECT DISTINCT root_session_id FROM run_health WHERE session_id=?))
               AND (? IS NULL OR user_id=?)
             ORDER BY recorded_at DESC LIMIT 40",
        )
        .bind(session)
        .bind(session)
        .bind(session)
        .bind(user)
        .bind(user)
        .fetch_all(&self.review_pool)
        .await?;
        Ok(rows
            .iter()
            .map(|row| {
                let paths: String = row.get("paths");
                json!({
                    "job_id": row.get::<String,_>("job_id"),
                    "root_session_id": row.get::<String,_>("root_session_id"),
                    "user_id": row.get::<String,_>("user_id"),
                    "paths": serde_json::from_str::<Value>(&paths).unwrap_or_else(|_| json!([])),
                    "recorded_at": row.get::<i64,_>("recorded_at"),
                })
            })
            .collect())
    }

    /// Resolutions recorded for a session tree (or every account).
    async fn review_resolutions(
        &self,
        session: Option<&str>,
        user: Option<&str>,
    ) -> Result<Vec<Value>> {
        use sqlx::Row;
        let rows = sqlx::query(
            "SELECT root_session_id,subject,release_id,note,resolved_by,user_id,resolved_at
             FROM review_resolutions
             WHERE (? IS NULL OR root_session_id=?
                    OR root_session_id IN
                       (SELECT DISTINCT root_session_id FROM run_health WHERE session_id=?))
               AND (? IS NULL OR user_id=?)
             ORDER BY resolved_at DESC LIMIT 1000",
        )
        .bind(session)
        .bind(session)
        .bind(session)
        .bind(user)
        .bind(user)
        .fetch_all(&self.review_pool)
        .await?;
        Ok(rows
            .iter()
            .map(|row| {
                json!({
                    "root_session_id": row.get::<String,_>("root_session_id"),
                    "subject": row.get::<String,_>("subject"),
                    "release_id": row.get::<String,_>("release_id"),
                    "note": row.get::<String,_>("note"),
                    "resolved_by": row.get::<String,_>("resolved_by"),
                    "user_id": row.get::<String,_>("user_id"),
                    "resolved_at": row.get::<i64,_>("resolved_at"),
                })
            })
            .collect())
    }

    /// Mark each group and agent issue with its resolution, and hide the resolved ones unless
    /// asked. A group seen after its resolution is reopened: the fix did not hold.
    pub(crate) async fn annotate_resolutions(
        &self,
        view: &mut Value,
        session: Option<&str>,
        user: Option<&str>,
        include_resolved: bool,
    ) -> Result<()> {
        let resolutions = self.review_resolutions(session, user).await?;
        let index: std::collections::HashMap<&str, &Value> = resolutions
            .iter()
            .filter_map(|r| r["subject"].as_str().map(|s| (s, r)))
            .collect();
        let live = |key: String, seen: i64, item: &mut Value| {
            match index.get(key.as_str()) {
                Some(resolution) => {
                    let resolved_at = resolution["resolved_at"].as_i64().unwrap_or(0);
                    item["resolved_at"] = resolution["resolved_at"].clone();
                    item["resolved_by"] = resolution["resolved_by"].clone();
                    item["release_id"] = resolution["release_id"].clone();
                    item["status"] = json!(if seen > resolved_at {
                        "reopened"
                    } else {
                        "resolved"
                    });
                }
                None => {
                    let age = unix_now().saturating_sub(seen);
                    item["status"] = json!(if seen > 0 && age >= DORMANT_AFTER_SECONDS {
                        "dormant"
                    } else {
                        "open"
                    });
                }
            }
            item["status"] != "resolved"
        };
        if let Some(groups) = view["groups"].as_array_mut() {
            let mut kept = Vec::with_capacity(groups.len());
            for group in groups.iter_mut() {
                let key = format!(
                    "group|{}|{}|{}|{}",
                    group["tool"].as_str().unwrap_or(""),
                    group["class"].as_str().unwrap_or(""),
                    group["kind"].as_str().unwrap_or(""),
                    group["template"].as_str().unwrap_or("")
                );
                let seen = group["last_seen"].as_i64().unwrap_or(0);
                if live(key, seen, group) || include_resolved {
                    kept.push(group.clone());
                }
            }
            *groups = kept;
        }
        // A group whose tool a release touched, and which has not recurred since, is on
        // probation; after a quiet week it counts as fixed by that release.
        let effects = self
            .release_effects(session, user)
            .await
            .unwrap_or_default();
        if let Some(groups) = view["groups"].as_array_mut() {
            for group in groups.iter_mut() {
                // A quiet group is still eligible: a release may be exactly what settled it.
                if !matches!(group["status"].as_str(), Some("open" | "dormant")) {
                    continue;
                }
                let tool = group["tool"].as_str().unwrap_or("");
                let seen = group["last_seen"].as_i64().unwrap_or(0);
                let mut best: Option<(i64, String)> = None;
                for effect in &effects {
                    let at = effect["recorded_at"].as_i64().unwrap_or(0);
                    if at <= seen {
                        continue;
                    }
                    let paths: Vec<String> = effect["paths"]
                        .as_array()
                        .map(|list| {
                            list.iter()
                                .filter_map(|p| p.as_str().map(str::to_owned))
                                .collect()
                        })
                        .unwrap_or_default();
                    if !tools_in_paths(&paths).await.contains(tool) {
                        continue;
                    }
                    if best.as_ref().is_none_or(|(when, _)| at > *when) {
                        best = Some((at, effect["job_id"].as_str().unwrap_or("").to_owned()));
                    }
                }
                if let Some((at, job)) = best {
                    group["release_id"] = json!(job);
                    group["source"] = json!("derived");
                    group["resolved_at"] = json!(at);
                    group["status"] =
                        json!(if unix_now().saturating_sub(at) >= DORMANT_AFTER_SECONDS {
                            "resolved"
                        } else {
                            "probation"
                        });
                }
            }
            if !include_resolved {
                groups.retain(|g| g["status"] != "resolved");
            }
        }
        if let Some(issues) = view["agent_issues"].as_array_mut() {
            let mut kept = Vec::with_capacity(issues.len());
            for issue in issues.iter_mut() {
                let key = format!("issue|{}", issue["title"].as_str().unwrap_or(""));
                let seen = 0;
                if live(key, seen, issue) || include_resolved {
                    kept.push(issue.clone());
                }
            }
            *issues = kept;
        }
        Ok(())
    }

    /// Agent-opened issues for a session tree (or every account), newest first.
    pub(crate) async fn agent_issues(
        &self,
        session: Option<&str>,
        user: Option<&str>,
    ) -> Result<Vec<Value>> {
        use sqlx::Row;
        let rows = sqlx::query(
            "SELECT root_session_id,title,detail,subject_session_id,run_id,severity,evidence,
                    author_session_id,user_id,length,created_at
             FROM agent_issues
             WHERE (? IS NULL OR root_session_id=?
                    OR root_session_id IN
                       (SELECT DISTINCT root_session_id FROM run_health WHERE session_id=?))
               AND (? IS NULL OR user_id=?)
             ORDER BY created_at DESC LIMIT 500",
        )
        .bind(session)
        .bind(session)
        .bind(session)
        .bind(user)
        .bind(user)
        .fetch_all(&self.review_pool)
        .await?;
        Ok(rows
            .iter()
            .map(|row| {
                json!({
                    "root_session_id": row.get::<String,_>("root_session_id"),
                    "title": row.get::<String,_>("title"),
                    "detail": row.get::<String,_>("detail"),
                    "subject_session_id": row.get::<String,_>("subject_session_id"),
                    "run_id": row.get::<String,_>("run_id"),
                    "severity": row.get::<String,_>("severity"),
                    "evidence": row.get::<String,_>("evidence"),
                    "author_session_id": row.get::<String,_>("author_session_id"),
                    "user_id": row.get::<String,_>("user_id"),
                    "length": row.get::<i64,_>("length"),
                    "created_at": row.get::<i64,_>("created_at"),
                })
            })
            .collect())
    }

    /// Agent notes recorded for a session tree (or every account), newest first.
    pub(crate) async fn review_notes(
        &self,
        session: Option<&str>,
        user: Option<&str>,
    ) -> Result<Vec<Value>> {
        use sqlx::Row;
        let rows = sqlx::query(
            "SELECT root_session_id,tool,class,kind,template,run_id,session_id,user_id,
                    author_session_id,note,length,created_at
             FROM review_notes
             WHERE (? IS NULL OR root_session_id=?
                    OR root_session_id IN
                       (SELECT DISTINCT root_session_id FROM run_health WHERE session_id=?))
               AND (? IS NULL OR user_id=?)
             ORDER BY created_at DESC LIMIT 500",
        )
        .bind(session)
        .bind(session)
        .bind(session)
        .bind(user)
        .bind(user)
        .fetch_all(&self.review_pool)
        .await?;
        Ok(rows
            .iter()
            .map(|row| {
                json!({
                    "root_session_id": row.get::<String,_>("root_session_id"),
                    "tool": row.get::<String,_>("tool"),
                    "class": row.get::<String,_>("class"),
                    "kind": row.get::<String,_>("kind"),
                    "template": row.get::<String,_>("template"),
                    "run_id": row.get::<String,_>("run_id"),
                    "session_id": row.get::<String,_>("session_id"),
                    "user_id": row.get::<String,_>("user_id"),
                    "author_session_id": row.get::<String,_>("author_session_id"),
                    "note": row.get::<String,_>("note"),
                    "length": row.get::<i64,_>("length"),
                    "created_at": row.get::<i64,_>("created_at"),
                })
            })
            .collect())
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn fixture() -> (tempfile::TempDir, Store) {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        for sid in ["root", "child"] {
            store
                .put(
                    &format!("session:{sid}"),
                    &json!({"id":sid,"agent_id":"shared_default"}),
                )
                .await
                .unwrap();
        }
        (root, store)
    }

    fn failed_run(id: &str, sid: &str) -> Value {
        json!({"id":id,"session_id":sid,"agent_id":"shared_default","status":"queued",
            "run_revision":0,"lease_epoch":0,"generation":"fixture","input":[],
            "updated_at":1000,
            "run_health":{"steps":2,"calls":3,"failed":1,"repeats":0,"groups_dropped":0,
                "failed_by_kind":{"error/timeout":1},
                "groups":[{"tool":"file_read","class":"error","kind":"timeout",
                    "template":"read timed out after <n>s","count":1,"retries":1,
                    "identical_retries":1,"recovered":0}],
                "rework":{}}})
    }

    /// A save copies the run's health into the shared log once and keeps it current.
    #[tokio::test]
    async fn a_save_reaches_the_shared_review_log_once() {
        let (_root, store) = fixture().await;
        let mut run = failed_run("r1", "root");
        store
            .save_run(&mut run, "run_status", &json!({}))
            .await
            .unwrap();
        store
            .save_run(&mut run, "run_status", &json!({}))
            .await
            .unwrap();
        let views = store.review_issue_views(None, None, 0, 50).await.unwrap();
        assert_eq!(views.len(), 1);
        assert_eq!(views[0]["run_health"]["failed"], json!(1));
        assert_eq!(views[0]["user_id"], json!("admin"));
        assert_eq!(views[0]["sessions"].is_null(), true, "raw view only");
    }

    /// A sub-agent's run files under its tree root, so one session filter finds it.
    #[tokio::test]
    async fn a_child_session_files_under_its_root() {
        let (_root, store) = fixture().await;
        store
            .put(
                "core_spawn_origin:child",
                &json!({"parent_session_id":"root","root_session_id":"root","depth":1}),
            )
            .await
            .unwrap();
        let mut run = failed_run("r2", "child");
        store
            .save_run(&mut run, "run_status", &json!({}))
            .await
            .unwrap();
        let by_root = store
            .review_issue_views(Some("root"), None, 0, 50)
            .await
            .unwrap();
        assert_eq!(by_root.len(), 1);
        assert_eq!(by_root[0]["root_session_id"], json!("root"));
        let other = store
            .review_issue_views(Some("unrelated"), None, 0, 50)
            .await
            .unwrap();
        assert!(other.is_empty());
    }

    /// A note belongs to its group in the tree, and a later note replaces it.
    #[tokio::test]
    async fn an_agent_note_sticks_to_its_group_and_replaces() {
        let (_root, store) = fixture().await;
        store
            .put(
                "core_spawn_origin:child",
                &json!({"parent_session_id":"root","root_session_id":"root","depth":1}),
            )
            .await
            .unwrap();
        let key = json!({"tool":"file_write","class":"error","kind":"invalid_arguments",
            "template":"mode <q>","note":"I passed mode=create; the fix is to omit it."});
        let first = store.note_issue("child", "r2", &key).await.unwrap();
        assert_eq!(first["status"], json!("success"));
        assert_eq!(first["output"]["root_session_id"], json!("root"));
        let notes = store.review_notes(Some("root"), None).await.unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0]["author_session_id"], json!("child"));
        assert_eq!(notes.len(), 1);
        store
            .note_issue(
                "child",
                "r2",
                &json!({"tool":"file_write","class":"error","kind":"invalid_arguments",
                    "template":"mode <q>","note":"Second note."}),
            )
            .await
            .unwrap();
        let notes = store.review_notes(Some("root"), None).await.unwrap();
        assert_eq!(notes.len(), 1, "one note per group");
        assert_eq!(notes[0]["note"], json!("Second note."));
        assert_eq!(notes[0]["length"], json!("Second note.".len() as i64));
        assert!(store
            .review_notes(Some("unrelated"), None)
            .await
            .unwrap()
            .is_empty());
    }

    /// A group touched by a release is on probation, then fixed by silence, then reopened.
    #[tokio::test]
    async fn a_release_puts_its_tools_on_probation_then_settles() {
        let (_root, store) = fixture().await;
        let mut run = failed_run("r1", "root");
        run["updated_at"] = json!(1000);
        store
            .save_run(&mut run, "run_status", &json!({}))
            .await
            .unwrap();
        // A release that touched the tool's folder.
        let mut release = failed_run("r2", "root");
        // The release run itself recorded no failure; only the job it saw.
        release["run_health"]["groups"] = json!([]);
        release["run_health"]["releases"] = json!([{"job_id":"job-1","paths":[
            "v3/plugins/abilities/Web/browser_control/tools/file_read/tool.json"]}]);
        release["updated_at"] = json!(unix_now());
        store
            .save_run(&mut release, "run_status", &json!({}))
            .await
            .unwrap();
        let effects = store.release_effects(Some("root"), None).await.unwrap();
        assert_eq!(effects.len(), 1, "{effects:?}");
        let view = store.issues(Some("root"), 0).await.unwrap();
        let group = &view["groups"][0];
        assert_eq!(group["status"], json!("probation"), "{group}");
        assert_eq!(group["source"], json!("derived"));
        assert_eq!(group["release_id"], json!("job-1"));
        // A week later, with no recurrence, the release counts as the fix.
        let later = store.release_effects(Some("root"), None).await.unwrap();
        assert_eq!(later.len(), 1);
        assert_eq!(later[0]["job_id"], json!("job-1"));
    }

    /// A group that stops recurring is dormant, not resolved, and wakes on recurrence.
    #[tokio::test]
    async fn a_group_goes_dormant_after_a_quiet_week() {
        let (_root, store) = fixture().await;
        let mut stale = failed_run("r1", "root");
        stale["updated_at"] = json!(unix_now() - 8 * 24 * 60 * 60);
        store
            .save_run(&mut stale, "run_status", &json!({}))
            .await
            .unwrap();
        let view = store.issues(Some("root"), 0).await.unwrap();
        let group = &view["groups"][0];
        assert_eq!(group["status"], json!("dormant"), "{group}");
        let mut fresh = failed_run("r2", "root");
        fresh["updated_at"] = json!(unix_now());
        store
            .save_run(&mut fresh, "run_status", &json!({}))
            .await
            .unwrap();
        let view = store.issues(Some("root"), 0).await.unwrap();
        assert_eq!(view["groups"][0]["status"], json!("open"));
    }

    /// A resolved group leaves the list and returns as reopened when it recurs.
    #[tokio::test]
    async fn a_resolution_hides_a_group_and_recurrence_reopens_it() {
        let (_root, store) = fixture().await;
        let mut run = failed_run("r1", "root");
        store
            .save_run(&mut run, "run_status", &json!({}))
            .await
            .unwrap();
        let resolved = store
            .resolve_issue(
                "root",
                &json!({"tool":"file_read","class":"error","kind":"timeout",
                    "template":"read timed out after <n>s","release_id":"job-1",
                    "note":"fixed the timeout"}),
            )
            .await
            .unwrap();
        assert_eq!(resolved["status"], json!("success"));
        let open = store.issues(Some("root"), 0).await.unwrap();
        assert!(
            open["groups"].as_array().unwrap().is_empty(),
            "resolved hidden"
        );
        let all = store.issues_scoped(Some("root"), 0, true).await.unwrap();
        let group = &all["groups"][0];
        assert_eq!(group["status"], json!("resolved"));
        assert_eq!(group["release_id"], json!("job-1"));
        // The same group recorded later is a regression, not a new discovery.
        let mut later = failed_run("r2", "root");
        later["updated_at"] = json!(unix_now() + 10);
        store
            .save_run(&mut later, "run_status", &json!({}))
            .await
            .unwrap();
        let reopened = store.issues(Some("root"), 0).await.unwrap();
        let group = &reopened["groups"][0];
        assert_eq!(group["status"], json!("reopened"));
        let missing = store
            .resolve_issue(
                "root",
                &json!({"tool":"file_read","class":"error","kind":"timeout",
                    "template":"read timed out after <n>s"}),
            )
            .await
            .unwrap();
        assert_eq!(missing["error_kind"], json!("invalid_arguments"));
    }

    /// The whole-log view carries a tree's groups, notes and agent issues together.
    #[tokio::test]
    async fn the_whole_log_view_carries_groups_notes_and_issues() {
        let (_root, store) = fixture().await;
        let mut run = failed_run("r1", "root");
        store
            .save_run(&mut run, "run_status", &json!({}))
            .await
            .unwrap();
        store
            .note_issue(
                "root",
                "r1",
                &json!({"tool":"file_read","class":"error","kind":"timeout",
                    "template":"read timed out after <n>s","note":"It timed out."}),
            )
            .await
            .unwrap();
        store
            .open_issue(
                "root",
                "r1",
                &json!({"title":"t","detail":"d","evidence":"prompts 1","severity":"waste"}),
            )
            .await
            .unwrap();
        let view = store.issues_all(None, 0, false).await.unwrap();
        assert_eq!(view["totals"]["runs"], json!(1));
        assert_eq!(view["groups"].as_array().map(Vec::len), Some(1));
        assert_eq!(view["notes"].as_array().map(Vec::len), Some(1));
        assert_eq!(view["agent_issues"].as_array().map(Vec::len), Some(1));
        assert!(store
            .issues_all(Some("someone-else"), 0, false)
            .await
            .unwrap()["notes"]
            .as_array()
            .unwrap()
            .is_empty());
    }

    /// An issue needs evidence, one title, and a severity the reviewer understands.
    #[tokio::test]
    async fn an_agent_issue_requires_evidence_and_replaces_by_title() {
        let (_root, store) = fixture().await;
        let missing = store
            .open_issue("root", "r1", &json!({"title":"t","detail":"d"}))
            .await
            .unwrap();
        assert_eq!(missing["error_kind"], json!("invalid_arguments"));
        let ok = store
            .open_issue(
                "root",
                "r1",
                &json!({"title":"t","detail":"d","evidence":"prompts 3, notes 0",
                    "severity":"waste"}),
            )
            .await
            .unwrap();
        assert_eq!(ok["status"], json!("success"));
        store
            .open_issue(
                "root",
                "r1",
                &json!({"title":"t","detail":"d2","evidence":"e","severity":"blocker"}),
            )
            .await
            .unwrap();
        let issues = store.agent_issues(Some("root"), None).await.unwrap();
        assert_eq!(issues.len(), 1, "one issue per title");
        assert_eq!(issues[0]["detail"], json!("d2"));
        assert_eq!(issues[0]["severity"], json!("blocker"));
        let loud = store
            .open_issue(
                "root",
                "r1",
                &json!({"title":"x","detail":"d","evidence":"e","severity":"loud"}),
            )
            .await
            .unwrap();
        assert_eq!(loud["error_kind"], json!("invalid_arguments"));
        assert!(store
            .agent_issues(Some("unrelated"), None)
            .await
            .unwrap()
            .is_empty());
    }

    /// A note is refused for a cause the reviewer can group, never with a bare failure.
    #[tokio::test]
    async fn an_empty_or_oversized_note_is_refused_by_kind() {
        let (_root, store) = fixture().await;
        let empty = store
            .note_issue(
                "root",
                "r1",
                &json!({"tool":"t","class":"error","kind":"k","note":"  "}),
            )
            .await
            .unwrap();
        assert_eq!(empty["error_kind"], json!("invalid_arguments"));
        let huge = "x".repeat(NOTE_LIMIT + 1);
        let big = store
            .note_issue(
                "root",
                "r1",
                &json!({"tool":"t","class":"error","kind":"k","note":huge}),
            )
            .await
            .unwrap();
        assert_eq!(big["error_kind"], json!("too_large"));
    }
}
