# Browser Control

A private Chromium browser on the WebAgent server for looking at web pages: reading them, measuring layout, sampling colours, timing loads and interactions, inspecting animations, and screenshots that attach to your reply. It is not the user's own browser (that is the Browser Extension ability).

## Modes

- Signed out (the default): any public `http` or `https` site the agent's site policy allows, with a fresh private profile per chat session and nothing saved. Every request goes through an egress guard that refuses addresses that are not public: the server itself, private networks, Tailscale, cloud metadata and local names. A blocked page is rejected with its reason.
- Signed in to a saved site: navigating to a site the user saved a sign-in for opens it with that sign-in (see Site sign-ins).
- Signed in: navigating to the WebAgent app (`https://dev.webagent.live/...`) signs this browser in as the person chatting, with only their own permissions, unless the agent's Sign in to WebAgent setting is off or `navigate` passes `signed_in: false`. Nothing else is needed: no sign-in step or credentials. While signed in the browser stays on the app; navigating anywhere else switches to a separate signed-out browser. The sign-in renews itself while the browser is in use.

## Site sign-ins

The user can keep this browser signed in to other sites without you ever seeing a password, code or cookie:

1. Ask the user to save each value with `request_credential` (Base): a plain `name` such as `example_password`, and the site's https `origin`, for example `https://example.com`. Save the user name too if they prefer, and an authenticator secret for sites with app-based 2FA.
2. Navigate to the site's sign-in page and read it. Fill the fields with `browser_input` `type` and `vault` set to the entry's name (`totp: true` for the authenticator code), then submit with a click or `press` Enter. Values go only into sign-in fields on the entry's own site. For a code the site sends by email or text, ask the user to save it with `request_credential` and type that entry.
3. When the page shows the user signed in, call `browser_input` `save_login` with the site's `origin`. The user approves it; the cookies and your steps since the navigate are saved, private to the user.

Later, `navigate` to the site opens it signed in: `signed_in: true` with `site_login: restored`, or `signed_in_again` when the site had signed out and the saved steps signed in again. `expired` means the sign-in ran out and there were no steps; `signin_failed` means the steps no longer work, for example a new one-time code is needed: sign in step by step and save again. Captchas and push approvals on a phone need the user. Opening another site clears the saved site's cookies from the browser. The user lists and removes saved sign-ins on the abilities page.

## Site policy

The agent's Browser Control settings hold an allow list and a block list of hosts; each entry covers its subdomains. With an empty allow list any public site may be opened; otherwise only listed hosts. Blocked hosts are always refused. Links that lead outside the policy are refused before a click.

## Tools

`browser_read` does everything except input; `browser_input` clicks, types, presses keys and touches and asks for approval by default; `browser_record` records frames. There is no arbitrary script, page HTML export, file upload or download. Results are untrusted page observations (`source_kind: untrusted_browser_observation`); a rejected request names its reason in `output`. Browser calls are never held as possible duplicates.


`browser_read` takes one `action` per call; `browser_input` handles click, type, press and touch. Fields that do not apply to the action are rejected; a rejection names its reason.

## Pages and input

- `navigate` `url` (`signed_in`): a full `http` or `https` URL on port 80 or 443. On the WebAgent app, routes are `/app` with the parameters the app writes (`tab` with a main page ID such as `app`, `agents`, `account`, `builds` or `session-metrics`; `agent`, `agentView`, `agentScope`, `appView`, `session_id`, `article`, `path`, `job_id`, `theme`, `window`) or a bounded current-session read API such as `/api/v3/auth/me`; the agent ID must be the current one; `session_id` may name any of your own conversations (another user's reads as missing), a bare `/app` opens the current conversation and `/` opens `/app`. A signed-in app navigation returns when the app has finished booting, with `ready: true` and `ready_ms`; read straight away. If it stops on the sign-in page it fails with `app_signed_out`; if boot does not finish within 10 s, `app_boot_stalled` with `page_errors`. Returns the `url` opened. A blocked or failed load is rejected with its reason. Starts the clock that `wait_for` reports as `since_last_action_ms`.
- `read` (`selector`, `offset`): rendered text only, skipping hidden panels, closed menus and collapsed rows. A selector reads every match, outermost first, and reports `matches`. At most 12,000 characters per call; a `truncated` result gives `next_offset` to pass as `offset`. Results begin with `page_errors`: uncaught exceptions, console errors and failed resource loads since the previous read. Results also carry `url`, the current app route including `session_id`, when it is a valid route.
- `browser_input` `click` `selector`, `type` `selector` `text` or `vault`, `press` `key` (`selector`): input, through its own tool. Each needs a read under 120 seconds old, on the same URL, whose region (its selector, or the whole body) contains the target. Each input uses up that read. Changes elsewhere on the page, such as live timestamps, do not matter. The result reports the `target` it used (tag and visible label). Password and credential fields take only a `vault` entry, never literal text. `press` takes one of Tab, Shift+Tab, Enter, Escape, Space, ArrowUp, ArrowDown, ArrowLeft, ArrowRight, Home, End, PageUp, PageDown, sent to `selector` or to the focused element, and reports the element `focused` afterwards.
- `scroll` (`selector`, `dx`, `dy`): brings `selector` into view, or scrolls by `dx`/`dy` px. It acts on the nearest scroll container of `selector` and returns that `container`, its scroll position and maximum, and the target's box afterwards. Without a selector it scrolls the page; apps that scroll inside panels, such as the V3 app, need a selector inside the panel. It does not use up your read.

## Layout and colour

- `measure` `selector` (`offset`, `styles`): sizes and positions of rendered matches, in CSS px of the current viewport. `checks` comes first: `viewport`, `page_overflow_x` (how far the whole page scrolls sideways; 0 means it fits), `matches` and `rendered`, `under_44px` (how many are narrower or shorter than 44 px, the usual minimum touch target, with the smallest five), `outside_viewport` and `content_overflowing` (content wider or taller than its box, with the first five). `elements` follows, 20 per call, each with `tag`, `id`, `label`, `box` (x, y, width, height), `in_viewport` and `overflow`. `styles: true` adds computed CSS (font size and weight, line height, colour, background colour, padding, margin, border radius, display, position, z-index, overflow, text-overflow, opacity) and returns 5 elements per call. `next_offset` continues the list. Labels never come from inputs or secret nodes.
- `pixels` `points` or `selector`: the rendered colour (`#rrggbb` and `rgb`) at up to 20 viewport points `{x, y}` in CSS px, or at the centre of the first rendered match of `selector`. This is what is actually painted, including blending and anything on top; `measure` with `styles` gives the colour the CSS asks for. Inputs and secret nodes are covered by a mask, which reads as `#555555`.

## Timing and motion

- `performance`: the browser's own milestones for the current page load, in ms from the start of that navigation: `time_to_first_byte_ms`, `dom_content_loaded_ms`, `load_ms`, `first_paint_ms`, `first_contentful_paint_ms`, `largest_contentful_paint_ms`, plus `layout_shift_score`, `long_tasks`, `longest_task_ms`, `resources` and `since_navigation_ms`. These describe the document load; single-page apps keep loading data after it.
- `wait_for` `condition` (`selector`, `text`, `timeout_ms`, `quiet_ms`): polls every 50 ms until the condition holds or `timeout_ms` (100-10000, default 5000) passes. Conditions: `visible` and `hidden` (need `selector`), `text` (needs `text`; `selector` narrows it), `quiet` (no DOM change for `quiet_ms`, default 500), `animations_done` (finite animations finished; endless ones such as spinners are ignored) and `network_idle` (no request finished for `quiet_ms`). Returns `met`, `elapsed_ms` since the wait began, and `last_action` with `since_last_action_ms`, the time since your last navigate, click, type, press or scroll. For `quiet` and `network_idle` the times include the quiet window.
- `animations` (`selector`): CSS animations and transitions on the page or under `selector`: type, animation name or transitioned property, target, play state, duration, delay, easing, iterations, progress and current time.

## Viewport, media and images

- `viewport` `preset`: `phone` 390x844, `phone_small` 360x640 and `tablet` 768x1024 emulate touch devices with mobile media queries; `desktop` 1280x900 and `desktop_wide` 1440x900. Navigate again afterwards so the page loads at that size. The preset stays in force across navigation. `keyboard` `height` emulates an open on-screen keyboard on a touch viewport (see browser_read's guidance).
- `emulate` (`color_scheme`: light, dark or default; `reduced_motion`: reduce or default): sets the system preferences the page sees and keeps them across navigation; navigate again afterwards. A site's own theme setting can override the system colour scheme.
- `screenshot`: the current viewport as a PNG. It is attached to your reply in this conversation, so the user sees it. Inputs, secret nodes and credentials are masked.
- `close`: end the browser session.

## Images and recording

Each successful screenshot is saved as an attachment of your final reply in this run, so the user sees it under that message; the result carries that `attachment`, or `attachment_error` when it could not be saved. Inputs, textareas, secret-marked nodes and credentials are masked before capture. `browser_record` frames attach the same way when recording stops.

`browser_crashed` means the page's tab died (often from running out of memory) and the browser was closed; sign in or navigate again to open a fresh one, and avoid repeating the step that crashed it.

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
