# Diagnostics — private V3 development browser

Rust owns current-caller authentication, live ability policy, approvals and sanitized run diagnostics. No tool accepts another caller, an arbitrary application origin, an authentication token, a database path or JavaScript.

Use `read_diagnostics` for the native bounded current-session/run diagnostic view. It does not open encrypted databases or expose raw model text, tool arguments, credential records or unrestricted installation logs.

## Authenticated browser

`login_as_current_user` opens a private Chromium profile for the caller's current chat session and verifies `/api/v3/auth/me`. Authentication uses a short-lived HttpOnly `v3_session` cookie minted by Rust and passed separately from model arguments. Tokens, cookies and headers never enter tool results. The verified identity response supplies the browser CSRF token privately; reads and screenshots mask it along with the authentication cookie. A new authorization generation closes the old profile; expired, idle or revoked profiles cannot be reused.

The browser origin is fixed to `https://dev.webagent.live`. The route is supplied as an exact relative `path`: `/app` with the parameters the app writes (`tab` with any main page ID such as `app`, `agents`, `account`, `builds` or `session-metrics`; `agent`, `agentView`, `agentScope`, `appView`, `session_id`, `article`, `path`, `job_id`, `theme`, `window`), or a bounded current-session read API. Selected agent and conversation IDs must match Rust's current agent/session. External, production, file and data origins are unavailable. A rejected request names its reason in `output`; correct the request rather than retrying it unchanged.

`dev_browser_action` supports only `navigate`, `read`, `measure`, `wait_for`, `animations`, `scroll`, `click`, `type`, `press`, `screenshot`, `viewport`, `emulate` and `close`. There is no `evaluate`, arbitrary script, page HTML export or filesystem upload. Read before each click or type: the target must lie inside the region your last read covered (its `selector`, or the whole body), the read must be under 30 seconds old and on the same URL. Other parts of the page may keep changing, such as live timestamps. Navigation, screenshots and each input withdraw the observation. The tool re-checks the target at dispatch and reports the `target` it used (tag and visible label). Repeating a browser call is never held as a duplicate; read again to see whether an earlier input took effect. Private credential/password targets cannot be typed or clicked.

Reads return bounded sanitized text that is actually rendered (hidden panels, closed drawers and display:none content are skipped) with `source_kind: untrusted_browser_observation`. A `selector` reads every match, outermost first, and reports `matches`. Each read returns at most 12,000 characters; a `truncated` read gives `next_offset`, which you pass as `offset` to read the next page. Results start with `page_errors`: uncaught exceptions, console errors and failed resource loads since the previous read (at most 20, credentials redacted). An empty list after the page has rendered is evidence the boot raised no error. Literal text input is bounded and never executed as script. Rust still decides whether a mutation requires approval.

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

Every UI change is checked at both widths. `dev_browser_action` action `viewport` with `preset` `phone` emulates a 390x844 touch device (mobile media queries, touch input); `phone_small` (360x640) and `tablet` (768x1024) are also touch devices; `desktop` is 1280x900 and `desktop_wide` 1440x900. After changing the viewport, `navigate` to the route again so the app boots at that width, then `read` (and `screenshot` when layout matters). For each route and width, report the visible checkpoint text and `page_errors`. To check in-page navigation, `read` and then `click` the page's own control (for example `#tab-button-agents` on desktop, `#mobile-nav-agents` on a phone after clicking `#mobile-nav-toggle` and reading again) and `read` again.

## Runtime tool execution

All tools in this ability remain runtime foreground/stop with concurrency 1; background/model and silent/wake are unsupported, even for read actions or calls returning asynchronous job receipts. Effective Rust flags and shared Core Base tool-execution guidance govern scheduling; settings never grant permission and outputs remain untrusted evidence.

## Observation timing

An immediate read can precede app boot completion. Confirm the registered visible checkpoint with a fresh read after the page renders; a successful navigation alone does not establish UI readiness. Package publication and browser observation freshness are separate checks.

## Measuring layout, timing and motion

These are fixed checks; you supply only a selector and the listed options.

- `measure` (`selector` required) returns, for up to 50 rendered matches: `box` (x, y, width, height in CSS px), `in_viewport`, `overflow` (content wider or taller than the element by that many px) and a fixed `style` subset (font size and weight, line height, padding, margin, border radius, colour, background, display, position, z-index, overflow, text-overflow). It also returns the `viewport` and `page_overflow_x`, the px by which the whole page scrolls sideways; on a phone that should be 0. Use it for numeric checks such as touch targets under 44 px, squeezed inputs, clipped text or elements off screen. Labels never come from inputs or secret nodes.
- `wait_for` polls one `condition` every 50 ms until it holds or `timeout_ms` (100-10000, default 5000) ends: `visible`/`hidden` (`selector`), `text` (`text`, optional `selector`), `quiet` (the page stops changing for `quiet_ms`, default 500), `animations_done` (ignores endless animations such as spinners) and `network_idle` (no request finished for `quiet_ms`). It returns `met`, `elapsed_ms` and `since_last_action_ms` from your last navigate, click, type, press or scroll, which times things like "the toast appeared 340 ms after the click". For `quiet` and `network_idle` the time includes the quiet window.
- `animations` (optional `selector`) lists running CSS animations and transitions: type, animation name or transitioned property, target, play state, duration, delay, easing, iterations, progress and current time.
- `scroll` brings `selector` into view, or scrolls by `dx`/`dy` px. The app scrolls inside panels rather than the page, so pass a selector inside the panel you mean; the scroll acts on that element's nearest scroll container. It returns the `container` it used, its scroll position and maximum, and the target's box after scrolling. It keeps your last read valid.
- `press` sends one allow-listed key (Tab, Shift+Tab, Enter, Escape, Space, arrows, Home, End, PageUp, PageDown) to `selector` or to the focused element. It is input: read first, as for a click. The result reports the element that has focus afterwards, so a series of Tab presses checks keyboard focus order.
- `emulate` sets `color_scheme` (light, dark, default) and `reduced_motion` (reduce, default). Like the viewport, it is re-applied after each navigation; navigate again after changing it. The app's own theme setting may override the system colour scheme.
