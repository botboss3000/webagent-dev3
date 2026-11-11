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
/// Interaction steps a working bubble repeats word for word, read from its chat ID:
/// `working:<run>:auto:<turn>:<ordinal>` (automatic promotion) or
/// `working:<run>:<turn>:<call>:echo:<o1>-<o2>` (`publish_main_message` without text).
fn echoed_steps(id: &str) -> Vec<(String, String)> {
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
/// groups hold only messages the user has not seen.
async fn in_chat_steps<'e, E: sqlx::SqliteExecutor<'e>>(
    executor: E,
    sid: &str,
) -> Result<HashSet<(String, String)>> {
    let rows = sqlx::query(
        "SELECT id AS a,NULL AS b FROM chat WHERE session_id=? AND id LIKE 'working:%' \
         UNION ALL SELECT i.run_id,i.step FROM interactions i JOIN chat c ON c.terminal_run_id=i.run_id \
         WHERE c.session_id=? AND i.session_id=? AND i.step LIKE 'model:%'",
    )
    .bind(sid)
    .bind(sid)
    .bind(sid)
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
fn event(
    row: &SqliteRow,
    store: &Store,
    sid: &str,
    in_chat: &HashSet<(String, String)>,
) -> Result<Value> {
    let seq = row.get::<i64, _>("seq");
    let run = row.get::<String, _>("run_id");
    let shown = in_chat.contains(&(run.clone(), row.get::<String, _>("step")));
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
            json!({"model":item["model"],"input_bytes":item["input_bytes"],"reason":item["reason"]}),
        ),
        "compaction_result" => (
            "compaction_result",
            json!({"summary":item["summary"],"error":item["error"]}),
        ),
        "retry_scheduled" => (
            "retry_scheduled",
            json!({"attempt":item["attempt"],"of":item["of"],"wait_seconds":item["wait_seconds"],
                "error":item["error"],"backup":item["backup"],"auto_resume":item["auto_resume"]}),
        ),
        _ => ("diagnostic", item),
    };
    let execution_metadata = row
        .get::<Option<Vec<u8>>, _>("execution_metadata")
        .map(|bytes| store.unseal(&bytes))
        .transpose()?;
    Ok(
        json!({"execution_metadata":execution_metadata,"seq":seq,"event_id":format!("interaction:{seq}"),"session_id":sid,"run_id":run,"kind":kind,"payload":payload,
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
        let in_chat = in_chat_steps(&self.pool, sid).await?;
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
    /// Only selected groups are decrypted, so cost follows the drawn window, not the history.
    pub async fn private_activity_summary(
        &self,
        sid: &str,
        after: i64,
        boundaries: Option<&[String]>,
    ) -> Result<Value> {
        let groups=sqlx::query("SELECT i.run_id,b.boundary_item_id,MIN(i.seq) first_seq,MAX(i.seq) last_seq FROM interactions i LEFT JOIN activity_boundaries b ON b.interaction_seq=i.seq WHERE i.session_id=? GROUP BY i.run_id,b.boundary_item_id ORDER BY MIN(i.seq)")
            .bind(sid).fetch_all(&self.pool).await?;
        let latest: Option<String> =
            sqlx::query_scalar("SELECT id FROM chat WHERE session_id=? ORDER BY seq DESC LIMIT 1")
                .bind(sid)
                .fetch_optional(&self.pool)
                .await?;
        let cursor = groups
            .iter()
            .map(|r| r.get::<i64, _>("last_seq"))
            .max()
            .unwrap_or(0);
        let mut last_of_run = HashMap::new();
        for (index, r) in groups.iter().enumerate() {
            last_of_run.insert(r.get::<String, _>("run_id"), index);
        }
        let in_chat = in_chat_steps(&self.pool, sid).await?;
        let mut segments = Vec::new();
        for (index, r) in groups.iter().enumerate() {
            let anchor = boundary(r);
            let (run, first, last) = (
                r.get::<String, _>("run_id"),
                r.get::<i64, _>("first_seq"),
                r.get::<i64, _>("last_seq"),
            );
            let wanted = match boundaries {
                Some(list) => list.iter().any(|item| *item == anchor),
                None => last > after,
            };
            if !wanted {
                continue;
            }
            let recorded = r.get::<Option<String>, _>("boundary_item_id");
            let items=sqlx::query("SELECT i.step,i.item FROM interactions i LEFT JOIN activity_boundaries b ON b.interaction_seq=i.seq WHERE i.session_id=? AND i.run_id=? AND i.seq BETWEEN ? AND ? AND b.boundary_item_id IS ? ORDER BY i.seq")
                .bind(sid).bind(&run).bind(first).bind(last).bind(&recorded).fetch_all(&self.pool).await?;
            // Same categories as the page's activityRows: one tool per call id, one update per message.
            let (mut tools, mut updates, mut compactions, mut retries) = (HashSet::new(), 0, 0, 0);
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
                    "retry_scheduled" => retries += 1,
                    _ => {}
                }
                retrying = match item["type"].as_str() {
                    Some("retry_scheduled") if item["auto_resume"] == true => json!("recovering"),
                    Some("retry_scheduled") => json!("retry"),
                    _ => Value::Null,
                };
            }
            segments.push(json!({"id":format!("activity:{run}:{anchor}"),"run_id":run,"boundary_item_id":anchor,"first_seq":first,"last_seq":last,
                "closed":latest.as_deref()!=Some(anchor.as_str()),"latest":index+1==groups.len(),"last_of_run":last_of_run.get(&run)==Some(&index),
                "boundary_quality":if recorded.is_some(){"recorded"}else{"legacy_unverified"},
                "retrying":retrying,
                "counts":{"tools":tools.len(),"subagents":subagents.len(),"updates":updates,"responses":0,"modes":0,"compactions":compactions,"retries":retries}}));
        }
        Ok(json!({"segments":segments,"cursor":cursor}))
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
        let in_chat = in_chat_steps(&mut *tx, sid).await?;
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
        let groups=sqlx::query("SELECT i.run_id,b.boundary_item_id,MIN(i.seq) first_seq,MAX(i.seq) last_seq FROM interactions i LEFT JOIN activity_boundaries b ON b.interaction_seq=i.seq WHERE i.session_id=? GROUP BY i.run_id,b.boundary_item_id ORDER BY MIN(i.seq)")
            .bind(sid).fetch_all(&mut *tx).await?;
        let latest: Option<String> =
            sqlx::query_scalar("SELECT id FROM chat WHERE session_id=? ORDER BY seq DESC LIMIT 1")
                .bind(sid)
                .fetch_optional(&mut *tx)
                .await?;
        let segments=groups.iter().map(|r|{
            let anchor=boundary(r);
            json!({"id":format!("activity:{}:{}",r.get::<String,_>("run_id"),anchor),"run_id":r.get::<String,_>("run_id"),"boundary_item_id":anchor,
                "first_seq":r.get::<i64,_>("first_seq"),"last_seq":r.get::<i64,_>("last_seq"),"closed":latest.as_deref()!=Some(anchor.as_str()),
                "boundary_quality":if r.get::<Option<String>,_>("boundary_item_id").is_some(){"recorded"}else{"legacy_unverified"}})
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
