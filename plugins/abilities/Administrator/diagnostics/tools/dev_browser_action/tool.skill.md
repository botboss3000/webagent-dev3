# Operate the private development browser

One `action` per call. Fields that do not apply to the action are rejected; a rejection names its reason.

## Pages and input

- `navigate` `path`: open a relative V3 route, such as `/app?tab=agents&agent=<current agent>&agentView=model`. Returns the route used. Starts the clock that `wait_for` reports as `since_last_action_ms`.
- `read` (`selector`, `offset`): rendered text only, skipping hidden panels, closed menus and collapsed rows. A selector reads every match, outermost first, and reports `matches`. At most 12,000 characters per call; a `truncated` result gives `next_offset` to pass as `offset`. Results begin with `page_errors`: uncaught exceptions, console errors and failed resource loads since the previous read.
- `click` `selector`, `type` `selector` `text`, `press` `key` (`selector`): input. Each needs a read under 30 seconds old, on the same URL, whose region (its selector, or the whole body) contains the target. Each input uses up that read. Changes elsewhere on the page, such as live timestamps, do not matter. The result reports the `target` it used (tag and visible label). Credential and password fields are refused. `press` takes one of Tab, Shift+Tab, Enter, Escape, Space, ArrowUp, ArrowDown, ArrowLeft, ArrowRight, Home, End, PageUp, PageDown, sent to `selector` or to the focused element, and reports the element `focused` afterwards.
- `scroll` (`selector`, `dx`, `dy`): brings `selector` into view, or scrolls by `dx`/`dy` px. It acts on the nearest scroll container of `selector` and returns that `container`, its scroll position and maximum, and the target's box afterwards. Without a selector it scrolls the page, which in this app does not move. It does not use up your read.

## Layout and colour

- `measure` `selector` (`offset`, `styles`): sizes and positions of rendered matches, in CSS px of the current viewport. `checks` comes first: `viewport`, `page_overflow_x` (how far the whole page scrolls sideways; 0 means it fits), `matches` and `rendered`, `under_44px` (how many are narrower or shorter than 44 px, the usual minimum touch target, with the smallest five), `outside_viewport` and `content_overflowing` (content wider or taller than its box, with the first five). `elements` follows, 20 per call, each with `tag`, `id`, `label`, `box` (x, y, width, height), `in_viewport` and `overflow`. `styles: true` adds computed CSS (font size and weight, line height, colour, background colour, padding, margin, border radius, display, position, z-index, overflow, text-overflow, opacity) and returns 5 elements per call. `next_offset` continues the list. Labels never come from inputs or secret nodes.
- `pixels` `points` or `selector`: the rendered colour (`#rrggbb` and `rgb`) at up to 20 viewport points `{x, y}` in CSS px, or at the centre of the first rendered match of `selector`. This is what is actually painted, including blending and anything on top; `measure` with `styles` gives the colour the CSS asks for. Inputs and secret nodes are covered by a mask, which reads as `#555555`.

## Timing and motion

- `performance`: the browser's own milestones for the current page load, in ms from the start of that navigation: `time_to_first_byte_ms`, `dom_content_loaded_ms`, `load_ms`, `first_paint_ms`, `first_contentful_paint_ms`, `largest_contentful_paint_ms`, plus `layout_shift_score`, `long_tasks`, `longest_task_ms`, `resources` and `since_navigation_ms`. These describe the document load; the app keeps loading data and pages after it.
- `wait_for` `condition` (`selector`, `text`, `timeout_ms`, `quiet_ms`): polls every 50 ms until the condition holds or `timeout_ms` (100-10000, default 5000) passes. Conditions: `visible` and `hidden` (need `selector`), `text` (needs `text`; `selector` narrows it), `quiet` (no DOM change for `quiet_ms`, default 500), `animations_done` (finite animations finished; endless ones such as spinners are ignored) and `network_idle` (no request finished for `quiet_ms`). Returns `met`, `elapsed_ms` since the wait began, and `last_action` with `since_last_action_ms`, the time since your last navigate, click, type, press or scroll. For `quiet` and `network_idle` the times include the quiet window.
- `animations` (`selector`): CSS animations and transitions on the page or under `selector`: type, animation name or transitioned property, target, play state, duration, delay, easing, iterations, progress and current time.

## Viewport, media and images

- `viewport` `preset`: `phone` 390x844, `phone_small` 360x640 and `tablet` 768x1024 emulate touch devices with mobile media queries; `desktop` 1280x900 and `desktop_wide` 1440x900. Navigate again afterwards so the app boots at that size. The preset stays in force across navigation.
- `emulate` (`color_scheme`: light, dark or default; `reduced_motion`: reduce or default): sets the system preferences the page sees and keeps them across navigation; navigate again afterwards. The app's own theme setting can override the system colour scheme.
- `screenshot`: the current viewport as a PNG. It is attached to your reply in this conversation, so the user sees it. Inputs, secret nodes and credentials are masked.
- `close`: end the browser session.
