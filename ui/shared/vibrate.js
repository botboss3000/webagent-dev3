// @code-index {"version":1,"feature":"ui-vibration","aliases":["touch haptics","button vibration"],"purpose":"Shared optional touch feedback. Config: ui/vibrate.json -> enabled and duration_ms; loaded once per page, adopted on reload.","span_lines":40,"configuration":{"status":"configured","references":[{"path":"ui/vibrate.json","pointers":["/enabled","/duration_ms"]}],"opportunities":[]}}
let config = null;

/** Loads shared timing once per page; failed or invalid configuration leaves haptics off. */
async function loadVibrationConfig() {
  try {
    const response = await fetch(new URL('../vibrate.json', import.meta.url), {cache: 'no-store'});
    if (!response.ok) return;
    const value = await response.json();
    if (typeof value.enabled !== 'boolean' || !Number.isSafeInteger(value.duration_ms) || value.duration_ms < 0) return;
    config = {enabled: value.enabled, duration_ms: value.duration_ms};
  } catch { /* Optional feedback must never block control activation. */ }
}
export const vibrationConfigReady = loadVibrationConfig();

/** Requests configured feedback synchronously for an eligible touch click.
 * Call once from the owning enabled control, before asynchronous action work.
 * Mouse/keyboard clicks and unavailable or blocked vibration remain quiet.
 */
export function requestTouchVibration(event) {
  if (!config?.enabled || config.duration_ms === 0 || event.detail === 0) return;
  const touch = event.pointerType ? event.pointerType === 'touch' : window.matchMedia?.('(pointer: coarse)').matches;
  if (!touch || typeof navigator.vibrate !== 'function') return;
  try { navigator.vibrate(config.duration_ms); } catch { /* Unsupported haptics never block a tap. */ }
}
