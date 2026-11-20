// Display policy from config/ui/chat/chat.json -> chat_common.screen_display, merged with desktop/mobile/widget overrides; on/dim/off modes and activity/input timeouts adopt with the chat profile.
const ACTIVE = new Set(['queued', 'running', 'waiting_gate', 'waiting_external']);
const WAITING = new Set(['waiting_approval', 'recovery']);
const MODES = new Set(['on', 'dim', 'off']);
// Missing optional profile values inherit these defaults; malformed explicit values disable the enhancement.
export const DEFAULT_SCREEN_POLICY = Object.freeze({enabled:true, mobile_only:true, after_activity_ms:120000, after_input_ms:120000, active:'dim', inactive:'off', dim_after_ms:30000, dim_opacity:0.6});
/** Reject malformed settings rather than allowing an accidental unlimited display lease. */
export function screenPolicy(value) {
  if (!value || typeof value.enabled !== 'boolean' || typeof value.mobile_only !== 'boolean') return null;
  if (!MODES.has(value.active) || !MODES.has(value.inactive)) return null;
  for (const key of ['after_activity_ms', 'after_input_ms', 'dim_after_ms']) {
    if (!Number.isSafeInteger(value[key]) || value[key] < 0 || value[key] > 2147483647) return null;
  }
  if (!Number.isFinite(value.dim_opacity) || value.dim_opacity < 0 || value.dim_opacity > 0.95) return null;
  return {...value};
}
/** Resolve the already-merged chat profile without refetching chat.json or changing its surface precedence. */
export function profileScreenPolicy(profile) {
  if (!profile) return null;
  const value = profile.screen_display;
  if (value !== undefined && (!value || typeof value !== 'object' || Array.isArray(value))) return null;
  return screenPolicy({...DEFAULT_SCREEN_POLICY, ...value});
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
    return policy ? Math.max(lastInput + policy.after_input_ms, lastNotice + policy.after_activity_ms, lastIdle + policy.after_activity_ms) : -Infinity;
  }
  /** Select the configured active/idle display mode; on/dim hold continuously and off respects grace deadlines. */
  function mode() { return policy ? (active ? policy.active : policy.inactive) : 'off'; }
  /** Only selected conversations in eligible foreground documents can hold a lease. */
  function eligible() {
    return !!policy?.enabled && !disposed && !suspended && !!session && doc.visibilityState === 'visible' &&
      (!policy.mobile_only || mobile()) && (mode() !== 'off' || clock() < deadline());
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
    setDim(!!lock && mode() === 'dim' && policy.dim_after_ms > 0 && now >= brightDeadline, policy.dim_opacity);
    const next = Math.min(mode() !== 'off' ? Infinity : deadline(), mode() === 'dim' && policy.dim_after_ms > 0 && brightDeadline > now ? brightDeadline : Infinity);
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
      lastIdle = clock();
      lastBright = clock(); blocked = false;
    }
    active = next; waiting = nextWaiting; refresh();
  }
  /** Adopt validated settings immediately without manufacturing chat activity or altering phone brightness. */
  function configure(value) {
    const next = screenPolicy(value);
    if (JSON.stringify(next) === JSON.stringify(policy)) return;
    policy = next; blocked = false; refresh();
  }
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
// Display policy from config/ui/chat/chat.json -> chat_common.screen_display, merged with desktop/mobile/widget overrides; on/dim/off modes and activity/input timeouts adopt with the chat profile.
// Display policy from config/ui/chat/chat.json -> chat_common.screen_display, merged with desktop/mobile/widget overrides; on/dim/off modes and activity/input timeouts adopt with the chat profile.
/** Calls from panel select/runs/live message callbacks; only changed notice text restores brightness. */
export function mountScreenAwake(root, {getProfile = () => null} = {}) {
  // chat.json is the only source: there is no per-device override or settings panel.
  let policy = profileScreenPolicy(getProfile());
  const overlay = document.createElement('div'); overlay.dataset.screenDim = ''; overlay.setAttribute('aria-hidden', 'true');
  Object.assign(overlay.style, {position:'fixed', inset:'0', background:'#000', pointerEvents:'none', zIndex:'2147483647', display:'none'});
  document.body.append(overlay);
  // The OS status bar follows theme-color and sits outside the overlay: black it out while dimmed, restore after.
  // shortcut: a system theme flip during a dim is undone on restore, fixed by the next applyAccountTheme.
  const bar = document.querySelector('meta[name="theme-color"]'); let barColor = null;
  const controller = createScreenAwake({config:policy, setDim:(dim, opacity) => {
    overlay.style.opacity = String(opacity); overlay.style.display = dim ? 'block' : 'none';
    if (!bar || dim === (barColor !== null)) return;
    if (dim) { barColor = bar.content; bar.content = '#000000'; } else { bar.content = barColor; barColor = null; }
  }});
  /** Adopt merged profile updates, including resize/widget transitions, without renewing activity clocks. */
  function profileChanged() {
    policy = profileScreenPolicy(getProfile()); controller.configure(policy);
  }
  document.addEventListener('v3:chat-config-applied', profileChanged);
  // Notice observation excludes streaming content, run repaint and initial/restored transcript history.
  const regions = [document.getElementById('notice'), root.querySelector('#chat-action-panel'), root.querySelector('#chat-above-pill')].filter(Boolean);
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
    destroy() { observer.disconnect(); document.removeEventListener('v3:chat-config-applied', profileChanged); controller.destroy(); overlay.remove(); }
  };
}
