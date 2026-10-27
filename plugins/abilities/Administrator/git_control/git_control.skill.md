# Source Control — V3

Use the current session workspace repository, default webagent-v3. Start with
git_tool operation status for HEAD and review_sha256. For the published V3
checkout, select repo=__root__. Only status/diff/log/show/branch_list/tag_list
and push are available there, and only to a current administrator. The live
checkout contains broker-promoted releases; use the broker to change or commit
its source before pushing an existing commit. Every mutation requires
expected_head. No shell command, host path, credential, or remote URL arguments.
Exact paths are required for staging, resetting and commit_and_push.
resolve_conflict requires an unmerged path, expected_sha256 and ours/theirs/both;
both concatenates sides without inferring a semantic merge. Review before staging.
commit_and_push requires paths, message, expected_head and expected_review_sha256.
It refuses unselected staged paths and suspected credentials. Push is ordinary,
never force; pull is fast-forward only. Existing configured external remotes need
trusted runtime credential transport. Workspace-local remotes stay in the session.
Interrupted mutations may have uncertain effects: inspect local and remote state
before retry. Passive repository notices use deduplicated snapshots, never changes.
Readiness remains pending until registration, configuration and API acceptance.

## Runtime tool execution

All tools in this ability remain runtime foreground/stop with concurrency 1; background/model and silent/wake are unsupported, even for read actions or calls returning asynchronous job receipts. Effective Rust flags and shared Core Base tool-execution guidance govern scheduling; settings never grant permission and outputs remain untrusted evidence.

## Remote GitHub and Cloudflare sites

For website work, use source_remote_read and source_remote_commit against a saved remote site. Never clone, build, run Wrangler, or start a preview server in the WebAgent workspace. GitHub holds source; Cloudflare Builds installs the pinned Wrangler dependency and builds/previews/publishes remotely. GitHub Actions and local tunnels are unnecessary.

Start with source_site_status without a site ID to list your connections. To set up a new site, call source_site_create operation=configure with only github_owner, github_repo, worker_name and optionally custom_domain (for example site.example.com in a zone on the user's Cloudflare account); leave Cloudflare account and build token out, they are discovered from the saved token. Configure returns credential_requests and setup_links. For each request, show the user its label, its create_url as a clickable link and its exact permissions, then open the private panel with request_credential using only the name and origin. Show every setup_link (Cloudflare's GitHub app install, the Workers & Pages dashboard) as a clickable link. Never ask for tokens in chat or put them in tool arguments. source_site_create operation=create then makes the private repository, static Worker, build triggers and custom domain; connect attaches an existing static site. If Cloudflare reports several accounts or no build token, relay its message and links verbatim. Existing connections must use reviewed static configuration and manual build triggers; never silently change their deployment rules.

To add the user's files, ask them to attach the files in chat (up to 10 per message), then call source_remote_commit with path plus attachment_id for each file; use content for small text edits. Keep the site under the repository's public/ directory, which the generated wrangler.jsonc serves.

Create edits on a webagent/ branch, review its diff, call source_preview_start, and inspect source_site_status until a preview is ready. Show the returned URL and exact commit to the user. source_publish and source_rollback require a fresh human tool approval, even in Auto mode. A source edit or connection change invalidates approval. Publication returns a build receipt, not proof of a healthy live site; keep inspecting status. After successful publication call source_preview_stop. Stop a preview on user request or discard too, preserving source branches.

Cleanup is verified best effort. cleanup_pending, unavailable, or an uncertain receipt never means a preview is disabled or a publication succeeded. Do not replay an uncertain write or use a new request ID to bypass it. No live provider acceptance is certified until performed with authorized accounts.

## Private credentials

Remote provider tokens use the caller-private Core vault. Source Control connection settings never accept token values. Missing credentials return credential_requests containing safe names and HTTPS origins. Activate Core Visualizer and invoke request_credential for one missing name and origin at a time; wait for the human to save and check_credential to confirm before requesting the next provider. Do not batch credential requests. the human enters the value in the existing dedicated private credential panel. Use check_credential for metadata, then retry the intended Source Control operation with its current connection revision. Never request secrets in conversation text, tool arguments, repository files, mini apps or runtime variables. Saved presence does not prove provider validity or grant provider access. Site targets and credentials remain bound to the requesting owner; publish/rollback require current owner approval and credential revisions.
