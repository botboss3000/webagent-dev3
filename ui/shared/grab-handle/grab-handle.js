'use strict';

// A grab handle: the short line on a panel edge that shows or hides what sits
// behind that edge. A tap toggles it with a bounce; a drag toward the hidden
// part opens it and a drag back closes it. While dragging, the line follows
// the pointer and the panel edge stretches with it on a rubber band; both
// spring back on release. Closed, the line sits just outside the edge; open,
// it moves just inside. The chat composer's footer handle and the rich text
// editor's toolbar handle both use this.

import { requestTouchVibration } from '../vibrate/vibrate.js';

const STYLE_ID = 'grab-handle-styles';
// Styles for handles made by createGrabHandle; an attached handle (the chat
// composer's) keeps its own stylesheet and uses the same variables and classes.
const CSS = `
.grab-handle{position:absolute;left:50%;z-index:10;display:flex;align-items:center;justify-content:center;width:48px;height:20px;margin:0;padding:0;border:0;background:none;cursor:ns-resize;touch-action:none;-webkit-tap-highlight-color:transparent;transform:translateX(-50%) translateY(var(--grab-drag-y, 0px));transition:top .25s ease,bottom .25s ease}
.grab-handle[data-edge="top"]{top:-14px}
.grab-handle[data-edge="top"][aria-expanded="true"]{top:-6px}
.grab-handle[data-edge="bottom"]{bottom:-14px}
.grab-handle[data-edge="bottom"][aria-expanded="true"]{bottom:-6px}
.grab-panel{transform:translateY(var(--grab-panel-y, 0px))}
.grab-handle.grab-spring,.grab-panel.grab-spring{transition:transform .35s cubic-bezier(.34,1.56,.64,1)}
.grab-handle.grab-spring-click,.grab-panel.grab-spring-click{transition:transform .75s cubic-bezier(.34,1.56,.64,1)}
.grab-handle-line{width:32px;height:3px;border-radius:2px;background:var(--fg-4);opacity:.35;transition:opacity .15s ease,width .2s ease,background .15s ease}
.grab-handle:hover .grab-handle-line,.grab-handle:focus-visible .grab-handle-line{opacity:.75;width:40px;background:var(--fg-2)}
.grab-handle:focus-visible{outline:none}
.grab-handle[aria-expanded="false"] .grab-handle-line{animation:grab-handle-pulse 3s ease-in-out infinite}
@keyframes grab-handle-pulse{0%,100%{opacity:.25}50%{opacity:.55}}
@media (prefers-reduced-motion: reduce){.grab-handle[aria-expanded="false"] .grab-handle-line{animation:none}}
`;

const THRESHOLD = 48;    // px of drag that opens or closes
const LIMIT = 80;        // px of linear travel before the rubber band kicks in
const RESIST = 40;       // higher = stiffer band past the limit
const PANEL_SHARE = 0.2; // part of the banded offset applied to the panel edge
const IMPULSE = 24;      // px a tap kicks both toward their new place before the bounce

/* Asymptotic rubber band: past the limit the offset grows ever slower instead of stopping dead. */
function band(value) {
  const abs = Math.abs(value);
  if (abs <= LIMIT) return value;
  return Math.sign(value) * (LIMIT + (abs - LIMIT) * RESIST / ((abs - LIMIT) + RESIST));
}

/** Give an existing element the grab-handle drag, rubber band and tap bounce.
 *  panel is the element whose edge stretches: it moves by --grab-panel-y and the handle
 *  by --grab-drag-y, both springing back under the grab-spring / grab-spring-click classes.
 *  opens is the drag direction that reveals the hidden part. isExpanded reads the state;
 *  setExpanded(next, event) applies a change the person made. */
export function attachGrabHandle(handle, { panel = handle.parentElement, opens = 'up', isExpanded, setExpanded }) {
  const sign = opens === 'up' ? -1 : 1; // screen direction of the opening drag
  const parts = [handle, panel];
  let startY = 0;
  let wasOpen = false;
  let downY = 0;
  let travel = 0; // furthest the pointer got from where it went down
  const shift = (handleY, panelY) => {
    handle.style.setProperty('--grab-drag-y', handleY + 'px');
    panel.style.setProperty('--grab-panel-y', panelY + 'px');
  };
  const settle = cls => {
    for (const el of parts) el.classList.add(cls);
    handle.style.removeProperty('--grab-drag-y');
    panel.style.removeProperty('--grab-panel-y');
  };

  handle.addEventListener('pointerdown', event => {
    startY = downY = event.clientY;
    wasOpen = isExpanded();
    travel = 0;
    for (const el of parts) el.classList.remove('grab-spring');
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
    settle('grab-spring');
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
    for (const el of parts) el.classList.remove('grab-spring', 'grab-spring-click');
    const kick = (opening ? sign : -sign) * IMPULSE;
    shift(kick, kick);
    requestAnimationFrame(() => settle('grab-spring-click'));
  });
}

/** Make a grab handle for a panel edge; returns its button plus state helpers.
 *  edge is the panel edge it sits on ('top' or 'bottom'); the panel must be positioned
 *  and must not clip the 14px the closed handle reaches past that edge. Dragging into
 *  the panel opens it. onToggle(expanded, event) runs on every change the person makes;
 *  setExpanded changes the state without it. */
export function createGrabHandle({ panel, edge = 'top', label = 'Show more', expanded = false, onToggle = null }) {
  if (!document.getElementById(STYLE_ID)) {
    const style = document.createElement('style');
    style.id = STYLE_ID;
    style.textContent = CSS;
    document.head.append(style);
  }
  const handle = document.createElement('button');
  handle.type = 'button';
  handle.className = 'grab-handle';
  handle.dataset.edge = edge;
  handle.title = label;
  handle.setAttribute('aria-label', label);
  handle.innerHTML = '<span class="grab-handle-line" aria-hidden="true"></span>';
  panel.classList.add('grab-panel');
  let open = !!expanded;
  const paint = () => handle.setAttribute('aria-expanded', String(open));
  paint();
  attachGrabHandle(handle, {
    panel,
    opens: edge === 'top' ? 'down' : 'up',
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
