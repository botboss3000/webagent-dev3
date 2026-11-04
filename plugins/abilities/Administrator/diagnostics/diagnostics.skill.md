# Diagnostics — private V3 development browser

Rust owns current-caller authentication, live ability policy, approvals and sanitized run diagnostics. No tool accepts another caller, an arbitrary application origin, an authentication token, a database path or JavaScript.

Use `read_diagnostics` for the native bounded current-session/run diagnostic view. It does not open encrypted databases or expose raw model text, tool arguments, credential records or unrestricted installation logs.

## Authenticated browser

`login_as_current_user` opens a private Chromium profile for the caller's current chat session and verifies `/api/v3/auth/me`. Authentication uses a short-lived HttpOnly `v3_session` cookie minted by Rust and passed separately from model arguments. Tokens, cookies and headers never enter tool results. The verified identity response supplies the browser CSRF token privately; reads and screenshots mask it along with the authentication cookie. A new authorization generation closes the old profile; expired, idle or revoked profiles cannot be reused.

The browser origin is fixed to `https://dev.webagent.live`. The route is supplied as an exact relative `path`: `/app` with the parameters the app writes (`tab` with any main page ID such as `app`, `agents`, `account`, `builds` or `session-metrics`; `agent`, `agentView`, `agentScope`, `appView`, `session_id`, `article`, `path`, `job_id`, `theme`, `window`), or a bounded current-session read API. Selected agent and conversation IDs must match Rust's current agent/session. External, production, file and data origins are unavailable. A rejected request names its reason in `output`; correct the request rather than retrying it unchanged.

`dev_browser_action` supports only `navigate`, `read`, `click`, `type`, `screenshot`, `viewport` and `close`. There is no `evaluate`, arbitrary script, page HTML export or filesystem upload. Read the current page before each click or type. DOM changes, navigation, screenshots or previous input withdraw the observation and require another read. Private credential/password targets cannot be typed or clicked.

Reads return bounded sanitized visible text with `source_kind: untrusted_browser_observation`, plus `page_errors`: uncaught exceptions, console errors and failed resource loads since the previous read (at most 20, credentials redacted). An empty list after the page has rendered is evidence the boot raised no error. Literal text input is bounded and never executed as script. Rust still decides whether a mutation requires approval.

## Images and recording

Screenshots are actual bounded viewport PNG files stored privately outside the coding workspace. Each successful screenshot is also saved as an attachment of your final reply in this run, so the user sees it as a thumbnail under that message after the browser closes; take the screenshots that show the user what you validated, and refer to them in the reply. A screenshot result carries that `attachment`, or `attachment_error` when it could not be saved. Inputs, textareas, secret-marked nodes and exact authentication-cookie and CSRF-token text are masked before capture. Results contain an opaque artifact ID, digest, dimensions and an authenticated current-session API download path; never a host path.

`dev_browser_record` has `start`, `status` and `stop`. It records at most 30 PNG frames over 30 seconds. The result is explicitly `bounded_png_frames`, not a video. Stop recording and close the browser after use. A screenshot quota or lease failure stops capture.

After uncertain input or timeout, inspect the current page before another action. Neither restart nor recovery replays clicks, typing or navigation mutations. Profiles are discarded rather than restored; the immutable interaction journal remains original evidence.

Use API validation when requested. This ability does not require a browser journey or force a screenshot for an API-only task.

## Independently published native package

The protected broker publishes this complete protocol-1 native package. Rust retains identity, cookie minting, policy, approvals, run diagnostics and artifact authorization. The worker supervises at most two private browser sessions; each process pins one immutable package revision through close, 180-second idle expiry or its current authorization lease. New browser sessions use the latest publication. Compatible runner/helper/guidance updates retain API, loop, gate and worker processes. Existing active sessions do not change implementation mid-action.

Results identify plugin_revision, implementation_version and implementation_sha256. Capacity, startup, authentication, invalid-request and backend errors have distinct sanitized error_kind values. Transport loss during input remains uncertain and never authorizes replay. Artifact reads use a short-lived process without opening a browser or consuming a browser slot. Restart discards authenticated profiles; artifacts retain their existing session ownership and access checks.

This is the private fixed-origin Diagnostics browser, not Web Browser Control or a personal-device browser. It does not emulate a phone operating system keyboard.

## Checking a UI change at phone and desktop width

Every UI change is checked at both widths. `dev_browser_action` action `viewport` with `preset` `phone` emulates a 390x844 touch device (mobile media queries, touch input); `desktop` restores 1280x900. After changing the viewport, `navigate` to the route again so the app boots at that width, then `read` (and `screenshot` when layout matters). For each route and width, report the visible checkpoint text and `page_errors`. To check in-page navigation, `read` and then `click` the page's own control (for example `#tab-button-agents` on desktop, `#mobile-nav-agents` on a phone after clicking `#mobile-nav-toggle` and reading again) and `read` again.

## Runtime tool execution

All tools in this ability remain runtime foreground/stop with concurrency 1; background/model and silent/wake are unsupported, even for read actions or calls returning asynchronous job receipts. Effective Rust flags and shared Core Base tool-execution guidance govern scheduling; settings never grant permission and outputs remain untrusted evidence.

## Observation timing

An immediate read can precede app boot completion. Confirm the registered visible checkpoint with a fresh read after the page renders; a successful navigation alone does not establish UI readiness. Package publication and browser observation freshness are separate checks.
