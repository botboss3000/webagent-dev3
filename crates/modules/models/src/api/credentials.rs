//! Personal vault of named provider API keys and bridge link tokens. Records are sealed in the saving
//! user's own database, so a key saved from one agent loads on any agent that user manages; the key
//! value is returned only to that owner on an explicit load.
use super::{
    bad, conflict,
    selection::{authority, byo_credential_permission, principal},
    Api, ApiError,
};
use crate::{provider::ProviderId, runtime::Runtime, storage::RuntimeStorage};
use axum::{http::StatusCode, Json};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::SqlitePool;
use webagent_access::Principal;

const PREFIX: &str = "model_credential:";
const QUOTA: i64 = 100;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Save {
    pub label: String,
    pub provider: ProviderId,
    #[serde(default)]
    pub base_url: String,
    pub api_key: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Label {
    pub label: String,
}

fn valid_label(label: &str) -> bool {
    !label.is_empty() && label.chars().count() <= 80 && !label.chars().any(char::is_control)
}
fn owner_prefix(owner: &str) -> String {
    format!("{PREFIX}{owner}:")
}
// Labels are display text; the digest keeps arbitrary characters out of storage keys.
fn key(owner: &str, label: &str) -> String {
    format!(
        "{}{}",
        owner_prefix(owner),
        webagent_configuration::digest(label)
    )
}
fn public(record: &Value) -> Value {
    let secret = record["api_key"].as_str().unwrap_or("");
    json!({"kind":"api_key","label":record["label"],"provider":record["provider"],"base_url":record["base_url"],
        "credential_suffix":secret.get(secret.len().saturating_sub(4)..).unwrap_or("")})
}
fn not_found() -> ApiError {
    ApiError(StatusCode::NOT_FOUND, "Saved key not found".into())
}
// The vault is reached through an agent the caller manages; each user sees only their own keys.
async fn caller<C: RuntimeStorage>(
    app: &Runtime<'_, C>,
    identity: &Value,
    agent: &str,
) -> Result<(Principal, SqlitePool), ApiError> {
    authority(app, identity, agent, true).await?;
    let caller = principal(app, identity).await?;
    let pool = app.store.for_user(&caller.user_id).await?;
    Ok((caller, pool))
}
async fn read<C: RuntimeStorage>(
    app: &Runtime<'_, C>,
    pool: &SqlitePool,
    owner: &str,
    label: &str,
) -> anyhow::Result<Option<Value>> {
    let raw: Option<Vec<u8>> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
        .bind(key(owner, label))
        .fetch_optional(pool)
        .await?;
    Ok(raw
        .map(|bytes| app.store.unseal(&bytes))
        .transpose()?
        .filter(|r| r["owner_id"] == owner))
}
async fn owned<C: RuntimeStorage>(
    app: &Runtime<'_, C>,
    pool: &SqlitePool,
    owner: &str,
) -> anyhow::Result<Vec<Value>> {
    let prefix = owner_prefix(owner);
    let rows: Vec<Vec<u8>> =
        sqlx::query_scalar("SELECT value FROM docs WHERE substr(key,1,length(?))=?")
            .bind(&prefix)
            .bind(&prefix)
            .fetch_all(pool)
            .await?;
    let mut records = Vec::new();
    for raw in rows {
        let record = app.store.unseal(&raw)?;
        if record["owner_id"] == owner {
            records.push(public(&record));
        }
    }
    records.sort_by(|a, b| a["label"].as_str().cmp(&b["label"].as_str()));
    Ok(records)
}
// Returns false when a new record would exceed the user's vault quota.
async fn upsert<C: RuntimeStorage>(
    app: &Runtime<'_, C>,
    pool: &SqlitePool,
    key: &str,
    record: &Value,
) -> anyhow::Result<bool> {
    let mut tx = pool.begin().await?;
    let exists: Option<i64> = sqlx::query_scalar("SELECT 1 FROM docs WHERE key=?")
        .bind(key)
        .fetch_optional(&mut *tx)
        .await?;
    if exists.is_none() {
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM docs WHERE substr(key,1,length(?))=?")
                .bind(PREFIX)
                .bind(PREFIX)
                .fetch_one(&mut *tx)
                .await?;
        if count >= QUOTA {
            return Ok(false);
        }
    }
    sqlx::query("INSERT INTO docs(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value")
        .bind(key).bind(app.store.seal(record)?).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(true)
}

pub async fn list<C: RuntimeStorage>(app: &Runtime<'_, C>, identity: Value, agent: String) -> Api {
    let (caller, pool) = caller(app, &identity, &agent).await?;
    Ok(Json(
        json!({"credentials":owned(app,&pool,&caller.user_id).await?}),
    ))
}
pub async fn save<C: RuntimeStorage>(
    app: &Runtime<'_, C>,
    identity: Value,
    agent: String,
    body: Save,
) -> Api {
    let (caller, pool) = caller(app, &identity, &agent).await?;
    byo_credential_permission(app, &caller).await?;
    let label = body.label.trim();
    if !valid_label(label) {
        return Err(bad("Name the key with 1–80 characters"));
    }
    if body.provider == ProviderId::OpenaiSubscription {
        return Err(bad("This provider does not use an API key"));
    }
    if body.api_key.len() < 8
        || body.api_key.len() > 512
        || !body.api_key.bytes().all(|b| b.is_ascii_graphic())
    {
        return Err(bad(
            "Invalid API key: use 8–512 printable non-space characters",
        ));
    }
    if body.base_url.len() > 500
        || body.base_url.chars().any(char::is_control)
        || !(body.base_url.is_empty() || public_https(&body.base_url))
    {
        return Err(bad("Invalid base URL: use a public HTTPS address"));
    }
    let record = json!({"owner_id":caller.user_id,"label":label,"provider":body.provider,"base_url":body.base_url,"api_key":body.api_key});
    let _guard = app.gate.lock().await;
    if !upsert(app, &pool, &key(&caller.user_id, label), &record).await? {
        return Err(conflict("Saved key limit reached"));
    }
    Ok(Json(json!({"saved":true,"credential":public(&record)})))
}
pub async fn load<C: RuntimeStorage>(
    app: &Runtime<'_, C>,
    identity: Value,
    agent: String,
    body: Label,
) -> Api {
    let (caller, pool) = caller(app, &identity, &agent).await?;
    let record = read(app, &pool, &caller.user_id, body.label.trim())
        .await?
        .ok_or_else(not_found)?;
    Ok(Json(
        json!({"credential":public(&record),"api_key":record["api_key"]}),
    ))
}
pub async fn delete<C: RuntimeStorage>(
    app: &Runtime<'_, C>,
    identity: Value,
    agent: String,
    body: Label,
) -> Api {
    let (caller, pool) = caller(app, &identity, &agent).await?;
    let _guard = app.gate.lock().await;
    let deleted = sqlx::query("DELETE FROM docs WHERE key=?")
        .bind(key(&caller.user_id, body.label.trim()))
        .execute(&pool)
        .await
        .map_err(anyhow::Error::from)?
        .rows_affected()
        > 0;
    if !deleted {
        return Err(not_found());
    }
    Ok(Json(json!({"deleted":true})))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{provider::Provider, storage::fixtures::Fixture};
    use std::sync::Arc;
    use tokio::sync::{Mutex, RwLock};
    use tokio_util::sync::CancellationToken;
    #[tokio::test]
    async fn named_keys_are_sealed_owner_private_and_kept_in_the_user_database() {
        let f = Fixture::new().await;
        let provider = Provider::new();
        let (gate, refresh, login, stop) = (
            Arc::new(Mutex::new(())),
            Arc::new(Mutex::new(())),
            Arc::new(RwLock::new(None)),
            CancellationToken::new(),
        );
        let app = Runtime::new(f.repo(), &provider, &gate, &refresh, &login, &stop);
        let (admin, alice) = (json!({"_user_id":"admin"}), json!({"_user_id":"alice"}));
        let agent = || "shared_default".to_owned();
        let value = ["fixture", "provider", "value", "0042"].join("-");
        let body = |label: &str, provider: ProviderId| Save {
            label: label.into(),
            provider,
            base_url: "https://api.deepseek.com".into(),
            api_key: value.clone(),
        };
        assert!(save(
            &app,
            admin.clone(),
            agent(),
            body(" ", ProviderId::Deepseek)
        )
        .await
        .is_err());
        assert!(save(
            &app,
            admin.clone(),
            agent(),
            body("Sub", ProviderId::OpenaiSubscription)
        )
        .await
        .is_err());
        let saved = save(
            &app,
            admin.clone(),
            agent(),
            body(" Work key ", ProviderId::Deepseek),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(saved["credential"]["label"], "Work key");
        assert_eq!(saved["credential"]["credential_suffix"], "0042");
        assert!(saved["credential"].get("api_key").is_none());
        let listed = list(&app, admin.clone(), agent()).await.unwrap().0;
        assert_eq!(listed["credentials"].as_array().unwrap().len(), 1);
        assert_eq!(listed["credentials"][0]["provider"], "deepseek");
        assert!(listed["credentials"][0].get("api_key").is_none());
        // Another manager of the same agent cannot see or load the owner's key.
        assert_eq!(
            list(&app, alice.clone(), agent()).await.unwrap().0["credentials"],
            json!([])
        );
        assert!(load(
            &app,
            alice.clone(),
            agent(),
            Label {
                label: "Work key".into()
            }
        )
        .await
        .is_err());
        assert!(delete(
            &app,
            alice,
            agent(),
            Label {
                label: "Work key".into()
            }
        )
        .await
        .is_err());
        let loaded = load(
            &app,
            admin.clone(),
            agent(),
            Label {
                label: "Work key".into(),
            },
        )
        .await
        .unwrap()
        .0;
        assert_eq!(loaded["api_key"], value.as_str());
        // Stored in the account's database, not the agent's, so any agent the owner manages can load it.
        let raw: Vec<Vec<u8>> =
            sqlx::query_scalar("SELECT value FROM docs WHERE key LIKE 'model_credential:%'")
                .fetch_all(&f.storage.user)
                .await
                .unwrap();
        assert_eq!(raw.len(), 1);
        let in_agent: i64 =
            sqlx::query_scalar("SELECT count(*) FROM docs WHERE key LIKE 'model_credential:%'")
                .fetch_one(&f.storage.agent)
                .await
                .unwrap();
        assert_eq!(in_agent, 0);
        assert!(!String::from_utf8_lossy(&raw[0]).contains(&value));
        // Saving the same label replaces the record rather than duplicating it.
        let _ = save(
            &app,
            admin.clone(),
            agent(),
            body("Work key", ProviderId::Deepseek),
        )
        .await
        .unwrap();
        assert_eq!(
            list(&app, admin.clone(), agent()).await.unwrap().0["credentials"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let _ = delete(
            &app,
            admin.clone(),
            agent(),
            Label {
                label: "Work key".into(),
            },
        )
        .await
        .unwrap();
        assert_eq!(
            list(&app, admin, agent()).await.unwrap().0["credentials"],
            json!([])
        );
    }
}

/// A provider base URL must be HTTPS to a public host: never loopback, private,
/// link-local, Tailscale (100.64/10) or a metadata address.
// shortcut: checks IP literals and local names only, not what a hostname resolves to; add a
// resolve-and-pin check if the server ever dials this URL.
fn public_https(raw: &str) -> bool {
    use std::net::IpAddr;
    let Ok(url) = reqwest::Url::parse(raw) else {
        return false;
    };
    if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    match host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<IpAddr>()
    {
        Ok(IpAddr::V4(ip)) => {
            let [a, b, ..] = ip.octets();
            !(ip.is_loopback()
                || ip.is_private()
                || ip.is_link_local()
                || ip.is_unspecified()
                || ip.is_broadcast()
                || ip.is_multicast()
                || a == 0
                || (a == 100 && (64..128).contains(&b)))
        }
        // Global unicast only (2000::/3); this also excludes v4-mapped, loopback and ULA.
        Ok(IpAddr::V6(ip)) => (0x2000..0x4000).contains(&ip.segments()[0]),
        Err(_) => {
            let name = host.trim_end_matches('.').to_ascii_lowercase();
            name.contains('.')
                && ![".localhost", ".local", ".internal"]
                    .iter()
                    .any(|suffix| name.ends_with(suffix))
        }
    }
}

#[cfg(test)]
mod base_url_tests {
    #[test]
    fn base_url_must_be_public_https() {
        for ok in ["https://api.example.com/v1", "https://8.8.8.8/"] {
            assert!(super::public_https(ok), "{ok}");
        }
        for bad in [
            "http://api.example.com",
            "https://localhost/v1",
            "https://127.0.0.1/",
            "https://169.254.169.254/latest",
            "https://10.0.0.1/",
            "https://100.87.9.65:8081/",
            "https://[::1]/",
            concat!("https://", "user", ":", "pass", "@api.example.com/"),
            "https://printer.local/",
            "not a url",
        ] {
            assert!(!super::public_https(bad), "{bad}");
        }
    }
}
