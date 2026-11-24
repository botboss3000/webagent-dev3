//! Read-only private projection. Public chat is never hydrated from this journal.
use super::*;
use std::collections::{HashMap, HashSet};
use webagent_engine::runtime::health::result_status;
fn boundary(row: &SqliteRow) -> String {
    row.get::<Option<String>, _>("boundary_item_id")
        .unwrap_or_else(|| format!("user:{}", row.get::<String, _>("run_id")))
}
/// The private control that publishes working bubbles; its calls stay out of activity groups.
const PUBLISH_CONTROL: &str = "publish_main_message";
/// The tool that starts a sub agent; its calls count as sub agents, not tools.
const SUBAGENT_START: &str = "start_spawn";
/// Interaction steps a working bubble or live row repeats word for word, read from its chat ID:
/// `working:<run>:auto:<turn>:<ordinal>` (automatic promotion) or
/// `working:<run>:<turn>:<call>:echo:<o1>-<o2>` (`publish_main_message` without text).
fn echoed_steps(id: &str) -> Vec<(String, String)> {
    // A live row shows one model step: `live:<run>:<turn>:<ordinal>`.
    if let Some(rest) = id.strip_prefix("live:") {
        return match rest.split(':').collect::<Vec<_>>().as_slice() {
            [run, turn, ordinal] => vec![(run.to_string(), format!("model:{turn}:{ordinal}"))],
            _ => Vec::new(),
        };
    }
    let parts = id
        .strip_prefix("working:")
        .map(|rest| rest.split(':').collect::<Vec<_>>())
        .unwrap_or_default();
    match parts.as_slice() {
        [run, "auto", turn, ordinal] => vec![(run.to_string(), format!("model:{turn}:{ordinal}"))],
        [run, turn, _, "echo", ordinals] => ordinals
            .split('-')
            .map(|ordinal| (run.to_string(), format!("model:{turn}:{ordinal}")))
            .collect(),
        _ => Vec::new(),
    }
}
/// Every (run, step) whose message the chat already shows: messages a working bubble repeats
/// and each completed run's final reply (the messages of its last model turn), so activity
/// groups hold only messages the user has not seen. Final replies are read only among the
/// interactions after `after`, the rows the caller is about to count or show.
async fn in_chat_steps<'e, E: sqlx::SqliteExecutor<'e>>(
    executor: E,
    sid: &str,
    after: i64,
) -> Result<HashSet<(String, String)>> {
    let rows = sqlx::query(
        "SELECT id AS a,NULL AS b FROM chat WHERE session_id=? AND (id LIKE 'working:%' OR id LIKE 'live:%') \
         UNION ALL SELECT i.run_id,i.step FROM interactions i JOIN chat c ON c.terminal_run_id=i.run_id \
         WHERE c.session_id=? AND i.session_id=? AND i.step LIKE 'model:%' AND i.seq>?",
    )
    .bind(sid)
    .bind(sid)
    .bind(sid)
    .bind(after)
    .fetch_all(executor)
    .await?;
    let turn = |step: &str| {
        step.split(':')
            .nth(1)
            .and_then(|turn| turn.parse::<u64>().ok())
    };
    let mut steps = HashSet::new();
    let mut finals: HashMap<String, (u64, Vec<String>)> = HashMap::new();
    for row in &rows {
        let a = row.get::<String, _>("a");
        let Some(step) = row.get::<Option<String>, _>("b") else {
            steps.extend(echoed_steps(&a));
            continue;
        };
        let Some(number) = turn(&step) else { continue };
        let last = finals.entry(a).or_insert((number, Vec::new()));
        if number > last.0 {
            *last = (number, Vec::new());
        }
        if number == last.0 {
            last.1.push(step);
        }
    }
    for (run, (_, last)) in finals {
        steps.extend(last.into_iter().map(|step| (run.clone(), step)));
    }
    Ok(steps)
}
/// One activity group: a run's interactions under one chat anchor, with its stored summary line
/// once the group has been sealed.
struct Group {
    run: String,
    recorded: Option<String>,
    anchor: String,
    first: i64,
    last: i64,
    stored: Option<Value>,
}
fn group(r: &SqliteRow, stored: Option<Value>) -> Group {
    Group {
        run: r.get("run_id"),
        recorded: r.get("boundary_item_id"),
        anchor: boundary(r),
        first: r.get("first_seq"),
        last: r.get("last_seq"),
        stored,
    }
}
/// Every activity group of a session in order, with the stored-through mark it was read at and
/// the mark saved in the database. Groups up to the mark come from stored summaries and only
/// rows after it are scanned. A run that wrote again after its groups were stored (a retry or
/// resume) moves the mark back before them, so they are counted again.
async fn activity_groups(
    conn: &mut sqlx::SqliteConnection,
    sid: &str,
) -> Result<(i64, i64, Vec<Group>)> {
    let saved: Option<i64> =
        sqlx::query_scalar("SELECT through_seq FROM activity_summary_marks WHERE session_id=?")
            .bind(sid)
            .fetch_optional(&mut *conn)
            .await?;
    let saved = saved.unwrap_or(0);
    let mut mark = saved;
    loop {
        let back: Option<i64> = sqlx::query_scalar("SELECT MIN(first_seq) FROM activity_summaries WHERE session_id=? AND first_seq<=? AND (last_seq>? OR run_id IN (SELECT run_id FROM interactions WHERE session_id=? AND seq>?))")
            .bind(sid).bind(mark).bind(mark).bind(sid).bind(mark).fetch_one(&mut *conn).await?;
        let Some(first) = back else { break };
        mark = first - 1;
    }
    let stored = sqlx::query("SELECT run_id,boundary_item_id,first_seq,last_seq,summary FROM activity_summaries WHERE session_id=? AND last_seq<=? ORDER BY first_seq")
        .bind(sid).bind(mark).fetch_all(&mut *conn).await?;
    let mut groups = Vec::new();
    for r in &stored {
        groups.push(group(r, Some(serde_json::from_str(r.get("summary"))?)));
    }
    let scanned = sqlx::query("SELECT i.run_id,b.boundary_item_id,MIN(i.seq) first_seq,MAX(i.seq) last_seq FROM interactions i LEFT JOIN activity_boundaries b ON b.interaction_seq=i.seq WHERE i.session_id=? AND i.seq>? GROUP BY i.run_id,b.boundary_item_id ORDER BY MIN(i.seq)")
        .bind(sid).bind(mark).fetch_all(&mut *conn).await?;
    groups.extend(scanned.iter().map(|r| group(r, None)));
    Ok((mark, saved, groups))
}
/// How far stored summaries can reach after this request: past every scanned group whose run
/// has ended (completed, failed or cancelled), stopping before the first group whose run is
/// still open and before any group that overlaps one.
async fn sealed_through(
    conn: &mut sqlx::SqliteConnection,
    mark: i64,
    groups: &[Group],
) -> Result<i64> {
    let mut ended = HashMap::new();
    for g in groups.iter().filter(|g| g.stored.is_none()) {
        if ended.contains_key(g.run.as_str()) {
            continue;
        }
        let status: Option<String> = sqlx::query_scalar("SELECT status FROM run_index WHERE id=?")
            .bind(&g.run)
            .fetch_optional(&mut *conn)
            .await?;
        let done = matches!(
            status.as_deref(),
            Some("completed" | "failed" | "cancelled")
        );
        ended.insert(g.run.as_str(), done);
    }
    let scanned = || groups.iter().filter(|g| g.stored.is_none());
    let mut through = scanned().map(|g| g.last).max().unwrap_or(mark);
    while let Some(first) = scanned()
        .filter(|g| g.first <= through && (!ended[g.run.as_str()] || g.last > through))
        .map(|g| g.first)
        .min()
    {
        through = first - 1;
    }
    Ok(through)
}
/// Store newly sealed group summaries and move the session's stored-through mark. Stored rows
/// above the mark this request read at (left by a run that wrote again) are dropped first.
async fn seal_groups(
    conn: &mut sqlx::SqliteConnection,
    sid: &str,
    mark: i64,
    through: i64,
    sealed: &[(&Group, Value)],
) -> Result<()> {
    sqlx::query("DELETE FROM activity_summaries WHERE session_id=? AND last_seq>?")
        .bind(sid)
        .bind(mark)
        .execute(&mut *conn)
        .await?;
    for (g, summary) in sealed {
        sqlx::query("INSERT OR REPLACE INTO activity_summaries(run_id,anchor,session_id,boundary_item_id,first_seq,last_seq,summary) VALUES(?,?,?,?,?,?,?)")
            .bind(&g.run).bind(&g.anchor).bind(sid).bind(&g.recorded).bind(g.first).bind(g.last).bind(summary.to_string()).execute(&mut *conn).await?;
    }
    sqlx::query("INSERT INTO activity_summary_marks(session_id,through_seq) VALUES(?,?) ON CONFLICT(session_id) DO UPDATE SET through_seq=excluded.through_seq")
        .bind(sid).bind(through).execute(&mut *conn).await?;
    Ok(())
}
fn event(
    row: &SqliteRow,
    store: &Store,
    sid: &str,
    in_chat: &HashSet<(String, String)>,
) -> Result<Value> {
    let seq = row.get::<i64, _>("seq");
    let run = row.get::<String, _>("run_id");
    let step = row.get::<String, _>("step");
    // The model turn that wrote this item (`model:<turn>:<ordinal>`); tool results have none.
    let turn = step
        .strip_prefix("model:")
        .and_then(|rest| rest.split(':').next())
        .and_then(|turn| turn.parse::<u64>().ok());
    let shown = in_chat.contains(&(run.clone(), step));
    let item = store.unseal(&row.get::<Vec<u8>, _>("item"))?;
    let (kind, payload) = match item["type"].as_str().unwrap_or("") {
        "function_call" => (
            "tool_call",
            json!({"tool_call_id":item["call_id"],"tool":item["name"],"arguments":item["arguments"]}),
        ),
        "function_call_output" => ("tool_result", {
            let (status, error_kind) = result_status(&item["output"]);
            let mut payload =
                json!({"tool_call_id":item["call_id"],"output":item["output"],"status":status});
            if let Some(kind) = error_kind {
                payload["error_kind"] = json!(kind);
            }
            payload
        }),
        "message" => {
            let text = if let Some(text) = item["content"].as_str() {
                text.to_owned()
            } else {
                item["content"]
                    .as_array()
                    .map(|parts| {
                        parts
                            .iter()
                            .filter_map(|p| p["text"].as_str())
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default()
            };
            (
                "agent_step_end",
                json!({"asst_id":format!("interaction:{seq}"),"content":text,"in_chat":shown}),
            )
        }
        "compaction_call" => (
            "compaction_call",
            json!({"model":item["model"],"input_bytes":item["input_bytes"],"reason":item["reason"],
                "mode":item["mode"],"summarizer":item["summarizer"],"fork":item["fork"]}),
        ),
        "compaction_result" => (
            "compaction_result",
            json!({"summary":item["summary"],"error":item["error"]}),
        ),
        "closing_reminder" => ("closing_reminder", json!({})),
        "model_usage" => (
            "model_usage",
            json!({"input_tokens":item["input_tokens"],"cached_input_tokens":item["cached_input_tokens"],
                "cache_write_tokens":item["cache_write_tokens"],"matched_bytes":item["matched_bytes"],
                "sent_bytes":item["sent_bytes"],"first_change":item["first_change"],
                "first_change_entry":item["first_change_entry"]}),
        ),
        "closing_status" => (
            "closing_status",
            json!({"attempt":item["attempt"],"of":item["of"],"model":item["model"],
                "flag":item["flag"],"notification":item["notification"],
                "error":item["error"],"fallback":item["fallback"]}),
        ),
        "retry_scheduled" => (
            "retry_scheduled",
            json!({"attempt":item["attempt"],"of":item["of"],"wait_seconds":item["wait_seconds"],
                "error":item["error"],"backup":item["backup"],"auto_resume":item["auto_resume"],
                "from_model":item["from_model"],"to_model":item["to_model"],"reason":item["reason"]}),
        ),
        // A tool call a saved approval let through without asking (engine `approve_by_saved`).
        "saved_approval" => (
            "saved_approval",
            json!({"tool":item["tool"],"reach":item["reach"],"match":item["match"]}),
        ),
        "model_unreachable" => (
            "model_unreachable",
            json!({"model":item["model"],"provider":item["provider"],"detail":item["detail"]}),
        ),
        _ => ("diagnostic", item),
    };
    let execution_metadata = row
        .get::<Option<Vec<u8>>, _>("execution_metadata")
        .map(|bytes| store.unseal(&bytes))
        .transpose()?;
    Ok(
        json!({"execution_metadata":execution_metadata,"seq":seq,"event_id":format!("interaction:{seq}"),"session_id":sid,"run_id":run,"turn":turn,"kind":kind,"payload":payload,
        "created_at":row.get::<i64,_>("created_at")*1000,"boundary_quality":if row.get::<Option<String>,_>("boundary_item_id").is_some(){"recorded"}else{"legacy_unverified"}}),
    )
}
impl Store {
    /// Resolve one immutable execution inside the already-authorized session, rejecting ambiguous IDs.
    pub async fn tool_result_origin(
        &self,
        sid: &str,
        call_id: &str,
    ) -> Result<Option<(String, Value)>> {
        let rows=sqlx::query("SELECT run_id,item FROM interactions WHERE session_id=? AND step=? ORDER BY seq DESC LIMIT 2")
            .bind(sid).bind(format!("tool:{call_id}")).fetch_all(&self.pool).await?;
        if rows.len() > 1 {
            anyhow::bail!("Ambiguous tool-result origin");
        }
        rows.into_iter()
            .next()
            .map(|row| {
                Ok((
                    row.get::<String, _>("run_id"),
                    self.unseal(&row.get::<Vec<u8>, _>("item"))?,
                ))
            })
            .transpose()
    }

    /// Exact encrypted interaction lookup for bounded current-run result recall.
    pub async fn tool_result(&self, sid: &str, rid: &str, call_id: &str) -> Result<Option<Value>> {
        let row:Option<Vec<u8>>=sqlx::query_scalar(
            "SELECT item FROM interactions WHERE session_id=? AND run_id=? AND step=? ORDER BY seq DESC LIMIT 1")
            .bind(sid).bind(rid).bind(format!("tool:{call_id}"))
            .fetch_optional(&self.pool).await?;
        row.map(|bytes| self.unseal(&bytes)).transpose()
    }
    pub async fn tool_call_record(
        &self,
        sid: &str,
        rid: &str,
        call_id: &str,
    ) -> Result<Option<Value>> {
        let rows:Vec<Vec<u8>>=sqlx::query_scalar(
            "SELECT item FROM interactions WHERE session_id=? AND run_id=? AND step LIKE 'model:%' ORDER BY seq DESC LIMIT 2048")
            .bind(sid).bind(rid).fetch_all(&self.pool).await?;
        for bytes in rows {
            let item = self.unseal(&bytes)?;
            if item["type"] == "function_call" && item["call_id"] == call_id {
                return Ok(Some(item));
            }
        }
        Ok(None)
    }
    pub async fn tool_call_name(
        &self,
        sid: &str,
        rid: &str,
        call_id: &str,
    ) -> Result<Option<String>> {
        let rows:Vec<Vec<u8>>=sqlx::query_scalar(
            "SELECT item FROM interactions WHERE session_id=? AND run_id=? AND step LIKE 'model:%' ORDER BY seq DESC LIMIT 2048")
            .bind(sid).bind(rid).fetch_all(&self.pool).await?;
        for bytes in rows {
            let item = self.unseal(&bytes)?;
            if item["type"] == "function_call" && item["call_id"] == call_id {
                return Ok(item["name"].as_str().map(str::to_owned));
            }
        }
        Ok(None)
    }
    /// Count model-issued tool calls in this user's sessions for one agent.
    /// Session ownership stays with the user store; the catalog receives counts,
    /// never encrypted interaction content or another user's activity.
    pub async fn tool_call_counts(&self, agent_id: &str) -> Result<HashMap<String, u64>> {
        let sessions = sqlx::query("SELECT value FROM docs WHERE key LIKE 'session:%'")
            .fetch_all(&self.pool)
            .await?;
        let mut ids = HashSet::new();
        for row in sessions {
            let session = self.unseal(&row.get::<Vec<u8>, _>("value"))?;
            if session["agent_id"].as_str().unwrap_or("shared_default") == agent_id {
                if let Some(id) = session["id"].as_str() {
                    ids.insert(id.to_owned());
                }
            }
        }
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let rows = sqlx::query(
            "SELECT session_id,item FROM interactions WHERE step LIKE 'model:%' ORDER BY seq",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut counts = HashMap::new();
        for row in rows {
            if !ids.contains(row.get::<&str, _>("session_id")) {
                continue;
            }
            let item = self.unseal(&row.get::<Vec<u8>, _>("item"))?;
            if item["type"] == "function_call" {
                if let Some(name) = item["name"].as_str() {
                    *counts.entry(name.to_owned()).or_insert(0) += 1;
                }
            }
        }
        Ok(counts)
    }
    /// Page only one caller-owned run's encrypted interactions. The API checks
    /// administrator and session authority before exposing this private log.
    pub async fn private_run_activity(&self, sid: &str, rid: &str, after: i64) -> Result<Value> {
        let rows=sqlx::query("SELECT i.seq,i.run_id,i.step,i.item,i.execution_metadata,i.created_at,b.boundary_item_id FROM interactions i LEFT JOIN activity_boundaries b ON b.interaction_seq=i.seq WHERE i.session_id=? AND i.run_id=? AND i.seq>? ORDER BY i.seq LIMIT 101")
            .bind(sid).bind(rid).bind(after).fetch_all(&self.pool).await?;
        let more = rows.len() > 100;
        let page = &rows[..rows.len().min(100)];
        let in_chat = in_chat_steps(&self.pool, sid, after).await?;
        let events = page
            .iter()
            .map(|row| event(row, self, sid, &in_chat))
            .collect::<Result<Vec<_>>>()?;
        let cursor = page
            .last()
            .map(|row| row.get::<i64, _>("seq"))
            .unwrap_or(after);
        Ok(json!({"session_id":sid,"run_id":rid,"events":events,
            "cursor":cursor,"has_more":more}))
    }
    /// Summary-line data for activity groups, without their events: counts plus the flags the
    /// transcript needs to label each group. `boundaries` selects the groups next to drawn
    /// messages; without it, only groups with events after the `after` cursor are returned.
    /// Groups whose run has ended are stored once and read back; only rows after the stored
    /// groups are scanned, and only selected or newly stored groups are decrypted.
    pub async fn private_activity_summary(
        &self,
        sid: &str,
        after: i64,
        boundaries: Option<&[String]>,
    ) -> Result<Value> {
        let mut tx = self.pool.begin().await?;
        let (mark, saved, groups) = activity_groups(&mut tx, sid).await?;
        let latest: Option<String> =
            sqlx::query_scalar("SELECT id FROM chat WHERE session_id=? ORDER BY seq DESC LIMIT 1")
                .bind(sid)
                .fetch_optional(&mut *tx)
                .await?;
        let cursor = groups.iter().map(|g| g.last).max().unwrap_or(0);
        let mut last_of_run = HashMap::new();
        for (index, g) in groups.iter().enumerate() {
            last_of_run.insert(g.run.as_str(), index);
        }
        let through = sealed_through(&mut tx, mark, &groups).await?;
        let wanted = |g: &Group| match boundaries {
            Some(list) => list.contains(&g.anchor),
            None => g.last > after,
        };
        let flagged = |mut summary: Value, index: usize, g: &Group| {
            summary["closed"] = json!(latest.as_deref() != Some(g.anchor.as_str()));
            summary["latest"] = json!(index + 1 == groups.len());
            summary["last_of_run"] = json!(last_of_run.get(g.run.as_str()) == Some(&index));
            summary
        };
        // Only groups after the mark are counted here, so only their rows are checked against
        // the messages the chat shows.
        let counting = groups
            .iter()
            .any(|g| g.stored.is_none() && (wanted(g) || g.last <= through));
        let in_chat = if counting {
            in_chat_steps(&mut *tx, sid, mark).await?
        } else {
            HashSet::new()
        };
        let (mut segments, mut sealed) = (Vec::new(), Vec::new());
        for (index, g) in groups.iter().enumerate() {
            let seal = g.stored.is_none() && g.last <= through;
            if !wanted(g) && !seal {
                continue;
            }
            if let Some(summary) = &g.stored {
                segments.push(flagged(summary.clone(), index, g));
                continue;
            }
            let run = g.run.clone();
            let items=sqlx::query("SELECT i.step,i.item FROM interactions i LEFT JOIN activity_boundaries b ON b.interaction_seq=i.seq WHERE i.session_id=? AND i.run_id=? AND i.seq BETWEEN ? AND ? AND b.boundary_item_id IS ? ORDER BY i.seq")
                .bind(sid).bind(&run).bind(g.first).bind(g.last).bind(&g.recorded).fetch_all(&mut *tx).await?;
            // Same categories as the page's activityRows: one tool per call id, one update per message.
            let (mut tools, mut updates, mut compactions, mut retries) = (HashSet::new(), 0, 0, 0);
            // Model calls and their summed input and cache reads, for the group's cache share.
            let (mut models, mut input, mut cached) = (0, 0, 0);
            // A sub agent start is one sub agent, not another tool in the group's tool count.
            let mut subagents = HashSet::new();
            // Whether the group's latest row is a scheduled retry, so its label can say so before
            // its events are loaded: "retry" while reconnecting, "recovering" for an automatic resume.
            let mut retrying = Value::Null;
            let mut controls = HashSet::new();
            for row in &items {
                let item = self.unseal(&row.get::<Vec<u8>, _>("item"))?;
                let call_id = item["call_id"].as_str().unwrap_or("").to_owned();
                match item["type"].as_str().unwrap_or("") {
                    "function_call" if item["name"] == PUBLISH_CONTROL => {
                        controls.insert(call_id);
                    }
                    "function_call_output" if controls.contains(&call_id) => {}
                    "function_call" if item["name"] == SUBAGENT_START => {
                        subagents.insert(call_id);
                    }
                    "function_call" | "function_call_output" if subagents.contains(&call_id) => {}
                    "function_call" | "function_call_output" => {
                        tools.insert(call_id);
                    }
                    "message" if in_chat.contains(&(run.clone(), row.get::<String, _>("step"))) => {
                    }
                    "message" => updates += 1,
                    "compaction_result" => compactions += 1,
                    "retry_scheduled" | "model_unreachable" => retries += 1,
                    "model_usage" => {
                        models += 1;
                        input += item["input_tokens"].as_u64().unwrap_or(0);
                        cached += item["cached_input_tokens"].as_u64().unwrap_or(0);
                    }
                    _ => {}
                }
                retrying = match item["type"].as_str() {
                    Some("retry_scheduled") if item["auto_resume"] == true => json!("recovering"),
                    Some("retry_scheduled") => json!("retry"),
                    _ => Value::Null,
                };
            }
            let summary = json!({"id":format!("activity:{run}:{}",g.anchor),"run_id":run,"boundary_item_id":g.anchor,"first_seq":g.first,"last_seq":g.last,
                "boundary_quality":if g.recorded.is_some(){"recorded"}else{"legacy_unverified"},
                "retrying":retrying,"cache":{"input":input,"cached":cached},
                "counts":{"tools":tools.len(),"subagents":subagents.len(),"updates":updates,"responses":0,"modes":0,"compactions":compactions,"retries":retries,"models":models}});
            if seal {
                sealed.push((g, summary.clone()));
            }
            if wanted(g) {
                segments.push(flagged(summary, index, g));
            }
        }
        // A write since this snapshot (new activity, a deleted message, an undo) makes storing
        // fail; the groups are then stored by a later request, never from stale rows.
        if (through != saved || !sealed.is_empty())
            && seal_groups(&mut tx, sid, mark, through, &sealed)
                .await
                .is_ok()
        {
            let _ = tx.commit().await;
        }
        Ok(json!({"segments":segments,"cursor":cursor}))
    }
    /// The newest interaction's sequence number in a session (0 when it has none), so an open
    /// chat learns from its stream that activity changed instead of polling for it.
    pub async fn activity_seq(&self, sid: &str) -> Result<i64> {
        let seq: Option<i64> =
            sqlx::query_scalar("SELECT MAX(seq) FROM interactions WHERE session_id=?")
                .bind(sid)
                .fetch_one(&self.pool)
                .await?;
        Ok(seq.unwrap_or(0))
    }
    /// The newest run's start time and the counts of its latest activity group, for a session
    /// list row while that run is live. Decrypts one run view and that run's groups only.
    pub async fn live_run_summary(&self, sid: &str) -> Result<Value> {
        let Some(run_id) = sqlx::query_scalar::<_, String>(
            "SELECT id FROM run_index WHERE session_id=? ORDER BY updated_at DESC,id DESC LIMIT 1",
        )
        .bind(sid)
        .fetch_optional(&self.pool)
        .await?
        else {
            return Ok(Value::Null);
        };
        let started = self
            .get(&format!("run_view:{run_id}"))
            .await?
            .map_or(Value::Null, |view| view["created_at"].clone());
        let first: Option<i64> =
            sqlx::query_scalar("SELECT MIN(seq) FROM interactions WHERE session_id=? AND run_id=?")
                .bind(sid)
                .bind(&run_id)
                .fetch_one(&self.pool)
                .await?;
        let counts = match first {
            Some(first) => self.private_activity_summary(sid, first - 1, None).await?["segments"]
                .as_array()
                .and_then(|groups| {
                    groups
                        .iter()
                        .rev()
                        .find(|group| group["run_id"] == run_id.as_str())
                })
                .map_or(Value::Null, |group| group["counts"].clone()),
            None => Value::Null,
        };
        Ok(json!({"run_started_at":started,"activity_counts":counts}))
    }
    /// One activity page; `errors_only` keeps just non-success tool results. The cursor and
    /// `has_more` still follow the scanned rows, so a filtered page may be short or empty.
    pub async fn private_activity(
        &self,
        sid: &str,
        after: i64,
        errors_only: bool,
    ) -> Result<Value> {
        let mut tx = self.pool.begin().await?;
        let rows=sqlx::query("SELECT i.seq,i.run_id,i.step,i.item,i.execution_metadata,i.created_at,b.boundary_item_id FROM interactions i LEFT JOIN activity_boundaries b ON b.interaction_seq=i.seq WHERE i.session_id=? AND i.seq>? ORDER BY i.seq LIMIT 201")
            .bind(sid).bind(after).fetch_all(&mut *tx).await?;
        let more = rows.len() > 200;
        let page = &rows[..rows.len().min(200)];
        let in_chat = in_chat_steps(&mut *tx, sid, after).await?;
        let events = page
            .iter()
            .map(|row| event(row, self, sid, &in_chat))
            .filter(|event| {
                !errors_only
                    || event.as_ref().map_or(true, |e| {
                        e["kind"] == "tool_result" && e["payload"]["status"] != "success"
                    })
            })
            .collect::<Result<Vec<_>>>()?;
        let cursor = page.last().map(|r| r.get::<i64, _>("seq")).unwrap_or(after);
        let (_, _, groups) = activity_groups(&mut tx, sid).await?;
        let latest: Option<String> =
            sqlx::query_scalar("SELECT id FROM chat WHERE session_id=? ORDER BY seq DESC LIMIT 1")
                .bind(sid)
                .fetch_optional(&mut *tx)
                .await?;
        let segments=groups.iter().map(|g|{
            json!({"id":format!("activity:{}:{}",g.run,g.anchor),"run_id":g.run,"boundary_item_id":g.anchor,
                "first_seq":g.first,"last_seq":g.last,"closed":latest.as_deref()!=Some(g.anchor.as_str()),
                "boundary_quality":if g.recorded.is_some(){"recorded"}else{"legacy_unverified"}})
        }).collect::<Vec<_>>();
        tx.commit().await?;
        Ok(json!({"events":events,"segments":segments,"cursor":cursor,"has_more":more}))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    /// Messages the chat shows (a bubble's repeated messages, the final reply) and the publish
    /// control leave activity groups; private notes stay.
    #[tokio::test]
    async fn chat_shown_messages_and_publish_control_leave_activity_groups() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        store
            .put(
                "session:s",
                &json!({"id":"s","agent_id":"shared_default","status":"active"}),
            )
            .await
            .unwrap();
        let note = |text: &str| json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]});
        let mut tx = store.pool.begin().await.unwrap();
        store
            .insert_chat(&mut tx, "s", "r", "user", &json!("Hello"))
            .await
            .unwrap();
        for (step, item) in [
            ("model:1:0", note("private note")),
            ("model:2:0", note("Plan: do X")),
            (
                "model:2:1",
                json!({"type":"function_call","call_id":"p1","name":"publish_main_message","arguments":"{}"}),
            ),
            (
                "tool:p1",
                json!({"type":"function_call_output","call_id":"p1","output":"{}"}),
            ),
        ] {
            store
                .insert_interaction(&mut tx, "s", "r", step, &item)
                .await
                .unwrap();
        }
        tx.commit().await.unwrap();
        let mut run = json!({"id":"r","session_id":"s","status":"running","generation":"g","lease_epoch":0,"input":[],"turns":2});
        store
            .save_run(
                &mut run,
                "working_message",
                &json!({"id":"working:r:2:0:echo:0","text":"Plan: do X"}),
            )
            .await
            .unwrap();
        let mut tx = store.pool.begin().await.unwrap();
        store
            .insert_interaction(&mut tx, "s", "r", "model:3:0", &note("All done"))
            .await
            .unwrap();
        store
            .insert_chat(&mut tx, "s", "r", "assistant", &json!("All done"))
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let data = store.private_activity("s", 0, false).await.unwrap();
        let messages = data["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|event| event["kind"] == "agent_step_end")
            .map(|event| {
                (
                    event["payload"]["content"].clone(),
                    event["payload"]["in_chat"].clone(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            messages,
            vec![
                (json!("private note"), json!(false)),
                (json!("Plan: do X"), json!(true)),
                (json!("All done"), json!(true))
            ]
        );
        // Each model item carries the turn that wrote it, so the page can split steps per loop.
        let turn_of = |kind: &str, field: &str, value: &str| {
            data["events"]
                .as_array()
                .unwrap()
                .iter()
                .find(|event| event["kind"] == kind && event["payload"][field] == value)
                .map(|event| event["turn"].clone())
        };
        assert_eq!(
            turn_of("agent_step_end", "content", "private note"),
            Some(json!(1))
        );
        assert_eq!(turn_of("tool_call", "tool_call_id", "p1"), Some(json!(2)));
        assert_eq!(
            turn_of("tool_result", "tool_call_id", "p1"),
            Some(Value::Null)
        );
        let summary = store.private_activity_summary("s", 0, None).await.unwrap();
        let (tools, updates) = summary["segments"].as_array().unwrap().iter().fold(
            (0, 0),
            |(tools, updates), segment| {
                (
                    tools + segment["counts"]["tools"].as_u64().unwrap(),
                    updates + segment["counts"]["updates"].as_u64().unwrap(),
                )
            },
        );
        assert_eq!((tools, updates), (0, 1));
        assert_eq!(
            echoed_steps("working:r:auto:3:1"),
            vec![("r".to_owned(), "model:3:1".to_owned())]
        );
        assert!(echoed_steps("working:r:2:0").is_empty());
    }
    /// A sub agent start is counted as a sub agent, leaving the group's tool count to real tools.
    #[tokio::test]
    async fn sub_agent_spawn_calls_count_apart_from_tools() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        store
            .put(
                "session:s",
                &json!({"id":"s","agent_id":"shared_default","status":"active"}),
            )
            .await
            .unwrap();
        let mut tx = store.pool.begin().await.unwrap();
        for (step, item) in [
            (
                "tool:s1",
                json!({"type":"function_call","call_id":"s1","name":"start_spawn","arguments":"{}"}),
            ),
            (
                "tool:s1:out",
                json!({"type":"function_call_output","call_id":"s1","output":"{}"}),
            ),
            (
                "tool:t1",
                json!({"type":"function_call","call_id":"t1","name":"file_read","arguments":"{}"}),
            ),
            (
                "tool:t1:out",
                json!({"type":"function_call_output","call_id":"t1","output":"{}"}),
            ),
        ] {
            store
                .insert_interaction(&mut tx, "s", "r", step, &item)
                .await
                .unwrap();
        }
        tx.commit().await.unwrap();
        let summary = store.private_activity_summary("s", 0, None).await.unwrap();
        let counts = &summary["segments"][0]["counts"];
        assert_eq!(
            (counts["tools"].as_u64(), counts["subagents"].as_u64()),
            (Some(1), Some(1))
        );
    }
    #[tokio::test]
    async fn compaction_rows_show_in_activity_but_not_in_context() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        store
            .put(
                "session:s",
                &json!({"id":"s","agent_id":"shared_default","status":"active"}),
            )
            .await
            .unwrap();
        let reply = json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"done"}]});
        let mut tx = store.pool.begin().await.unwrap();
        store
            .insert_interaction(&mut tx, "s", "r", "model:1:0", &reply)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let before = store.context("s").await.unwrap();
        let mut tx = store.pool.begin().await.unwrap();
        let call = json!({"type":"compaction_call","model":"system-model","input_bytes":12});
        let result = json!({"type":"compaction_result","summary":"Summary text"});
        store
            .insert_interaction(&mut tx, "s", "r", "compaction:1:x:call", &call)
            .await
            .unwrap();
        store
            .insert_interaction(&mut tx, "s", "r", "compaction:1:x:result", &result)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        // Both the cached cursor path and a cold rebuild skip the rows.
        assert_eq!(store.context("s").await.unwrap(), before);
        let cold = Store::open(root.path()).await.unwrap();
        assert_eq!(cold.context("s").await.unwrap(), before);
        let events = store.private_run_activity("s", "r", 0).await.unwrap()["events"].clone();
        let kinds: Vec<_> = events
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["kind"].clone())
            .collect();
        assert_eq!(
            kinds,
            [
                json!("agent_step_end"),
                json!("compaction_call"),
                json!("compaction_result")
            ]
        );
        assert_eq!(events[1]["payload"]["model"], "system-model");
        assert_eq!(events[2]["payload"]["summary"], "Summary text");
        // A scheduled retry is an activity-only row too: counted, flagged as the group's latest
        // state, and kept out of the model's context.
        let retry = json!({"type":"retry_scheduled","attempt":1,"of":2,"wait_seconds":8,
            "error":"Bridge provider overloaded","backup":false,"auto_resume":false});
        let mut tx = store.pool.begin().await.unwrap();
        store
            .insert_interaction(&mut tx, "s", "r", "retry:1", &retry)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(store.context("s").await.unwrap(), before);
        let summary = store.private_activity_summary("s", 0, None).await.unwrap();
        let group = &summary["segments"][0];
        assert_eq!(
            (
                group["counts"]["retries"].as_u64(),
                group["retrying"].as_str()
            ),
            (Some(1), Some("retry"))
        );
        let events = store.private_run_activity("s", "r", 0).await.unwrap()["events"].clone();
        assert_eq!(events[3]["kind"], "retry_scheduled");
        assert_eq!(events[3]["payload"]["wait_seconds"], 8);
    }
    #[tokio::test]
    async fn counts_recorded_calls_for_only_the_selected_agent() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        for (sid, agent) in [("first", "shared_default"), ("second", "other")] {
            store
                .put(
                    &format!("session:{sid}"),
                    &json!({"id":sid,"agent_id":agent,"status":"active","created_at":1}),
                )
                .await
                .unwrap();
        }
        let mut tx = store.pool.begin().await.unwrap();
        let call =
            json!({"type":"function_call","call_id":"c1","name":"file_read","arguments":"{}"});
        store
            .insert_interaction(&mut tx, "first", "r1", "model:1:0", &call)
            .await
            .unwrap();
        store
            .insert_interaction(&mut tx, "first", "r1", "model:1:0", &call)
            .await
            .unwrap();
        store
            .insert_interaction(
                &mut tx,
                "first",
                "r1",
                "model:1:1",
                &json!({"type":"message","content":"done"}),
            )
            .await
            .unwrap();
        store
            .insert_interaction(&mut tx, "second", "r2", "model:1:0", &call)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(
            store
                .tool_call_counts("shared_default")
                .await
                .unwrap()
                .get("file_read"),
            Some(&1)
        );
        assert_eq!(
            store
                .tool_call_counts("other")
                .await
                .unwrap()
                .get("file_read"),
            Some(&1)
        );
    }
    #[tokio::test]
    async fn boundaries_split_at_promotion_and_retries_keep_original_anchor() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        store
            .put(
                "session:s",
                &json!({"id":"s","agent_id":"shared_default","status":"active"}),
            )
            .await
            .unwrap();
        let mut tx = store.pool.begin().await.unwrap();
        store
            .insert_chat(&mut tx, "s", "r", "user", &json!("Hello"))
            .await
            .unwrap();
        let first = json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"PRIVATE step"}]});
        store
            .insert_interaction(&mut tx, "s", "r", "step:1", &first)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let mut run = json!({"id":"r","session_id":"s","status":"running","generation":"g","lease_epoch":0,"input":[],"turns":1});
        store
            .save_run(
                &mut run,
                "working_message",
                &json!({"id":"working:r:c1","text":"Promoted"}),
            )
            .await
            .unwrap();
        let mut tx = store.pool.begin().await.unwrap();
        store
            .insert_interaction(
                &mut tx,
                "s",
                "r",
                "step:2",
                &json!({"type":"function_call","call_id":"tool1","name":"read","arguments":"{}"}),
            )
            .await
            .unwrap();
        store
            .insert_interaction(&mut tx, "s", "r", "step:1", &first)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let data = store.private_activity("s", 0, false).await.unwrap();
        assert_eq!(data["events"].as_array().unwrap().len(), 2);
        assert_eq!(data["segments"].as_array().unwrap().len(), 2);
        assert_eq!(data["segments"][0]["boundary_item_id"], "user:r");
        assert_eq!(data["segments"][1]["boundary_item_id"], "working:r:c1");
        assert_eq!(data["segments"][0]["closed"], true);
        assert_eq!(data["events"][0]["payload"]["content"], "PRIVATE step");
        let bytes: Vec<u8> =
            sqlx::query_scalar("SELECT item FROM interactions WHERE step='step:1'")
                .fetch_one(&store.pool)
                .await
                .unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains("PRIVATE step"));
        let (chat, _) = store.chat_snapshot("s").await.unwrap();
        assert_eq!(chat.len(), 2);
        assert!(chat.iter().all(|r| !r.to_string().contains("PRIVATE step")));
        let next = store
            .private_activity("s", data["cursor"].as_i64().unwrap(), false)
            .await
            .unwrap();
        assert!(next["events"].as_array().unwrap().is_empty());
        assert_eq!(next["segments"], data["segments"]);
        drop(store);
        let reopened = Store::open(root.path()).await.unwrap();
        assert_eq!(
            reopened.private_activity("s", 0, false).await.unwrap(),
            data
        );
    }
    #[tokio::test]
    async fn summary_counts_groups_without_events_and_selects_by_window_or_cursor() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        store
            .put(
                "session:s",
                &json!({"id":"s","agent_id":"shared_default","status":"active"}),
            )
            .await
            .unwrap();
        let mut tx = store.pool.begin().await.unwrap();
        store
            .insert_chat(&mut tx, "s", "r", "user", &json!("Hello"))
            .await
            .unwrap();
        let step = json!({"type":"message","role":"assistant","content":"PRIVATE step"});
        store
            .insert_interaction(&mut tx, "s", "r", "step:1", &step)
            .await
            .unwrap();
        store
            .insert_interaction(
                &mut tx,
                "s",
                "r",
                "step:2",
                &json!({"type":"function_call","call_id":"t1","name":"read","arguments":"{}"}),
            )
            .await
            .unwrap();
        store
            .insert_interaction(
                &mut tx,
                "s",
                "r",
                "step:3",
                &json!({"type":"function_call_output","call_id":"t1","output":"ok"}),
            )
            .await
            .unwrap();
        store
            .insert_interaction(
                &mut tx,
                "s",
                "r",
                "step:4",
                &json!({"type":"function_call","call_id":"t2","name":"list","arguments":"{}"}),
            )
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let mut run = json!({"id":"r","session_id":"s","status":"running","generation":"g","lease_epoch":0,"input":[],"turns":1});
        store
            .save_run(
                &mut run,
                "working_message",
                &json!({"id":"working:r:c1","text":"Promoted"}),
            )
            .await
            .unwrap();
        let mut tx = store.pool.begin().await.unwrap();
        store
            .insert_interaction(&mut tx, "s", "r", "step:5", &step)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let full = store.private_activity("s", 0, false).await.unwrap();
        let first = store
            .private_activity_summary("s", 0, Some(&["user:r".to_owned()]))
            .await
            .unwrap();
        assert!(
            !first.to_string().contains("PRIVATE step"),
            "summaries carry no event content"
        );
        let segments = first["segments"].as_array().unwrap();
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0]["counts"]["tools"], 2);
        assert_eq!(segments[0]["counts"]["updates"], 1);
        assert_eq!(
            (
                segments[0]["closed"].clone(),
                segments[0]["latest"].clone(),
                segments[0]["last_of_run"].clone()
            ),
            (json!(true), json!(false), json!(false))
        );
        assert_eq!(first["cursor"], full["cursor"]);
        let empty = store
            .private_activity_summary("s", 0, Some(&[]))
            .await
            .unwrap();
        assert!(empty["segments"].as_array().unwrap().is_empty());
        assert_eq!(empty["cursor"], full["cursor"]);
        let changed = store
            .private_activity_summary("s", full["segments"][0]["last_seq"].as_i64().unwrap(), None)
            .await
            .unwrap();
        let changed = changed["segments"].as_array().unwrap();
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0]["boundary_item_id"], "working:r:c1");
        assert_eq!(changed[0]["counts"]["updates"], 1);
        assert_eq!(changed[0]["latest"], true);
        assert_eq!(changed[0]["last_of_run"], true);
        assert!(store
            .private_activity_summary("s", full["cursor"].as_i64().unwrap(), None)
            .await
            .unwrap()["segments"]
            .as_array()
            .unwrap()
            .is_empty());
    }
    async fn stored_rows(store: &Store) -> (i64, i64) {
        let rows: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM activity_summaries WHERE session_id='s'")
                .fetch_one(&store.pool)
                .await
                .unwrap();
        let mark: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(through_seq),0) FROM activity_summary_marks WHERE session_id='s'",
        )
        .fetch_one(&store.pool)
        .await
        .unwrap();
        (rows, mark)
    }
    async fn set_status(store: &Store, status: &str) {
        sqlx::query("INSERT INTO run_index(id,session_id,status,updated_at) VALUES('r','s',?,1) ON CONFLICT(id) DO UPDATE SET status=excluded.status")
            .bind(status)
            .execute(&store.pool)
            .await
            .unwrap();
    }
    /// Finished groups are stored once and read back unchanged; a run that writes again is
    /// counted afresh, and a deleted message clears stored groups so they rebuild.
    #[tokio::test]
    async fn ended_groups_are_stored_and_rebuilt_after_resume_or_delete() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        store
            .put(
                "session:s",
                &json!({"id":"s","agent_id":"shared_default","status":"active"}),
            )
            .await
            .unwrap();
        let mut tx = store.pool.begin().await.unwrap();
        store
            .insert_chat(&mut tx, "s", "r", "user", &json!("Hello"))
            .await
            .unwrap();
        store
            .insert_interaction(
                &mut tx,
                "s",
                "r",
                "step:1",
                &json!({"type":"function_call","call_id":"t1","name":"read","arguments":"{}"}),
            )
            .await
            .unwrap();
        store
            .insert_interaction(
                &mut tx,
                "s",
                "r",
                "step:2",
                &json!({"type":"function_call_output","call_id":"t1","output":"ok"}),
            )
            .await
            .unwrap();
        tx.commit().await.unwrap();
        set_status(&store, "completed").await;
        let first = store.private_activity_summary("s", 0, None).await.unwrap();
        assert_eq!(first["segments"][0]["counts"]["tools"], 1);
        let last = first["segments"][0]["last_seq"].as_i64().unwrap();
        assert_eq!(stored_rows(&store).await, (1, last));
        assert_eq!(
            store.private_activity_summary("s", 0, None).await.unwrap(),
            first,
            "a stored group reads back unchanged"
        );
        for table in ["activity_summaries", "activity_summary_marks"] {
            sqlx::query(&format!("DELETE FROM {table}"))
                .execute(&store.pool)
                .await
                .unwrap();
        }
        assert_eq!(
            store.private_activity_summary("s", 0, None).await.unwrap(),
            first,
            "stored equals recomputed"
        );
        assert!(store
            .private_activity_summary("s", last, None)
            .await
            .unwrap()["segments"]
            .as_array()
            .unwrap()
            .is_empty());
        // A resumed run writes again: its group is counted afresh and not stored while open.
        set_status(&store, "running").await;
        let mut tx = store.pool.begin().await.unwrap();
        store
            .insert_interaction(
                &mut tx,
                "s",
                "r",
                "step:3",
                &json!({"type":"function_call","call_id":"t2","name":"list","arguments":"{}"}),
            )
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let resumed = store.private_activity_summary("s", 0, None).await.unwrap();
        assert_eq!(resumed["segments"][0]["counts"]["tools"], 2);
        assert_eq!(stored_rows(&store).await, (0, 0));
        set_status(&store, "completed").await;
        store.private_activity_summary("s", 0, None).await.unwrap();
        assert_eq!(stored_rows(&store).await.0, 1);
        // Deleting a message clears stored groups; they rebuild on the next read.
        let id: String = sqlx::query_scalar("SELECT id FROM chat WHERE session_id='s'")
            .fetch_one(&store.pool)
            .await
            .unwrap();
        let mut tx = store.pool.begin().await.unwrap();
        store.remove_chat_row(&mut tx, "s", &id).await.unwrap();
        tx.commit().await.unwrap();
        assert_eq!(stored_rows(&store).await, (0, 0));
        let rebuilt = store.private_activity_summary("s", 0, None).await.unwrap();
        assert_eq!(rebuilt["segments"][0]["boundary_item_id"], "user:r");
        assert_eq!(rebuilt["segments"][0]["counts"]["tools"], 2);
        assert_eq!(stored_rows(&store).await.0, 1);
        assert_eq!(store.activity_seq("s").await.unwrap(), rebuilt["cursor"]);
    }
    #[tokio::test]
    async fn errors_filter_keeps_only_failed_results_and_paging() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        store
            .put(
                "session:s",
                &json!({"id":"s","agent_id":"shared_default","status":"active"}),
            )
            .await
            .unwrap();
        let mut tx = store.pool.begin().await.unwrap();
        for (step, item) in [
            (
                "model:1:0",
                json!({"type":"function_call","call_id":"a","name":"t","arguments":"{}"}),
            ),
            (
                "tool:a",
                json!({"type":"function_call_output","call_id":"a","output":r#"{"status":"ok"}"#}),
            ),
            (
                "model:1:1",
                json!({"type":"function_call","call_id":"b","name":"t","arguments":"{}"}),
            ),
            (
                "tool:b",
                json!({"type":"function_call_output","call_id":"b","output":r#"{"status":"error","error_kind":"bad"}"#}),
            ),
        ] {
            store
                .insert_interaction(&mut tx, "s", "r", step, &item)
                .await
                .unwrap();
        }
        tx.commit().await.unwrap();
        let full = store.private_activity("s", 0, false).await.unwrap();
        let errors = store.private_activity("s", 0, true).await.unwrap();
        let events = errors["events"].as_array().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["payload"]["tool_call_id"], "b");
        assert_eq!(events[0]["payload"]["status"], "error");
        assert_eq!(events[0]["payload"]["error_kind"], "bad");
        assert_eq!(full["events"][1]["payload"]["status"], "success");
        assert_eq!(
            (errors["cursor"].clone(), errors["has_more"].clone()),
            (full["cursor"].clone(), full["has_more"].clone())
        );
    }
}
