// @code-index {"version":1,"feature":"chat-screen-awake","aliases":["mobile wake lock","idle dimming"],"purpose":"Selected-conversation mobile display lease. Config: ui/screen-awake.json -> enabled, mobile_only, after_chat_ms, after_input_ms, dim_after_ms, dim_opacity; validated per-device overrides apply immediately.","span_lines":40,"configuration":{"status":"configured","references":[{"path":"ui/screen-awake.json","pointers":["/enabled","/mobile_only","/after_chat_ms","/after_input_ms","/dim_after_ms","/dim_opacity"]}],"opportunities":[]}}
const ACTIVE = new Set(['queued', 'running', 'waiting_gate', 'waiting_external']);
const WAITING = new Set(['waiting_approval', 'recovery']);
const PREFERENCES = ['enabled', 'after_chat_ms', 'after_input_ms', 'dim_after_ms', 'dim_opacity'];
const STORAGE_KEY = 'v3.chat.screen-awake';
/** Reject malformed settings rather than allowing an accidental unlimited display lease. */
export function screenPolicy(value) {
  if (!value || typeof value.enabled !== 'boolean' || typeof value.mobile_only !== 'boolean') return null;
  for (const key of ['after_chat_ms', 'after_input_ms', 'dim_after_ms']) {
    if (!Number.isSafeInteger(value[key]) || value[key] < 0 || value[key] > 2147483647) return null;
  }
  if (!Number.isFinite(value.dim_opacity) || value.dim_opacity < 0 || value.dim_opacity > 0.95) return null;
  return {...value};
}
/** Only display preferences may override the static policy; invalid or unavailable device storage is ignored. */
export function deviceScreenPolicy(defaults, storage) {
  if (!screenPolicy(defaults)) return null;
  try {
    const stored = JSON.parse(storage?.getItem(STORAGE_KEY) || 'null');
    if (stored && typeof stored === 'object' && !Array.isArray(stored) && Object.keys(stored).every(key => PREFERENCES.includes(key))) {
      return screenPolicy({...defaults, ...stored}) || screenPolicy(defaults);
    }
  } catch { /* Storage errors do not disable chat or its valid defaults. */ }
  return screenPolicy(defaults);
}
/** Own one serialized wake-lock request and real activity timestamps, independent of snapshot polling.
 * Dimming changes only app appearance, and only while a screen lock is actually held.
 * Backgrounding, switching conversations and teardown fence late acquisition results.
 */
export function createScreenAwake({config, doc = document, nav = navigator, win = window,
  clock = () => performance.now(), later = setTimeout, cancel = clearTimeout,
  mobile = () => win.matchMedia('(pointer: coarse)').matches, setDim = () => {}}) {
  let session = '', active = false, waiting = '', epoch = 0, lock = null;
  let lastInput = -Infinity, lastNotice = -Infinity, lastIdle = -Infinity, lastBright = clock();
  let pending = false, timer = null, disposed = false, suspended = false, blocked = false;
  let policy = screenPolicy(config);
  const listeners = [];
  /** Register removable passive input/lifecycle handlers without interfering with their original actions. */
  function listen(target, type, fn) {
    target.addEventListener(type, fn, {capture:true, passive:true});
    listeners.push(() => target.removeEventListener(type, fn, true));
  }
  /** Separate grace periods retain the later deadline, and configuration changes use actual event times. */
  function deadline() {
    return policy ? Math.max(lastInput + policy.after_input_ms, lastNotice + policy.after_chat_ms, lastIdle + policy.after_chat_ms) : -Infinity;
  }
  /** Only selected conversations in eligible foreground documents can hold a lease. */
  function eligible() {
    return !!policy?.enabled && !disposed && !suspended && !!session && doc.visibilityState === 'visible' &&
      (!policy.mobile_only || mobile()) && (active || clock() < deadline());
  }
  /** Clear identity before asynchronous release so an old release event cannot clear a replacement. */
  function release() {
    const old = lock; lock = null;
    if (old) Promise.resolve().then(() => old.release()).catch(() => {});
  }
  /** Repaint and schedule real expiry boundaries; denied/revoked locks retry only after meaningful activity. */
  function refresh() {
    if (timer !== null) cancel(timer);
    timer = null;
    const wanted = eligible(), now = clock();
    if (!wanted) { release(); setDim(false, policy?.dim_opacity || 0); return; }
    const brightDeadline = lastBright + policy.dim_after_ms;
    setDim(!!lock && policy.dim_after_ms > 0 && now >= brightDeadline, policy.dim_opacity);
    const next = Math.min(active ? Infinity : deadline(), policy.dim_after_ms > 0 && brightDeadline > now ? brightDeadline : Infinity);
    if (Number.isFinite(next)) timer = later(refresh, Math.max(1, next - now));
    if (lock || pending || blocked || !nav.wakeLock?.request) return;
    pending = true;
    const requestEpoch = epoch;
    Promise.resolve().then(() => nav.wakeLock.request('screen')).then(sentinel => {
      pending = false;
      if (!eligible() || requestEpoch !== epoch || sentinel.released) {
        Promise.resolve().then(() => sentinel.release()).catch(() => {});
        if (sentinel.released && requestEpoch === epoch) blocked = true;
        refresh(); return;
      }
      lock = sentinel;
      sentinel.addEventListener('release', () => {
        if (lock !== sentinel) return;
        lock = null; blocked = true; setDim(false, policy.dim_opacity);
      }, {once:true});
      refresh();
    }).catch(() => { pending = false; blocked = true; setDim(false, policy?.dim_opacity || 0); });
  }
  /** Restore appearance and start the corresponding grace period only for input/live message/notice activity. */
  function attention(kind = 'message') {
    if (!session || disposed || suspended || doc.visibilityState !== 'visible') return;
    const now = clock();
    if (kind === 'input') lastInput = now; else lastNotice = now;
    lastBright = now; blocked = false; refresh();
  }
  /** Changing conversations discards old deadlines and releases even when a replacement has active work. */
  function select(id) {
    if ((id || '') === session) { refresh(); return; }
    ++epoch; release(); session = id || ''; active = false; waiting = '';
    lastInput = lastNotice = lastIdle = -Infinity; lastBright = clock(); blocked = false; refresh();
  }
  /** Work holds the lease continuously; approval/recovery transitions grant grace once, never on repeated polls. */
  function runs(items) {
    if (!session || disposed) return;
    const relevant = items.filter(item => !item.session_id || item.session_id === session);
    const next = relevant.some(item => !item.uncertain && ACTIVE.has(item.status));
    const nextWaiting = relevant.filter(item => item.uncertain || WAITING.has(item.status))
      .map(item => [item.id, item.status, item.pending?.id || item.approval_id || ''].join(':')).sort().join('|');
    if (next !== active || (nextWaiting && nextWaiting !== waiting)) {
      if (active && !next || nextWaiting && nextWaiting !== waiting) lastIdle = clock();
      lastBright = clock(); blocked = false;
    }
    active = next; waiting = nextWaiting; refresh();
  }
  /** Adopt validated settings immediately without manufacturing chat activity or altering phone brightness. */
  function configure(value) { policy = screenPolicy(value); blocked = false; refresh(); }
  /** Count real contact, editing and scrolling; hover and programmatic UI updates do not prolong the display. */
  const input = event => { if (event.isTrusted && (event.type !== 'pointermove' || event.buttons || event.pointerType === 'touch')) attention('input'); };
  for (const type of ['pointerdown', 'pointermove', 'touchstart', 'touchmove', 'keydown', 'input', 'wheel']) listen(doc, type, input);
  listen(doc, 'visibilitychange', () => { ++epoch; if (doc.visibilityState === 'visible') { lastBright = clock(); blocked = false; } refresh(); });
  listen(win, 'pagehide', () => { ++epoch; suspended = true; refresh(); });
  listen(win, 'pageshow', () => { suspended = false; lastBright = clock(); blocked = false; refresh(); });
  /** Stop timers/listeners and fence outstanding asynchronous acquisition. */
  function destroy() { disposed = true; ++epoch; refresh(); for (const remove of listeners) remove(); }
  return {select, runs, attention, configure, destroy};
}
// @code-index {"version":1,"feature":"chat-screen-preferences","aliases":["device display settings"],"purpose":"Conversation-menu controls persist validated device display preferences. Config: ui/screen-awake.json -> enabled, after_chat_ms, after_input_ms, dim_after_ms, dim_opacity; localStorage overrides them immediately.","span_lines":65,"configuration":{"status":"configured","references":[{"path":"ui/screen-awake.json","pointers":["/enabled","/after_chat_ms","/after_input_ms","/dim_after_ms","/dim_opacity"]}],"opportunities":[]}}
/** Build accessible device-only controls; parent popup owns dismissal and the shared chat feedback owner handles summary taps. */
function preferenceControls({getPolicy, save, supported}) {
  const details = document.createElement('details'); details.className = 'screen-awake-settings';
  const summary = document.createElement('summary'); summary.textContent = 'Screen display'; details.append(summary);
  const description = document.createElement('p'); description.textContent = supported
    ? 'This device only. Active chat keeps the mobile screen awake. Dimming changes app appearance, not phone brightness.'
    : 'Screen wake lock is unavailable in this browser. Preferences are saved for supported mobile browsers.';
  details.append(description);
  const feedback = document.createElement('p'); feedback.setAttribute('role', 'status'); feedback.className = 'screen-awake-save-status';
  /** Validate one edit against the whole policy before applying and persisting it. */
  function change(key, value, input) {
    const next = screenPolicy({...getPolicy(), [key]:value});
    if (!next || !input.checkValidity()) { input.setAttribute('aria-invalid', 'true'); feedback.textContent = 'Enter a valid display setting.'; return; }
    input.removeAttribute('aria-invalid'); feedback.textContent = save(next) ? 'Saved on this device.' : 'Applied for this page; device storage is unavailable.';
  }
  const enabledLabel = document.createElement('label'); enabledLabel.className = 'screen-awake-field';
  const enabled = document.createElement('button'); enabled.type = 'button'; enabled.setAttribute('role', 'switch');
  enabled.dataset.screenSetting = 'enabled'; enabled.setAttribute('aria-label', 'Keep mobile screen awake during chat');
  // Keep the visible switch label and its accessible state aligned with the current device policy.
  const paintEnabled = () => { enabled.setAttribute('aria-checked', String(getPolicy().enabled)); enabled.textContent = getPolicy().enabled ? 'On' : 'Off'; };
  paintEnabled(); enabled.addEventListener('click', () => { change('enabled', !getPolicy().enabled, enabled); paintEnabled(); });
  enabledLabel.append(document.createTextNode('Keep mobile screen awake during chat'), enabled); details.append(enabledLabel);
  for (const [key, text, scale, max, step] of [
    ['after_chat_ms', 'After chat ends (seconds)', 1000, 2147483, 1],
    ['after_input_ms', 'After last input (seconds)', 1000, 2147483, 1],
    ['dim_after_ms', 'Dim after inactivity (seconds; 0 disables)', 1000, 2147483, 1],
    ['dim_opacity', 'Dim strength (%)', 0.01, 95, 1]
  ]) {
    const label = document.createElement('label'); label.className = 'screen-awake-field'; label.textContent = text;
    const input = document.createElement('input'); input.type = 'number'; input.min = '0'; input.max = String(max); input.step = String(step);
    input.value = String(Math.round(getPolicy()[key] / scale)); input.dataset.screenSetting = key; input.setAttribute('aria-label', text);
    input.addEventListener('change', () => change(key, input.value === '' ? NaN : Math.round(Number(input.value) * scale * 1e6) / 1e6, input));
    label.append(input); details.append(label);
  }
  details.append(feedback); return details;
}
// @code-index {"version":1,"feature":"chat-screen-awake-mount","aliases":["notification display wake"],"purpose":"Panel lifecycle mounts dim overlay, validated settings and distinct notice observation. Config: ui/screen-awake.json -> complete display policy, adopted on mount with device overrides.","span_lines":65,"configuration":{"status":"configured","references":[{"path":"ui/screen-awake.json","pointers":["/enabled","/mobile_only","/after_chat_ms","/after_input_ms","/dim_after_ms","/dim_opacity"]}],"opportunities":[]}}
/** Calls from panel select/runs/live message callbacks; only changed notice text restores brightness. */
export async function mountScreenAwake(root) {
  let defaults = null;
  try { const response = await fetch('/v3-ui/screen-awake.json', {credentials:'same-origin'}); if (response.ok) defaults = screenPolicy(await response.json()); }
  catch { /* Optional display enhancement must never block chat. */ }
  let storage = null; try { storage = window.localStorage; } catch { /* Per-page settings still work. */ }
  let policy = deviceScreenPolicy(defaults, storage);
  const css = document.createElement('link'); css.rel = 'stylesheet'; css.href = '/v3-ui/shared/screen-awake.css'; document.head.append(css);
  const overlay = document.createElement('div'); overlay.dataset.screenDim = ''; overlay.setAttribute('aria-hidden', 'true');
  Object.assign(overlay.style, {position:'fixed', inset:'0', background:'#000', pointerEvents:'none', zIndex:'2147483647', display:'none'});
  document.body.append(overlay);
  const controller = createScreenAwake({config:policy, setDim:(dim, opacity) => {
    overlay.style.opacity = String(opacity); overlay.style.display = dim ? 'block' : 'none';
  }});
  /** Apply preferences even if persistence is blocked, and report the true storage result to the user. */
  function save(next) {
    policy = next; controller.configure(next);
    try { if (!storage) return false; storage.setItem(STORAGE_KEY, JSON.stringify(Object.fromEntries(PREFERENCES.map(key => [key, next[key]])))); return true; }
    catch { return false; }
  }
  /** Re-read device overrides when another same-origin tab saves its display preferences. */
  function storageChanged(event) { if (event.key === STORAGE_KEY || event.key === null) { policy = deviceScreenPolicy(defaults, storage); controller.configure(policy); } }
  window.addEventListener('storage', storageChanged);
  // Notice observation excludes streaming content, run repaint and initial/restored transcript history.
  const regions = [document.getElementById('notice'), root.querySelector('#chat-notifications'), root.querySelector('#chat-above-pill')].filter(Boolean);
  // The above-pill host also owns an elapsed-work clock: only its explicit notice children count.
  const noticeText = region => region.id === 'chat-above-pill'
    ? [...region.querySelectorAll('.v3-repository-notice:not([hidden])')].map(node => node.textContent).join('\n')
    : region.textContent;
  const previous = new Map(regions.map(region => [region, noticeText(region)]));
  const observer = new MutationObserver(() => {
    for (const region of regions) { const text = noticeText(region); if (text !== previous.get(region)) { previous.set(region, text); if (text.trim()) controller.attention('notification'); } }
  });
  for (const region of regions) observer.observe(region, {childList:true, subtree:true, characterData:true});
  return {...controller,
    settings() { return policy ? preferenceControls({getPolicy:() => policy, save, supported:!!navigator.wakeLock?.request}) : null; },
    destroy() { observer.disconnect(); window.removeEventListener('storage', storageChanged); controller.destroy(); overlay.remove(); css.remove(); }
  };
}
