import { mountControlFeedback } from '../chat/widget/window.js';
import { requestTouchVibration } from './vibrate.js';

// @code-index {"version":1,"feature":"main-header-feedback","aliases":["app tabs haptics","header button feedback"],"purpose":"Owns touch feedback for app navigation and its body-portaled account menu. Config: ui/vibrate.json -> enabled, duration_ms. Reuses the chat gesture registry to avoid duplicate Chat/Float pulses.","span_lines":80,"configuration":{"status":"configured","references":[{"path":"ui/vibrate.json","pointers":["/enabled","/duration_ms"]}],"opportunities":[]}}
/** Registers dynamic header controls and the account portal without intercepting their actions.
 * The shared registry rejects scrolling/dragging; armed tab holds are also quiet,
 * because page-tabs.js suppresses navigation after its reorder hold completes.
 * Returns an idempotent disposer; BFCache lifecycle is managed below.
 */
export function mountHeaderFeedback(doc = document) {
  const header = doc.getElementById('main-header');
  if (!header) return () => {};
  const heldTabs = new WeakSet();
  const resetHold = event => {
    const tab = event.target.closest?.('.main-tab');
    if (tab) heldTabs.delete(tab);
  };
  const rememberHold = event => {
    const tab = event.target.closest?.('.main-tab.dragging');
    if (tab) heldTabs.add(tab);
  };
  // Capture before the tab strip removes its dragging class on pointerup.
  header.addEventListener('pointerdown', resetHold, true);
  header.addEventListener('pointerup', rememberHold, true);
  const vibrate = event => {
    const tab = event.target.closest?.('.main-tab');
    if (tab && heldTabs.has(tab)) { heldTabs.delete(tab); return; }
    requestTouchVibration(event);
  };
  const dispose = [header, doc.getElementById('user-dropdown-menu')]
    .filter(Boolean).map(root => mountControlFeedback(root, {vibrate}));
  return () => {
    for (const release of dispose) release();
    header.removeEventListener('pointerdown', resetHold, true);
    header.removeEventListener('pointerup', rememberHold, true);
  };
}

let dispose = mountHeaderFeedback();
window.addEventListener('pagehide', () => { dispose(); dispose = () => {}; });
window.addEventListener('pageshow', event => { if (event.persisted) dispose = mountHeaderFeedback(); });

// The shell interaction entry also discovers the independently mounted Agents surface.
import './agents-feedback.js';
