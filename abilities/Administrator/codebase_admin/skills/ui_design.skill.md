---
id: ui_design
title: UI design and controls
when_to_use: Use before building or changing any WebAgent UI — layout, buttons and controls, theming and palette, option lists and config pages — and when investigating how an element looks or lays out.
default_mode: discoverable
examples: add a button to the page; change the app's colours; build a list of options with toggles; fix the layout on mobile; the keyboard hides when I touch the row below the composer; check the chat panel on phone width in the browser
keywords: ui, button, theme, layout, toggle list, mobile, keyboard, composer
minimum_access: registered
---

# UI design and controls

Frontend guidance for the v3 interface; it grants no tool permissions. Ship UI
source through the broker task workflow. Before a browser journey, resolve its
registered route and navigate straight to it.

## Mobile and desktop, one layout

Most use is on a phone. Every UI change must work at phone width (about
360–412 CSS px) and at desktop width with one shared layout. Put controls and
help text below their heading rather than beside a fixed label column, let rows
wrap rather than shrink inputs, and keep text and buttons inside their
container. Check both widths in a browser before release and report both. Test
dark and light themes too.

### Investigate presentation and layout

Use this workflow for any visual property: dimensions, spacing, position,
alignment, opacity/transparency, backgrounds, borders, radius, shadows,
typography, visibility, stacking, responsive behavior, and interaction states.
These examples do not limit the properties or elements you can investigate.

1. Identify the element, requested properties, and relevant surfaces, themes,
   roles, breakpoints, and states from the request and existing context.
   Ask only when ambiguity changes the answer; continue independent inspection.
2. Locate the owning feature and actual renderer/style consumer. Read the
   configuration it references alongside source text. Check each value's own
   status and truncation before relying on it; another value's truncation does not
   erase a complete value.
   Unset overrides inherit according to the verified merge rules.
3. Collect related requested values together. Search the owning file for the
   setting, then read its complete bounded section. Follow a next page only when
   it contains remaining evidence. Reuse successful results; repeat a read only
   for changed/stale input, a named missing field, or insufficient output bounds.
4. Distinguish current configuration, shipped defaults, effective merged settings,
   and rendered/computed behavior. Trace applicable profiles, CSS variables,
   cascade, inherited styles, units, layout constraints, and fallback rules as
   needed. The live file's description alone does not establish factory defaults.
5. Use a registered browser journey when computed layout or state is needed to
   answer the request. A source/configuration question can be answered from
   verified reads; do not require screenshots or UI mutations for every lookup.
6. Answer every requested property with its controlling path/key or selector,
   verified value, and plain-language effect, including units and state/surface
   scope. Separate established facts from unresolved effective behavior.
   Continue available reads instead of ending with an acknowledgment or claiming
   an already-returned value is unavailable. If blocked, identify the exact
   missing evidence and concrete blocker.

### Tap highlight and focus

- Make the browser's native tap highlight transparent with
  `-webkit-tap-highlight-color: transparent` on shared control primitives.
  The property inherits, but check nested controls and menus moved to
  `document.body`; they need the same guidance on their own root.
- Remove only the native tap wash. Preserve keyboard `:focus-visible` outlines,
  intentional hover/pressed feedback, selected state, disabled styling, and
  error or destructive-action indicators.
- Prefer shared styles and tokens to duplicated page-specific overrides. Apply
  changes to v3-owned assets through the protected task workflow; bundled shared
  UI and v2 remain read-only inputs.

### Optional touch haptics

- On a completed, enabled touch activation, request one brief pulse with
  `requestTouchVibration(event)` from `ui/shared/vibrate.js` where available.
  Config: `ui/vibrate.json` -> `enabled`, `duration_ms` (milliseconds); loaded
  once per page and adopted on reload. Call from the user interaction
  handler before asynchronous work. Treat this as a request, not proof that
  hardware vibrated.
- Detect touch from the click's `pointerType`. For older touch-generated mouse
  events, a coarse-pointer media query is a fallback. Suppress zero-detail
  keyboard/programmatic clicks; mouse and keyboard interaction must stay quiet.
- Feature-detect the method. A missing API, a false return, or an exception must
  leave the action working normally. Do not simulate unsupported haptics with
  hidden inputs, permission changes, or browser-specific workarounds.
- Do not pulse on rendering, automatic updates, focus, typing, search-field
  taps, scrolling, dragging, disabled/inert controls, or denied actions.
- Use one shared feedback helper or an owned delegated listener when performing
  the button rollout. Avoid duplicate pulses from nested buttons and containers.
  Capture may be needed before action handlers stop propagation or detach a
  menu; assign one owning surface per event and remove listeners on teardown.
- Keep existing activation, authorization, confirmation, and cancellation
  behavior intact. Haptics never constitute approval or successful completion.

### Borders and connected surfaces

- A control designed with an outline keeps a visible, theme-appropriate outline
  before and after activation, including lingering mobile hover. Never make a
  closed outlined control's border transparent merely because it is hovered,
  pressed, focused, or was previously expanded.
- Preserve the intended appearance of explicitly borderless variants. Do not
  globally erase outlines or mask selected, warning, error, or disabled states.
- Use the existing border tokens and shared primitives. A session-selector-like
  chip should return to the same normal border treatment as its adjacent plus
  button after closing.
- A connected expanded menu may omit only the shared interior edge while open.
  Scope that exception to the expanded state. Restore the closed border for
  every close path: trigger tap, outside click/tap, selection, Escape,
  programmatic close, remount, or interruption.
- Keep state-dependent corner shapes compatible with configured sizing. A pill's
  rounded end has radius half its rendered height. When it joins a menu, shorten
  the joining corners to quarter circles and continue straight side borders from
  the tangents, reversing the cap for an upward-opening menu.
- Do not let an inline `!important` radius defeat expanded-state CSS. Use a
  state-controlled variable or another deliberate override. If control or icon
  sizes change, keep their geometry and centering calculated from rendered sizes.

### Verification before release

- Check default, hover, pressed, keyboard focus, expanded, closing, and restored
  states. Exercise repeated open/close cycles and every relevant dismissal path.
  Test dark and light themes, desktop input, and mobile touch.
- Configure a real mobile layout viewport in fixtures: include the viewport
  meta tag as well as device width and touch emulation. Assert the actual CSS
  viewport and `hover: none` / `pointer: coarse` media states. A narrow desktop
  screenshot alone does not reproduce persistent touch hover.
- Compare computed border width/style/color and all four closed edges against
  the intended normal control. Check connected menu seams and tangent geometry.
  Visually inspect the result after closing and after reload.
- Verify exactly one vibration request per eligible tap, including nested and
  portaled controls. Check absent, rejected, and throwing API cases; verify no
  pulses for mouse, keyboard, typing, or disabled controls, and test teardown.
- Mocks/emulation prove event handling, not physical vibration. Test a supported
  physical phone when available and state that limitation when unavailable.
- Capture source inputs before checks and confirm they are unchanged afterward.
  Use the broker's reviewed release and healthy-runtime workflow. Guidance edits
  do not authorize a button rollout; perform that rollout only when requested.

### Current reference implementation

The shared chat rollout applies these defaults through `ui/chat/controls/feedback.js`,
`ui/chat/widget/window.js`, `ui/chat/chat.css`, and `ui/chat/widget/window.css`,
using `ui/shared/vibrate.js` and the central `ui/vibrate.json` configuration.
The panel owns dynamic controls and body portals; widget chrome registers its
own roots. External hosts receive only validated configuration through the
existing source/origin/channel-bound iframe bridge and request pulses locally.
Its placement and state-dependent radius are applied by
`ui/chat/controls/chat-config.js`. The rollout covers chat controls; unrelated application pages retain their own
interaction wiring.

The public `ui/chat/widget/window.js` module must stay dependency-free: the
external embed host imports that asset directly. Inject the shared synchronous
feedback callback into its shell/launcher APIs. Pass only validated UI settings
through the existing iframe bridge; never forward vibration requests across it.

Browser references:
[Tap highlight](https://developer.mozilla.org/en-US/docs/Web/CSS/Reference/Properties/-webkit-tap-highlight-color),
[Vibration API](https://developer.mozilla.org/en-US/docs/Web/API/Navigator/vibrate).

## Theming and surfaces

### Theming — one palette drives the whole app (dark AND light)

Every new UI feature in `ui/` **must be tested and visually correct in both dark mode and light mode** before it is considered done. Themes toggle via `body.light-mode`; all theme colours live as CSS variables in **`ui/shared/css/design-system.css`**.

#### The palette is the single source of truth — re-theming is a one-file edit

`design-system.css` is layered:

1. **PALETTE block (the edit-here zone)** — at the top of **both** the `:root` (dark) and `body.light-mode` (light) blocks. Raw colours only: brand hue, purple, status hues, neutral surfaces, ambient glow.
2. **Each hue has an `-rgb` triple**; build opacity as `rgba(var(--brand-rgb), 0.2)` — a hue is authored **once** per theme.
3. **Derived/semantic tokens (wiring — don't edit to re-theme)** — `--accent`, `--accent-soft`, etc. are `var(--brand)` / `rgba(var(--brand-rgb), a)` formulas, re-declared in **both** blocks (so they pick up each block's palette).

Dark and light are **independent palettes** (factory default: **Ocean** light, **Amber** dark; **Mono** is the neutral preset).

**Rules:**

1. **Never hard-code a brand/scheme colour** (hex or rgb) in CSS, inline styles, HTML attributes, or JS templates — reference the token. Backgrounds: `--bg-0/1/2`, `--bg-elev`, `--bg-elev-2`, `--bg-tint` · Borders: `--border`, `--border-soft`, `--border-strong` (colour); width always `var(--border-width)` · Text: `--fg-1`…`--fg-4` · Brand/semantic: `--accent` (= `--brand`), `--accent-soft/-mid/-line`, `--purple`, `--success`, `--warning`, `--danger`; opacities via the `-rgb` triples.
2. **Never write `var(--foo, #darkhex)` fallbacks** — a literal dark hex silently breaks light mode; add missing variables to the palette.
3. **A new theme-sensitive colour goes in the PALETTE block of BOTH theme blocks** (+ `-rgb` triple if alpha needed) — never sprinkled through feature code.
4. **Verify before pushing:** toggle `body.light-mode` and confirm legibility in both modes.

#### The animated background reads the palette too

Backgrounds are plugins: a drop-in folder `ui/background/<id>/` (`<id>.json` + `<id>.js` + `<id>.css`); the engine in `ui/background/_engine/` loads the admin's **per-theme** pick (App Settings → **Appearance**). Built-in **None** = plain themed background. A plugin's tokens live in its own `<id>.css` but **reference the central palette**; renderers re-read tokens on theme flip. Deleting the folder removes the background everywhere (fallback: Stargaze). Cursor glow / card spotlights are **not** part of a background — `ui/shared/js/cursor-effects.js`.

#### Global border control (one knob for colour · thickness · on-off)

Write every **neutral container / table / divider border** as `var(--border-width) solid var(--border)` (or `-soft` / `-strong`). Then: **recolour** = `--border`/`-soft`/`-strong` in CSS, or `border_color_dark`/`border_color_light` in `data/config/app-settings.json`; **thickness** = `--border-width` or `border_width` (default `1px`); **all off** = width `0`. Edit surfaces: App Settings → **Appearance**; the JSON (served via `/api/v1/auth/ui-config`, injected at boot by `ui/shared/js/appearance.js`); or the PALETTE defaults.

Rules: (1) a neutral border's width is `var(--border-width)`, never a literal px — a hard-coded `1px` opts out of the thickness/off control. (2) Accent/semantic borders (focus rings, hover/active highlights, error/drag outlines, status borders, spinner rings) are deliberately **not** wired to this — leave them. (3) Colour-only exceptions: the floating-glass surfaces in `index.css` (`#main-panel`, chat pill family, bubbles, `#chat-header`) keep a translucent `color-mix(currentColor)` border *colour* but **do** read `var(--border-width)`.

#### Known self-contained palette exceptions

Not driven by the global scheme — edit these too in a full re-skin: the **agent-loop visualizer** (`ui/main-panel/agents/agent-loop/loop-visual.css` + its JS) owns a complete dark **and** light palette; the **terminal ANSI palette** (`ui/shared/js/terminal.js`) is terminal-standard; **animated backgrounds** own bespoke values in their plugin CSS.

#### Appearance panel, per-user themes, token pipeline (summary)

App Settings → **Appearance** is **one** group: a two-row expandable table (**Light** and **Dark** rows) + a shared theme-independent card (border thickness, fonts). Each theme row: preset chips (Mono / Amber / Ocean / Forest / Grape / Rose — a preset click is a **complete reset** of that mode), the background select, and the full palette as swatches; all auto-saving. **Per-user themes:** the admin switch `allow_user_appearance` (off by default) gives each user a "My appearance" editor; their overrides (`user_profiles.appearance`) layer **over** the global theme inside `/api/v1/auth/ui-config` — unset tokens keep the global value.

A token reaches the screen as `app-settings.json` keys `<base>_dark`/`<base>_light` → `/api/v1/auth/ui-config` → `appearance.js` injection (derives `-rgb` triples; honours alpha). The **graduated neutrals are derived, not editable** — `--bg-1`, `--border-soft`, `--fg-2`, etc. are `color-mix` functions of the **6 raw neutral knobs** (`--bg-0`, `--bg-elev`, `--bg-tint`, `--border`, `--fg-1`, `--fg-3`); derive any new neutral step the same way, never hard-code a neutral hex. **Adding a token to the editor = three mirrored places:** `TOKENS` in `appearance.js`; `_DEFAULT_APPEARANCE` + `AppSettings` field in `app/admin/settings.py`; `TOKEN_META` in `ui/shared/js/appearance-editor.js`. A brand-new key needs a **server restart**. Full wiring: **`docs/claude/ui-theming-reference.md`**.

### Edge fades over a dynamic/themed background — mask, never a colour

When a scrolling strip needs its edges to **fade out**, **fade the content with a CSS alpha mask — never paint a coloured gradient.** Over the app's live, moving backgrounds a guessed gradient colour shows as a mismatched rectangle; a mask paints no colour and always matches. Pattern: (1) **transparent affordances** over the edge (`background: none`, glyph colour from a design-system var); (2) a **`mask-image` linear-gradient on the scroller itself** (`transparent → #000` over the fade width), only on sides that can still scroll. Canonical: `.agent-card-tabs` in `ui/main-panel/agents/agents.css`; the `#main-tabs` block in `design-system.css`.

### Pinned rounded scroll corners — mask, reveal the background

For rounded corners that **stay put while content scrolls**, use **`.rounded-scroll-corners`** in `ui/shared/css/design-system.css`: an alpha mask (`mask-composite: add` union) knocking out only the four outer corners — the scroller stays full-width and the corners reveal the real background. Knobs: **`--rsc-r`** (radius, default `--r-lg`), **`--rsc-side`** (per-side inset to a centred column, default `0`). A `0` radius makes it a no-op, so a shared scroller can carry the class inert until switched on (App Settings feeds a computed `--rsc-side`).

### Mobile soft keyboard — `body.kb-open`

On mobile-width viewports, when the visual viewport is meaningfully shorter than the layout viewport **and** an editable field holds focus, `index.html`'s viewport tracker sets **`body.kb-open`** (never on desktop). Use it to hide bottom-docked clutter while the keyboard is up — App Config does this in `app3.css` (chat pills hide; sticky headings drop from `sticky` to `static`). Scope any new reaction to the page's own selectors.

Design history and provenance: docs/ui-theming-design-history.md — not needed for operative work.

## Option lists and config pages

### Toggle-list (category + rows of options) — one shared design

Whenever a surface shows a category/section heading followed by a vertical list of options — each row an icon, a title and a control (usually a toggle) on one line, description below — it must use the app's single shared "compact row list" look (reference: Admin Tools → Agent Settings → Agent Tools): one bordered, rounded container holding flush rows. Never wrap each option in its own card. This is a hard rule: per-copy styling drifts — one set of classes drives all of them, so a change made once cascades everywhere.

**Stacked-row layout.** In the row (a `flex-wrap` container), `.ac-ability-label` is `display: contents`: the title (`.ac-ability-name`, `flex: 1`) keeps the control at the right edge, and the description (`.ac-ability-desc`, `flex: 0 1 100%`, `order: 1`) drops to its own full-width line below. Fixed-width-column variants opt out with `display: block`.

**Two separate patterns, don't conflate them:** (a) the collapsible section header (`.ac-category-group`) and (b) the compact option-row list inside it (`.ac-abilities-compact`, legacy alias of `.ac-list`).

#### Canonical implementation — copy this, don't re-invent

The whole component lives in **`ui/shared/css/app3.css`** under the boxed comment **"SHARED EXPANDABLE OPTION-ROW LIST"**:

- **`.ac-list`** — the bordered, rounded container; it draws the hairline dividers (`.ac-list > * + *`). `.ac-abilities-compact` is its legacy alias.
- **`.ac-ability-row`** — one row: `.ac-ability-icon` + `.ac-ability-label` (→ `.ac-ability-name` + `.ac-ability-desc`) + optional `.ac-ability-status`, `.ac-ability-toggle-wrap`, `.ac-row-chevron`.
- **`.ac-row`** — wraps a row + its body to make it **expandable** (JS adds `.expanded`); `.ac-row-static` for a non-expanding item.
- **`.ac-ability-body`** — the collapsible region revealed on expand, hidden until opened (`.ac-row.expanded`; `.ac-body-open` for admin member rows). An **empty** body is suppressed (`:empty` → `display:none`).

Light-mode overrides (peach tint) sit in the `body.light-mode` rules below.

> **Restyle the toggle-list look in `app3.css` only.** Don't fork a per-page copy. The agent card keeps its `.conn-*` class names purely as JS/behaviour hooks.

#### It's not just for simple toggle rows — the row is a flexible shell

The compact table is the right home for **any "category → list of options" surface**; the row shell accepts whatever control the option needs:

- **Toggle rows** — the default (a switch on the right). **Other controls** also live on a row's right: a radio, a select, a button, a small input, or a status pill.
  - **Number fields are horizontal steppers, not native spinners.** Every `.ac-config-num` renders inside `.ac-num-stepper` with back + forward Lucide arrows pinned **inside** the field's edges. Arrows step by `step`, clamped to `min`/`max`, firing `input` + `change`.
- **Expandable rows** — `.ac-row` + `.ac-ability-body`; put the **control itself in the expanded body** when too big for the collapsed line.
- **Reorderable rows** — a row can carry a drag handle and be `draggable`. Panel Pages rows carry a 3-position **`.ac-mp-tri`** visibility toggle (Off · Signed-in only · Always visible). A **locked** page can never be **off** — its tri skips the off slot (`_VIS_STATES_LOCKED`) and `off` is rejected server-side.
- **Locked / disabled / system rows** — non-interactive rows that just render the state, kept in the same list.
- **Grouped rows** — the group is an **expandable header row** (`.ac-row.ac-group`) *inside the one `.ac-list`* — never a nested bordered box; members are plain `.ac-ability-row`s. Group head: a **3-position `.ac-tri`** (Off · Mixed · On; halves set all members, centre Mixed is **derived**); a group with no togglable members shows a greyed `.ac-tri.ac-tri-disabled`, not a gap. Member config renders as **one continuous `.ac-list`** in the body (never a sub-table); builder rows are inserted as nodes, **never via `innerHTML`**.

> **Sister panels — keep the two ability tables MIRRORED.** The admin **Agent Settings** table and the per-agent **Abilities tab** render the *same* table design: any change to one side's look, structure, grouping, or toggle behaviour must be applied to the other in the same change. Both sites carry the banner **`SISTER-PANEL: AGENT-ABILITY-TABLE`** — grep it. (Models likewise: **`SISTER-PANEL: MODEL-TABLE`**.)

**Unified save-status indicator.** Every persisted config control confirms a save the SAME way: an overlay centred **on top of** the control — spinner while saving (held ~400ms) → green ✓ on backend confirm → fade; on failure an orange ⚠ over the control, a tiny popup above it ("Not saved"), **and the control reverts to its prior value**. Helpers: `_saveOverlay`, `_markSaving`, `_flashSaveCheck` in `dom-utils.js`.

**When NOT to use it:** a genuine **form** (free text/number fields, no per-option category structure) stays a form; **one complex configurator** (many interdependent fields, live status, action buttons) is not a list of options. Rule of thumb: "a list of things, each turned on / chosen / reordered" → compact table; "a single thing with many fields" → not. A hybrid is fine (rows expanding to hold a configurator).

The full per-area conformance audit (status legend, exact files per surface) lives in **`docs/claude/ui-toggle-list-conformance.md`** — read it whenever this design changes.

#### The standard config-page layout (App Settings is the reference)

1. **No page-title header.** Don't open the section with an `.ac-section-header` — the tab carousel already names the page.
2. **Subheadings are title-only.** A group is the plain-text `.ac-category-summary` (icon + title) — **no `.ac-category-desc`** explainer under it.
3. **Every group body is one `.ac-list` of uniform rows** — `.ac-ability-row` with its control in a right-aligned `.ac-config-control` slot. **Each group is capped at 750px and centred** — the cap goes on `.ac-category-summary`, `.ac-category-body` *and* `.ac-list`, **not** on `.ac-category-group` (`display:contents` under sticky-nav).
4. **JS-built sections ship a build gate.** `.ac-section.is-building` + one `.ac-section-loader` child hide every real child behind a spinner until reveal. Reveal **only after structure AND the first data load**; `init()` owns the whole reveal (never gate it on `load()`); clear the class directly — **no `requestAnimationFrame`**.
5. **Sticky section navigator (shared, opt-in).** Add class `ac-stickynav` to the page's `<section class="ac-section">` and call `initStickyNav(...)`. Rules: `.ac-stickynav .ac-category-group { display: contents }`; headings are **SQUARE** (`border-radius: 0`); collapsible groups are `<div class="ac-category-group ac-collapsible">` toggled by `.ac-open` — **never `<details>`**.

**Related shared controls.** The on/off switch (`.conn-toggle`) lives in global `app3.css`; its active colour is a legacy blue, a known quirk. The per-agent Abilities row uses **one** 3-position visibility tri (`.ac-tri.ac-tri-vis`: Off · Discoverable · Visible) instead of a switch + eye pair — **agent-only** (the admin table keeps its plain switch + delete button), plus an **"Available to"** dropdown (`.ac-access-select`: Everyone · Registered · Admins), a server-enforced boundary. Destructive confirmations use **`hazardConfirm(...)`** from `ui/shared/js/confirm-dialog.js` — never native `confirm()`.

Design history and provenance: docs/ui-toggle-list-design-history.md — not needed for operative work.
