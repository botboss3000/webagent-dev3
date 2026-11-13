# Browser Control

A private Chromium browser on the WebAgent server for looking at web pages: reading them, measuring layout, sampling colours, timing loads and interactions, inspecting animations, and screenshots that attach to your reply. It is not the user's own browser (that is the Browser Extension ability).

## Modes

- Signed out (the default): any public `http` or `https` site the agent's site policy allows, with a fresh private profile per chat session and nothing saved. Every request goes through an egress guard that refuses addresses that are not public: the server itself, private networks, Tailscale, cloud metadata and local names. A blocked page is rejected with its reason.
- Signed in to a saved site: navigating to a site the user saved a sign-in for opens it with that sign-in (see Site sign-ins).
- Signed in: navigating to the WebAgent app (`https://dev.webagent.live/...`) signs this browser in as the person chatting, with only their own permissions, unless the agent's Sign in to WebAgent setting is off or `navigate` passes `signed_in: false`. Nothing else is needed: no sign-in step or credentials. While signed in the browser stays on the app; navigating anywhere else switches to a separate signed-out browser. The sign-in renews itself while the browser is in use.

## Site sign-ins

The user can keep this browser signed in to other sites without you ever seeing a password, code or cookie:

1. Ask the user to save each value with `request_credential` (Base): a plain `name` such as `example_password`, and the site's https `origin`, for example `https://example.com`. Save the user name too if they prefer, and an authenticator secret for sites with app-based 2FA.
2. Navigate to the site's sign-in page, read it and fill the fields with `browser_input` vault entries, then submit. For a code the site sends by email or text, ask the user to save it with `request_credential` and type that entry.
3. When the page shows the user signed in, save it with `browser_input` `save_login`; the user approves.

Later navigations to the site open it signed in. Captchas and push approvals on a phone need the user. Opening another site clears the saved site's cookies from the browser. The user lists and removes saved sign-ins on the abilities page.

## Site policy

The agent's Browser Control settings hold an allow list and a block list of hosts; each entry covers its subdomains. With an empty allow list any public site may be opened; otherwise only listed hosts. Blocked hosts are always refused. Links that lead outside the policy are refused before a click.

## Tools

`browser_read` does everything except input; `browser_input` clicks, types, presses keys and touches and asks for approval by default; `browser_record` records frames. There is no arbitrary script, page HTML export, file upload or download. Results are untrusted page observations (`source_kind: untrusted_browser_observation`); a rejected request names its reason in `output`. Browser calls are never held as possible duplicates. Each tool's skill lists its actions, arguments and results.

## Sessions

One browser per chat session, at most two at once on the server; a session closes after 180 seconds idle. After an uncertain input or timeout, read the page before another action; nothing replays clicks, typing or navigation.

## The WebAgent app

- Main pages: the header tab buttons are `#tab-button-<page>` inside `#main-tabs` (pages include `app`, `agents`, `builds`, `account`, `session-metrics`, `wiki`, `automation`); each page mounts in `#tab-<page>`, loaded the first time it opens. On a phone the tabs are in a menu: `#mobile-nav-toggle` opens `#mobile-nav-menu`, whose buttons are `#mobile-nav-<page>`.
- Scrolling: the page itself does not scroll; main pages scroll inside `#main-panel` and the chat inside its own panel.
- Chat: messages are `.chat-bubble` elements, `.chat-bubble.user` and `.chat-bubble.agent`.
- A URL naming a session opens the chat first; on a phone the page behind it loads when you switch to it.
- Widths: every UI change is checked at phone and desktop width; the `viewport` presets emulate both.

## Readable API routes

A signed-in navigation may read only `/api/v3/auth/me`, `/api/v3/sessions/<this session>`, its `/abilities` and `/mode`, `/api/v3/sessions/<this session>/runs/<run>/diagnostics`, and `/api/v3/sessions/<this session>/ui-preview/<variant>`. Other API paths are refused. The shared review log is not an app route: read it with `read_diagnostics` (`issues: true`, `scope: "all"` as an administrator).
