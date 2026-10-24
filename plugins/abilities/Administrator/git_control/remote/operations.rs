//! Remote-only effects. Every external write is preceded by a sealed intent receipt.
use super::field;
use super::{model::*, provider::*};
use crate::*;
use base64::Engine;
pub struct Operation<'a, T: Transport> {
    pub credential_digest: String,
    pub store: &'a Store,
    pub site: Site,
    pub transport: &'a T,
    pub receipt: Value,
    pub key: String,
}
impl<T: Transport> Operation<'_, T> {
    pub fn repo(&self) -> String {
        format!(
            "/repos/{}/{}",
            self.site.configuration.github_owner, self.site.configuration.github_repo
        )
    }
    pub fn account(&self) -> String {
        format!(
            "/client/v4/accounts/{}",
            self.site.configuration.cloudflare_account_id
        )
    }
    pub fn worker(&self) -> String {
        format!(
            "{}/workers/workers/{}",
            self.account(),
            self.site.configuration.worker_name
        )
    }
    pub async fn persist(&self) -> Result<()> {
        self.store.put(&self.key, &self.receipt).await
    }
    pub async fn remember(&mut self, key: &str, value: Value) -> Result<()> {
        self.site.runtime[key] = value;
        self.store
            .put(
                &site_key(&self.site.agent_id, &self.site.id),
                &serde_json::to_value(&self.site)?,
            )
            .await
    }
    pub async fn send(&mut self, request: ProviderRequest) -> Result<Reply> {
        let current = load(self.store, &self.site.agent_id, &self.site.id).await?;
        anyhow::ensure!(
            current.revision == self.site.revision,
            "Site connection changed before provider dispatch"
        );
        anyhow::ensure!(
            digest(&serde_json::to_value(
                credentials(self.store, &current).await?
            )?) == self.credential_digest,
            "Vault credential changed or was revoked before provider dispatch"
        );
        let account = self
            .store
            .account_by_id(self.store.current_user_id())
            .await?
            .ok_or_else(|| anyhow!("Account unavailable"))?;
        anyhow::ensure!(
            access::Principal::from_account(&account)?.active(),
            "Account access revoked before provider dispatch"
        );
        let write = request.method != "GET";
        if write {
            anyhow::ensure!(
                self.receipt["steps"]
                    .as_array()
                    .is_some_and(|a| a.len() < 256),
                "Operation write bound exceeded"
            );
            let intent = json!({"provider":format!("{:?}",request.provider),"method":request.method,"path":request.path,"body_sha256":digest(&request.body),"target_sha":request.body["sha"],"ref":request.body["ref"],"commit_hash":request.body["commit_hash"],"branch":request.body["branch"],"state":"possibly_sent"});
            self.receipt["steps"].as_array_mut().unwrap().push(intent);
            self.persist().await?;
        }
        let response = self.transport.request(request).await?;
        if write {
            let step = self.receipt["steps"]
                .as_array_mut()
                .unwrap()
                .last_mut()
                .unwrap();
            step["http_status"] = json!(response.status);
            step["state"] = json!(if (200..300).contains(&response.status) {
                "acknowledged"
            } else if (400..500).contains(&response.status) && response.status != 408 {
                "rejected"
            } else {
                "possibly_sent"
            });
            let value = response.value.get("result").unwrap_or(&response.value);
            step["identities"] = json!({"sha":value["sha"],"id":value["id"],"build_uuid":value["build_uuid"],"trigger_uuid":value["trigger_uuid"],"repo_connection_uuid":value["repo_connection_uuid"]});
            self.persist().await?;
        }
        Ok(response)
    }
    pub async fn call(&mut self, request: ProviderRequest) -> Result<Value> {
        success(self.send(request).await?)
    }
    pub async fn reference(&mut self, branch: &str) -> Result<Option<String>> {
        anyhow::ensure!(super::model::branch(branch), "Invalid branch");
        let response = self
            .send(ProviderRequest::gh(
                "GET",
                format!("{}/git/ref/heads/{branch}", self.repo()),
                Value::Null,
            ))
            .await?;
        if response.status == 404 {
            return Ok(None);
        }
        let response = success(response)?;
        let value = response["object"]["sha"]
            .as_str()
            .filter(|s| sha(s))
            .ok_or_else(|| anyhow!("GitHub returned an invalid branch identity"))?;
        Ok(Some(value.into()))
    }
    pub async fn read(&mut self, args: &Value) -> Result<Value> {
        let commit = field(args, "commit_sha", 40)?;
        anyhow::ensure!(sha(commit), "Exact commit SHA required");
        let root = self.repo();
        match args["operation"].as_str().unwrap_or("") {
            "list_files" => {
                let mut req =
                    ProviderRequest::gh("GET", format!("{root}/git/trees/{commit}"), Value::Null);
                req.query.push(("recursive".into(), "1".into()));
                let value = self.call(req).await?;
                anyhow::ensure!(
                    value["truncated"] != true,
                    "Repository tree exceeds remote read bound"
                );
                Ok(value)
            }
            "read_file" => {
                let file = field(args, "path", 512)?;
                anyhow::ensure!(path(file), "Invalid source path");
                let mut req =
                    ProviderRequest::gh("GET", format!("{root}/contents/{file}"), Value::Null);
                req.query.push(("ref".into(), commit.into()));
                let value = self.call(req).await?;
                anyhow::ensure!(
                    value["type"] == "file"
                        && value["encoding"] == "base64"
                        && value["size"].as_u64().is_some_and(|s| s <= 262144),
                    "Only bounded ordinary source files can be read"
                );
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(value["content"].as_str().unwrap_or("").replace('\n', ""))?;
                Ok(
                    json!({"path":file,"commit_sha":commit,"blob_sha":value["sha"],"content":String::from_utf8(bytes).map_err(|_|anyhow!("Only UTF-8 source files can be edited"))?,"source_kind":"untrusted_repository_source"}),
                )
            }
            "log" => {
                let mut req = ProviderRequest::gh("GET", format!("{root}/commits"), Value::Null);
                req.query = vec![
                    ("sha".into(), commit.into()),
                    ("per_page".into(), "20".into()),
                ];
                self.call(req).await
            }
            "diff" => {
                let base = field(args, "base_sha", 40)?;
                anyhow::ensure!(sha(base), "Exact base commit SHA required");
                self.call(ProviderRequest::gh(
                    "GET",
                    format!("{root}/compare/{base}...{commit}"),
                    Value::Null,
                ))
                .await
            }
            _ => anyhow::bail!("Unsupported remote read"),
        }
    }
    pub async fn commit(&mut self, args: &Value) -> Result<Value> {
        let branch = field(args, "branch", 200)?;
        let expected = field(args, "expected_head", 40)?;
        anyhow::ensure!(
            super::model::branch(branch)
                && branch.starts_with("webagent/")
                && branch != self.site.configuration.production_branch
                && sha(expected),
            "Use a webagent/ draft branch and exact expected HEAD"
        );
        let changes = args["changes"]
            .as_array()
            .filter(|a| !a.is_empty() && a.len() <= 64)
            .ok_or_else(|| anyhow!("Provide 1–64 exact file changes"))?;
        let mut seen = std::collections::BTreeSet::new();
        let mut bytes = 0;
        for change in changes {
            let file = field(change, "path", 512)?;
            anyhow::ensure!(
                path(file) && seen.insert(file),
                "Invalid or duplicate source path"
            );
            anyhow::ensure!(
                change
                    .as_object()
                    .is_some_and(|o| o.keys().all(|k| matches!(k.as_str(), "path" | "content"))),
                "Unsupported change fields"
            );
            anyhow::ensure!(
                change["content"].is_null() || change["content"].is_string(),
                "Content must be UTF-8 text or null for deletion"
            );
            let len = change["content"].as_str().map_or(0, str::len);
            bytes += len;
            anyhow::ensure!(
                len <= 262144 && bytes <= 1024 * 1024,
                "Source change exceeds bound"
            );
        }
        let message = field(args, "message", 500)?;
        let old = self.reference(branch).await?;
        let base = if let Some(old) = &old {
            old.clone()
        } else {
            self.reference(&self.site.configuration.production_branch.clone())
                .await?
                .ok_or_else(|| anyhow!("Production branch not found"))?
        };
        anyhow::ensure!(
            base == expected,
            "Branch changed; read and review the latest commit"
        );
        let commit = self
            .call(ProviderRequest::gh(
                "GET",
                format!("{}/git/commits/{expected}", self.repo()),
                Value::Null,
            ))
            .await?;
        let base_tree = commit["tree"]["sha"]
            .as_str()
            .filter(|s| sha(s))
            .ok_or_else(|| anyhow!("Invalid base tree"))?
            .to_owned();
        let mut tree = Vec::new();
        for change in changes {
            let blob = if let Some(content) = change["content"].as_str() {
                self.call(ProviderRequest::gh(
                    "POST",
                    format!("{}/git/blobs", self.repo()),
                    json!({"content":content,"encoding":"utf-8"}),
                ))
                .await?["sha"]
                    .clone()
            } else {
                Value::Null
            };
            tree.push(json!({"path":change["path"],"mode":"100644","type":"blob","sha":blob}));
        }
        let created = self
            .call(ProviderRequest::gh(
                "POST",
                format!("{}/git/trees", self.repo()),
                json!({"base_tree":base_tree,"tree":tree}),
            ))
            .await?;
        let created = self
            .call(ProviderRequest::gh(
                "POST",
                format!("{}/git/commits", self.repo()),
                json!({"message":message,"tree":created["sha"],"parents":[expected]}),
            ))
            .await?;
        let candidate = created["sha"]
            .as_str()
            .filter(|s| sha(s))
            .ok_or_else(|| anyhow!("Invalid created commit"))?
            .to_owned();
        self.receipt["candidate_sha"] = json!(candidate);
        self.persist().await?;
        if old.is_some() {
            self.call(ProviderRequest::gh(
                "PATCH",
                format!("{}/git/refs/heads/{branch}", self.repo()),
                json!({"sha":candidate,"force":false}),
            ))
            .await?;
        } else {
            self.call(ProviderRequest::gh(
                "POST",
                format!("{}/git/refs", self.repo()),
                json!({"ref":format!("refs/heads/{branch}"),"sha":candidate}),
            ))
            .await?;
        }
        Ok(
            json!({"branch":branch,"commit_sha":candidate,"previous_head":expected,"production_unchanged":true}),
        )
    }
    pub async fn verify_triggers(&mut self) -> Result<()> {
        let tag = self.site.runtime["worker_tag"]
            .as_str()
            .filter(|s| identifier(s))
            .ok_or_else(|| anyhow!("Worker tag unavailable"))?
            .to_owned();
        let inventory = self
            .call(ProviderRequest::cf(
                "GET",
                format!("{}/builds/workers/{tag}/triggers", self.account()),
                Value::Null,
            ))
            .await?;
        let rows = inventory
            .as_array()
            .or_else(|| inventory["triggers"].as_array())
            .ok_or_else(|| anyhow!("Build trigger inventory unavailable"))?;
        anyhow::ensure!(rows.len() <= 2, "Unexpected build trigger inventory");
        for key in ["preview_trigger", "production_trigger"] {
            let id = self.site.runtime[key]
                .as_str()
                .filter(|s| uuid::Uuid::parse_str(s).is_ok())
                .ok_or_else(|| {
                    anyhow!("Connect or create this site's remote build triggers first")
                })?
                .to_owned();
            let trigger = rows
                .iter()
                .find(|t| t["trigger_uuid"] == id)
                .ok_or_else(|| anyhow!("Configured build trigger was removed"))?
                .clone();
            anyhow::ensure!(
                trigger["branch_excludes"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|v| v == "*")),
                "Automatic builds are not disabled; publication refused"
            );
            let expected = if key == "preview_trigger" {
                PREVIEW_COMMAND
            } else {
                DEPLOY_COMMAND
            };
            anyhow::ensure!(
                trigger["deploy_command"] == expected
                    && trigger["build_command"] == self.site.configuration.build_command
                    && trigger["root_directory"] == self.site.configuration.root_directory,
                "Remote build configuration changed; reconnect and review"
            );
            let variables = self
                .call(ProviderRequest::cf(
                    "GET",
                    format!(
                        "{}/builds/triggers/{id}/environment_variables",
                        self.account()
                    ),
                    Value::Null,
                ))
                .await?;
            anyhow::ensure!(variables["SKIP_DEPENDENCY_INSTALL"]["value"]=="true"&&variables.as_object().is_some_and(|o|o.len()==1),"Remote build environment must disable automatic dependency installation and contain no custom variables");
            let repo = &trigger["repo_connection"];
            anyhow::ensure!(
                repo["provider_type"] == "github"
                    && repo["provider_account_name"] == self.site.configuration.github_owner
                    && repo["repo_name"] == self.site.configuration.github_repo,
                "Build repository connection changed"
            );
        }
        Ok(())
    }
    pub async fn start_build(&mut self, kind: &str, branch: &str, commit: &str) -> Result<Value> {
        self.validate_static(commit).await?;
        self.verify_triggers().await?;
        let trigger = self.site.runtime[format!("{kind}_trigger")]
            .as_str()
            .ok_or_else(|| anyhow!("Remote build trigger unavailable"))?
            .to_owned();
        let value = self
            .call(ProviderRequest::cf(
                "POST",
                format!("{}/builds/triggers/{trigger}/builds", self.account()),
                json!({"branch":branch,"commit_hash":commit}),
            ))
            .await?;
        let id = value["build_uuid"]
            .as_str()
            .filter(|s| uuid::Uuid::parse_str(s).is_ok())
            .ok_or_else(|| anyhow!("Build response lacks its durable ID"))?
            .to_owned();
        let record = json!({"build_id":id,"commit_sha":commit,"branch":branch,"status":"queued","connection_revision":self.site.revision,"preview_name":format!("wa-{commit}")});
        self.remember(kind, record.clone()).await?;
        Ok(record)
    }
    pub async fn preview(&mut self, args: &Value) -> Result<Value> {
        let branch = field(args, "branch", 200)?;
        let commit = field(args, "commit_sha", 40)?;
        anyhow::ensure!(
            branch.starts_with("webagent/") && super::model::branch(branch) && sha(commit),
            "Exact draft branch and commit required"
        );
        anyhow::ensure!(
            self.reference(branch).await?.as_deref() == Some(commit),
            "Draft branch changed; preview its latest commit"
        );
        if let Some(old) = self.site.runtime["preview"]["status"].as_str() {
            anyhow::ensure!(
                matches!(old, "disabled" | "failed" | "cancelled"),
                "Stop or inspect the existing preview first"
            );
        }
        self.start_build("preview", branch, commit).await
    }
    pub async fn poll(&mut self, kind: &str) -> Result<Value> {
        let mut record = self.site.runtime[kind].clone();
        if kind == "preview"
            && matches!(
                record["status"].as_str(),
                Some("disabled" | "cleanup_pending" | "cleanup_failed")
            )
        {
            return Ok(record);
        }
        let Some(id) = record["build_id"].as_str().map(str::to_owned) else {
            return Ok(record);
        };
        let build = self
            .call(ProviderRequest::cf(
                "GET",
                format!("{}/builds/builds/{id}", self.account()),
                Value::Null,
            ))
            .await?;
        anyhow::ensure!(
            build["build_trigger_metadata"]["commit_hash"] == record["commit_sha"],
            "Build source identity mismatch"
        );
        record["status"] = if build["status"] == "stopped" {
            match build["build_outcome"].as_str() {
                Some("success") => json!("ready"),
                Some("cancelled") => json!("cancelled"),
                _ => json!("failed"),
            }
        } else {
            build["status"].clone()
        };
        if let Some(url) = build["preview_url"].as_str() {
            anyhow::ensure!(preview_url(url), "Unexpected preview URL");
            record["url"] = json!(url);
        }
        record["build_outcome"] = build["build_outcome"].clone();
        if kind == "production" && record["status"] == "ready" {
            let deployed = self
                .call(ProviderRequest::cf(
                    "GET",
                    format!(
                        "{}/workers/scripts/{}/deployments",
                        self.account(),
                        self.site.configuration.worker_name
                    ),
                    Value::Null,
                ))
                .await?;
            let active = &deployed["deployments"][0];
            anyhow::ensure!(
                active["versions"]
                    .as_array()
                    .is_some_and(|v| v.len() == 1 && v[0]["percentage"] == 100),
                "Production is not on a single full-traffic version"
            );
            let version = active["versions"][0]["version_id"]
                .as_str()
                .filter(|s| uuid::Uuid::parse_str(s).is_ok())
                .ok_or_else(|| anyhow!("Active version identity unavailable"))?
                .to_owned();
            let details = self
                .call(ProviderRequest::cf(
                    "GET",
                    format!("{}/versions/{version}", self.worker()),
                    Value::Null,
                ))
                .await?;
            anyhow::ensure!(
                details["annotations"]["workers/tag"] == record["commit_sha"],
                "Active deployment does not match the requested commit"
            );
            let account = self
                .call(ProviderRequest::cf(
                    "GET",
                    format!("{}/workers/subdomain", self.account()),
                    Value::Null,
                ))
                .await?;
            let subdomain = account["subdomain"]
                .as_str()
                .filter(|s| identifier(s))
                .ok_or_else(|| anyhow!("Worker subdomain unavailable"))?;
            let url = format!(
                "https://{}.{subdomain}.workers.dev/",
                self.site.configuration.worker_name
            );
            anyhow::ensure!(
                self.transport.healthy(&url).await?,
                "Live site did not pass its health check"
            );
            record["status"] = json!("verified");
            record["deployment_id"] = active["id"].clone();
            record["version_id"] = json!(version);
            record["url"] = json!(url);
        }
        self.remember(kind, record.clone()).await?;
        Ok(record)
    }
    pub async fn stop(&mut self) -> Result<Value> {
        let mut preview = self.poll("preview").await?;
        if preview.is_null() {
            return Ok(json!({"status":"disabled","reason":"No preview recorded"}));
        }
        let id = preview["build_id"]
            .as_str()
            .ok_or_else(|| anyhow!("Preview build identity missing"))?
            .to_owned();
        if !matches!(
            preview["status"].as_str(),
            Some(
                "ready"
                    | "failed"
                    | "cancelled"
                    | "disabled"
                    | "cleanup_pending"
                    | "cleanup_failed"
            )
        ) {
            self.call(ProviderRequest::cf(
                "PUT",
                format!("{}/builds/builds/{id}/cancel", self.account()),
                json!({}),
            ))
            .await?;
            preview = self.poll("preview").await?;
            if !matches!(
                preview["status"].as_str(),
                Some("ready" | "failed" | "cancelled")
            ) {
                preview["cleanup_requested"] = json!(true);
                self.remember("preview", preview).await?;
                return Ok(
                    json!({"status":"cleanup_pending","build_id":id,"reason":"Waiting for build cancellation before deletion"}),
                );
            }
        }
        let name = preview["preview_name"]
            .as_str()
            .filter(|s| identifier(s))
            .ok_or_else(|| anyhow!("Recorded preview identity missing"))?
            .to_owned();
        let mut urls = std::collections::BTreeSet::new();
        if let Some(url) = preview["url"].as_str() {
            urls.insert(url.to_owned());
        }
        if let Some(old) = preview["verified_urls"].as_array() {
            for url in old.iter().filter_map(Value::as_str) {
                urls.insert(url.to_owned());
            }
        }
        let resource = self
            .send(ProviderRequest::cf(
                "GET",
                format!("{}/previews/{name}", self.worker()),
                Value::Null,
            ))
            .await?;
        if resource.status != 404 {
            collect_urls(&success(resource)?, &mut urls)?;
            let mut listing = ProviderRequest::cf(
                "GET",
                format!("{}/previews/{name}/deployments", self.worker()),
                Value::Null,
            );
            listing.query.push(("per_page".into(), "100".into()));
            let deployments = self.call(listing).await?;
            collect_urls(&deployments, &mut urls)?;
        }
        preview["verified_urls"] = json!(urls);
        self.remember("preview", preview.clone()).await?;
        let response = self
            .send(ProviderRequest::cf(
                "DELETE",
                format!("{}/previews/{name}", self.worker()),
                Value::Null,
            ))
            .await?;
        if response.status != 404 {
            success(response)?;
        }
        let absent = self
            .send(ProviderRequest::cf(
                "GET",
                format!("{}/previews/{name}", self.worker()),
                Value::Null,
            ))
            .await?
            .status
            == 404;
        let mut disabled = absent;
        for url in &urls {
            disabled &= self
                .transport
                .reachable(url)
                .await
                .is_ok_and(|serving| !serving);
        }
        preview["status"] = json!(if disabled {
            "disabled"
        } else {
            "cleanup_pending"
        });
        preview["verified_urls"] = json!(urls);
        self.remember("preview", preview.clone()).await?;
        Ok(preview)
    }
    pub async fn publish(&mut self, args: &Value) -> Result<Value> {
        let commit = field(args, "commit_sha", 40)?;
        let base = field(args, "expected_production_sha", 40)?;
        anyhow::ensure!(sha(commit) && sha(base), "Exact source identities required");
        let preview = self.poll("preview").await?;
        anyhow::ensure!(
            preview["status"] == "ready"
                && preview["commit_sha"] == commit
                && preview["connection_revision"] == self.site.revision,
            "Preview or connection changed; review a fresh preview"
        );
        let branch = preview["branch"]
            .as_str()
            .ok_or_else(|| anyhow!("Preview branch missing"))?;
        anyhow::ensure!(
            self.reference(branch).await?.as_deref() == Some(commit),
            "Draft changed after preview"
        );
        let main = self.site.configuration.production_branch.clone();
        anyhow::ensure!(
            self.reference(&main).await?.as_deref() == Some(base),
            "Production branch changed; review before publishing"
        );
        self.verify_triggers().await?;
        let deployment = self
            .call(ProviderRequest::cf(
                "GET",
                format!(
                    "{}/workers/scripts/{}/deployments",
                    self.account(),
                    self.site.configuration.worker_name
                ),
                Value::Null,
            ))
            .await?;
        let previous = deployment["deployments"]
            .as_array()
            .and_then(|a| a.first())
            .cloned()
            .unwrap_or(Value::Null);
        self.remember("rollback_target", previous).await?;
        // Non-force update rejects divergence; the following build remains pinned to the approved SHA.
        self.call(ProviderRequest::gh(
            "PATCH",
            format!("{}/git/refs/heads/{main}", self.repo()),
            json!({"sha":commit,"force":false}),
        ))
        .await?;
        self.remember(
            "production_source",
            json!({"commit_sha":commit,"status":"source_updated_deployment_pending"}),
        )
        .await?;
        self.start_build("production", &main, commit).await
    }
    pub async fn rollback(&mut self, args: &Value) -> Result<Value> {
        let version = field(args, "version_id", 80)?;
        anyhow::ensure!(
            uuid::Uuid::parse_str(version).is_ok(),
            "Exact version ID required"
        );
        let target = self.site.runtime["rollback_target"].clone();
        anyhow::ensure!(
            target["versions"]
                .as_array()
                .is_some_and(|a| a.len() == 1 && a[0]["version_id"] == version),
            "Rollback must select the recorded single-version deployment"
        );
        let current = self
            .call(ProviderRequest::cf(
                "GET",
                format!(
                    "{}/workers/scripts/{}/deployments",
                    self.account(),
                    self.site.configuration.worker_name
                ),
                Value::Null,
            ))
            .await?;
        anyhow::ensure!(
            current["deployments"][0]["id"] == args["expected_deployment_id"],
            "Production deployment changed; review rollback again"
        );
        let result=self.call(ProviderRequest::cf("POST",format!("{}/workers/scripts/{}/deployments",self.account(),self.site.configuration.worker_name),json!({"strategy":"percentage","versions":[{"version_id":version,"percentage":100}]}))).await?;
        self.remember("rollback", result.clone()).await?;
        Ok(result)
    }
}

impl<T: Transport> Operation<'_, T> {
    pub async fn reconcile_previous(&mut self) -> Result<()> {
        let request = self.site.runtime["uncertain_receipt"]
            .as_str()
            .or_else(|| self.site.runtime["active_receipt"].as_str())
            .map(str::to_owned);
        let Some(request) = request else {
            return Ok(());
        };
        let key = receipt_key(&self.site.agent_id, &self.site.id, &request);
        let Some(mut old) = self.store.get(&key).await? else {
            // No external call can precede its persisted intent. A missing receipt is pre-dispatch.
            self.remember("active_receipt", Value::Null).await?;
            return Ok(());
        };
        let mut checks = Vec::new();
        let mut unresolved = false;
        for step in old["steps"].as_array().into_iter().flatten() {
            if step["state"] != "possibly_sent" {
                continue;
            }
            let target = step["path"]
                .as_str()
                .ok_or_else(|| anyhow!("Malformed operation receipt"))?;
            let method = step["method"].as_str().unwrap_or("");
            let confirmed = if step["provider"] == "Github"
                && method == "POST"
                && ["/git/blobs", "/git/trees", "/git/commits"]
                    .iter()
                    .any(|s| target.ends_with(s))
            {
                // Immutable orphan objects cannot advance a branch or publish source.
                true
            } else if step["provider"] == "Github"
                && (target.contains("/git/refs/heads/") || target.ends_with("/git/refs"))
            {
                let endpoint = if target.ends_with("/git/refs") {
                    format!(
                        "{}/git/ref/{}",
                        self.repo(),
                        step["ref"]
                            .as_str()
                            .unwrap_or("")
                            .strip_prefix("refs/")
                            .unwrap_or("")
                    )
                } else {
                    target.replace("/git/refs/heads/", "/git/ref/heads/")
                };
                let result = self
                    .send(ProviderRequest::gh("GET", endpoint, Value::Null))
                    .await?;
                result.status == 200 && result.value["object"]["sha"] == step["target_sha"]
            } else if step["provider"] == "Cloudflare"
                && method == "DELETE"
                && target.contains("/previews/")
            {
                self.send(ProviderRequest::cf("GET", target.into(), Value::Null))
                    .await?
                    .status
                    == 404
            } else if step["provider"] == "Cloudflare"
                && method == "POST"
                && target.contains("/builds/triggers/")
                && target.ends_with("/builds")
            {
                let tag = self.site.runtime["worker_tag"]
                    .as_str()
                    .ok_or_else(|| anyhow!("Worker tag missing for build reconciliation"))?
                    .to_owned();
                let mut req = ProviderRequest::cf(
                    "GET",
                    format!("{}/builds/workers/{tag}/builds", self.account()),
                    Value::Null,
                );
                req.query.push(("per_page".into(), "50".into()));
                let value = self.call(req).await?;
                let matches: Vec<_> = value
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|v| {
                        v["build_trigger_metadata"]["commit_hash"] == step["commit_hash"]
                            && v["build_trigger_metadata"]["branch"] == step["branch"]
                            && v["build_trigger_metadata"]["build_trigger_source"] == "api"
                            && v["trigger"]["trigger_uuid"]
                                .as_str()
                                .is_some_and(|id| target.ends_with(&format!("/{id}/builds")))
                            && v["created_on"]
                                .as_str()
                                .and_then(|date| chrono::DateTime::parse_from_rfc3339(date).ok())
                                .is_some_and(|date| {
                                    old["started_at_ms"]
                                        .as_i64()
                                        .is_some_and(|start| date.timestamp_millis() >= start)
                                })
                            && v["build_uuid"].is_string()
                    })
                    .collect();
                if matches.len() == 1 {
                    let kind = if old["tool"] == "source_preview_start" {
                        "preview"
                    } else {
                        "production"
                    };
                    self.remember(kind,json!({"build_id":matches[0]["build_uuid"],"commit_sha":step["commit_hash"],"branch":step["branch"],"connection_revision":self.site.revision,"status":"queued","preview_name":format!("wa-{}",step["commit_hash"].as_str().unwrap_or(""))})).await?;
                    true
                } else {
                    false
                }
            } else {
                false
            };
            checks.push(json!({"method":method,"path":target,"effect_known":confirmed}));
            unresolved |= !confirmed;
        }
        old["reconciliation"] = json!({"status":if unresolved{"unresolved"}else{"known_partial_effects"},"checks":checks,"at":now(),"original_status_preserved":true});
        self.store.put(&key, &old).await?;
        if !unresolved {
            self.remember("last_reconciled_receipt", json!(request))
                .await?;
            self.remember("uncertain_receipt", Value::Null).await?;
            self.remember("active_receipt", Value::Null).await?;
        }
        Ok(())
    }
}
