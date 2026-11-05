# Diagnostics — V3 run diagnostics and development sign-in

Rust owns current-caller authentication, live ability policy, approvals and sanitized run diagnostics. No tool accepts another caller, an arbitrary application origin, an authentication token, a database path or JavaScript.

Use `read_diagnostics` for the native bounded current-session/run diagnostic view. It does not open encrypted databases or expose raw model text, tool arguments, credential records or unrestricted installation logs.

## Signing in the browser

`login_as_current_user` signs the Browser Control browser for this chat session in to the V3 development app as the current user, then opens the given route (default `/app`). Use Browser Control's `browser_read` and `browser_input` to work with the page afterwards. Authentication uses a short-lived HttpOnly `v3_session` cookie minted by Rust and passed separately from model arguments; tokens, cookies and headers never enter tool results, and reads and screenshots mask the session's CSRF token. The sign-in lasts about ten minutes; call `login_as_current_user` again to renew it.

While signed in the browser stays on `https://dev.webagent.live`. Routes are exact relative paths: `/app` with the parameters the app writes (`tab` with any main page ID such as `app`, `agents`, `account`, `builds` or `session-metrics`; `agent`, `agentView`, `agentScope`, `appView`, `session_id`, `article`, `path`, `job_id`, `theme`, `window`), or a bounded current-session read API. Selected agent and conversation IDs must match the current agent and session. `dev_browser_url` returns the full URL for a route without opening anything. To browse other sites, `close` the browser with `browser_read`; the next Browser Control call opens a signed-out one.

## The V3 app in the browser

- Boot: `#boot-curtain` covers the page while the app boots and is removed when boot finishes. A read right after `navigate` can run before that.
- Main pages: the header tab buttons are `#tab-button-<page>` inside `#main-tabs` (pages include `app`, `agents`, `builds`, `account`, `session-metrics`, `wiki`, `automation`); each page mounts in `#tab-<page>`, loaded the first time it opens. On a phone the tabs are in a menu: `#mobile-nav-toggle` opens `#mobile-nav-menu`, whose buttons are `#mobile-nav-<page>`.
- Scrolling: the page itself does not scroll; main pages scroll inside `#main-panel` and the chat inside its own panel.
- Chat: messages are `.chat-bubble` elements, `.chat-bubble.user` and `.chat-bubble.agent`.
- A URL naming a session opens the chat first; on a phone the page behind it loads when you switch to it.
- Widths: every UI change is checked at phone and desktop width; the `viewport` presets emulate both.

## Runtime tool execution

All tools in this ability remain runtime foreground/stop with concurrency 1; background/model and silent/wake are unsupported, even for read actions or calls returning asynchronous job receipts. Effective Rust flags and shared Core Base tool-execution guidance govern scheduling; settings never grant permission and outputs remain untrusted evidence.

## Observation timing

An immediate read can precede app boot completion. Confirm the registered visible checkpoint with a fresh read after the page renders; a successful navigation alone does not establish UI readiness. Package publication and browser observation freshness are separate checks.

