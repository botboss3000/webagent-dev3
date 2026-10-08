---
id: ui_toggle_list
title: Toggle-list / config-page pattern
when_to_use: Use when building or changing a "category to list of options" surface — ability tables, config pages, remote access, the shared compact option-row list.
default_mode: visible
minimum_access: registered
---

# Toggle-list / config-page pattern

## Toggle-list (category + rows of options) — one shared design

Whenever a surface shows a category/section heading followed by a vertical list of options — each row an icon, a title and a control (usually a toggle) on one line, description below — it must use the app's single shared "compact row list" look (reference: Admin Tools → Agent Settings → Agent Tools): one bordered, rounded container holding flush rows. Never wrap each option in its own card. This is a hard rule: per-copy styling drifts — one set of classes drives all of them, so a change made once cascades everywhere.

**Stacked-row layout.** In the row (a `flex-wrap` container), `.ac-ability-label` is `display: contents`: the title (`.ac-ability-name`, `flex: 1`) keeps the control at the right edge, and the description (`.ac-ability-desc`, `flex: 0 1 100%`, `order: 1`) drops to its own full-width line below. Fixed-width-column variants opt out with `display: block`.

**Two separate patterns, don't conflate them:** (a) the collapsible section header (`.ac-category-group`) and (b) the compact option-row list inside it (`.ac-abilities-compact`, legacy alias of `.ac-list`).

### Canonical implementation — copy this, don't re-invent

The whole component lives in **`ui/shared/css/app3.css`** under the boxed comment **"SHARED EXPANDABLE OPTION-ROW LIST"**:

- **`.ac-list`** — the bordered, rounded container; it draws the hairline dividers (`.ac-list > * + *`). `.ac-abilities-compact` is its legacy alias.
- **`.ac-ability-row`** — one row: `.ac-ability-icon` + `.ac-ability-label` (→ `.ac-ability-name` + `.ac-ability-desc`) + optional `.ac-ability-status`, `.ac-ability-toggle-wrap`, `.ac-row-chevron`.
- **`.ac-row`** — wraps a row + its body to make it **expandable** (JS adds `.expanded`); `.ac-row-static` for a non-expanding item.
- **`.ac-ability-body`** — the collapsible region revealed on expand, hidden until opened (`.ac-row.expanded`; `.ac-body-open` for admin member rows). An **empty** body is suppressed (`:empty` → `display:none`).

Light-mode overrides (peach tint) sit in the `body.light-mode` rules below.

> **Restyle the toggle-list look in `app3.css` only.** Don't fork a per-page copy. The agent card keeps its `.conn-*` class names purely as JS/behaviour hooks.

### It's not just for simple toggle rows — the row is a flexible shell

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

### The standard config-page layout (App Settings is the reference)

1. **No page-title header.** Don't open the section with an `.ac-section-header` — the tab carousel already names the page.
2. **Subheadings are title-only.** A group is the plain-text `.ac-category-summary` (icon + title) — **no `.ac-category-desc`** explainer under it.
3. **Every group body is one `.ac-list` of uniform rows** — `.ac-ability-row` with its control in a right-aligned `.ac-config-control` slot. **Each group is capped at 750px and centred** — the cap goes on `.ac-category-summary`, `.ac-category-body` *and* `.ac-list`, **not** on `.ac-category-group` (`display:contents` under sticky-nav).
4. **JS-built sections ship a build gate.** `.ac-section.is-building` + one `.ac-section-loader` child hide every real child behind a spinner until reveal. Reveal **only after structure AND the first data load**; `init()` owns the whole reveal (never gate it on `load()`); clear the class directly — **no `requestAnimationFrame`**.
5. **Sticky section navigator (shared, opt-in).** Add class `ac-stickynav` to the page's `<section class="ac-section">` and call `initStickyNav(...)`. Rules: `.ac-stickynav .ac-category-group { display: contents }`; headings are **SQUARE** (`border-radius: 0`); collapsible groups are `<div class="ac-category-group ac-collapsible">` toggled by `.ac-open` — **never `<details>`**.

**Related shared controls.** The on/off switch (`.conn-toggle`) lives in global `app3.css`; its active colour is a legacy blue, a known quirk. The per-agent Abilities row uses **one** 3-position visibility tri (`.ac-tri.ac-tri-vis`: Off · Discoverable · Visible) instead of a switch + eye pair — **agent-only** (the admin table keeps its plain switch + delete button), plus an **"Available to"** dropdown (`.ac-access-select`: Everyone · Registered · Admins), a server-enforced boundary. Destructive confirmations use **`hazardConfirm(...)`** from `ui/shared/js/confirm-dialog.js` — never native `confirm()`.

Design history and provenance: docs/ui-toggle-list-design-history.md — not needed for operative work.
