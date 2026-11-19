import {requestTouchVibration} from '../vibrate/vibrate.js';
import {pushBackHandler} from '../history-back/history-back.js';

const FALLBACK_ICON = '<circle cx="12" cy="12" r="3"/><path d="M12 3v3M12 18v3M3 12h3M18 12h3"/>';
const CHEV_BACK = '<svg class="spl-chev-back" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="m15 18-6-6 6-6"/></svg>';

// Custom icons (an ability's or group's .svg) are drawn inline, so only drawing elements and
// attributes survive; the server checks them too. Anything else is dropped, never escaped in.
const SVG_TAGS = new Set(['svg', 'g', 'path', 'circle', 'ellipse', 'line', 'polyline', 'polygon', 'rect', 'title',
  'defs', 'lineargradient', 'radialgradient', 'stop', 'clippath', 'mask']);
const SVG_ATTRS = /^(viewbox|width|height|fill|fill-opacity|fill-rule|clip-rule|stroke|stroke-[a-z]+|d|cx|cy|r|rx|ry|x|y|x1|y1|x2|y2|points|transform|opacity|id|offset|stop-color|stop-opacity|gradientunits|gradienttransform|clip-path|mask|xmlns)$/;
export function safeSvg(markup) {
  if (!markup) return null;
  const doc = new DOMParser().parseFromString(markup, 'image/svg+xml');
  const root = doc.documentElement;
  if (!root || root.nodeName.toLowerCase() !== 'svg') return null;
  const clean = element => {
    for (const child of [...element.children]) {
      if (!SVG_TAGS.has(child.nodeName.toLowerCase())) child.remove(); else clean(child);
    }
    for (const attribute of [...element.attributes]) {
      const value = attribute.value.toLowerCase();
      if (!SVG_ATTRS.test(attribute.name.toLowerCase()) || (value.includes('url(') && !/^url\(#[\w-]+\)$/.test(value.trim()))) {
        element.removeAttribute(attribute.name);
      }
    }
  };
  clean(root);
  root.setAttribute('class', 'spl-icon');
  root.setAttribute('aria-hidden', 'true');
  return document.importNode(root, true);
}

function iconMarkup(icon) {
  return '<svg class="spl-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">' + (icon || FALLBACK_ICON) + '</svg>';
}

/** One setting page: the register shows its content, so it never expands in place. */
export function createSubpage(host, {id, title, toggleLabel}) {
  const section = document.createElement('section');
  section.className = 'spl-section';
  section.dataset.registerSection = id;
  section.dataset.registerTitle = title;
  const body = document.createElement('div');
  body.className = 'spl-section-body';
  body.id = 'settings-' + id + '-body';
  // The register shows this body as the page, so an expandable row inside it stays a row.
  body.dataset.registerBody = '';
  section.append(body);
  let toggle = null;
  if (toggleLabel) {
    const wrap = document.createElement('label');
    wrap.className = 'conn-toggle-wrap ac-ability-toggle-wrap';
    toggle = document.createElement('input');
    toggle.type = 'checkbox';
    toggle.className = 'conn-toggle';
    toggle.setAttribute('aria-label', toggleLabel);
    const track = document.createElement('span');
    track.className = 'conn-toggle-track';
    track.setAttribute('aria-hidden', 'true');
    wrap.append(toggle, track);
    section.append(wrap);
  }
  section.addEventListener('click', event => {
    if (event.target.closest('button:not(:disabled),input[type=checkbox]:not(:disabled)')) requestTouchVibration(event);
  }, {capture:true});
  host.append(section);
  const register = host.__settingsRegister || null;
  /* Opening a category means going to its page; there is nothing to expand in place. */
  const setExpanded = open => { if (open) register?.open(id); };
  return {section, body, toggle, setExpanded};
}

/** A list of rows that each open a page elsewhere instead of revealing one here. */
export function createSubpageLinks(host, {label = 'Settings', items = []} = {}) {
  const shell = document.createElement('div');
  shell.className = 'spl-shell';
  const menu = document.createElement('div');
  menu.className = 'spl-menu is-intro';
  menu.setAttribute('aria-label', label);
  for (const [index, item] of items.entries()) {
    const row = document.createElement('div');
    row.className = 'spl-item';
    const button = document.createElement('button');
    button.type = 'button';
    button.className = 'spl-row';
    button.dataset.registerLink = item.id;
    button.style.setProperty('--i', String(index));
    button.setAttribute('aria-label', 'Open ' + item.title);
    const tile = document.createElement('span');
    tile.className = 'spl-tile';
    tile.setAttribute('aria-hidden', 'true');
    const custom = safeSvg(item.svg);
    if (custom) tile.append(custom);
    else tile.innerHTML = item.icon ? '<i data-lucide="' + item.icon + '"></i>' : iconMarkup('');
    const text = document.createElement('span');
    text.className = 'spl-text';
    const title = document.createElement('span');
    title.className = 'spl-title';
    title.textContent = item.title;
    const hint = document.createElement('span');
    hint.className = 'spl-hint';
    hint.textContent = item.hint || '';
    text.append(title, hint);
    button.append(tile, text);
    button.addEventListener('click', event => {
      requestTouchVibration(event);
      item.onSelect?.(item.id);
    });
    row.append(button);
    if (item.trailing) {
      const slot = document.createElement('span');
      slot.className = 'spl-trailing';
      slot.append(item.trailing);
      row.append(slot);
    }
    menu.append(row);
  }
  shell.append(menu);
  host.append(shell);
  window.lucide?.createIcons();
  return {menu};
}

/** A list of setting categories that opens one category's page at a time. */
export function createSubpageList(host, {label = 'Settings', categories = []}) {
  const shell = document.createElement('div');
  shell.className = 'spl-shell';
  const stack = document.createElement('div');
  stack.className = 'spl-stack';
  const menu = document.createElement('div');
  menu.className = 'spl-menu is-intro';
  menu.setAttribute('aria-label', label);
  const detail = document.createElement('div');
  detail.className = 'spl-detail';
  detail.hidden = true;
  const head = document.createElement('div');
  head.className = 'spl-detail-head';
  const back = document.createElement('button');
  back.type = 'button';
  back.className = 'spl-back';
  back.innerHTML = CHEV_BACK + '<span class="spl-back-title"></span>';
  const slot = document.createElement('span');
  slot.className = 'spl-toggle-slot';
  head.append(back, slot);
  const detailBody = document.createElement('div');
  detailBody.className = 'spl-detail-body';
  detail.append(head, detailBody);
  stack.append(menu, detail);
  shell.append(stack);
  host.append(shell);
  const stage = document.createElement('div');
  stage.className = 'spl-stage';
  host.append(stage);

  const pages = new Map();
  let currentId = null;
  let animating = false;
  // The open pages, outermost first: a category's page, then any page pushed inside it.
  // Each level holds its own phone back entry, so back leaves one level at a time.
  const levels = [];
  const phone = () => !!window.matchMedia?.('(max-width: 800px)').matches;
  const claimBack = (level, anyWidth = false) => {
    if (!anyWidth && !phone()) return;
    level.release = pushBackHandler(() => {
      if (detail.hidden || !shell.getClientRects().length) return false;
      level.release = null;
      if (levels.length > 1) pop(); else close({align: true});
      return true;
    });
  };
  const dropBack = level => {
    if (!level.release) return;
    const release = level.release;
    level.release = null;
    release();
  };
  function render() {
    const top = levels[levels.length - 1];
    back.querySelector('.spl-back-title').textContent = top.title;
    detailBody.replaceChildren(top.body);
    slot.replaceChildren(top.toggleWrap || '');
  }
  function slide(direction) {
    detailBody.classList.add(direction);
    void detailBody.offsetWidth;
    requestAnimationFrame(() => detailBody.classList.remove(direction));
  }

  const api = {stage, open, close, present, push, pop, depth: () => levels.length};
  stage.__settingsRegister = api;

  function pin() {
    if (!menu.isConnected || menu.hidden) return;
    stack.style.minHeight = menu.offsetHeight + 'px';
  }

  /* Switching pages starts the new page at the top; a pinned tab header above it stays pinned. */
  function alignPageTop() {
    const scroller = host.closest('#main-panel') || host.ownerDocument.scrollingElement;
    if (!scroller) return;
    const scrollerTop = scroller === host.ownerDocument.scrollingElement ? 0 : scroller.getBoundingClientRect().top;
    // A sticky box already covering the scroller's top edge is a pinned header the page sits below.
    let cover = 0;
    for (let node = host; node && node !== scroller; node = node.parentElement) {
      for (let prev = node.previousElementSibling; prev; prev = prev.previousElementSibling) {
        if (getComputedStyle(prev).position !== 'sticky') continue;
        const rect = prev.getBoundingClientRect();
        if (rect.top <= scrollerTop + 1 && rect.bottom > scrollerTop) cover = Math.max(cover, rect.bottom - scrollerTop);
      }
    }
    if (!cover) { scroller.scrollTop = 0; return; }
    scroller.scrollTop = Math.max(0, host.getBoundingClientRect().top - scrollerTop + scroller.scrollTop - cover);
  }

  /* Rows are built once every category has mounted its section into the stage. */
  function present() {
    const meta = new Map(categories.map(category => [category.id, category]));
    const order = new Map(categories.map((category, index) => [category.id, index]));
    const entries = [...stage.children].flatMap(section => {
      const id = section.dataset.registerSection;
      if (!id) return [];
      const body = section.querySelector('[data-register-body]')
        || section.querySelector('.ac-ability-body')
        || section;
      // The row's own switch; switches marked data-inline-switch stay inside the page.
      const toggleWrap = section.querySelector('.conn-toggle-wrap:not([data-inline-switch])');
      const toggle = toggleWrap?.querySelector('input.conn-toggle') || null;
      const category = meta.get(id) || {};
      section.querySelector('.spl-section-head')?.remove();
      section.querySelector('.conn-card-header')?.remove();
      body.hidden = false;
      body.style.display = 'block';
      body.classList.add('spl-page-body');
      return [{id, section, body, toggleWrap, toggle,
        title: category.title || section.dataset.registerTitle || 'Settings',
        hint: category.hint || '', icon: category.icon || '',
        lucide: category.lucide || '', color: category.color || '', svg: category.svg || ''}];
    });
    entries.sort((a, b) => (order.get(a.id) ?? Infinity) - (order.get(b.id) ?? Infinity));
    stage.remove();
    // Sections stay in the document so one that reloads itself still sees itself connected.
    const anchors = document.createElement('div');
    anchors.className = 'spl-anchors';
    anchors.append(...entries.map(entry => entry.section));
    shell.append(anchors);

    entries.forEach((entry, index) => {
      pages.set(entry.id, entry);
      const row = document.createElement('button');
      row.type = 'button';
      row.className = 'spl-row';
      row.dataset.registerFor = entry.id;
      row.style.setProperty('--i', String(index));
      // A category gives raw SVG paths (icon) or a Lucide name (lucide), plus an optional color.
      row.innerHTML = '<span class="spl-tile">'
        + (entry.lucide ? '<i data-lucide="' + entry.lucide + '"></i>' : iconMarkup(entry.icon)) + '</span>'
        + '<span class="spl-text"><span class="spl-title"></span><span class="spl-hint"></span></span>'
        + '<span class="spl-state"></span>';
      const custom = safeSvg(entry.svg);
      if (custom) row.querySelector('.spl-tile').replaceChildren(custom);
      if (entry.color) row.querySelector('.spl-tile').style.color = entry.color;
      row.querySelector('.spl-title').textContent = entry.title;
      const hintEl = row.querySelector('.spl-hint');
      hintEl.textContent = entry.hint;
      if (!entry.hint) hintEl.remove();
      entry.state = row.querySelector('.spl-state');
      if (entry.toggle) {
        entry.state.textContent = entry.toggle.checked ? 'On' : 'Off';
        entry.toggle.addEventListener('change', () => {
          entry.state.textContent = entry.toggle.checked ? 'On' : 'Off';
        });
      } else {
        entry.state.remove();
      }
      row.addEventListener('click', event => { requestTouchVibration(event); open(entry.id, {align: true}); });
      menu.append(row);
    });

    pin();
    if (window.ResizeObserver) new ResizeObserver(pin).observe(menu);
    window.addEventListener('resize', pin);
    setTimeout(() => menu.classList.remove('is-intro'), entries.length * 45 + 500);
  }

  function open(id, {align = false} = {}) {
    const entry = pages.get(id);
    if (!entry || animating) return;
    // A section that asks to open while its own page is showing only needs the focus kept.
    if (currentId === id && !detail.hidden) return;
    currentId = id;
    while (levels.length) dropBack(levels.pop());
    levels.push({title: entry.title, body: entry.body, toggleWrap: entry.toggleWrap, release: null});
    render();
    animating = true;
    menu.classList.add('is-out-left');
    detail.hidden = false;
    detail.classList.add('is-in-right');
    void detail.offsetWidth;
    if (align) alignPageTop();
    // On a phone the back press returns to the list here instead of reloading the page.
    claimBack(levels[0]);
    requestAnimationFrame(() => detail.classList.remove('is-in-right'));
    setTimeout(() => {
      menu.hidden = true;
      menu.classList.remove('is-out-left');
      animating = false;
      back.focus({preventScroll: true});
    }, 220);
  }

  function close({align = false} = {}) {
    if (animating || detail.hidden) return;
    // The list is back, so the history entries these pages claimed are no longer needed.
    while (levels.length > 1) { const level = levels.pop(); dropBack(level); level.onPop?.(); }
    if (levels[0]) dropBack(levels[0]);
    animating = true;
    detail.classList.add('is-out-right');
    menu.hidden = false;
    menu.classList.add('is-in-left');
    void menu.offsetWidth;
    if (align) alignPageTop();
    requestAnimationFrame(() => menu.classList.remove('is-in-left'));
    setTimeout(() => {
      detail.hidden = true;
      detail.classList.remove('is-out-right');
      slot.replaceChildren();
      animating = false;
      menu.querySelector('[data-register-for="' + currentId + '"]')?.focus({preventScroll: true});
    }, 220);
  }

  /** Open a page inside the current one (one level deeper), under the same back header.
   *  Returns false when no category page is open. onPop runs when the page is left.
   *  claim:false adds no back press (a page restored after a rerender); anyWidth claims
   *  the back press on every screen width, not only on phones. */
  function push({title, body, toggleWrap = null, onPop = null, claim = true, anyWidth = false}) {
    if (detail.hidden || !levels.length) return false;
    const level = {title, body, toggleWrap, onPop, release: null};
    levels.push(level);
    render();
    slide('is-in-right');
    if (claim) claimBack(level, anyWidth);
    alignPageTop();
    back.focus({preventScroll: true});
    return true;
  }

  /** Leave the innermost pushed page; at the category page this returns to the list. */
  function pop() {
    if (levels.length <= 1) { close({align: true}); return; }
    const level = levels.pop();
    dropBack(level);
    level.onPop?.();
    render();
    slide('is-in-left');
    back.focus({preventScroll: true});
  }

  back.addEventListener('click', event => { requestTouchVibration(event); pop(); });
  detail.addEventListener('keydown', event => {
    if (event.key === 'Escape') { event.preventDefault(); pop(); }
  });

  return api;
}
