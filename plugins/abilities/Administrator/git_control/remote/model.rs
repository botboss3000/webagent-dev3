//! User-owned connections and immutable provider operation receipts.
use crate::*;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::os::unix::fs::OpenOptionsExt;

pub const BUILD_COMMAND:&str="npm install --prefix /tmp/webagent-wrangler --ignore-scripts --no-audit --no-fund --package-lock=false --registry=https://registry.npmjs.org wrangler@4.145.0";
pub const PREVIEW_COMMAND:&str="node /tmp/webagent-wrangler/node_modules/wrangler/bin/wrangler.js preview --config wrangler.jsonc --ignore-base-config --name wa-$WORKERS_CI_COMMIT_SHA --json";
pub const DEPLOY_COMMAND:&str="node /tmp/webagent-wrangler/node_modules/wrangler/bin/wrangler.js deploy --config wrangler.jsonc --tag $WORKERS_CI_COMMIT_SHA";
pub const PREFIX: &str = "core_source_control:";
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Configuration {
    pub github_owner: String,
    pub github_repo: String,
    pub production_branch: String,
    pub cloudflare_account_id: String,
    pub worker_name: String,
    pub build_token_uuid: String,
    pub build_command: String,
    pub root_directory: String,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Site {
    pub id: String,
    pub agent_id: String,
    pub owner_id: String,
    pub revision: u64,
    pub revoked: bool,
    pub configuration: Configuration,
    #[serde(default)]
    pub runtime: Value,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Credentials {
    pub github: String,
    pub cloudflare: String,
    pub github_revision: u64,
    pub cloudflare_revision: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Save {
    pub expected_revision: u64,
    pub configuration: Configuration,
    pub revoked: bool,
}
pub fn identifier(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 80
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
}
pub fn sha(s: &str) -> bool {
    s.len() == 40
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}
pub fn path(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 512
        && !s.contains(['\\', '\0', '?', '#', '%'])
        && !s.chars().any(char::is_control)
        && s.split('/')
            .all(|p| !p.is_empty() && p != "." && p != ".." && p != ".git")
}
pub fn branch(s: &str) -> bool {
    path(s)
        && s.len() <= 200
        && !s.contains([' ', ':', '~', '^', '[', '*', '@'])
        && !s.contains("..")
        && !s.ends_with('.')
        && !s.ends_with(".lock")
}
impl Configuration {
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            identifier(&self.github_owner)
                && !self.github_repo.is_empty()
                && self.github_repo.len() <= 100
                && self.github_repo != "."
                && self.github_repo != ".."
                && self
                    .github_repo
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b)),
            "Invalid GitHub owner or repository"
        );
        anyhow::ensure!(branch(&self.production_branch), "Invalid production branch");
        anyhow::ensure!(
            self.cloudflare_account_id.len() == 32
                && self
                    .cloudflare_account_id
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit()),
            "Invalid Cloudflare account ID"
        );
        anyhow::ensure!(
            !self.worker_name.is_empty()
                && self.worker_name.len() <= 63
                && self
                    .worker_name
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
            "Invalid Worker name"
        );
        anyhow::ensure!(
            uuid::Uuid::parse_str(&self.build_token_uuid).is_ok(),
            "Cloudflare build-token UUID required"
        );
        anyhow::ensure!(
            self.build_command == BUILD_COMMAND,
            "Invalid remote build command"
        );
        anyhow::ensure!(
            self.root_directory == "/" || path(&self.root_directory),
            "Invalid repository root"
        );
        Ok(())
    }
}
pub fn site_key(agent: &str, id: &str) -> String {
    format!("{PREFIX}site:{agent}:{id}")
}
pub fn vault_name(site: &Site, provider: &str) -> String {
    let target = if provider == "github" {
        &site.configuration.github_owner
    } else {
        &site.configuration.cloudflare_account_id
    };
    format!(
        "source_{}_{}",
        &digest(&json!([site.agent_id, site.id, target]))[..32],
        provider
    )
}
pub fn vault_requests(site: &Site) -> Value {
    json!([{"name":vault_name(site,"github"),"origin":"https://api.github.com"},
        {"name":vault_name(site,"cloudflare"),"origin":"https://api.cloudflare.com"}])
}
async fn vault_record(store: &Store, site: &Site, provider: &str) -> Result<Value> {
    anyhow::ensure!(
        site.owner_id == store.current_user_id(),
        "Credential owner mismatch"
    );
    let raw: Option<Vec<u8>> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
        .bind(format!("core_vault:{}", vault_name(site, provider)))
        .fetch_optional(&store.pool)
        .await?;
    let record = raw
        .map(|bytes| store.unseal(&bytes))
        .transpose()?
        .unwrap_or(Value::Null);
    let expected = if provider == "github" {
        "https://api.github.com"
    } else {
        "https://api.cloudflare.com"
    };
    anyhow::ensure!(
        record.is_null() || record["origin"] == expected,
        "Vault service origin mismatch"
    );
    Ok(record)
}
pub fn receipt_key(agent: &str, id: &str, request: &str) -> String {
    format!("{PREFIX}receipt:{agent}:{id}:{request}")
}
pub fn lock(store: &Store, agent: &str, id: &str) -> Result<File> {
    anyhow::ensure!(
        identifier(agent) && identifier(id),
        "Invalid Source Control identity"
    );
    let folder = store.root.join("control");
    std::fs::create_dir_all(&folder)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(folder.join(format!(
            "source-control-{}.lock",
            digest(&json!([store.current_user_id(), agent, id]))
        )))?;
    file.try_lock_exclusive()
        .map_err(|_| anyhow!("Source Control site is busy; inspect status before retry"))?;
    Ok(file)
}
pub async fn load(store: &Store, agent: &str, id: &str) -> Result<Site> {
    anyhow::ensure!(identifier(agent) && identifier(id), "Invalid site identity");
    let site: Site = serde_json::from_value(
        store
            .get(&site_key(agent, id))
            .await?
            .ok_or_else(|| anyhow!("Configure this remote site in Source Control first"))?,
    )?;
    anyhow::ensure!(
        site.owner_id == store.current_user_id()
            && site.agent_id == agent
            && site.id == id
            && !site.revoked,
        "Remote site unavailable for this caller"
    );
    Ok(site)
}
pub async fn credentials(store: &Store, site: &Site) -> Result<Credentials> {
    let gh = vault_record(store, site, "github").await?;
    let cf = vault_record(store, site, "cloudflare").await?;
    anyhow::ensure!(
        [&gh, &cf]
            .into_iter()
            .all(|v| v["value"].as_str().is_none_or(|s| token(s))),
        "Invalid provider credential in vault"
    );
    Ok(Credentials {
        github: gh["value"].as_str().unwrap_or("").into(),
        cloudflare: cf["value"].as_str().unwrap_or("").into(),
        github_revision: gh["revision"].as_u64().unwrap_or(0),
        cloudflare_revision: cf["revision"].as_u64().unwrap_or(0),
    })
}
pub fn token(s: &str) -> bool {
    (8..=4096).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_graphic())
}
pub fn projection(site: &Site, credentials: &Credentials) -> Value {
    json!({"id":site.id,"agent_id":site.agent_id,"owner_id":site.owner_id,"revision":site.revision,"revoked":site.revoked,
        "configuration":site.configuration,"runtime":site.runtime,
        "github_configured":!credentials.github.is_empty(),"cloudflare_configured":!credentials.cloudflare.is_empty(),"credential_requests":vault_requests(site),"github_credential_revision":credentials.github_revision,"cloudflare_credential_revision":credentials.cloudflare_revision})
}
pub async fn list(store: &Store, agent: &str) -> Result<Value> {
    anyhow::ensure!(identifier(agent), "Invalid agent");
    let keys: Vec<String> =
        sqlx::query_scalar("SELECT key FROM docs WHERE key LIKE ? ORDER BY key LIMIT 33")
            .bind(format!("{PREFIX}site:{agent}:%"))
            .fetch_all(&store.pool)
            .await?;
    anyhow::ensure!(keys.len() <= 32, "Remote site catalog exceeds bound");
    let mut sites = Vec::new();
    for key in keys {
        let site: Site = serde_json::from_value(
            store
                .get(&key)
                .await?
                .ok_or_else(|| anyhow!("Site catalog changed"))?,
        )?;
        anyhow::ensure!(
            site.owner_id == store.current_user_id() && site.agent_id == agent,
            "Invalid site owner"
        );
        let secret = credentials(store, &site).await?;
        sites.push(projection(&site, &secret));
    }
    Ok(json!({"agent_id":agent,"sites":sites}))
}
pub async fn save(store: &Store, agent: &str, id: &str, body: Save) -> Result<Value> {
    let _lock = lock(store, agent, id)?;
    body.configuration.validate()?;
    let old = store.get(&site_key(agent, id)).await?;
    if let Some(old) = &old {
        anyhow::ensure!(
            old["owner_id"] == store.current_user_id()
                && old["revision"].as_u64() == Some(body.expected_revision),
            "Site revision changed; reload before saving"
        );
    } else {
        anyhow::ensure!(body.expected_revision == 0, "Site not found");
        let catalog = list(store, agent).await?;
        anyhow::ensure!(
            catalog["sites"].as_array().is_some_and(|a| a.len() < 32),
            "Remote site limit reached"
        );
    }
    if let Some(old) = &old {
        anyhow::ensure!((old["runtime"]["active_receipt"].is_null()&&old["runtime"]["uncertain_receipt"].is_null())||old["configuration"]==serde_json::to_value(&body.configuration)?,"Reconcile the outstanding operation before changing site targets; credential rotation and revocation remain available");
        if old["runtime"]["provisioned"] == true {
            anyhow::ensure!(
                old["configuration"] == serde_json::to_value(&body.configuration)?,
                "Create a separate connection to change a provisioned site target"
            );
        }
    }
    let site = Site {
        id: id.into(),
        agent_id: agent.into(),
        owner_id: store.current_user_id().into(),
        revision: body
            .expected_revision
            .checked_add(1)
            .ok_or_else(|| anyhow!("Revision exhausted"))?,
        revoked: body.revoked,
        configuration: body.configuration,
        runtime: old
            .as_ref()
            .map(|v| v["runtime"].clone())
            .unwrap_or(Value::Null),
    };
    // Credentials have one writer: the existing nonce-bound private vault form.
    let mut tx = store.pool.begin().await?;
    sqlx::query("INSERT INTO docs(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value")
        .bind(site_key(agent,id)).bind(store.seal(&serde_json::to_value(&site)?)?).execute(&mut *tx).await?;
    tx.commit().await?;
    let secret = credentials(store, &site).await?;
    Ok(projection(&site, &secret))
}
