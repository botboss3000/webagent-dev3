# Source Control — V3

Use the current session workspace repository, default webagent-v3. Start with
git_tool operation status for HEAD and review_sha256. For the published V3
checkout, select repo=__root__. Only status/diff/log/show/branch_list/tag_list
and push are available there, and only to a current administrator. The live
checkout contains broker-promoted releases; use the broker to change or commit
its source before pushing an existing commit. Every mutation requires
expected_head. No shell command, host path, credential, or remote URL arguments.
Review before staging. Push is ordinary, never force; pull is fast-forward only. Existing configured external remotes need
trusted runtime credential transport. Workspace-local remotes stay in the session.
Interrupted mutations may have uncertain effects: inspect local and remote state
before retry. Passive repository notices use deduplicated snapshots, never changes.
Readiness remains pending until registration, configuration and API acceptance.


## Remote GitHub and Cloudflare sites

For website work, use source_remote_read and source_remote_commit against a saved remote site. Never clone, build, run Wrangler, or start a preview server in the WebAgent workspace. GitHub holds source; Cloudflare Builds installs the pinned Wrangler dependency and builds/previews/publishes remotely. GitHub Actions and local tunnels are unnecessary.

Start with source_site_status without a site ID to list your connections. Set up a new site with source_site_create: configure, then the credential cards and setup links it returns, then create (or connect for an existing static site). Never ask for tokens in chat or put them in tool arguments. If Cloudflare reports several accounts or no build token, relay its message and links verbatim. Existing connections must use reviewed static configuration and manual build triggers; never silently change their deployment rules.

To add the user's files, ask them to attach the files in chat (up to 10 per message) and commit them with source_remote_commit. Keep the site under the repository's public/ directory, which the generated wrangler.jsonc serves.

Create edits on a webagent/ branch, review its diff, call source_preview_start, and inspect source_site_status until a preview is ready. Show the returned URL and exact commit to the user. Publication returns a build receipt, not proof of a healthy live site; keep inspecting status. After successful publication call source_preview_stop. Stop a preview on user request or discard too, preserving source branches.

Cleanup is verified best effort. cleanup_pending, unavailable, or an uncertain receipt never means a preview is disabled or a publication succeeded. Do not replay an uncertain write or use a new request ID to bypass it. No live provider acceptance is certified until performed with authorized accounts.

## Private credentials

Remote provider tokens use the caller-private credential vault. Source Control connection settings never accept token values. Missing credentials return credential_requests containing safe names and HTTPS origins. Invoke the Base tool request_credential for one missing credential at a time (name, origin, label, create_url); wait for the human to save and check_credential to confirm before requesting the next provider. Do not batch credential requests. the human enters the value in the existing dedicated private credential panel. Use check_credential for metadata, then retry the intended Source Control operation with its current connection revision. Never request secrets in conversation text, tool arguments, repository files or runtime variables. Saved presence does not prove provider validity or grant provider access. Site targets and credentials remain bound to the requesting owner.
