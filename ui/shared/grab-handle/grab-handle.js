'use strict';

// A grab handle: the short line on a panel edge that shows or hides what sits
// behind that edge. A tap toggles it; a drag toward the hidden part opens it
// and a drag back closes it. The line follows the pointer with a rubber band
// and springs back on release, like the chat composer's footer handle.

import { requestTouchVibration } from '../vibrate/vibrate.js';

const STYLE_ID = 'grab-handle-styles';
const CSS = `
.grab-handle{display:flex;align-items:center;justify-content:center;width:48px;height:20px;margin:0;padding:0;border:0;background:none;cursor:ns-resize;touch-action:none;-webkit-tap-highlight-color:transparent;transform:translateY(var(--grab-drag-y, 0px))}
.grab-handle.spring-back{transition:transform .35s cubic-bezier(.34,1.56,.64,1)}
.grab-handle-line{width:32px;height:3px;border-radius:2px;background:var(--fg-4);opacity:.35;transition:opacity .15s ease,width .2s ease,background .15s ease}
.grab-handle:hover .grab-handle-line,.grab-handle:focus-visible .grab-handle-line{opacity:.75;width:40px;background:var(--fg-2)}
.grab-handle:focus-visible{outline:none}
.grab-handle[aria-expanded="false"] .grab-handle-line{animation:grab-handle-pulse 3s ease-in-out infinite}
.grab-handle:disabled{cursor:default;pointer-events:none}
@keyframes grab-handle-pulse{0%,100%{opacity:.25}50%{opacity:.55}}
@media (prefers-reduced-motion: reduce){.grab-handle[aria-expanded="false"] .grab-handle-line{animation:none}}
`;

const THRESHOLD = 32; // px of drag that opens or closes
const LIMIT = 12;     // px of free travel before the rubber band stiffens
const RESIST = 16;    // higher = stiffer band past the limit

/* Grows ever slower past the limit instead of stopping dead. */
function band(value) {
  const abs = Math.abs(value);
  if (abs <= LIMIT) return value;
  return Math.sign(value) * (LIMIT + (abs - LIMIT) * RESIST / ((abs - LIMIT) + RESIST));
}

/** Make a grab handle; returns its button plus state helpers.
 *  opens is the drag direction that reveals the hidden part: 'down' for a handle
 *  on a top edge, 'up' for one on a bottom edge. onToggle(expanded, event) runs on
 *  every change the person makes; setExpanded changes the state without it. */
export function createGrabHandle({ label = 'Show more', expanded = false, opens = 'down', onToggle = null } = {}) {
  if (!document.getElementById(STYLE_ID)) {
    const style = document.createElement('style');
    style.id = STYLE_ID;
    style.textContent = CSS;
    document.head.append(style);
  }
  const handle = document.createElement('button');
  handle.type = 'button';
  handle.className = 'grab-handle';
  handle.title = label;
  handle.setAttribute('aria-label', label);
  handle.innerHTML = '<span class="grab-handle-line" aria-hidden="true"></span>';
  let open = !!expanded;
  const paint = () => handle.setAttribute('aria-expanded', String(open));
  const change = (next, event) => {
    if (next === open) return;
    open = next;
    paint();
    requestTouchVibration(event);
    onToggle?.(open, event);
  };
  paint();

  const sign = opens === 'up' ? -1 : 1;
  let startY = 0;
  let moved = 0;
  handle.addEventListener('pointerdown', event => {
    startY = event.clientY;
    moved = 0;
    handle.classList.remove('spring-back');
    handle.setPointerCapture(event.pointerId);
    event.preventDefault();
  });
  handle.addEventListener('pointermove', event => {
    if (!handle.hasPointerCapture(event.pointerId)) return;
    const dy = event.clientY - startY;
    moved = Math.max(moved, Math.abs(dy));
    handle.style.setProperty('--grab-drag-y', band(dy) + 'px');
    if (!open && dy * sign > THRESHOLD) { change(true, event); startY = event.clientY; }
    else if (open && dy * sign < -THRESHOLD) { change(false, event); startY = event.clientY; }
  });
  const release = event => {
    if (!handle.hasPointerCapture(event.pointerId)) return;
    handle.releasePointerCapture(event.pointerId);
    handle.classList.add('spring-back');
    handle.style.removeProperty('--grab-drag-y');
  };
  handle.addEventListener('pointerup', release);
  handle.addEventListener('pointercancel', release);
  // A drag already did its work; only a press that stayed put is a tap.
  handle.addEventListener('click', event => { if (moved <= 4) change(!open, event); moved = 0; });

  return {
    element: handle,
    isExpanded: () => open,
    setExpanded: next => { open = !!next; paint(); },
  };
}
