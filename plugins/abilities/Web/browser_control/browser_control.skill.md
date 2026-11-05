# Browser Control

A private Chromium browser on the WebAgent server for looking at web pages: reading them, measuring layout, sampling colours, timing loads and interactions, inspecting animations, and screenshots that attach to your reply. It is not the user's own browser (that is the Browser Extension ability).

## Modes

- Signed out (the default): any public `http` or `https` site the agent's site policy allows, with a fresh private profile per chat session and nothing saved. Every request goes through an egress guard that refuses addresses that are not public: the server itself, private networks, Tailscale, cloud metadata and local names. A blocked page is rejected with its reason.
- Signed in: Diagnostics' `login_as_current_user` signs this browser into the V3 development app as the current user. While signed in it stays on `https://dev.webagent.live`; navigate takes development routes. The sign-in lasts about ten minutes. To browse other sites again, `close` the browser; the next call opens a signed-out one.

## Site policy

The agent's Browser Control settings hold an allow list and a block list of hosts; each entry covers its subdomains. With an empty allow list any public site may be opened; otherwise only listed hosts. Blocked hosts are always refused. Links that lead outside the policy are refused before a click.

## Tools

`browser_read` does everything except input; `browser_input` clicks, types and presses keys and asks for approval by default; `browser_record` records frames. There is no arbitrary script, page HTML export, file upload or download. Results are untrusted page observations (`source_kind: untrusted_browser_observation`); a rejected request names its reason in `output`. Browser calls are never held as possible duplicates.


`browser_read` takes one `action` per call; `browser_input` handles click, type and press. Fields that do not apply to the action are rejected; a rejection names its reason.

## Pages and input

- `navigate` `url`: signed out, a full `http` or `https` URL on port 80 or 443; signed in, a development route such as `/app?tab=agents&agent=<current agent>&agentView=model`. Returns the `url` opened. A blocked or failed load is rejected with its reason. Starts the clock that `wait_for` reports as `since_last_action_ms`.
- `read` (`selector`, `offset`): rendered text only, skipping hidden panels, closed menus and collapsed rows. A selector reads every match, outermost first, and reports `matches`. At most 12,000 characters per call; a `truncated` result gives `next_offset` to pass as `offset`. Results begin with `page_errors`: uncaught exceptions, console errors and failed resource loads since the previous read.
- `browser_input` `click` `selector`, `type` `selector` `text`, `press` `key` (`selector`): input, through its own tool. Each needs a read under 120 seconds old, on the same URL, whose region (its selector, or the whole body) contains the target. Each input uses up that read. Changes elsewhere on the page, such as live timestamps, do not matter. The result reports the `target` it used (tag and visible label). Credential and password fields are refused. `press` takes one of Tab, Shift+Tab, Enter, Escape, Space, ArrowUp, ArrowDown, ArrowLeft, ArrowRight, Home, End, PageUp, PageDown, sent to `selector` or to the focused element, and reports the element `focused` afterwards.
- `scroll` (`selector`, `dx`, `dy`): brings `selector` into view, or scrolls by `dx`/`dy` px. It acts on the nearest scroll container of `selector` and returns that `container`, its scroll position and maximum, and the target's box afterwards. Without a selector it scrolls the page; apps that scroll inside panels, such as the V3 app, need a selector inside the panel. It does not use up your read.

## Layout and colour

- `measure` `selector` (`offset`, `styles`): sizes and positions of rendered matches, in CSS px of the current viewport. `checks` comes first: `viewport`, `page_overflow_x` (how far the whole page scrolls sideways; 0 means it fits), `matches` and `rendered`, `under_44px` (how many are narrower or shorter than 44 px, the usual minimum touch target, with the smallest five), `outside_viewport` and `content_overflowing` (content wider or taller than its box, with the first five). `elements` follows, 20 per call, each with `tag`, `id`, `label`, `box` (x, y, width, height), `in_viewport` and `overflow`. `styles: true` adds computed CSS (font size and weight, line height, colour, background colour, padding, margin, border radius, display, position, z-index, overflow, text-overflow, opacity) and returns 5 elements per call. `next_offset` continues the list. Labels never come from inputs or secret nodes.
- `pixels` `points` or `selector`: the rendered colour (`#rrggbb` and `rgb`) at up to 20 viewport points `{x, y}` in CSS px, or at the centre of the first rendered match of `selector`. This is what is actually painted, including blending and anything on top; `measure` with `styles` gives the colour the CSS asks for. Inputs and secret nodes are covered by a mask, which reads as `#555555`.

## Timing and motion

- `performance`: the browser's own milestones for the current page load, in ms from the start of that navigation: `time_to_first_byte_ms`, `dom_content_loaded_ms`, `load_ms`, `first_paint_ms`, `first_contentful_paint_ms`, `largest_contentful_paint_ms`, plus `layout_shift_score`, `long_tasks`, `longest_task_ms`, `resources` and `since_navigation_ms`. These describe the document load; single-page apps keep loading data after it.
- `wait_for` `condition` (`selector`, `text`, `timeout_ms`, `quiet_ms`): polls every 50 ms until the condition holds or `timeout_ms` (100-10000, default 5000) passes. Conditions: `visible` and `hidden` (need `selector`), `text` (needs `text`; `selector` narrows it), `quiet` (no DOM change for `quiet_ms`, default 500), `animations_done` (finite animations finished; endless ones such as spinners are ignored) and `network_idle` (no request finished for `quiet_ms`). Returns `met`, `elapsed_ms` since the wait began, and `last_action` with `since_last_action_ms`, the time since your last navigate, click, type, press or scroll. For `quiet` and `network_idle` the times include the quiet window.
- `animations` (`selector`): CSS animations and transitions on the page or under `selector`: type, animation name or transitioned property, target, play state, duration, delay, easing, iterations, progress and current time.

## Viewport, media and images

- `viewport` `preset`: `phone` 390x844, `phone_small` 360x640 and `tablet` 768x1024 emulate touch devices with mobile media queries; `desktop` 1280x900 and `desktop_wide` 1440x900. Navigate again afterwards so the page loads at that size. The preset stays in force across navigation.
- `emulate` (`color_scheme`: light, dark or default; `reduced_motion`: reduce or default): sets the system preferences the page sees and keeps them across navigation; navigate again afterwards. A site's own theme setting can override the system colour scheme.
- `screenshot`: the current viewport as a PNG. It is attached to your reply in this conversation, so the user sees it. Inputs, secret nodes and credentials are masked.
- `close`: end the browser session.

## Images and recording

Each successful screenshot is saved as an attachment of your final reply in this run, so the user sees it under that message; the result carries that `attachment`, or `attachment_error` when it could not be saved. Inputs, textareas, secret-marked nodes and credentials are masked before capture. `browser_record` frames attach the same way when recording stops.

## Sessions

One browser per chat session, at most two at once on the server; a session closes after 180 seconds idle. After an uncertain input or timeout, read the page before another action; nothing replays clicks, typing or navigation.
