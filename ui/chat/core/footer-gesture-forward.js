'use strict';
// @code-index {"version":1,"feature":"chat-footer-gesture-forward","aliases":["footer scroll","header scroll","composer scroll forwarding"],"purpose":"Routes wheel and touch gestures that begin over the chat footer, chat header, or mobile app header to the transcript scroller and marks reader scroll intent.","span_lines":80,"configuration":{"status":"not_applicable","rationale":"Gesture forwarding owns no tunable configuration; pull-to-refresh priority is read from the live refresh indicator and the transcript anchor's scroll_intent_ms governs intent duration."}}
// Ported from v2 ui/chat/js/footer-gesture-forward.js. The footer (composer pill
// and control rows) and the headers have no scrollable ancestor — #chat-panel,
// #stage and <body> are all overflow:hidden — so a gesture that starts there
// never reaches the transcript. Forward wheel and touch drags to #chat-messages
// and report them as transcript intent, so the anchor treats them like a gesture
// over the messages.
const TOUCH_ENGAGE_PX = 6;

/**
 * True when the gesture target sits inside a scrollable descendant of `root` —
 * a nested scroller that should own the gesture (for example a long draft in the
 * composer textarea or a scrolling action card inside the footer).
 */
function _isInsideScrollableChild(root, target) {
  let node = target && target.nodeType === 1 ? target : null;
  while (node && node !== root && node !== document.body && node !== document.documentElement) {
    const cs = window.getComputedStyle(node);
    if ((cs.overflowY === 'auto' || cs.overflowY === 'scroll' || cs.overflowY === 'overlay')
        && node.scrollHeight > node.clientHeight + 1) return true;
    node = node.parentElement;
  }
  return false;
}

/**
 * True when a downward pull is already owned by the app's pull-to-refresh
 * indicator. Upward movement never yields to refresh, so a gesture can reverse
 * direction and start scrolling the transcript.
 */
export function pullRefreshOwnsPull({ lastY, currentY, indicator } = {}) {
  if (!(currentY > lastY)) return false;
  const node = indicator || (typeof document === 'undefined' ? null : document.getElementById('ptr-indicator'));
  return !!node && !node.hidden;
}

/**
 * Wires targets to the transcript scroller. Returns a disposer that removes
 * every listener it added; wiring is idempotent per element.
 *
 * `canForward(event, element)` can veto a target for the current event (for
 * example the app header only forwards in mobile mode).
 * `touchGuard({ event, element, startY, lastY, currentY })` lets a
 * higher-priority surface own one touch move; returning true leaves the move
 * unclaimed by the transcript.
 */
export function forwardFooterGestures({ targets = [], scroller, markIntent, canForward, touchGuard } = {}) {
  if (!scroller) return () => {};
  const abort = new AbortController();
  const { signal } = abort;

  for (const el of targets) {
    if (!el || el.dataset.chatFooterGestureWired === '1') continue;
    el.dataset.chatFooterGestureWired = '1';

    // ── Press ──────────────────────────────────────────────────────────────
    // Pressing a row under the composer would move focus off the editor and
    // close the mobile keyboard before any scroll starts. Keep the editor that
    // owns the gesture focused for presses on non-editable footer content;
    // presses inside an editor still place its caret. Scrolling is driven by
    // touch events, so cancelling pointerdown does not block it, and clicks
    // (including the row's buttons) still fire.
    el.addEventListener('pointerdown', (event) => {
      if (event.button !== 0 || event.defaultPrevented) return;
      const active = document.activeElement;
      if (!active || active === event.target || !active.matches?.('input, textarea, select, [contenteditable]:not([contenteditable="false"])')) return;
      if (el.contains && !el.contains(active)) return;
      if (event.target?.closest?.('input, textarea, select, [contenteditable]:not([contenteditable="false"])')) return;
      event.preventDefault();
    }, { signal });

    // ── Wheel ──────────────────────────────────────────────────────────────
    el.addEventListener('wheel', (event) => {
      // Ctrl+wheel belongs to chat-zoom; a handled or zero-delta event stays put.
      if (event.ctrlKey || event.defaultPrevented || !event.deltaY) return;
      if (canForward && !canForward(event, el)) return;
      // A nested scroller inside the footer keeps its own gesture.
      if (_isInsideScrollableChild(el, event.target)) return;
      event.preventDefault();
      markIntent?.();
      const delta = event.deltaMode === 1 ? event.deltaY * 16
        : event.deltaMode === 2 ? event.deltaY * scroller.clientHeight
        : event.deltaY;
      scroller.scrollBy({ top: delta, behavior: 'auto' });
    }, { passive: false, signal });

    // ── Touch ──────────────────────────────────────────────────────────────
    // Touch has no wheel-style chaining: the browser only scrolls the touch
    // target's scrollable ancestor, and the footer has none. Translate the drag
    // into scrollBy() by hand, one pixel of movement per pixel of finger travel.
    let active = false;
    let engaged = false;
    let startY = 0;
    let lastY = 0;
    el.addEventListener('touchstart', (event) => {
      active = false;
      engaged = false;
      const t = event.touches && event.touches[0];
      if (!t) return;
      if (canForward && !canForward(event, el)) return;
      // A nested scroller inside the footer keeps its own gesture.
      if (_isInsideScrollableChild(el, event.target)) return;
      active = true;
      startY = lastY = t.clientY;
    }, { passive: true, signal });
    el.addEventListener('touchmove', (event) => {
      if (!active) return;
      // A descendant forwarder already claimed this move — never double-scroll.
      if (event.defaultPrevented) { active = false; return; }
      if (canForward && !canForward(event, el)) { active = false; return; }
      const t = event.touches && event.touches[0];
      if (!t) return;
      const currentY = t.clientY;
      // A higher-priority surface (pull-to-refresh) owns this move. Keep tracking
      // the finger so a reversal can still start scrolling without a new touch.
      if (touchGuard && touchGuard({ event, element: el, startY, lastY, currentY })) {
        lastY = currentY;
        return;
      }
      if (!engaged) {
        if (Math.abs(currentY - startY) < TOUCH_ENGAGE_PX) return;
        engaged = true;
        // The drag is a scroll, not a caret placement — drop any selection the
        // touchstart began in the composer so text does not smear while scrolling.
        const sel = window.getSelection && window.getSelection();
        if (sel && sel.removeAllRanges) sel.removeAllRanges();
      }
      const dy = lastY - currentY; // > 0 = finger up = scroll down
      lastY = currentY;
      if (!dy) return;
      event.preventDefault();
      markIntent?.();
      scroller.scrollBy({ top: dy, behavior: 'auto' });
    }, { passive: false, signal });
    const release = () => { active = false; engaged = false; };
    el.addEventListener('touchend', release, { passive: true, signal });
    el.addEventListener('touchcancel', release, { passive: true, signal });
  }

  return () => abort.abort();
}
