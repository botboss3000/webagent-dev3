import {requestTouchVibration} from './vibrate.js';
import {pushBackHandler} from './history-back.js';

const FALLBACK_ICON = '<circle cx="12" cy="12" r="3"/><path d="M12 3v3M12 18v3M3 12h3M18 12h3"/>';
const CHEV_BACK = '<svg class="sr-chev-back" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="m15 18-6-6 6-6"/></svg>';

function iconMarkup(icon) {
  return '<svg class="sr-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">' + (icon || FALLBACK_ICON) + '</svg>';
}

/** One setting page: the register shows its content, so it never expands in place. */
export function createSettingsRow(host, {id, title, toggleLabel}) {
  const section = document.createElement('section');
  section.className = 'sr-section';
  section.dataset.registerSection = id;
  section.dataset.registerTitle = title;
  const body = document.createElement('div');
  body.className = 'sr-section-body';
  body.id = 'settings-' + id + '-body';
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
export function createSettingsMenu(host, {label = 'Settings', items = []} = {}) {
  const shell = document.createElement('div');
  shell.className = 'sr-shell';
  const menu = document.createElement('div');
  menu.className = 'sr-menu is-intro';
  menu.setAttribute('aria-label', label);
  for (const [index, item] of items.entries()) {
    const row = document.createElement('div');
    row.className = 'sr-item';
    const button = document.createElement('button');
    button.type = 'button';
    button.className = 'sr-row';
    button.dataset.registerLink = item.id;
    button.style.setProperty('--i', String(index));
    button.setAttribute('aria-label', 'Open ' + item.title);
    const tile = document.createElement('span');
    tile.className = 'sr-tile';
    tile.setAttribute('aria-hidden', 'true');
    tile.innerHTML = item.icon ? '<i data-lucide="' + item.icon + '"></i>' : iconMarkup('');
    const text = document.createElement('span');
    text.className = 'sr-text';
    const title = document.createElement('span');
    title.className = 'sr-title';
    title.textContent = item.title;
    const hint = document.createElement('span');
    hint.className = 'sr-hint';
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
      slot.className = 'sr-trailing';
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
export function createSettingsRegister(host, {label = 'Settings', categories = []}) {
  const shell = document.createElement('div');
  shell.className = 'sr-shell';
  const stack = document.createElement('div');
  stack.className = 'sr-stack';
  const menu = document.createElement('div');
  menu.className = 'sr-menu is-intro';
  menu.setAttribute('aria-label', label);
  const detail = document.createElement('div');
  detail.className = 'sr-detail';
  detail.hidden = true;
  const head = document.createElement('div');
  head.className = 'sr-detail-head';
  const back = document.createElement('button');
  back.type = 'button';
  back.className = 'sr-back';
  back.innerHTML = CHEV_BACK + '<span class="sr-back-title"></span>';
  const slot = document.createElement('span');
  slot.className = 'sr-toggle-slot';
  head.append(back, slot);
  const detailBody = document.createElement('div');
  detailBody.className = 'sr-detail-body';
  detail.append(head, detailBody);
  stack.append(menu, detail);
  shell.append(stack);
  host.append(shell);
  const stage = document.createElement('div');
  stage.className = 'sr-stage';
  host.append(stage);

  const pages = new Map();
  let currentId = null;
  let animating = false;
  let releaseBack = null;
  /* The phone's back button returns to the list instead of leaving the page. */
  const handleBack = () => {
    if (detail.hidden || !shell.getClientRects().length) return false;
    releaseBack = null;
    close({align: true});
    return true;
  };

  const api = {stage, open, close, present};
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
      section.querySelector('.sr-section-head')?.remove();
      section.querySelector('.conn-card-header')?.remove();
      body.hidden = false;
      body.style.display = 'block';
      body.classList.add('sr-page-body');
      return [{id, section, body, toggleWrap, toggle,
        title: category.title || section.dataset.registerTitle || 'Settings',
        hint: category.hint || '', icon: category.icon || ''}];
    });
    entries.sort((a, b) => (order.get(a.id) ?? Infinity) - (order.get(b.id) ?? Infinity));
    stage.remove();
    // Sections stay in the document so one that reloads itself still sees itself connected.
    const anchors = document.createElement('div');
    anchors.className = 'sr-anchors';
    anchors.append(...entries.map(entry => entry.section));
    shell.append(anchors);

    entries.forEach((entry, index) => {
      pages.set(entry.id, entry);
      const row = document.createElement('button');
      row.type = 'button';
      row.className = 'sr-row';
      row.dataset.registerFor = entry.id;
      row.style.setProperty('--i', String(index));
      row.innerHTML = '<span class="sr-tile">' + iconMarkup(entry.icon) + '</span>'
        + '<span class="sr-text"><span class="sr-title"></span><span class="sr-hint"></span></span>'
        + '<span class="sr-state"></span>';
      row.querySelector('.sr-title').textContent = entry.title;
      const hintEl = row.querySelector('.sr-hint');
      hintEl.textContent = entry.hint;
      if (!entry.hint) hintEl.remove();
      entry.state = row.querySelector('.sr-state');
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
    back.querySelector('.sr-back-title').textContent = entry.title;
    detailBody.replaceChildren(entry.body);
    slot.replaceChildren(entry.toggleWrap || '');
    animating = true;
    menu.classList.add('is-out-left');
    detail.hidden = false;
    detail.classList.add('is-in-right');
    void detail.offsetWidth;
    if (align) alignPageTop();
    // On a phone the back press returns to the list here instead of reloading the page.
    if (!releaseBack && window.matchMedia?.('(max-width: 800px)').matches) releaseBack = pushBackHandler(handleBack);
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
    // The list is back, so the history entry this page claimed is no longer needed.
    if (releaseBack) { const release = releaseBack; releaseBack = null; release(); }
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

  back.addEventListener('click', event => { requestTouchVibration(event); close({align: true}); });
  detail.addEventListener('keydown', event => {
    if (event.key === 'Escape') { event.preventDefault(); close({align: true}); }
  });

  return api;
}
