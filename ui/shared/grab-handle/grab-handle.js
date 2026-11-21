'use strict';

// A grab handle: the short line on a panel edge that shows or hides what sits
// behind that edge. A tap toggles it with a bounce; a drag into the panel opens
// it and a drag back out closes it. While dragging, the line follows the
// pointer and the panel edge stretches with it on a rubber band; both spring
// back on release. Closed, the line sits just outside the edge; open, it moves
// just inside. The chat composer's footer handle and the rich text editor's
// toolbar handle both use this.

import { requestTouchVibration } from '../vibrate/vibrate.js';

const STYLE_ID = 'grab-handle-styles';
const CSS = `
.grab-handle{position:absolute;left:50%;z-index:10;display:flex;align-items:center;justify-content:center;width:48px;height:20px;margin:0;padding:0;border:0;background:none;cursor:ns-resize;touch-action:none;-webkit-tap-highlight-color:transparent;transform:translateX(-50%) translateY(var(--grab-drag-y, 0px));transition:top .25s ease,bottom .25s ease}
.grab-handle[data-edge="top"]{top:-14px}
.grab-handle[data-edge="top"][aria-expanded="true"]{top:-6px}
.grab-handle[data-edge="bottom"]{bottom:-14px}
.grab-handle[data-edge="bottom"][aria-expanded="true"]{bottom:-6px}
.grab-panel{transform:translateY(var(--grab-panel-y, 0px))}
.grab-handle-line{width:32px;height:3px;border-radius:2px;background:var(--fg-4);opacity:.35;transition:opacity .15s ease,width .2s ease,background .15s ease}
.grab-handle:hover .grab-handle-line,.grab-handle:focus-visible .grab-handle-line{opacity:.75;width:40px;background:var(--fg-2)}
.grab-handle:focus-visible{outline:none}
.grab-handle[aria-expanded="false"] .grab-handle-line{animation:grab-handle-pulse 3s ease-in-out infinite}
@keyframes grab-handle-pulse{0%,100%{opacity:.25}50%{opacity:.55}}
@media (prefers-reduced-motion: reduce){.grab-handle[aria-expanded="false"] .grab-handle-line{animation:none}}
`;

// Added on load so a handle written into page markup (the chat composer's) is styled before it shows.
if (!document.getElementById(STYLE_ID)) {
  const style = document.createElement('style');
  style.id = STYLE_ID;
  style.textContent = CSS;
  document.head.append(style);
}

const THRESHOLD = 48;    // px of drag that opens or closes
const LIMIT = 80;        // px of linear travel before the rubber band kicks in
const RESIST = 40;       // higher = stiffer band past the limit
const PANEL_SHARE = 0.2; // part of the banded offset applied to the panel edge
const IMPULSE = 24;      // px a tap kicks both toward their new place before the bounce
const SPRING = 'cubic-bezier(.34,1.56,.64,1)'; // overshoots, then settles
const RELEASE_SECONDS = 0.35;
const TAP_SECONDS = 0.75;

/* Asymptotic rubber band: past the limit the offset grows ever slower instead of stopping dead. */
function band(value) {
  const abs = Math.abs(value);
  if (abs <= LIMIT) return value;
  return Math.sign(value) * (LIMIT + (abs - LIMIT) * RESIST / ((abs - LIMIT) + RESIST));
}

/** Turn an element on a panel edge into a grab handle.
 *  edge is the panel edge it sits on ('top' or 'bottom'); the panel must be positioned
 *  and must not clip the 14px the closed handle reaches past that edge. Dragging into the
 *  panel opens it. isExpanded reads the state; setExpanded(next, event) applies a change
 *  the person made and keeps the handle's aria-expanded current, which places the line. */
export function attachGrabHandle(handle, { panel = handle.parentElement, edge = 'top', isExpanded, setExpanded }) {
  handle.classList.add('grab-handle');
  handle.dataset.edge = edge;
  panel.classList.add('grab-panel');
  const sign = edge === 'top' ? 1 : -1; // screen direction of the opening drag
  const parts = [handle, panel];
  let startY = 0;
  let wasOpen = false;
  let downY = 0;
  let travel = 0; // furthest the pointer got from where it went down
  let springTimer = 0;
  const shift = (handleY, panelY) => {
    handle.style.setProperty('--grab-drag-y', handleY + 'px');
    panel.style.setProperty('--grab-panel-y', panelY + 'px');
  };
  // An inline transition outranks the panel's own rules (the composer's is an id rule)
  // for the spring's length only, then hands transitions back to the stylesheet.
  const unspring = () => {
    clearTimeout(springTimer);
    for (const el of parts) el.style.removeProperty('transition');
  };
  const spring = seconds => {
    clearTimeout(springTimer);
    for (const el of parts) el.style.transition = `transform ${seconds}s ${SPRING}`;
    handle.style.removeProperty('--grab-drag-y');
    panel.style.removeProperty('--grab-panel-y');
    springTimer = setTimeout(unspring, seconds * 1000);
  };

  handle.addEventListener('pointerdown', event => {
    startY = downY = event.clientY;
    wasOpen = isExpanded();
    travel = 0;
    unspring();
    handle.setPointerCapture(event.pointerId);
    event.preventDefault();
  });
  handle.addEventListener('pointermove', event => {
    if (!handle.hasPointerCapture(event.pointerId)) return;
    const dy = event.clientY - startY;
    const offset = band(dy);
    travel = Math.max(travel, Math.abs(event.clientY - downY));
    // The handle sits inside the panel, so the two shares add up to the full offset.
    shift(offset * (1 - PANEL_SHARE), offset * PANEL_SHARE);
    // Switch as soon as the drag crosses the threshold, then measure afresh from here.
    if (!wasOpen && dy * sign > THRESHOLD) { setExpanded(true, event); wasOpen = true; startY = event.clientY; }
    else if (wasOpen && dy * sign < -THRESHOLD) { setExpanded(false, event); wasOpen = false; startY = event.clientY; }
  });
  const release = event => {
    if (!handle.hasPointerCapture(event.pointerId)) return;
    handle.releasePointerCapture(event.pointerId);
    spring(RELEASE_SECONDS);
  };
  handle.addEventListener('pointerup', release);
  handle.addEventListener('pointercancel', release);

  // A tap (no real drag) toggles, kicking both toward their new place so the spring overshoots and settles.
  handle.addEventListener('click', event => {
    event.preventDefault();
    // The click that ends a drag is not a tap: the drag already did its work.
    const dragged = travel > 4;
    travel = 0;
    if (dragged) { event.stopPropagation(); return; }
    const opening = !isExpanded();
    setExpanded(opening, event);
    unspring();
    const kick = (opening ? sign : -sign) * IMPULSE;
    shift(kick, kick);
    handle.getBoundingClientRect(); // commit the kicked position so the spring starts from it
    spring(TAP_SECONDS);
  });
}

/** Make a grab handle for a panel edge (see attachGrabHandle); returns its button plus
 *  state helpers. onToggle(expanded, event) runs on every change the person makes;
 *  setExpanded changes the state without it. */
export function createGrabHandle({ panel, edge = 'top', label = 'Show more', expanded = false, onToggle = null }) {
  const handle = document.createElement('button');
  handle.type = 'button';
  handle.title = label;
  handle.setAttribute('aria-label', label);
  handle.innerHTML = '<span class="grab-handle-line" aria-hidden="true"></span>';
  let open = !!expanded;
  const paint = () => handle.setAttribute('aria-expanded', String(open));
  paint();
  attachGrabHandle(handle, {
    panel,
    edge,
    isExpanded: () => open,
    setExpanded: (next, event) => {
      if (next === open) return;
      open = next;
      paint();
      requestTouchVibration(event);
      onToggle?.(open, event);
    },
  });
  return {
    element: handle,
    isExpanded: () => open,
    setExpanded: next => { open = !!next; paint(); },
  };
}
