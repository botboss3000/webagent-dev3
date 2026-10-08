---
id: ui_theming
title: UI theming and surfaces
when_to_use: Use when building or changing WebAgent UI appearance — the design-system palette, borders, the Appearance panel, per-user themes, edge fades, rounded scroll corners, and mobile keyboard behaviour.
default_mode: visible
minimum_access: registered
---

# UI theming and surfaces

## Theming — one palette drives the whole app (dark AND light)

Every new UI feature in `ui/` **must be tested and visually correct in both dark mode and light mode** before it is considered done. Themes toggle via `body.light-mode`; all theme colours live as CSS variables in **`ui/shared/css/design-system.css`**.

### The palette is the single source of truth — re-theming is a one-file edit

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

### The animated background reads the palette too

Backgrounds are plugins: a drop-in folder `ui/background/<id>/` (`<id>.json` + `<id>.js` + `<id>.css`); the engine in `ui/background/_engine/` loads the admin's **per-theme** pick (App Settings → **Appearance**). Built-in **None** = plain themed background. A plugin's tokens live in its own `<id>.css` but **reference the central palette**; renderers re-read tokens on theme flip. Deleting the folder removes the background everywhere (fallback: Stargaze). Cursor glow / card spotlights are **not** part of a background — `ui/shared/js/cursor-effects.js`.

### Global border control (one knob for colour · thickness · on-off)

Write every **neutral container / table / divider border** as `var(--border-width) solid var(--border)` (or `-soft` / `-strong`). Then: **recolour** = `--border`/`-soft`/`-strong` in CSS, or `border_color_dark`/`border_color_light` in `data/config/app-settings.json`; **thickness** = `--border-width` or `border_width` (default `1px`); **all off** = width `0`. Edit surfaces: App Settings → **Appearance**; the JSON (served via `/api/v1/auth/ui-config`, injected at boot by `ui/shared/js/appearance.js`); or the PALETTE defaults.

Rules: (1) a neutral border's width is `var(--border-width)`, never a literal px — a hard-coded `1px` opts out of the thickness/off control. (2) Accent/semantic borders (focus rings, hover/active highlights, error/drag outlines, status borders, spinner rings) are deliberately **not** wired to this — leave them. (3) Colour-only exceptions: the floating-glass surfaces in `index.css` (`#main-panel`, chat pill family, bubbles, `#chat-header`) keep a translucent `color-mix(currentColor)` border *colour* but **do** read `var(--border-width)`.

### Known self-contained palette exceptions

Not driven by the global scheme — edit these too in a full re-skin: the **agent-loop visualizer** (`ui/main-panel/agents/agent-loop/loop-visual.css` + its JS) owns a complete dark **and** light palette; the **terminal ANSI palette** (`ui/shared/js/terminal.js`) is terminal-standard; **animated backgrounds** own bespoke values in their plugin CSS.

### Appearance panel, per-user themes, token pipeline (summary)

App Settings → **Appearance** is **one** group: a two-row expandable table (**Light** and **Dark** rows) + a shared theme-independent card (border thickness, fonts). Each theme row: preset chips (Mono / Amber / Ocean / Forest / Grape / Rose — a preset click is a **complete reset** of that mode), the background select, and the full palette as swatches; all auto-saving. **Per-user themes:** the admin switch `allow_user_appearance` (off by default) gives each user a "My appearance" editor; their overrides (`user_profiles.appearance`) layer **over** the global theme inside `/api/v1/auth/ui-config` — unset tokens keep the global value.

A token reaches the screen as `app-settings.json` keys `<base>_dark`/`<base>_light` → `/api/v1/auth/ui-config` → `appearance.js` injection (derives `-rgb` triples; honours alpha). The **graduated neutrals are derived, not editable** — `--bg-1`, `--border-soft`, `--fg-2`, etc. are `color-mix` functions of the **6 raw neutral knobs** (`--bg-0`, `--bg-elev`, `--bg-tint`, `--border`, `--fg-1`, `--fg-3`); derive any new neutral step the same way, never hard-code a neutral hex. **Adding a token to the editor = three mirrored places:** `TOKENS` in `appearance.js`; `_DEFAULT_APPEARANCE` + `AppSettings` field in `app/admin/settings.py`; `TOKEN_META` in `ui/shared/js/appearance-editor.js`. A brand-new key needs a **server restart**. Full wiring: **`docs/claude/ui-theming-reference.md`**.

## Edge fades over a dynamic/themed background — mask, never a colour

When a scrolling strip needs its edges to **fade out**, **fade the content with a CSS alpha mask — never paint a coloured gradient.** Over the app's live, moving backgrounds a guessed gradient colour shows as a mismatched rectangle; a mask paints no colour and always matches. Pattern: (1) **transparent affordances** over the edge (`background: none`, glyph colour from a design-system var); (2) a **`mask-image` linear-gradient on the scroller itself** (`transparent → #000` over the fade width), only on sides that can still scroll. Canonical: `.agent-card-tabs` in `ui/main-panel/agents/agents.css`; the `#main-tabs` block in `design-system.css`.

## Pinned rounded scroll corners — mask, reveal the background

For rounded corners that **stay put while content scrolls**, use **`.rounded-scroll-corners`** in `ui/shared/css/design-system.css`: an alpha mask (`mask-composite: add` union) knocking out only the four outer corners — the scroller stays full-width and the corners reveal the real background. Knobs: **`--rsc-r`** (radius, default `--r-lg`), **`--rsc-side`** (per-side inset to a centred column, default `0`). A `0` radius makes it a no-op, so a shared scroller can carry the class inert until switched on (App Settings feeds a computed `--rsc-side`).

## Mobile soft keyboard — `body.kb-open`

On mobile-width viewports, when the visual viewport is meaningfully shorter than the layout viewport **and** an editable field holds focus, `index.html`'s viewport tracker sets **`body.kb-open`** (never on desktop). Use it to hide bottom-docked clutter while the keyboard is up — App Config does this in `app3.css` (chat pills hide; sticky headings drop from `sticky` to `static`). Scope any new reaction to the page's own selectors.

Design history and provenance: docs/ui-theming-design-history.md — not needed for operative work.
