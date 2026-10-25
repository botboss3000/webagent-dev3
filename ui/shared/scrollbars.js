/* Desktop Firefox half of the shared scrollbar; Chromium/Safari draw it from the
   wa-scrollbar layer in /v3-ui/app.css. Firefox offers only auto/thin widths and
   its GTK overlay bars fade when idle, so on fine-pointer desktops each scroller's
   native bar is hidden and the same persistent 6px thumb is drawn as a positioned
   child, keeping the scroller's own clipping and stacking. */

/** Thumb length and offset along one axis, or null when nothing overflows. */
function scrollThumb(view, content, pos, min = 24) {
  if (content <= view + 1) return null;
  const size = Math.min(view, Math.max(min, Math.round(view * view / content)));
  const ratio = Math.min(1, Math.max(0, pos / (content - view)));
  return {size, offset: Math.round((view - size) * ratio)};
}

(() => {
  if (typeof document === 'undefined' || CSS.supports('selector(::-webkit-scrollbar)')
    || !matchMedia('(hover: hover) and (pointer: fine)').matches) return;
  const SIZE = 6; // --wa-scrollbar-size in app.css
  const bars = new Map(); // scroller -> {y, x} thumbs
  const scrolls = v => v === 'auto' || v === 'scroll';

  function thumb(el, axis) {
    const t = document.createElement('div');
    t.className = 'wa-scrollbar';
    t.setAttribute('aria-hidden', 'true');
    t.addEventListener('pointerdown', e => {
      if (e.button !== 0) return;
      e.preventDefault();
      t.setPointerCapture(e.pointerId);
      t.classList.add('dragging');
      const y = axis === 'y', start = y ? e.clientY : e.clientX, from = y ? el.scrollTop : el.scrollLeft;
      const view = y ? el.clientHeight : el.clientWidth, content = y ? el.scrollHeight : el.scrollWidth;
      const g = scrollThumb(view, content, from);
      const move = m => {
        const delta = (y ? m.clientY : m.clientX) - start;
        el[y ? 'scrollTop' : 'scrollLeft'] = from + delta * (content - view) / Math.max(1, view - g.size);
      };
      const end = () => { t.classList.remove('dragging'); t.removeEventListener('pointermove', move); };
      t.addEventListener('pointermove', move);
      t.addEventListener('pointerup', end, {once: true});
      t.addEventListener('pointercancel', end, {once: true});
    });
    return t;
  }

  function place(el, t, axis, enabled) {
    const y = axis === 'y';
    const g = enabled && scrollThumb(y ? el.clientHeight : el.clientWidth,
      y ? el.scrollHeight : el.scrollWidth, y ? el.scrollTop : el.scrollLeft);
    if (!g) { t.hidden = true; return; }
    // Content coordinates: the thumb rides at the visible edge of the scrolled box.
    const x0 = el.scrollLeft + (y ? el.clientWidth - SIZE : g.offset);
    const y0 = el.scrollTop + (y ? g.offset : el.clientHeight - SIZE);
    t.hidden = false;
    t.style.transform = `translate(${x0}px, ${y0}px)`;
    t.style[y ? 'height' : 'width'] = g.size + 'px';
    if (!t.isConnected) el.append(t);
  }

  function draw(el) {
    const bar = bars.get(el);
    place(el, bar.y, 'y', bar.cy);
    place(el, bar.x, 'x', bar.cx);
  }

  // shortcut: rescans every element after DOM changes (throttled); upgrade to
  // per-component registration if Firefox profiling shows scan cost.
  function scan() {
    // Positioned thumbs count as scrollable overflow; hide them so shrunken
    // content is measured without the previous thumb position.
    for (const [el, bar] of bars) {
      if (!el.isConnected) { bar.y.remove(); bar.x.remove(); bars.delete(el); }
      else bar.y.hidden = bar.x.hidden = true;
    }
    for (const el of document.body.querySelectorAll('*')) {
      if (el.classList.contains('wa-scrollbar') || el.isContentEditable
        || /^(TEXTAREA|INPUT|SELECT|IFRAME)$/.test(el.tagName)) continue;
      let bar = bars.get(el);
      if (!bar && el.scrollHeight <= el.clientHeight + 1 && el.scrollWidth <= el.clientWidth + 1) continue;
      const cs = getComputedStyle(el);
      const cy = scrolls(cs.overflowY), cx = scrolls(cs.overflowX);
      if (!bar) {
        // Scrollers that hide their bar on purpose (carousels) stay hidden.
        if ((!cy && !cx) || cs.scrollbarWidth === 'none') continue;
        el.style.scrollbarWidth = 'none';
        if (cs.position === 'static') el.style.position = 'relative';
        bar = {y: thumb(el, 'y'), x: thumb(el, 'x')};
        bars.set(el, bar);
      }
      bar.cy = cy; bar.cx = cx;
      draw(el);
    }
  }

  let scanTimer = 0, lastScan = 0;
  function queueScan() {
    if (scanTimer) return;
    scanTimer = setTimeout(() => { scanTimer = 0; lastScan = Date.now(); scan(); },
      Math.max(0, 250 - (Date.now() - lastScan)));
  }
  const pending = new Set();
  let frame = 0;
  document.addEventListener('scroll', e => {
    if (!bars.has(e.target)) return;
    pending.add(e.target);
    frame ||= requestAnimationFrame(() => { frame = 0; for (const el of pending) draw(el); pending.clear(); });
  }, {capture: true, passive: true});
  for (const type of ['input', 'load', 'transitionend']) document.addEventListener(type, queueScan, true);
  addEventListener('resize', queueScan);
  new MutationObserver(records => {
    // Thumb moves and insertions are this component's own writes.
    if (records.some(r => !(r.target.classList?.contains('wa-scrollbar')
      || [...r.addedNodes, ...r.removedNodes].every(n => n.classList?.contains('wa-scrollbar'))
        && r.addedNodes.length + r.removedNodes.length > 0))) queueScan();
  }).observe(document.documentElement, {subtree: true, childList: true, attributes: true, characterData: true});
  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', queueScan);
  else queueScan();
})();
