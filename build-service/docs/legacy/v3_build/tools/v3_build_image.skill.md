# v3_build_image

Provide stable `request_id` (8–80 letters, digits, hyphens or underscores) and the 64-character `expected_inputs_sha256` from status. Returns an asynchronous build receipt. The broker builds both API and loop from one frozen allowlisted tar, with fixed Dockerfiles and generated job tags; no command, image tag or path arguments are accepted. Poll the returned job ID. A raced build is marked stale and cannot deploy.

The asynchronous receipt is a broker job ID, not a generic runtime background task handle. This tool remains foreground/stop with concurrency 1; _execution_mode cannot make it background. Follow the explicit receipt polling and reconciliation contract above. Settings never authorize a build or deployment.
