//! Read-only model usage for the Models pages: the signed-in user's own runs, or
//! every user's runs for administrators. Only numeric public usage leaves here.
use crate::{bad, Api, App, Store};
use axum::{extract::{Query, State}, Extension, Json};
use serde::Deserialize;
use std::collections::HashMap;
use webagent_usage::Sample;

#[derive(Deserialize)]
pub(crate) struct UsageArgs {
    interval: Option<String>,
    group_by: Option<String>,
    agent_id: Option<String>,
    model: Option<String>,
    entry_id: Option<String>,
    include_unassigned: Option<String>,
}

/// Runs with recorded usage, labelled with session titles. With `user`, only runs
/// of agents that user may currently access are counted; administrators pass None.
async fn collect(store: &Store, user: Option<&str>, agent: Option<&str>, samples: &mut Vec<Sample>, labels: &mut HashMap<String, String>) -> anyhow::Result<()> {
    let mut sessions = HashMap::new();
    for recycled in [false, true] {
        for summary in store.ordered_sessions_status(recycled).await? {
            let Some(id) = summary["id"].as_str() else { continue };
            let agent_id = summary["agent_id"].as_str().unwrap_or("shared_default");
            if let Some(title) = summary["title"].as_str().filter(|t| !t.is_empty()) { labels.insert(id.to_owned(), title.to_owned()); }
            sessions.insert(id.to_owned(), agent_id.to_owned());
        }
    }
    let rows: Vec<(String, i64, Vec<u8>)> = sqlx::query_as("SELECT r.session_id,r.updated_at,d.value FROM run_index r JOIN docs d ON d.key='run_view:'||r.id")
        .fetch_all(&store.pool).await?;
    let mut admitted = HashMap::new();
    for (session, updated_at, raw) in rows {
        // Background runs without a listed session have no agent to authorize.
        let Some(agent_id) = sessions.get(&session) else { continue };
        if agent.is_some_and(|a| a != agent_id) { continue; }
        if let Some(user) = user {
            if !admitted.contains_key(agent_id) {
                let allowed = store.live_access(user, agent_id).await.map(|a| a.admitted()).unwrap_or(false);
                admitted.insert(agent_id.clone(), allowed);
            }
            if !admitted[agent_id] { continue; }
        }
        let view = store.unseal(&raw)?;
        let usage = webagent_usage::public(&view["usage"]);
        if !usage.is_object() { continue; }
        samples.push(Sample { at: view["updated_at"].as_i64().unwrap_or(updated_at), session, agent: agent_id.clone(),
            entry: view["model"]["entry_id"].as_str().unwrap_or("").to_owned(), model: view["model"]["model"].as_str().unwrap_or("").to_owned(), usage });
    }
    Ok(())
}

fn respond(args: &UsageArgs, samples: &[Sample], labels: &HashMap<String, String>, totals: bool) -> Api {
    if totals {
        return Ok(Json(webagent_usage::model_totals(samples, args.entry_id.as_deref().unwrap_or(""), args.model.as_deref().unwrap_or(""), args.include_unassigned.as_deref() == Some("1"))));
    }
    webagent_usage::analytics(samples, args.interval.as_deref().unwrap_or("day"), args.group_by.as_deref().unwrap_or("models"), labels, crate::now() as i64)
        .map(Json).ok_or_else(|| bad("Invalid usage interval or grouping"))
}

async fn own(app: App, args: UsageArgs, totals: bool) -> Api {
    let (mut samples, mut labels) = (Vec::new(), HashMap::new());
    collect(&app.store, Some(app.store.current_user_id()), args.agent_id.as_deref(), &mut samples, &mut labels).await?;
    respond(&args, &samples, &labels, totals)
}

async fn everyone(app: App, args: UsageArgs, totals: bool) -> Api {
    let (mut samples, mut labels) = (Vec::new(), HashMap::new());
    for user in app.store.all_user_ids().await? {
        collect(&app.store.for_user(&user).await?, None, args.agent_id.as_deref(), &mut samples, &mut labels).await?;
    }
    respond(&args, &samples, &labels, totals)
}

pub(crate) async fn analytics(Extension(app): Extension<App>, Query(args): Query<UsageArgs>) -> Api { own(app, args, false).await }
pub(crate) async fn totals(Extension(app): Extension<App>, Query(args): Query<UsageArgs>) -> Api { own(app, args, true).await }
pub(crate) async fn admin_analytics(State(app): State<App>, Query(args): Query<UsageArgs>) -> Api { everyone(app, args, false).await }
pub(crate) async fn admin_totals(State(app): State<App>, Query(args): Query<UsageArgs>) -> Api { everyone(app, args, true).await }
