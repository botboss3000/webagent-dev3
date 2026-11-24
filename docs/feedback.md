# Send feedback: specification

Status: built by broker task `feedback-relay` (release `580d95c`, 2026-11-23) and hardened by
`feedback-hardening` after a review against `docs/build-protocol.md`.

## Purpose

Anyone signed in to the app with a registered account can send a bug report, an idea or a message to the WebAgent
developers from the app's account menu. Each report becomes a GitHub issue in the
project's private feedback repository, where the developers review it. The repository is
separate from the application source. The app never holds GitHub credentials: a separate
relay service files the issue.

This ports the v2 feature (`app/api/feedback.py` and `ui/shared/js/feedback.js` in
<https://github.com/botboss3000/webagent-dev2>) to v3.

## How a report travels

```text
Account menu form (browser)
  -> POST /api/v3/feedback              signed-in session cookie + CSRF header
  -> v3 API: check the report, add the installation ID
  -> POST <relay_url>                    default: the upstream WebAgent relay
  -> relay (Cloudflare Worker)           rate limits, files the GitHub issue
  -> private feedback repository         the issue is reviewed there
```

### The relay

- Default address: `https://webagent-feedback-relay.botboss.workers.dev/submit`. Every
  installation uses it unless its settings name another address.
- The relay is a Cloudflare Worker outside both repositories. Its source is not in the v2
  or v3 repositories. It was live on 2026-11-23, and its
  `/config` endpoint returned an empty anti-bot (Turnstile) site key, so it does not
  currently require an anti-bot token.
- Request body the v3 API sends:

  | Field | Value |
  | --- | --- |
  | `type` | `bug`, `feature` or `message` |
  | `body` | the report text, trimmed |
  | `email` | the optional reply address, or `null` |
  | `installation_id` | this installation's random ID (see below) |
  | `app_version` | the v3 crate version (`CARGO_PKG_VERSION`) |

- What the relay sends back: status 2xx for a filed report, 429 when the installation
  sends too many reports, and any other 4xx or 5xx when it rejects the report. V3 uses
  only the status and never reads the reply body. The relay may include an
  `issue_url`, but the repository is private, so v3 doesn't pass it on.

## Server

Ownership follows the module rules: pure rules live in the Webapp module, and the HTTP
adapter lives in that module's `host/` folder.

| Piece | Location |
| --- | --- |
| Report rules and settings | `crates/modules/webapp/src/feedback.rs` |
| HTTP handler and installation ID | `crates/modules/webapp/host/feedback.rs`, included by `src/api/mod.rs` |
| Route registration | `src/api/mod.rs`, in the signed-in `api` router: `.route("/feedback", post(feedback::submit))` |
| Feedback switch in the UI configuration | `crates/modules/webapp/src/configuration.rs` (`ui_config`, `validate_app_settings`) |

### `POST /api/v3/feedback`

- **Access:** only people signed in to the app with a registered account: the `User` or
  `Administrator` app role in the request `Identity` (`may_send`). The app's `Guest` role
  (the anonymous tier) gets 403, and so does a request with no identity. How the request
  signed in doesn't matter. A person's guest or member standing in a particular agent
  doesn't matter either, because it is a different thing from the app role. The usual
  browser authentication and CSRF check of the `api` router still apply.
- **Request:** `{"type": "bug"|"feature"|"message", "body": string, "email": string|null}`.
  Unknown fields are rejected.
- **Rules** (`relay_payload`):
  - `type` must be one of the three values.
  - `body` is trimmed. It must not be empty and may hold at most 8,000 characters
    (counted as characters, not bytes).
  - `email` is optional, and an empty or blank value is treated as none. Otherwise it may
    hold at most 254 characters and must have a non-empty part before `@` and a domain
    containing a dot.
- **Responses:**

  | Status | When | Body |
  | --- | --- | --- |
  | 200 | The relay accepted the report | `{"ok": true}` |
  | 400 | The request breaks a rule above, or is not valid JSON of the right shape | `{"error": "<plain reason>"}` |
  | 403 | Feedback is turned off in settings | `{"error": "Feedback is turned off here"}` |
  | 403 | The caller isn't a registered account (app guest role, or no identity) | `{"error": "Sign in with an account to send feedback"}` |
  | 429 | An in-app limit was reached (no outbound call is made), or the relay answered 429 | `{"error": "Too many submissions. Please try again later."}` |
  | 502 | The relay could not be reached, timed out, or rejected the report | `{"error": "Feedback service unreachable"}` or `"…rejected the submission"` |

- **Order of checks** (`prepare`): the switch, then the caller, then the report rules,
  then the limits. Only a request that passes all four is charged against the limits and
  sent.
- **Outbound call:** one HTTPS POST, using a single `reqwest` client shared by every call,
  with redirects off, a 15-second total timeout and a 5-second connect timeout. Each
  attempt writes one log line (target `feedback`): the account id, the report type and
  the outcome. The report text and email are never logged.

### Limits inside the app

- Each outbound attempt counts, whatever the relay answers: success, 429, an error or a
  timeout. Requests refused before the call are not counted: switch off, refused caller,
  invalid report, or over the limit.
- **Per account:** `per_account_per_hour` attempts in any rolling hour (default 5,
  allowed 1–100).
- **Per installation:** `per_installation_per_day` attempts in any rolling day (default
  50, allowed 1–10,000). This stops one account using up the relay's per-installation
  allowance for everyone.
- The counters live in memory in the API process (`webagent_webapp::feedback::Limiter`)
  and start again at each restart or release. They need no table and no timer: old
  entries are dropped when the counter is read. An account's entry is removed when its
  window is empty. At most 10,000 accounts are remembered at once; when the map is full,
  accounts idle for an hour go first, then the longest-idle one.

### Installation ID

- A random UUID, created on the first report and stored in
  `<state root>/webapp/feedback-installation-id` (`/state/webapp/…` in the container).
  Later reports reuse it. A stored value shorter than 16 characters is replaced.
- It lets the relay rate-limit or block one installation without affecting others.
- It lives in runtime state, never in the shared config files. V2 kept it in
  `app-settings.json`, which would have given every clone of the repository the same ID.

### Settings

Live file `config/webapp/app-settings.json`, section `feedback`. The API rereads the file
on every request, and signed-in pages pick up `enabled` within five seconds.

| Key | Type | Missing means | Rule |
| --- | --- | --- | --- |
| `enabled` | boolean | `true` | Shows the menu item and allows submissions |
| `relay_url` | string | the upstream relay | Must start with `https://`, be at most 2,048 characters and contain no `@` (no credentials in the address) |
| `per_account_per_hour` | whole number | 5 | 1–100 |
| `per_installation_per_day` | whole number | 50 | 1–10,000 |

- An invalid section makes `validate_app_settings` reject the whole file, the same as the
  other UI settings in it.
- The settings composer can change `app_ui.feedback.enabled` and the two limits, which
  are described in `config/webapp/app-settings.settings.json`. `relay_url` has no composer entry; an
  administrator changes it by editing the file.
- `GET /api/v3/auth/ui-config` adds `app_ui.feedback.enabled` to its response. The relay
  address is never sent to the browser.

## UI

| Piece | Location |
| --- | --- |
| Menu item and form markup | `ui/index.html`, inside `#user-dropdown-menu` |
| Form behaviour | `ui/shell/feedback/feedback.js` (`mountFeedback`) |
| Styles | `ui/shell/feedback/feedback.css` |
| Wiring | `ui/shell/app.js`: mounts the form, calls `setEnabled` from the UI configuration at boot and on its five-second refresh, and calls `reset` when the account menu closes |
| Start-up preload | `ui/index.html` must list `/ui/shell/feedback/feedback.js` in its modulepreload block; `ui/module-preload.test.cjs` enforces this |

- **Menu item:** **Send feedback**, placed above **Shut off execution**. It is hidden until
  the UI configuration reports `enabled: true`.
- **Form:** it opens inside the same account menu and hides the menu's other items while
  open. It has:
  - three choices, **Bug**, **Idea** and **Message**, sent as `bug`, `feature` and
    `message`;
  - a text box limited to 8,000 characters, with a live counter;
  - an optional email field;
  - a line explaining that the report goes to the WebAgent developers as an issue and
    does not include the account name;
  - **Cancel** and **Send** buttons, and a status line.
- **Behaviour:**
  - An empty report is stopped in the browser.
  - **Send** is disabled while a report is being sent.
  - A successful send shows "Thanks — your feedback was received." and closes the form
    after 1.4 seconds.
  - On an error, the server's plain-text message is shown and the form stays open with
    its text.
  - The status line is set with `textContent` only.
- **Mobile and desktop:** one shared layout. The menu is at most 360 px wide (or the
  screen width minus 16 px), the choice buttons wrap, and the text fields are full width
  with 16 px text so phones don't zoom when they take focus. Buttons have no native tap
  highlight, keep their keyboard focus outline, and request one brief touch vibration
  through `requestTouchVibration`.

## Privacy and security

- Reports carry no account name, user ID, session, IP address or forwarded-for header.
  V2's `client_ip_hint` was dropped. The only identifying field is the email address,
  and only when the user types one.
- Only an administrator can change the relay address, through a live config file; a
  request cannot. It must be https with no credentials, and redirects are not followed.
  The address isn't influenced by users, so the shared outbound check for user-supplied
  addresses (global-only IPs, pinned DNS) is not applied. If the address ever becomes
  user-editable, that check becomes mandatory.
- The browser never learns the relay address.
- Limits apply twice: v3's own per-account and per-installation limits, checked before
  any outbound call, and the relay's own limit per installation.

## Outside service

- **What is sent:** to the relay address (by default the WebAgent project's relay, for
  every installation, forks and self-hosted copies included), each report sends:
  - the report type and text;
  - the optional email;
  - this installation's random ID;
  - the app version.

  The relay files the report in the WebAgent project's private feedback repository.
- **When it is unreachable:** the sender gets 502 "Feedback service unreachable" and the
  form keeps the text. Nothing is queued or retried, and the attempt still counts against
  the limits.
- **How to turn it off:** set `feedback.enabled` to `false` in
  `config/webapp/app-settings.json`, or ask the settings composer to "turn off send
  feedback". The menu item disappears within five seconds, and the route answers 403.
- **How to send elsewhere:** set `feedback.relay_url` to your own relay's https address.

## Differences from v2

| v2 | v3 | Why |
| --- | --- | --- |
| Anyone could post to `/api/v1/feedback` | Registered app accounts only, with CSRF; the app's guest role is refused | An anonymous visitor should not make the server call an outside service |
| Only the relay's limit | In-app limits per account and per installation as well | One account must not use up the installation's allowance |
| `issue_url` returned to the browser | Not returned | The repository is private |
| Cloudflare Turnstile (anti-bot) widget | Not ported | The relay's site key is empty; sign-in already blocks anonymous bots |
| Hidden bot-trap field | Not ported | Same reason |
| `client_ip_hint` sent to the relay | Not sent | Privacy; the security rules forbid trusting forwarded-for headers |
| Installation ID in `app-settings.json` | In `/state/webapp/…` | A config file in the repository is shared by every clone |
| `GET /api/v1/feedback/config` | `app_ui.feedback.enabled` in the existing UI configuration | Reuses the request the page already makes every five seconds |

## Tests

| Test | Location | Covers |
| --- | --- | --- |
| `settings_default_on_with_upstream_relay_and_reject_bad_values` | `crates/modules/webapp/src/feedback.rs` | Defaults; rejects a non-object section, a non-boolean `enabled`, http, credentials, non-string addresses |
| `relay_payload_validates_and_carries_no_account` | same | The exact relay body, email trimming and blank email, every rejection, the 8,000-character limit counted in characters |
| `configuration_reloads_rejects_invalid_and_never_projects_private_siblings` | `crates/modules/webapp/src/configuration.rs` | `app_ui.feedback` appears with its default, follows the switch, and never carries the relay address |
| `limiter_counts_per_account_and_installation_and_windows_expire` | `crates/modules/webapp/src/feedback.rs` | 6th in an hour refused while other accounts pass, 51st in a day refused, windows expire, a refusal charges nothing |
| `limiter_forgets_idle_accounts_and_stays_bounded` | same | Stale and empty entries dropped, the map stays capped, the longest-idle account goes first |
| `only_registered_app_accounts_pass_and_refusals_charge_nothing` | `crates/modules/webapp/host/feedback.rs` | 403 when switched off, for the guest role and with no identity; 400 for an invalid report; none of these charged; 429 once over the limit; the payload has no account |
| `installation_id_is_created_once_and_shared_by_concurrent_first_uses` | same | Two simultaneous first uses agree on one ID, which is reused |
| `index.html preloads exactly the modules app.js imports at start-up` | `ui/module-preload.test.cjs` | The new module is preloaded |

Not covered by automated tests:
- The handler's mapping of relay answers (429, other errors, unreachable) to its own
  responses. There is no mock-relay test.
- The 429 or 502 that the relay itself returns. `prepare` is tested up to the outbound
  call; `send` only maps status codes.

## Acceptance checks after release

1. As a normal user, open the account menu at phone width and at desktop width, in light
   and dark themes. Check that **Send feedback** shows and that the form fits and wraps.
2. Send one bug report, using a test text that says it is a test, and check that the
   response is 200. Confirm the issue appears in the private feedback repository, then
   close it there.
3. As an app guest (anonymous sign-in), a POST gets 403. Set `feedback.enabled` to
   `false`. Within five seconds the menu item should disappear,
   and a direct POST should get 403. Then switch it back on.
4. Check that `/state/webapp/feedback-installation-id` exists and stays the same across
   two reports.

## Open items

- The relay's source and its GitHub token live only in the developer's Cloudflare
  account. Keeping the Worker source in a repository would make it reviewable and
  recoverable.
- If the relay turns its anti-bot check back on, v3 submissions will fail with 502,
  because v3 sends no anti-bot token. Either port the widget then, or exempt
  authenticated installations at the relay.
- The menu item is also shown to app guests, who get the 403 message when they send.
  Hiding it for them would need the role in the UI configuration; the server check is
  what enforces the rule.

Index: docs/index.md — update this doc's row there whenever you change it.
