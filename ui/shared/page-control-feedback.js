import { mountControlFeedback } from '../chat/widget/window.js';
import { requestTouchVibration } from './vibrate.js';

export const PAGE_ACTIONS = 'button, [role="button"], [role="menuitem"], [role="switch"], [role="checkbox"], [role="radio"], summary, a[href], input[type="checkbox"], input[type="radio"], label:has(input[type="checkbox"]), label:has(input[type="radio"])';
const markers = new WeakMap();
/** Reference-counts shared portal styling while independent page owners coexist. */
function mark(surface, delta) {
  const count = (markers.get(surface) || 0) + delta;
  if (count > 0) { markers.set(surface, count);surface.setAttribute('data-page-control-feedback', ''); }
  else { markers.delete(surface);surface.removeAttribute('data-page-control-feedback'); }
}

// @code-index {"version":1,"feature":"page-control-feedback","aliases":["page button haptics","main panel touch feedback"],"purpose":"Registers dynamic page controls and known body portals with the shared gesture registry. Config: ui/vibrate.json -> enabled, duration_ms. Default editing, disabled and drag exclusions remain; ownership is gated by the visible page and DOM menu owner.","span_lines":65,"configuration":{"status":"configured","references":[{"path":"ui/vibrate.json","pointers":["/enabled","/duration_ms"]}],"opportunities":[]}}
/** Owns feedback for one page, its dynamic descendants and declared detached surfaces.
 * Short taps on draggable tabs remain eligible; armed reorder holds and movement stay quiet.
 * Caller supplies only verified custom action selectors and portal roots; no business handlers change.
 */
export function mountPageControlFeedback(root, {actions = PAGE_ACTIONS, portals = ''} = {}) {
  const doc = root.ownerDocument, surfaces = new Map(), held = new WeakSet();
  const active = () => root.isConnected && !root.hidden && root.getClientRects().length > 0;
  const resetHold = event => { const tab = event.target.closest?.('[role="tab"]');if (tab) held.delete(tab); };
  const rememberHold = event => { const tab = event.target.closest?.('[role="tab"].dragging');if (tab) held.add(tab); };
  const vibrate = event => {
    const path = event.composedPath(), target = path.find(node => node?.matches?.(actions));
    if (!target || path.some(node => node?.matches?.('.ac-saved-drag-handle, .ac-tri-disabled, .ac-tri[tabindex="-1"]'))) return;
    if (target.matches('[role="tab"]') && held.has(target)) { held.delete(target);return; }
    requestTouchVibration(event);
  };
  const add = surface => {
    if (surfaces.has(surface)) return;
    mark(surface, 1);
    surfaces.set(surface, mountControlFeedback(surface, {controlSelector: actions, vibrate,
      isActive: () => active() && !(surface._owner?.nodeType === 1 && !root.contains(surface._owner))}));
  };
  const refresh = () => {
    for (const [surface, release] of surfaces) if (surface !== root && !surface.isConnected) { release();mark(surface, -1);surfaces.delete(surface); }
    if (active() && portals) for (const surface of doc.querySelectorAll(portals)) add(surface);
  };
  add(root);
  root.addEventListener('pointerdown', resetHold, true);root.addEventListener('pointerup', rememberHold, true);
  const observer = new MutationObserver(refresh);
  observer.observe(doc.body, {childList: true});observer.observe(root, {attributes: true, attributeFilter: ['hidden']});refresh();
  return () => {
    observer.disconnect();root.removeEventListener('pointerdown', resetHold, true);root.removeEventListener('pointerup', rememberHold, true);
    for (const [surface, release] of surfaces) { release();mark(surface, -1); }surfaces.clear();
  };
}

/** Discovers catalog-inserted roots and restores owned registrations after BFCache navigation. */
export function discoverPageControlFeedback(selector, options = {}) {
  const doc = document, mounts = new Map();
  const refresh = () => {
    for (const [root, release] of mounts) if (!root.isConnected) { release();mounts.delete(root); }
    for (const root of doc.querySelectorAll(selector)) if (!mounts.has(root)) mounts.set(root, mountPageControlFeedback(root, options));
  };
  const clear = () => { for (const release of mounts.values()) release();mounts.clear(); };
  const observer = new MutationObserver(refresh);
  const start = () => { observer.observe(doc.body, {childList: true});const panel = doc.getElementById('main-panel');if (panel) observer.observe(panel, {childList: true});refresh(); };
  const hide = () => { observer.disconnect();clear(); };
  const show = event => { if (event.persisted) start(); };
  window.addEventListener('pagehide', hide);window.addEventListener('pageshow', show);start();
  return () => { hide();window.removeEventListener('pagehide', hide);window.removeEventListener('pageshow', show); };
}

const stylesheet = document.createElement('link');
stylesheet.rel = 'stylesheet';stylesheet.href = new URL('./page-control-feedback.css', import.meta.url).href;
document.head.append(stylesheet);
