// @code-index {"version":1,"feature":"ui-vibration","aliases":["touch haptics","button vibration"],"purpose":"Shared optional touch feedback. Config: ui/vibrate.json -> enabled and duration_ms; loaded once per page, adopted on reload.","span_lines":40,"configuration":{"status":"configured","references":[{"path":"ui/vibrate.json","pointers":["/enabled","/duration_ms"]}],"opportunities":[]}}
import {createTouchVibration} from '../chat/widget/window.js';
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

/** Returns a fresh UI-settings copy; embed-app/loader own source, origin and channel validation. */
export function getVibrationConfig() {
  return config ? {...config} : null;
}

/** Shared synchronous touch pulse; Config: ui/vibrate.json -> enabled, duration_ms. */
export const requestTouchVibration = createTouchVibration(() => config);
