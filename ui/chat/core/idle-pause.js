// Pauses an unused chat tab's server traffic. Config: config/ui/chat/chat.json ->
// chat_common.idle_pause ({enabled, hidden_ms, idle_ms}; 0 turns either rule off).
// A tab in the background for hidden_ms, or visible with no input for idle_ms, stops
// polling and closes its event stream; the next input or showing the tab resumes it.

let pausedNow = false;

/** Whether this page's chat is paused; pollers skip their requests while it is. */
export function idlePaused() { return pausedNow; }

/** Reads bounded settings from the chat profile (chat_common merged), or null when off. */
export function idleSettings(profile) {
  const value = profile?.idle_pause;
  if (!value || value.enabled !== true) return null;
  const bound = (ms, min, max) => (typeof ms === 'number' && Number.isFinite(ms) && ms > 0)
    ? Math.max(min, Math.min(max, ms)) : 0;
  const settings = {hidden: bound(value.hidden_ms, 30000, 3600000), idle: bound(value.idle_ms, 60000, 14400000)};
  return settings.hidden || settings.idle ? settings : null;
}

/**
 * Calls onPause once the tab goes unused and onResume on the next input or when it is shown.
 * Returns {paused, destroy}; destroy removes listeners and resumes nothing.
 */
export function watchIdlePause({getConfig, onPause, onResume, doc = document, now = () => Date.now(),
  schedule = setTimeout, cancel = clearTimeout}) {
  let lastInput = now(), hiddenSince = doc.hidden ? now() : 0, timer = null, stopped = false;
  const arm = ms => { cancel(timer); timer = stopped ? null : schedule(check, Math.max(1000, ms)); };
  function check() {
    timer = null;
    if (stopped || pausedNow) return;
    const settings = idleSettings(getConfig());
    if (!settings) return arm(60000);
    const t = now(), rule = doc.hidden ? settings.hidden : settings.idle;
    if (!rule) return arm(60000);
    const left = (doc.hidden ? hiddenSince : lastInput) + rule - t;
    if (left > 0) return arm(Math.min(left, 60000));
    pausedNow = true;
    onPause();
  }
  function wake() {
    if (stopped) return;
    lastInput = now();
    if (pausedNow) { pausedNow = false; onResume(); }
    arm(1000);
  }
  function visibility() {
    if (doc.hidden) { hiddenSince = now(); arm(1000); } else wake();
  }
  const events = ['pointerdown', 'keydown', 'wheel', 'touchstart'];
  for (const type of events) doc.addEventListener(type, wake, {capture: true, passive: true});
  doc.addEventListener('visibilitychange', visibility);
  arm(1000);
  return {
    get paused() { return pausedNow; },
    destroy() {
      stopped = true; cancel(timer); timer = null; pausedNow = false;
      for (const type of events) doc.removeEventListener(type, wake, {capture: true});
      doc.removeEventListener('visibilitychange', visibility);
    },
  };
}
