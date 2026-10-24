//! Provision only new resources; existing installations are connected after explicit configuration review.
use super::{model::*, operations::Operation, provider::*};
use crate::*;
impl<T: Transport> Operation<'_, T> {
    pub async fn validate_static(&mut self, commit: &str) -> Result<()> {
        anyhow::ensure!(sha(commit), "Exact source SHA required");
        let cfg = self.site.configuration.clone();
        let tree = self
            .read(&json!({"operation":"list_files","commit_sha":commit}))
            .await?;
        let entries = tree["tree"]
            .as_array()
            .ok_or_else(|| anyhow!("Source inventory unavailable"))?;
        anyhow::ensure!(
            entries.len() <= 20000,
            "Static source inventory exceeds bound"
        );
        for entry in entries {
            let path = entry["path"]
                .as_str()
                .ok_or_else(|| anyhow!("Invalid source inventory"))?;
            anyhow::ensure!(
                entry["mode"] != "120000" && entry["mode"] != "160000",
                "Symlinks and submodules are outside static-site V1"
            );
            anyhow::ensure!(!path.split('/').any(|p|p=="node_modules"||p==".npmrc"||p==".env"||p.starts_with(".env.")),"Static remote builds cannot consume source-supplied runtime credentials or npm configuration");
        }
        let config_path = if cfg.root_directory == "/" {
            "wrangler.jsonc".into()
        } else {
            format!("{}/wrangler.jsonc", cfg.root_directory)
        };
        let source = self
            .read(&json!({"operation":"read_file","commit_sha":commit,"path":config_path}))
            .await?;
        let config: Value = serde_json::from_str(source["content"].as_str().unwrap_or(""))
            .map_err(|_| anyhow!("V1 requires strict-JSON wrangler.jsonc"))?;
        anyhow::ensure!(
            config["name"] == cfg.worker_name
                && config
                    .as_object()
                    .is_some_and(|o| o.keys().all(|k| matches!(
                        k.as_str(),
                        "$schema"
                            | "name"
                            | "compatibility_date"
                            | "assets"
                            | "previews"
                            | "workers_dev"
                    ))),
            "Only the configured static Worker can be previewed or deployed"
        );
        anyhow::ensure!(
            config["previews"].as_object().is_some_and(|o| o.is_empty()),
            "Preview bindings or backend execution are outside static-site V1"
        );
        let assets = &config["assets"];
        anyhow::ensure!(
            assets
                .as_object()
                .is_some_and(|o| o.keys().all(|k| matches!(
                    k.as_str(),
                    "directory" | "not_found_handling" | "html_handling"
                )))
                && assets["directory"]
                    .as_str()
                    .is_some_and(|p| super::model::path(p.strip_prefix("./").unwrap_or(p))),
            "Static assets must come from the repository with no Worker-first execution"
        );
        let directory = assets["directory"]
            .as_str()
            .unwrap()
            .trim_start_matches("./");
        let index = if cfg.root_directory == "/" {
            format!("{directory}/index.html")
        } else {
            format!("{}/{directory}/index.html", cfg.root_directory)
        };
        anyhow::ensure!(
            entries
                .iter()
                .any(|e| e["path"] == index && e["type"] == "blob"),
            "Static site must contain its assets index.html"
        );
        Ok(())
    }
    pub async fn provision(&mut self, connect: bool) -> Result<Value> {
        let cfg = self.site.configuration.clone();
        let repo = self
            .send(ProviderRequest::gh("GET", self.repo(), Value::Null))
            .await?;
        let worker = self
            .send(ProviderRequest::cf("GET", self.worker(), Value::Null))
            .await?;
        if connect {
            let repository = success(repo)?;
            let worker = success(worker)?;
            self.remember(
                "worker_tag",
                worker
                    .get("tag")
                    .cloned()
                    .or_else(|| worker.get("id").cloned())
                    .ok_or_else(|| anyhow!("Worker tag missing"))?,
            )
            .await?;
            let head = self
                .reference(&cfg.production_branch)
                .await?
                .ok_or_else(|| anyhow!("Production branch unavailable"))?;
            let content=self.read(&json!({"operation":"read_file","commit_sha":head,"path":if cfg.root_directory=="/"{"wrangler.jsonc".into()}else{format!("{}/wrangler.jsonc",cfg.root_directory)}})).await?;
            // V1 deliberately accepts strict JSON, which is also valid JSONC. Dynamic executable configuration is outside the static-site contract.
            let config: Value = serde_json::from_str(content["content"].as_str().unwrap_or(""))
                .map_err(|_| {
                    anyhow!("V1 requires a strict-JSON wrangler.jsonc static configuration")
                })?;
            anyhow::ensure!(
                config["name"] == cfg.worker_name
                    && config.get("assets").is_some()
                    && config.get("previews").is_some(),
                "Static assets and preview configuration required"
            );
            anyhow::ensure!(
                config
                    .as_object()
                    .is_some_and(|o| o.keys().all(|k| matches!(
                        k.as_str(),
                        "$schema"
                            | "name"
                            | "compatibility_date"
                            | "assets"
                            | "previews"
                            | "workers_dev"
                    ))),
                "Backend bindings or executable Worker configuration are outside V1"
            );
            anyhow::ensure!(
                config["previews"].as_object().is_some_and(|o| o.is_empty())
                    && config["assets"]["directory"]
                        .as_str()
                        .is_some_and(|p| path(p.strip_prefix("./").unwrap_or(p))),
                "V1 requires static-only previews and a repository-relative assets directory"
            );
            let metadata = self
                .call(ProviderRequest::cf(
                    "GET",
                    format!(
                        "{}/builds/workers/{}/triggers",
                        self.account(),
                        self.site.runtime["worker_tag"].as_str().unwrap_or("")
                    ),
                    Value::Null,
                ))
                .await?;
            let rows = metadata
                .as_array()
                .or_else(|| metadata["triggers"].as_array())
                .ok_or_else(|| anyhow!("Build trigger inventory unavailable"))?;
            anyhow::ensure!(rows.len() <= 2, "Unexpected build trigger inventory");
            if rows.is_empty() {
                let tag = self.site.runtime["worker_tag"]
                    .as_str()
                    .unwrap_or("")
                    .to_owned();
                self.install_triggers(&tag, &repository).await?;
            }
            for trigger in rows {
                let key = if trigger["deploy_command"] == PREVIEW_COMMAND {
                    "preview_trigger"
                } else if trigger["deploy_command"] == DEPLOY_COMMAND {
                    "production_trigger"
                } else {
                    anyhow::bail!("Existing build commands require explicit setup review; not changed automatically")
                };
                let id = trigger["trigger_uuid"]
                    .as_str()
                    .ok_or_else(|| anyhow!("Trigger identity missing"))?
                    .to_owned();
                self.remember(key, json!(id)).await?;
            }
            self.validate_static(&head).await?;
            self.verify_triggers().await?;
        } else {
            anyhow::ensure!(repo.status==404&&worker.status==404,"Site creation requires unused repository and Worker names; connect existing resources instead");
            let user = self
                .call(ProviderRequest::gh("GET", "/user".into(), Value::Null))
                .await?;
            let endpoint = if user["login"]
                .as_str()
                .is_some_and(|u| u.eq_ignore_ascii_case(&cfg.github_owner))
            {
                "/user/repos".into()
            } else {
                format!("/orgs/{}/repos", cfg.github_owner)
            };
            self.call(ProviderRequest::gh("POST",endpoint,json!({"name":cfg.github_repo,"private":true,"auto_init":true,"description":"Website managed through WebAgent Source Control"}))).await?;
            let repository = self
                .call(ProviderRequest::gh("GET", self.repo(), Value::Null))
                .await?;
            let initial = repository["default_branch"]
                .as_str()
                .ok_or_else(|| anyhow!("New repository branch unavailable"))?;
            let head = self
                .reference(initial)
                .await?
                .ok_or_else(|| anyhow!("Repository initialization pending; inspect receipt"))?;
            if initial != cfg.production_branch {
                self.call(ProviderRequest::gh(
                    "POST",
                    format!("{}/git/refs", self.repo()),
                    json!({"ref":format!("refs/heads/{}",cfg.production_branch),"sha":head}),
                ))
                .await?;
                self.call(ProviderRequest::gh(
                    "PATCH",
                    self.repo(),
                    json!({"default_branch":cfg.production_branch}),
                ))
                .await?;
            }
            let changes = json!([
                {"path":"public/index.html","content":"<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>New website</title><link rel=\"stylesheet\" href=\"/style.css\"></head><body><main><h1>Your new website</h1><p>Ready for your changes.</p></main></body></html>\n"},
                {"path":"public/style.css","content":"body{font:1rem/1.6 system-ui,sans-serif;max-width:60rem;margin:4rem auto;padding:0 1rem;color:#172033;background:#f8fafc}\n"},
                {"path":"wrangler.jsonc","content":format!("{}\n",serde_json::to_string_pretty(&json!({"name":cfg.worker_name,"compatibility_date":"2026-10-01","assets":{"directory":"./public"},"previews":{},"workers_dev":true}))?)}
            ]);
            let candidate=self.commit(&json!({"branch":"webagent/initial-site","expected_head":head,"message":"Create static website scaffold","changes":changes})).await?;
            self.remember("initial_candidate", candidate).await?;
            let mut upload = ProviderRequest::cf(
                "PUT",
                format!("{}/workers/scripts/{}", self.account(), cfg.worker_name),
                Value::Null,
            );
            upload.multipart = true;
            let created = self.call(upload).await?;
            let tag = created["tag"]
                .as_str()
                .ok_or_else(|| anyhow!("Created Worker tag unavailable"))?
                .to_owned();
            self.remember("worker_tag", json!(tag)).await?;
            self.install_triggers(&tag, &repository).await?;
            self.verify_triggers().await?;
        }
        self.remember("provisioned", json!(true)).await?;
        Ok(
            json!({"status":"configured","site_id":self.site.id,"runtime":self.site.runtime,"live_provider_acceptance":"not_certified"}),
        )
    }
    async fn install_triggers(&mut self, tag: &str, repository: &Value) -> Result<()> {
        let cfg = self.site.configuration.clone();
        let owner = self
            .call(ProviderRequest::gh(
                "GET",
                format!("/users/{}", cfg.github_owner),
                Value::Null,
            ))
            .await?;
        let connection=self.call(ProviderRequest::cf("PUT",format!("{}/builds/repos/connections",self.account()),json!({"provider_type":"github","provider_account_id":owner["id"].as_u64().map(|n|n.to_string()),"provider_account_name":cfg.github_owner,"repo_id":repository["id"].as_u64().map(|n|n.to_string()),"repo_name":cfg.github_repo}))).await?;
        let connection=connection["repo_connection_uuid"].as_str().ok_or_else(||anyhow!("Authorize the repository in Cloudflare's GitHub App, then inspect this receipt"))?.to_owned();
        for (key, command) in [
            ("preview_trigger", PREVIEW_COMMAND),
            ("production_trigger", DEPLOY_COMMAND),
        ] {
            let trigger=self.call(ProviderRequest::cf("POST",format!("{}/builds/triggers",self.account()),json!({"external_script_id":tag,"repo_connection_uuid":connection,"build_token_uuid":cfg.build_token_uuid,"trigger_name":format!("WebAgent {key}"),"build_command":cfg.build_command,"deploy_command":command,"root_directory":cfg.root_directory,"branch_includes":["*"],"branch_excludes":["*"],"path_includes":["*"],"path_excludes":[]}))).await?;
            let id = trigger["trigger_uuid"]
                .as_str()
                .filter(|s| uuid::Uuid::parse_str(s).is_ok())
                .ok_or_else(|| anyhow!("Build trigger identity unavailable"))?
                .to_owned();
            self.remember(key, json!(id)).await?;
            self.call(ProviderRequest::cf(
                "PATCH",
                format!(
                    "{}/builds/triggers/{id}/environment_variables",
                    self.account()
                ),
                json!({"SKIP_DEPENDENCY_INSTALL":{"value":"true","is_secret":false}}),
            ))
            .await?;
        }
        Ok(())
    }
}
