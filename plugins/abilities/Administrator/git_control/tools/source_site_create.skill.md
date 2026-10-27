# source_site_create

Use operation=configure first with site_id, request_id, connection_revision=0 (or the current revision when editing) and configuration holding github_owner, github_repo, worker_name and optional custom_domain. Cloudflare account ID and build-token UUID are optional and normally omitted: create discovers them with the saved Cloudflare token. No external repository or Worker is created during configure. Invalid input returns status failed with the reason and an example.

The response contains credential_requests (each with label, create_url and permissions) and setup_links. Show those links and permissions to the user, then activate Core Visualizer and call request_credential with one credential's name, origin, label and create_url at a time, waiting for check_credential before the next. Only the human private panel accepts token values; never include secrets in configuration or tool arguments.

After credentials are saved and Cloudflare's GitHub app is installed, use operation=create to create a private GitHub repository, static Cloudflare Worker, build triggers and the optional custom domain, or connect for an existing static site. Omit configuration for these operations and supply the current connection_revision. Reuse request_id for identical retries; reconcile uncertain external effects before new writes.
