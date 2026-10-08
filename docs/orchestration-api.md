# V3 orchestration API

Status: implemented in the v3 development release. The run-log and
single-build-result routes passed authenticated checks at the v3 origin.
The current public development URL is `https://dev.webagent.live`. This API
does not expose SQLite files, encryption keys, provider transcripts, or the
protected build broker socket.

## Authentication and authority

The general `/api/v3` routes use the existing browser session. Sign in with
`POST /api/v3/auth/login` using an approved account and a JSON body containing
`username` and `password`. Keep the returned `v3_session` cookie and
`csrf_token` private. Send the cookie on reads. Also send
`X-CSRF-Token: <csrf_token>` on writes. The cookie is Secure on the public
HTTPS origin; HTTP clients using the Tailscale origin must explicitly attach
it. Routes under `/api/v3/admin` additionally require live administrator
authority. Session routes check user ownership and the selected agent's live
access. A changed or revoked grant can make a later read fail.

The optional `/api/v3/admin/messaging` bearer API is separate. If an
administrator has already configured `V3_MESSAGE_API_TOKEN`, it supports only
session creation, message submission, and snapshot reads as the administrator.
The token does not authorize the general orchestration, run-log, approval, or
build-result routes. An unset token disables that API. No bearer credential is
needed to use the browser-session routes.

## Route map

| Method and path | Purpose | Authority |
| --- | --- | --- |
| `POST /api/v3/sessions` | Create a session; body includes `client_request_id`, optional `title` and `agent_id` | Signed-in user with agent access; CSRF |
| `POST /api/v3/sessions/{session_id}/messages` | Submit text with `client_message_id`; returns 202 and `run_id` | Session owner with agent access; CSRF |
| `GET /api/v3/sessions/{session_id}` | Public chat, recent runs and last sequence | Session owner with agent access |
| `GET /api/v3/sessions/{session_id}/events?after={seq}` | Session event stream | Session owner with agent access |
| `GET /api/v3/sessions/{session_id}/runs/{run_id}/progress` | Content-free status and checkpoint summary | Session owner with agent access |
| `GET /api/v3/sessions/{session_id}/runs/{run_id}/diagnostics` | Bounded checkpoint decisions and revisions | Session owner with agent access |
| `GET /api/v3/sessions/{session_id}/runs/{run_id}/logs?after={seq}` | One run's private interaction log, at most 100 events per page | Administrator who owns the session and retains agent access |
| `POST /api/v3/runs/{run_id}/{action}` | Existing run control such as cancellation or continuation | Session owner with agent access; CSRF |
| `POST /api/v3/approvals/{approval_id}/decision` | Existing approval decision | Session owner with agent access; CSRF |
| `GET /api/v3/admin/builds` | Recent build dashboard and container status | Administrator |
| `GET /api/v3/admin/builds/{job_id}/result` | One durable build job, including bounded log and structured Rust test result | Administrator |

The run-log endpoint returns `session_id`, `run_id`, `events`, `cursor`,
and `has_more`. Pass its cursor as the next `after` value until
`has_more` is false. An empty page retains the requested cursor. The
events are private model and tool interactions, including arguments and
outputs; treat them as sensitive. It rejects a negative cursor with 400 and
an absent or mismatched run with 404. It never reads another session's rows.

The single-build endpoint returns `{"job": ...}` or 404. The job contains
durable status, phase, timestamps, at most 32 phase-history entries, a
sanitized log tail of at most 16 KiB, `log_truncated`, `log_bytes`, and
`rust_tests`. The Rust result reports `running`, `passed`,
`not_started`, `not_required`, `unconfirmed`, or `unavailable` as
recorded or conservatively inferred for older jobs. `passed` means the
locked tests completed in the tested image build; detailed test output was
not retained for older jobs. The protected broker removes its authorization
fingerprint before returning this projection. The dashboard remains limited
to its most recent 20 jobs; the single-job route can retrieve an older known ID.

## Calling sequence

1. Use the public HTTPS origin and sign in with an existing approved account.
   Retain the cookie and CSRF token in the calling client's protected state.
2. Create or choose a session, then submit a message with stable client IDs.
   Retry an uncertain POST with the **same ID and body**.
3. Poll the session snapshot or run-progress route using the returned run ID.
   A 202 receipt means admission, not a completed assistant answer. Stop when
   the run is terminal and read the persisted assistant message.
4. If authorized as administrator, page the selected run's private log or
   request a known build job result. Resolve approval-required runs through
   the existing authenticated approval UI/API; polling never approves a tool.

## Development setup

1. Apply the reviewed source patch in `/home/ar/webagent-dev3` and run the
   locked Rust tests plus `python3 -B -m unittest discover -s tests -p
   test_admin_build.py`.
2. Build and release the v3 API and loop images with the project's
   `v3_build` workflow. Replacing the v3 containers interrupts the public app.
3. Install the reviewed build broker code with
   `sudo -n python3 -B /home/ar/webagent-dev3/plugins/abilities/Administrator/v3_build/install.py --install`.
   Stop its existing supervisor cleanly, then run
   `sudo -n python3 -B /home/ar/webagent-dev3/host-build-service/bootstrap.py --start`.
4. Verify both v3 containers are healthy, `/health/ready` responds, and an
   authenticated administrator can read a scoped run log and a known build
   result. Existing account login and permissions supply access; no new
   credential or database access is required.

For an unattended client that only needs to converse, an operator may later
set `V3_MESSAGE_API_TOKEN` to an independently supplied, random 32–256
character printable ASCII value in the v3 deployment environment and replace
the v3 container. That optional step creates administrator machine access to
the three messaging routes only. It does not enable the orchestration routes
above. Do not put the token in a URL, source file, frontend asset, or log.

The v3 source lives in `/home/ar/webagent-dev3` on the development server.
A source change is not active until an authorized, tested release updates the
API and protected broker. This document does not enable the optional bearer
API.
