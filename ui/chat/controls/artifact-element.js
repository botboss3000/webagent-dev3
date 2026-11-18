// A UI element in an Artifacts card: the agent's saved page running in a sandboxed frame
// (its own route serves it as an opaque origin with no network), and the bridge it talks
// to. Every request is checked here before the app acts on it:
// - preview on/off: this chat's UI previews, or a sub-agent's named in the card's params
//   (the server checks it), after a click in the element; the page then reloads in the
//   same tab;
// - prompt: sent as the user's own message after a countdown the user can cancel, after
//   a click;
// - open: allow-listed app routes (or this chat) in the same tab, after a click;
// - resize: the card's height, bounded.
import {requestTouchVibration} from '../../shared/vibrate/vibrate.js';

const el = (tag, cls, text) => { const n = document.createElement(tag); if (cls) n.className = cls; if (text != null) n.textContent = text; return n; };
/** How long after a click in the element its requests count as the user's. */
const CLICK_WINDOW_MS = 5000;
const MIN_HEIGHT = 80, MAX_HEIGHT = 560;

async function csrf() {
  const me = await (await fetch('/api/v3/auth/me', {credentials: 'same-origin', cache: 'no-store'})).json();
  return me.csrf_token || '';
}

/** Whether `route` is an app page an element may open from chat `sid`. */
export function routeAllowed(route, routes, sid) {
  if (typeof route !== 'string' || !route.startsWith('/') || route.startsWith('//') || /[\s\\]/.test(route)) return false;
  if (route === '/app?session_id=' + encodeURIComponent(sid)) return true;
  return (Array.isArray(routes) ? routes : []).some(prefix => route === prefix || route.startsWith(prefix + '&'));
}

/**
 * Builds the element part of card `row` in chat `sid`: the frame, and the countdown strip
 * for prompts. `send(text)` sends the user's message.
 */
export function elementFrame(row, sid, send) {
  const box = el('div', 'artifact-element');
  const status = el('p', 'artifact-element-status', 'Loading…'); status.setAttribute('role', 'status');
  const strip = el('div', 'artifact-element-countdown'); strip.hidden = true;
  box.append(status, strip);
  const scope = row.element?.scope === 'shared' ? 'shared' : 'mine';
  const name = String(row.element?.name || '');
  if (!/^[a-z0-9_-]{1,40}$/.test(name) || !sid) { status.textContent = 'This element is unavailable.'; return box; }
  const base = '/api/v3/sessions/' + encodeURIComponent(sid) + '/ui-elements/' + scope + '/' + encodeURIComponent(name);
  const frame = el('iframe', 'artifact-element-frame');
  frame.setAttribute('sandbox', 'allow-scripts');
  frame.setAttribute('referrerpolicy', 'no-referrer');
  frame.title = row.title || 'UI element';
  frame.style.height = '160px';
  let bridge = {prompt_countdown_seconds: 3, routes: []};
  // The session whose workspace holds the card's variant: this chat, or a sub-agent of it
  // named in the card's params (the server checks it belongs to this conversation).
  const named = row.params?.preview_session;
  const previewSession = typeof named === 'string' && /^[0-9a-f-]{36}$/.test(named) ? named : sid;
  let clickedAt = 0;
  // A click into the frame moves focus to it; that is the only click the page can see.
  const noteClick = () => { if (document.activeElement === frame) clickedAt = Date.now(); };
  window.addEventListener('blur', noteClick);
  // A click inside the frame also activates this page (user activation reaches ancestor
  // frames); focus moving into the frame is the fallback where that is unavailable.
  const clicked = () => navigator.userActivation ? navigator.userActivation.isActive : Date.now() - clickedAt < CLICK_WINDOW_MS;
  const reply = (id, ok, value, error) => frame.contentWindow?.postMessage({webagent: 1, id, ok, value, error}, '*');
  let pending = null;
  function countdown(text) {
    pending?.cancel();
    let left = Math.max(1, Number(bridge.prompt_countdown_seconds) || 3);
    const label = el('span', 'artifact-element-countdown-text');
    const cancel = el('button', 'artifact-element-cancel', 'Cancel'); cancel.type = 'button';
    strip.replaceChildren(label, cancel); strip.hidden = false;
    const paint = () => { label.textContent = 'Sending “' + (text.length > 60 ? text.slice(0, 60) + '…' : text) + '” in ' + left + ' s'; };
    paint();
    return new Promise(resolve => {
      const timer = setInterval(() => { left -= 1; if (left > 0) return paint(); done(true); }, 1000);
      const done = sent => { clearInterval(timer); strip.hidden = true; strip.replaceChildren(); pending = null; resolve(sent); };
      pending = {cancel: () => done(false)};
      cancel.onclick = e => { requestTouchVibration(e); done(false); };
    });
  }
  async function handle(data) {
    const {id, type} = data;
    try {
      if (type === 'resize') {
        const height = Math.round(Number(data.height));
        // Two pixels for the frame's own border, so content of that height needs no scrollbar.
        if (Number.isFinite(height)) frame.style.height = Math.min(MAX_HEIGHT, Math.max(MIN_HEIGHT, height + 2)) + 'px';
        return reply(id, true, null);
      }
      if (type === 'preview.status') {
        const current = await (await fetch('/api/v3/ui-preview', {credentials: 'same-origin', cache: 'no-store'})).json();
        return reply(id, true, {variant: current.session_id === previewSession ? current.variant : null});
      }
      if (!clicked()) return reply(id, false, null, 'Only after a click in the element');
      clickedAt = 0;
      if (type === 'preview.set') {
        const variant = data.variant == null ? null : String(data.variant);
        if (variant !== null && !/^[a-z0-9_-]{1,32}$/.test(variant)) return reply(id, false, null, 'Invalid variant');
        status.textContent = variant ? 'Turning the preview on…' : 'Turning the preview off…'; status.hidden = false;
        const response = await fetch('/api/v3/sessions/' + encodeURIComponent(sid) + '/ui-preview', {
          method: 'POST', credentials: 'same-origin',
          headers: {'Content-Type': 'application/json', 'x-csrf-token': await csrf()},
          body: JSON.stringify({variant, source_session: previewSession})});
        if (!response.ok) { status.textContent = 'The preview could not be changed.'; return reply(id, false, null, 'Refused'); }
        reply(id, true, {variant});
        // The preview changes which UI files load, so the page reloads in this tab.
        location.reload();
        return;
      }
      if (type === 'prompt') {
        const text = String(data.text || '').trim();
        if (!text || text.length > 2000) return reply(id, false, null, 'Prompt must be 1-2000 characters');
        if (!(await countdown(text))) return reply(id, false, null, 'Cancelled');
        await send(text);
        return reply(id, true, null);
      }
      if (type === 'open') {
        if (!routeAllowed(data.route, bridge.routes, sid)) return reply(id, false, null, 'Route not allowed');
        reply(id, true, null);
        location.assign(data.route);
        return;
      }
      reply(id, false, null, 'Unknown request');
    } catch (error) { reply(id, false, null, String(error?.message || error)); }
  }
  const onMessage = event => {
    if (event.source !== frame.contentWindow || event.data?.webagent !== 1 || !Number.isInteger(event.data.id)) return;
    void handle(event.data);
  };
  window.addEventListener('message', onMessage);
  // The card is removed from the page when it closes; stop listening then.
  new MutationObserver((_, observer) => { if (!box.isConnected) { window.removeEventListener('message', onMessage); window.removeEventListener('blur', noteClick); pending?.cancel(); observer.disconnect(); } })
    .observe(document.body, {childList: true});
  fetch(base, {credentials: 'same-origin', cache: 'no-store'})
    .then(response => response.ok ? response.json() : Promise.reject(new Error('This element is no longer saved.')))
    .then(loaded => {
      bridge = loaded.bridge || bridge;
      frame.src = base + '/frame?card=' + encodeURIComponent(row.id);
      status.hidden = true;
      box.prepend(frame);
    })
    .catch(error => { status.textContent = error.message || 'This element is unavailable.'; });
  return box;
}
