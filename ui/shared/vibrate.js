// Shared optional touch feedback.
import {createTouchVibration} from '../chat/widget/window.js';
let config = null;

/** Accepts one stage object; malformed stages are ignored. */
function stage(value) {
  if (!value || typeof value !== 'object' || typeof value.enabled !== 'boolean' || !Number.isSafeInteger(value.duration_ms) || value.duration_ms < 0) return null;
  return {enabled: value.enabled, duration_ms: value.duration_ms};
}

/** Loads shared timing once per page; failed or invalid configuration leaves haptics off.
 * Release falls back to the legacy top-level duration_ms so older saved configs keep working.
 */
async function loadVibrationConfig() {
  try {
    const response = await fetch(new URL('../vibrate.json', import.meta.url), {cache: 'no-store'});
    if (!response.ok) return;
    const value = await response.json();
    if (typeof value.enabled !== 'boolean') return;
    const release = stage(value.release) || (Number.isSafeInteger(value.duration_ms) && value.duration_ms >= 0 ? {enabled: true, duration_ms: value.duration_ms} : null);
    if (!release) return;
    config = {enabled: value.enabled, impact: stage(value.impact), release};
  } catch { /* Optional feedback must never block control activation. */ }
}
export const vibrationConfigReady = loadVibrationConfig();

/** Returns a fresh UI-settings copy; embed-app/loader own source, origin and channel validation. */
export function getVibrationConfig() {
  return config ? {...config} : null;
}

/** Shared synchronous touch pulse; Config: ui/vibrate.json -> enabled, duration_ms. */
export const requestTouchVibration = createTouchVibration(() => config);
