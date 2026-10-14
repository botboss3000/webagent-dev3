# UI Admin — Default button interaction guidance

Read this before creating or changing WebAgent UI controls. These defaults apply
across the v3 interface: text and icon buttons, tabs, switches, menu actions,
button-like links, and actionable rows, including panel, mobile, widget, and
portaled controls. Preserve an explicit user-requested exception.

This is frontend guidance, not execution authority. UI Admin remains pending
in v3; its legacy tool names are not callable. Use the authorized Codebase Admin
workflow to inspect and edit frontend HTML, CSS, and JavaScript. Read the
checkout instructions, relevant implementation, and registered UI journey.
Loading or reading this document grants no tool permissions.

## Tap highlight and focus

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

## Optional touch haptics

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

## Borders and connected surfaces

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

## Verification before release

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

## Current reference implementation

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
