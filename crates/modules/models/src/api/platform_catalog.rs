//! Models endpoint policy and mutations. Root supplies authenticated identity and route extraction.
use super::{Api, ApiError};
use crate::platform_catalog::{Entry, Mutation};
use crate::{runtime::Runtime, storage::RuntimeStorage};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Draft {
    pub expected_revision: u64,
    pub entries: BTreeMap<String, Entry>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fence {
    pub expected_revision: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rollback {
    pub expected_revision: u64,
    pub revision: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyUpdate {
    pub expected_revision: u64,
    pub api_key: Option<String>,
}
pub async fn mutate<C: RuntimeStorage>(
    app: &Runtime<'_, C>,
    identity: &Value,
    expected: u64,
    operation: Mutation,
) -> Api {
    let _guard = app.gate.lock().await;
    super::administrator(app, identity).await?;
    let current = app.store.platform_roster().await?;
    if current.as_ref().map_or(0, |r| r.revision) != expected {
        return Err(super::conflict(
            "Catalog revision changed; reload before saving",
        ));
    }
    // Validate on a private copy before persistence; immutable history is never client writable.
    match &operation {
        Mutation::Draft { entries } => {
            let mut probe = current.clone().unwrap_or(crate::platform_catalog::Roster {
                revision: 1,
                draft: BTreeMap::new(),
                published_revision: None,
                history: vec![],
            });
            probe.draft = entries.clone();
            probe
                .validate()
                .map_err(|_| super::bad("Invalid catalog metadata"))?;
        }
        Mutation::Publish { rollback } => {
            let mut probe = current
                .clone()
                .ok_or_else(|| super::bad("Save a draft first"))?;
            probe
                .publish(expected, *rollback)
                .map_err(|_| super::bad("Publication unavailable"))?;
        }
    }
    let roster = app
        .store
        .mutate_platform_roster(
            expected,
            operation,
            identity["_user_id"].as_str().unwrap_or(""),
        )
        .await?;
    Ok(Json(json!({"roster":roster})))
}
/// Reads the app catalog; before its first save it is an empty, unpublished draft.
pub async fn read<C: RuntimeStorage>(app: &Runtime<'_, C>, identity: Value) -> Api {
    super::administrator(&app, &identity).await?;
    let roster = app.store.platform_roster().await?;
    Ok(Json(json!({"roster":roster})))
}
/// The models the host's local models relay serves, with whether each answers, for the
/// catalog's add form. Their addresses and keys stay on the host.
pub async fn local_models<C: RuntimeStorage>(app: &Runtime<'_, C>, identity: Value) -> Api {
    super::administrator(&app, &identity).await?;
    let models = crate::decision::local_models().await.map_err(|_| {
        ApiError(
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "Local models relay unavailable".into(),
        )
    })?;
    Ok(Json(json!({ "models": models })))
}
pub async fn history<C: RuntimeStorage>(app: &Runtime<'_, C>, identity: Value) -> Api {
    super::administrator(&app, &identity).await?;
    let roster = app.store.platform_roster().await?.ok_or_else(|| {
        ApiError(
            axum::http::StatusCode::NOT_FOUND,
            "Catalog not saved yet".into(),
        )
    })?;
    Ok(Json(
        json!({"history":roster.history,"published_revision":roster.published_revision}),
    ))
}
pub async fn draft<C: RuntimeStorage>(app: &Runtime<'_, C>, identity: Value, input: Draft) -> Api {
    mutate(
        &app,
        &identity,
        input.expected_revision,
        Mutation::Draft {
            entries: input.entries,
        },
    )
    .await
}
pub async fn publish<C: RuntimeStorage>(
    app: &Runtime<'_, C>,
    identity: Value,
    input: Fence,
) -> Api {
    mutate(
        &app,
        &identity,
        input.expected_revision,
        Mutation::Publish { rollback: None },
    )
    .await
}
pub async fn rollback<C: RuntimeStorage>(
    app: &Runtime<'_, C>,
    identity: Value,
    input: Rollback,
) -> Api {
    mutate(
        &app,
        &identity,
        input.expected_revision,
        Mutation::Publish {
            rollback: Some(input.revision),
        },
    )
    .await
}
pub async fn credential<C: RuntimeStorage>(
    app: &Runtime<'_, C>,
    identity: Value,
    entry_id: String,
    input: KeyUpdate,
) -> Api {
    let _guard = app.gate.lock().await;
    super::administrator(&app, &identity).await?;
    if !webagent_access::valid_id(&entry_id) {
        return Err(super::bad("Invalid catalog entry"));
    }
    let roster = app
        .store
        .platform_roster()
        .await?
        .ok_or_else(|| super::bad("Catalog not saved yet"))?;
    if roster.revision != input.expected_revision {
        return Err(super::conflict("Catalog revision changed"));
    }
    let entry = roster
        .draft
        .get(&entry_id)
        .ok_or_else(|| super::bad("Draft entry not found"))?;
    if !entry.transport.uses_api_key()
        || input.api_key.as_ref().is_some_and(|key| {
            key.is_empty() || key.len() > 4096 || !key.bytes().all(|b| b.is_ascii_graphic())
        })
    {
        return Err(super::bad(
            "Invalid credential input or unavailable transport",
        ));
    }
    app.store
        .platform_credential_set(
            &entry_id,
            input.expected_revision,
            input.api_key.as_deref(),
            identity["_user_id"].as_str().unwrap_or(""),
        )
        .await?;
    Ok(Json(
        json!({"saved":true,"revision":input.expected_revision+1}),
    ))
}
pub async fn validate<C: RuntimeStorage>(app: &Runtime<'_, C>, identity: Value) -> Api {
    super::administrator(&app, &identity).await?;
    let roster = app
        .store
        .platform_roster()
        .await?
        .ok_or_else(|| super::bad("Catalog not saved yet"))?;
    let mut entries = Vec::new();
    for entry in roster.draft.values() {
        let configured = app.store.platform_credential_configured(entry).await?;
        entries.push(json!({"entry_id":entry.id,"configured":configured,
            "warning":if configured {Value::Null} else {json!("Credential not configured")}}));
    }
    Ok(Json(json!({"revision":roster.revision,"entries":entries,
        "provider_acceptance_verified":false})))
}
pub async fn preview<C: RuntimeStorage>(app: &Runtime<'_, C>, identity: Value) -> Api {
    super::administrator(&app, &identity).await?;
    let roster = app
        .store
        .platform_roster()
        .await?
        .ok_or_else(|| super::bad("Catalog not saved yet"))?;
    let published = roster.published().map(|snapshot| &snapshot.entries);
    let mut added = Vec::new();
    let mut changed = Vec::new();
    let mut removed = Vec::new();
    for (id, entry) in &roster.draft {
        match published.and_then(|entries| entries.get(id)) {
            None => added.push(id.clone()),
            Some(old) if old != entry => changed.push(id.clone()),
            _ => {}
        }
    }
    if let Some(entries) = published {
        for id in entries.keys() {
            if !roster.draft.contains_key(id) {
                removed.push(id.clone());
            }
        }
    }
    Ok(Json(
        json!({"revision":roster.revision,"published_revision":roster.published_revision,
        "added_entry_ids":added,"changed_entry_ids":changed,
        "removed_entry_ids":removed,"provider_acceptance_verified":false}),
    ))
}
