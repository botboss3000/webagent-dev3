---
id: ui_to_web
title: Porting UI from /ui to /web
when_to_use: Use for any change under web/ that ports a feature from the current /ui app, when adding a component or page to web/, or when adding configuration for the new UI under config/web/.
default_mode: on_request
examples: port the chat header into web/chat/header; replace a placeholder page with the real feature; add chat-shell.json; add a component to the library
keywords: web, port, ui, migration, components, config/web, chat shell, composer, tokens
minimum_access: administrator
---

# Porting UI from /ui to /web

`web/` is the replacement UI, served at `/web/…` and released like `ui/`. Port it
feature by feature: read the current `/ui` implementation first, then rebuild the
same behaviour in `web/`, never a new design of your own.

## Port completely

- "Port X" means the whole feature: markup, styles, behaviour and the logic it
  depends on. If the original relies on another module, port that module too.
- Delete what the new feature supersedes in the same change — the old page, the
  old stylesheet, the old keys. Never leave the replaced path beside the new one,
  and name what was removed in the report.
- Replace, never layer. If an in-place edit fails twice on an ambiguous anchor,
  read the region and edit precisely. Do not fall back to appending overrides that
  leave the old declarations in the file: that is dead CSS and it contradicts the
  rule above. If you append as a stopgap, deleting the superseded block is part of
  the same task, not a follow-up.
- If a port cannot be complete inside the current structure, stop and ask. Never
  ship a partial port and mention the gap afterwards.

## Empty means empty — including the frame

- When the user asks for an empty page with header and footer sections, ship an
  empty page: no divider lines, no placeholder copy, no invented chrome.
- The shell's own chrome counts as content. On a surface that should be empty the
  shell's panel border, radius and shadow, the header's bottom rule, the footer's
  top rule and any placeholder footer text come off too.
- Verify the box that draws the thing. To claim a surface is borderless or empty,
  name every box between the viewport and the content with its border, background
  and radius. Measuring the innermost element and calling the page clean is wrong
  when an ancestor draws the visible frame — that mistake shipped a "clean" page
  whose frame, header rule and footer rule were all still on screen.

## Independence and stack limits

- `web/` never imports from `/ui/`. Anything needed is copied in first.
- Plain JS and CSS only: no npm, no build step, no CDN, no external fonts or icon
  libraries. Inline SVG is fine. CSS classes on plain HTML, small JS modules only
  for behaviour.
- Tokens are copied into `web/` (from `ui/shared/css/design-system.css`) and never
  imported. No hard-coded scheme colours; dark and light must both be correct.
- One shared layout that works at phone width (~360–412 px) and desktop.
- Every stylesheet, module and partial URL carries a release stamp (`?v=…`, bumped
  with `web/shell/version.js` and `index.html`). Assets are served from a frozen
  release under URLs that never change, so without the stamp a stale file can
  masquerade as a broken build.

## Folder ownership

```
web/shell/            tokens.css, shell.css, shell.js, version.js
web/chat/             chat.html, chat.css, chat.js, then one folder per area
web/chat/composer/           the dock plus the 3-row × 3-cell grid
web/chat/composer-header/    features above the composer
web/chat/composer-footer/    features below the composer
web/components/<name>/   one primitive: <name>.css and, if it has behaviour, <name>.js
web/components/components.js   the single inventory the pages and gallery read
web/pages/<entry>/page.js      one page module per sidebar entry
web/admin/pages/<entry>/page.js  administrator pages
web/admin/gallery/    the component gallery
```

- Each area owns its files; the shell never holds another area's settings.
- The composer is `composer-header`, `composer` and `composer-footer`. The composer
  itself is three rows of three cells (`left`, `center`, `right`); a row whose cells
  are all empty takes no space, and no row's cells influence another row's widths,
  so one row can hold a full-width textarea while the next holds unequal cells.
- A page module exports `meta` (`title`, `note`) and `render(host, context)`.
- Administrator pages live under `web/admin/` so `administrator_file()` gates them,
  and they load by dynamic `import()` so a signed-out visitor never fetches them.
  Never weaken that gate.
- The gallery lists every component from the inventory; a component without a
  specimen shows a note rather than disappearing, and a fixture compares the
  inventory with the folders on disk.

## Configuration

- The current app keeps `config/ui/chat/chat.json` (and `config/browser/…`,
  `config/chat/…`). The new UI splits configuration per area by ownership, flat,
  under `config/web/chat/`: `chat-shell.json`, `chat-composer.json`,
  `chat-content.json`. Code path and config path mirror each other.
- Every file carries the same four blocks: `common`, `desktop`, `mobile`,
  `widget`. `common` is the base and the active profile overlays it: objects
  merge, arrays replace entirely, `null` inherits.
- Profiles are **mount-declared**, never inferred from width: the desktop column
  mounts as `desktop`, the chat page as `mobile`, an embed as `widget`.
  `narrow_follows_mobile` on a profile decides whether a narrow window hands over
  to the mobile profile. Widget is always widget.
- Configuration is served by the app, not shipped inside `web/`:
  `GET /config/web/<path>` reads `config/web/<path>` (JSON only, `no-store`), the
  sibling of the existing `/config/ui` route.
- A missing or unreachable config must never remove a surface: fall back to the
  mount's own shape and the documented defaults.
- New live patterns need two entries: the pattern in `live-files.json` and either
  a settings descriptor or an `excluded_files` entry **with a reason** in
  `config/configuration/settings-catalog.json`, or
  `every_live_setting_is_described_or_excluded_with_a_reason` fails the release.

## Serving and safety facts

- `web/` is served at `/web/…`; `/web/admin/…` answers 404 to non-administrators.
- CSP forbids inline `<script>`: external module scripts only, inline `<style>` is
  allowed. Never put script tags inside an HTML partial.
- `/web/` pages report a false "app did not finish booting" in Browser Control —
  the tool waits for the `/app` boot signal. The page still loads: navigate, then
  interact and capture in the following step.
- The private browser may open `/web/…` only as a test account, and that sign-in
  lasts for the turn it is made in: sign in with `test_login` and capture in the
  same turn.

## Verification and release

- One isolated broker task per outcome. `web/`-only or config-only changes take
  the asset lane; anything that touches Rust compiles and takes the normal lane.
- A fixture reference is part of the change: when an element, id or text is removed
  or moved, grep the fixtures for it in the same change, and prefer stable hooks
  (an account row) over transient status text.
- Prove the change with checks on one unchanged digest — fmt, check and the
  filtered test or `ui_test` — before finishing, then `preflight` (it must be
  synchronized with zero promotion conflicts) and `finish`.
- Browser fixtures run at 1280 and 390 px in dark and light, with a self-contained
  `node:http` server, and assert no page errors and no sideways overflow. A
  fixture fault is fixed as a fixture; a product bug is fixed in the product.
- A screenshot is evidence only once you have looked at it. Do not describe a
  capture from its measurements alone: read the image before claiming what it
  shows.
- Never attribute a difference to caching without proof. Fetch the served file,
  quote its bytes, measure the rendered box, and compare both with what the user
  reports. If they match, the difference is on their device — ask for one decisive
  test instead of repeating the cache theory.
- After release, verify over HTTP and with your own browser capture where you can,
  and label fixture-only evidence as fixture-only. Report the job ID, release ID
  and commit, what was removed, and what remains open.
