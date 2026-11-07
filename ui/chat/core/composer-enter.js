/**
 * Purpose: Resolves the composer Enter policy from chat_common.composer.enter_behavior and
 * decides whether a keydown submits. Kept pure so the policy is testable without the panel.
 */

/** Accepted enter_behavior values; an unset or unknown value falls back to newline. */
export const ENTER_BEHAVIORS = ['newline', 'send', 'auto'];

/** Coerces the configured value to a supported behavior; newline is the default. */
export function resolveEnterBehavior(profile) {
  const configured = profile?.composer?.enter_behavior;
  return ENTER_BEHAVIORS.includes(configured) ? configured : 'newline';
}

/**
 * Reports whether this keydown submits the composer.
 * Shift+Enter always inserts a newline, IME composition never submits, and Ctrl/Cmd+Enter always sends.
 */
export function enterSubmits(event, behavior, touch) {
  if (!event || event.key !== 'Enter' || event.shiftKey || event.isComposing) return false;
  if (event.ctrlKey || event.metaKey) return true;
  if (behavior === 'send') return true;
  if (behavior === 'auto') return !touch;
  return false;
}
