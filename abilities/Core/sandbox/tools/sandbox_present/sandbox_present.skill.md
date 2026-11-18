# sandbox_present

## Usage

- Place one saved UI element in this chat's Artifacts chip as a card: your `title`, your short `description` and the element itself, which the user opens from the chip.
- `params` (an object) reaches the element as `webagent.params`; use it for the values one card needs, such as a preview `variant`, and `preview_session` when that variant is in one of this chat's sub-agent sessions.

Pass `element` (a saved element's name), optional `scope` (`mine`, the default, or `shared`), `title`, `description`, optional `params` and optional `id`. Cards with the same `id` replace each other; the default id is `element-<element>`, so give each card its own `id` (for example `option-a`) when one element backs several cards. The element must already be saved with `sandbox_save`.

The card runs the element in a sandboxed frame. Its actions (preview on or off, sending a prompt after a countdown, opening an allowed app page) all go through the app, which checks each one; see the Sandbox skill.
