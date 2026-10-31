'use strict';
import { applyRubberBand } from './rubber-band.js';

// V2 tab carousel (agents/js/view.js _wireTabCarousel): the strip scrolls natively
// with a hidden scrollbar; edge chevrons and fades (shared agents.css) show while
// more tabs lie off that side.
export function wirePageTabCarousel(strip) {
  const wrap = strip.parentElement;
  const chevron = (side, glyph) => {
    const button = document.createElement('button');
    button.type = 'button'; button.className = 'agent-card-tabs-chev ' + side; button.tabIndex = -1;
    button.setAttribute('aria-label', 'Scroll tabs ' + side); button.textContent = glyph;
    button.addEventListener('click', event => {
      event.stopPropagation();
      const page = Math.max(strip.clientWidth * 0.7, 120);
      strip.scrollBy({ left: side === 'left' ? -page : page, behavior: 'smooth' });
    });
    return button;
  };
  wrap.prepend(chevron('left', '❮'));
  wrap.append(chevron('right', '❯'));
  const update = () => {
    const maxScroll = strip.scrollWidth - strip.clientWidth, overflowing = maxScroll > 1;
    wrap.classList.toggle('can-scroll-left', overflowing && strip.scrollLeft > 1);
    wrap.classList.toggle('can-scroll-right', overflowing && strip.scrollLeft < maxScroll - 1);
  };
  strip.addEventListener('scroll', update, { passive: true });
  applyRubberBand(strip);
  new ResizeObserver(update).observe(strip);
  // Tabs are re-rendered, hidden and re-selected in place: keep the active one in view.
  let active = null;
  new MutationObserver(() => {
    const current = strip.querySelector('[role=tab][aria-selected=true]');
    if (current && current !== active) current.scrollIntoView({ inline: 'nearest', block: 'nearest' });
    active = current;
    update();
  }).observe(strip, { subtree: true, childList: true, attributes: true, attributeFilter: ['hidden', 'aria-selected'] });
  requestAnimationFrame(update);
}

// Shared Agents/App interaction: a 500ms hold arms reorder; a short tap navigates.
export function wirePageTabDrag(strip, { getOrder, save, prefix }) {
  let press = null, suppress = false;
  const tabFor = id => [...strip.children].find(tab => tab.id === prefix + id);
  const paint = () => { for (const id of getOrder()) strip.append(tabFor(id)); };
  const finish = event => {
    if (!press || event.pointerId !== press.pointerId) return;
    clearTimeout(press.timer);
    if (press.armed) { suppress = true; save(); setTimeout(() => { suppress = false; }, 500); }
    press.tab.classList.remove('dragging');
    if (strip.hasPointerCapture(event.pointerId)) strip.releasePointerCapture(event.pointerId);
    press = null;
  };
  strip.addEventListener('pointerdown', event => {
    const tab = event.target.closest('[role=tab]');
    if (!tab || tab.classList.contains('ephemeral') || event.button !== 0 || press) return;
    press = { tab, pointerId: event.pointerId, x: event.clientX, y: event.clientY, armed: false };
    const current = press;
    current.timer = setTimeout(() => {
      if (press !== current) return;
      current.armed = true; tab.classList.add('dragging');
      if (strip.hasPointerCapture(event.pointerId) || event.isTrusted) strip.setPointerCapture(event.pointerId);
    }, 500);
  });
  strip.addEventListener('pointermove', event => {
    if (!press || event.pointerId !== press.pointerId) return;
    if (!press.armed) {
      if (Math.hypot(event.clientX - press.x, event.clientY - press.y) > 8) { clearTimeout(press.timer); press = null; }
      return;
    }
    event.preventDefault();
    const target = [...strip.querySelectorAll('[role=tab]:not([hidden]):not(.ephemeral)')].find(tab => {
      const rect = tab.getBoundingClientRect(); return event.clientX >= rect.left && event.clientX <= rect.right;
    });
    if (!target || target === press.tab) return;
    const order = getOrder(), from = order.indexOf(press.tab.id.slice(prefix.length)), to = order.indexOf(target.id.slice(prefix.length));
    if (from < 0 || to < 0) return;
    const [id] = order.splice(from, 1); order.splice(to, 0, id); paint();
  });
  // Tabs allow pan-x so a swipe scrolls the strip; once a hold arms reorder, the drag owns the touch.
  strip.addEventListener('touchmove', event => { if (press?.armed) event.preventDefault(); }, { passive: false });
  strip.addEventListener('pointerup', finish);
  strip.addEventListener('pointercancel', finish);
  strip.addEventListener('click', event => { if (suppress) { event.preventDefault(); event.stopImmediatePropagation(); } }, true);
  strip.addEventListener('keydown', event => {
    const tab = event.target.closest('[role=tab]');
    if (!tab || tab.classList.contains('ephemeral') || !event.altKey || !['ArrowLeft', 'ArrowRight'].includes(event.key)) return;
    event.preventDefault();
    const order = getOrder(), from = order.indexOf(tab.id.slice(prefix.length)), to = from + (event.key === 'ArrowLeft' ? -1 : 1);
    if (from < 0 || to < 0 || to >= order.length) return;
    const [id] = order.splice(from, 1); order.splice(to, 0, id); save(); tab.focus();
  });
}
