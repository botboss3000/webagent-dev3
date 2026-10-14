import { mountControlFeedback } from '../chat/widget/window.js';
import { requestTouchVibration } from './vibrate.js';

// @code-index {"version":1,"feature":"agents-control-feedback","aliases":["Agents clickable controls","Agents touch haptics"],"purpose":"Owns Agents cards, tabs, Config, Identity, Primary Models, Abilities and Sessions actions plus their body portals. Config: ui/vibrate.json -> enabled, duration_ms. Selectors follow actual custom click handlers; editing and pointer-only charts stay quiet.","span_lines":80,"configuration":{"status":"configured","references":[{"path":"ui/vibrate.json","pointers":["/enabled","/duration_ms"]}],"opportunities":[]}}
export const AGENT_ACTIONS = [
  'button', '[role="button"]', '[role="menuitem"]', '[role="switch"]', '[role="checkbox"]', '[role="radio"]', 'summary', 'a[href]',
  'input[type="checkbox"]', 'input[type="radio"]', 'label:has(input[type="checkbox"]), label:has(input[type="radio"])',
  '.ac-group-head', '.conn-card-header-clickable', '.prompt-item', '.ac-tri',
  '.ac-model-item', '.ac-model-search-chevron', '.ac-saved-name-click', '.ac-model-meta-editable',
  '.ac-tip-gallery img', '.ac-tip-lightbox',
  '#model-page .v3-pending-feature > .ac-ability-row',
  '#sessions-table tbody tr[data-session-id]', 'th.col-sortable[data-sort-key]', '.session-row-id[tabindex="0"]'
].join(', ');
const PORTALS = '#files-floating-menu, .v3-tool-skill-dialog, .ac-model-dropdown, .ac-directive-overlay, .ac-tip-pop, .ac-tip-lightbox, .session-row-actions[data-source="sessions-page"]';

/** Registers the whole dynamic page and its known detached surfaces, sharing chat/header gesture ownership.
 * Native toggles include their labels; text/select edits, drag handles and charts are not actions.
 * Portals are enabled only while Agents is visible; shared ability menus also verify their owner.
 * Returns teardown for observers/listeners and releases every registration.
 */
export function mountAgentsFeedback(root) {
  const doc = root.ownerDocument, heldTabs = new WeakSet(), surfaces = new Map();
  const active = () => root.isConnected && !root.hidden;
  const resetHold = event => { const tab = event.target.closest?.('[role="tab"]'); if (tab) heldTabs.delete(tab); };
  const rememberHold = event => { const tab = event.target.closest?.('[role="tab"].dragging'); if (tab) heldTabs.add(tab); };
  const vibrate = event => {
    const path = event.composedPath(), target = path.find(node => node?.matches?.(AGENT_ACTIONS));
    if (!target || path.some(node => node?.matches?.('.ac-saved-drag-handle, .ac-tri-disabled, .ac-tri[tabindex="-1"]'))) return;
    if (target.matches('[role="tab"]') && heldTabs.has(target)) { heldTabs.delete(target); return; }
    if (target.matches('th.col-sortable.col-resizable') && event.clientX >= target.getBoundingClientRect().right - 8) return;
    requestTouchVibration(event);
  };
  const add = surface => {
    if (surfaces.has(surface)) return;
    surface.setAttribute('data-agents-feedback', '');
    surfaces.set(surface, mountControlFeedback(surface, {vibrate, controlSelector: AGENT_ACTIONS,
      isActive: () => active() && (!surface._owner || root.contains(surface._owner))}));
  };
  const refreshPortals = () => {
    for (const [surface, release] of surfaces) if (surface !== root && !surface.isConnected) { release(); surfaces.delete(surface); }
    if (active()) for (const surface of doc.querySelectorAll(PORTALS)) add(surface);
  };
  root.addEventListener('pointerdown', resetHold, true);
  root.addEventListener('pointerup', rememberHold, true);
  add(root);
  const observer = new MutationObserver(refreshPortals);
  observer.observe(doc.body, {childList: true});
  observer.observe(root, {attributes: true, attributeFilter: ['hidden']});
  refreshPortals();
  return () => {
    observer.disconnect();
    root.removeEventListener('pointerdown', resetHold, true);root.removeEventListener('pointerup', rememberHold, true);
    for (const [surface, release] of surfaces) { release(); surface.removeAttribute('data-agents-feedback'); }
    surfaces.clear();
  };
}

/** Binds document lifetime to the existing Agents mount, including BFCache restoration. */
export function bindAgentsFeedback(root) {
  let dispose = mountAgentsFeedback(root);
  const hide = () => { dispose(); dispose = () => {}; };
  const show = event => { if (event.persisted) dispose = mountAgentsFeedback(root); };
  window.addEventListener('pagehide', hide);window.addEventListener('pageshow', show);
  return () => { hide();window.removeEventListener('pagehide', hide);window.removeEventListener('pageshow', show); };
}

// The app shell creates #tab-agents from its catalog after this module loads.
const stylesheet = document.createElement('link');
stylesheet.rel = 'stylesheet';stylesheet.href = new URL('./agents-feedback.css', import.meta.url).href;
document.head.append(stylesheet);
const mounts = new Map(), panel = document.getElementById('main-panel');
const discover = () => {
  for (const [root, release] of mounts) if (!root.isConnected) { release();mounts.delete(root); }
  const root = document.getElementById('tab-agents');
  if (root && !mounts.has(root)) mounts.set(root, bindAgentsFeedback(root));
};
if (panel) { const observer = new MutationObserver(discover);observer.observe(panel,{childList:true});discover(); }
