---
id: ui_to_web
title: Porting UI from /ui to /web
when_to_use: Use for any change under web/ that ports a feature from the current /ui app, when adding a component or page to web/, or when adding configuration for the new UI under config/web/.
default_mode: on_request
examples: port the chat header into web/chat/header; replace a placeholder page with the real feature; add chat-shell.json; add a component to the library
keywords: web, port, ui, migration, components, config/web, chat shell, tokens
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
- If a port cannot be complete inside the current structure, stop and ask. Never
  ship a partial port and mention the gap afterwards.
- Empty means empty. If the user asks for an empty page with header and footer
  sections, ship an empty page: no divider lines, no placeholder copy, no
  invented chrome. Match the current UI, which may have none.

## Independence and stack limits

- `web/` never imports from `/ui/`. Anything needed is copied in first.
- Plain JS and CSS only: no npm, no build step, no CDN, no external fonts or icon
  libraries. Inline SVG is fine. CSS classes on plain HTML, small JS modules only
  for behaviour.
- Tokens are copied into `web/` (from `ui/shared/css/design-system.css`) and never
  imported. No hard-coded scheme colours; dark and light must both be correct.
- One shared layout that works at phone width (~360–412 px) and desktop.

## Folder ownership

```
web/shell/            tokens.css, shell.css, shell.js
web/chat/             chat.html, chat.css, chat.js, then one folder per area
web/chat/content/     the content area: scroller, transcript, bubbles, gutters
web/chat/header/      header features (rows, zones, controls)
web/chat/footer/      footer features (pill, above/below rows)
web/components/<name>/   one primitive: <name>.css and, if it has behaviour, <name>.js
web/components/components.js   the single inventory the pages and gallery read
web/pages/<entry>/page.js      one page module per sidebar entry
web/admin/pages/<entry>/page.js  administrator pages
web/admin/gallery/    the component gallery
```

- Each area owns its files; the shell never holds another area's settings.
- A page module exports `meta` (`title`, `note`) and `render(host, context)`.
- Administrator pages live under `web/admin/` so `administrator_file()` gates them,
  and they load by dynamic `import()` so a signed-out visitor never fetches them.
  Never weaken that gate.
- The gallery lists every component from the inventory; a component without a
  specimen shows a note rather than disappearing, and a fixture compares the
  inventory with the folders on disk.

## Configuration

- The current app keeps one `config/ui/chat/chat.json`. The new UI splits it per
  area by ownership, flat, under `config/web/chat/`: `chat-shell.json`,
  `chat-content.json`, and so on. Code path and config path mirror each other.
- Every file carries the same four blocks: `common`, `desktop`, `mobile`,
  `widget`. `common` is the base and the active profile overlays it: objects
  merge, arrays replace entirely, `null` inherits.
- Profiles are **mount-declared**, never inferred from width: the desktop column
  mounts as `desktop`, the chat page as `mobile`, an embed as `widget`.
  `narrow_follows_mobile` on a profile decides whether a narrow window hands over
  to the mobile profile. Widget is always widget.
- Configuration is served by the app, not shipped inside `web/`:
  `GET /api/v3/config/web/<path>` reads `config/web/<path>` (JSON only,
  `no-store`), the sibling of the existing `/config/ui` route.
- New live patterns need two entries: the pattern in `live-files.json` and either
  a settings descriptor or an `excluded_files` entry **with a reason** in
  `config/configuration/settings-catalog.json`, or
  `every_live_setting_is_described_or_excluded_with_a_reason` fails the release.

## Serving and safety facts

- `web/` is served at `/web/…`; `/web/admin/…` answers 404 to non-administrators.
- CSP forbids inline `<script>`: external module scripts only, inline `<style>` is
  allowed. Never put script tags inside an HTML partial.
- The private browser cannot open `/web/` pages (`sign_in_app` is off), so live
  checks are HTTP requests and rendering evidence comes from fixtures.

## Verification and release

- One isolated broker task per outcome. `web/`-only or config-only changes take
  the asset lane; anything that touches Rust compiles and takes the normal lane.
- Prove the change with checks on one unchanged digest — fmt, check and the
  filtered test or `ui_test` — before finishing, then `preflight` (it must be
  synchronized with zero promotion conflicts) and `finish`.
- Browser fixtures run at 1280 and 390 px in dark and light, with a self-contained
  `node:http` server, and assert no page errors and no sideways overflow. A
  fixture fault is fixed as a fixture; a product bug is fixed in the product.
- After release, verify over HTTP what you can, and label fixture-only evidence as
  fixture-only. Report the job ID, release ID and commit, what was removed, and
  what remains open.
