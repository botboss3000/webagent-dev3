// V2 schema adapter with V3-specific incremental layout and focus-preserving reconciliation.
// @code-index {"version":1,"feature":"chat-ui-config","aliases":["chat configuration","chat pill layout"],"purpose":"Normalizes chat pill settings and renders configured composer controls Config: data/config/chat.json -> chat_common, chat_desktop, chat_mobile, chat_widget, chat_display.default.","span_lines":40,"configuration":{"status":"configured","references":[{"path":"data/config/chat.json","pointers":["/chat_display/default","/chat_common","/chat_desktop","/chat_mobile","/chat_widget"],"profile_overrides":true}],"opportunities":[]}}
'use strict';

// Canonical chat-pill schema and renderer.
//
// ⚠️ JSON DRIVES SIZING — all button sizes (element_size/container_size),
// textarea properties (font_size, min_height, max_height, padding), and
// zone padding are authored in data/config/chat.json. This file applies
// them as inline styles. Change the JSON, not the CSS or JS defaults.
//
// The public schema is always two rows with left/center/right zones. Older
// row/column configs are normalized here so stored per-agent overrides keep
// working while every surface consumes one layout shape.

import { showLeftOverlay } from './login-hook.js';

const ZONES = ['left', 'center', 'right'];
const ALIGN_VALUES = { top: 'flex-start', center: 'center', bottom: 'flex-end' };
const CONTROL_ALIASES = { voice: 'mic' };
const MODE_CLASSES = [
  'chat-pill-mic-send',
  'chat-pill-distinct-buttons',
  'chat-pill-mic-only',
  'chat-pill-send-only',
  'chat-pill-no-action',
];

/** Shallowly combines schema layers before legacy/canonical normalization. */
function mergeObjects(base, overrides) {
  return { ...(base || {}), ...(overrides || {}) };
}

/** Maps the legacy voice name to the canonical mic control key. */
function canonicalName(name) {
  return CONTROL_ALIASES[name] || name;
}

/** Creates the normalized metadata shape expected for each layout zone. */
function emptyZoneMeta() {
  return { controls: [], carousel: null, align: null, padding: null };
}

/** Creates the fixed two-row, three-zone normalized layout. */
function emptyRows() {
  return [
    { left: emptyZoneMeta(), center: emptyZoneMeta(), right: emptyZoneMeta() },
    { left: emptyZoneMeta(), center: emptyZoneMeta(), right: emptyZoneMeta() },
  ];
}

/** Converts legacy arrays or current zone objects into canonical controls and styles. */
function normalizeZone(value) {
  // Legacy: bare array of control names → default align/padding
  if (Array.isArray(value)) return { controls: value.map(canonicalName), carousel: null, align: null, padding: null };
  // Object form: { controls, carousel, align, padding }
  if (value && typeof value === 'object' && !Array.isArray(value)) {
    return {
      controls: Array.isArray(value.controls) ? value.controls.map(canonicalName) : [],
      carousel: Array.isArray(value.carousel) ? value.carousel.map(canonicalName) : null,
      align: ALIGN_VALUES[value.align] || null,
      padding: value.padding || null,
    };
  }
  return emptyZoneMeta();
}

/** Normalizes at most two configured rows and fills all absent zones. */
function normalizeRows(rows) {
  const out = emptyRows();
  for (let index = 0; index < Math.min(2, rows?.length || 0); index += 1) {
    for (const zone of ZONES) {
      out[index][zone] = normalizeZone(rows[index]?.[zone]);
    }
  }
  return out;
}

/** Resolves enabled legacy mic/send settings to their combined or individual control. */
function legacyButtonName(buttons) {
  const micEnabled = buttons?.voice?.enabled !== false;
  const sendEnabled = buttons?.send?.enabled !== false;
  if (micEnabled && sendEnabled) return 'mic_send';
  if (micEnabled) return 'mic';
  if (sendEnabled) return 'send';
  return null;
}

/** Translates a legacy row,column coordinate into the matching named zone. */
function placeLegacyControl(rows, cell, name) {
  if (!cell || !name) return;
  const [rowRaw, colRaw] = String(cell).split(',');
  const row = Number.parseInt(rowRaw, 10) - 1;
  const col = Number.parseInt(colRaw, 10);
  if (row < 0 || row > 1) return;
  const zone = col === 1 ? 'left' : col === 3 ? 'right' : 'center';
  rows[row][zone].controls.push(name);
}

/** Removes duplicate controls and individual mic/send entries shadowed by mic_send. */
function finalizeRows(rows) {
  const hasCombined = rows.some(row => ZONES.some(
    zone => row[zone].controls.includes('mic_send') || (row[zone].carousel || []).includes('mic_send'),
  ));
  const seen = new Set();
  for (const row of rows) {
    for (const zone of ZONES) {
      row[zone].controls = row[zone].controls.filter(name => {
        if (hasCombined && (name === 'mic' || name === 'send')) return false;
        if (seen.has(name)) return false;
        seen.add(name);
        return true;
      });
      if (row[zone].carousel) {
        row[zone].carousel = row[zone].carousel.filter(name => {
          if (hasCombined && (name === 'mic' || name === 'send')) return false;
          if (seen.has(name)) return false;
          seen.add(name);
          return true;
        });
      }
    }
  }
  return rows;
}

/** Converts old and current pill schemas into canonical rows and control settings. */
export function normalizeChatPill(pill = {}) {
  const legacyButtons = pill.buttons || {};
  const canonicalControls = pill.controls || {};
  const hasLegacyShape = Boolean(
    pill.layout || pill.buttons || pill.textarea || pill.stats || pill.attach,
  );
  const micCfg = mergeObjects(
    canonicalControls.mic || canonicalControls.voice,
    legacyButtons.voice,
  );
  const sendCfg = mergeObjects(canonicalControls.send, legacyButtons.send);
  const combinedCfg = mergeObjects(micCfg, sendCfg);
  const controls = {
    ...canonicalControls,
    textarea: mergeObjects(canonicalControls.textarea, pill.textarea),
    stats: mergeObjects(canonicalControls.stats, pill.stats),
    attach: mergeObjects(canonicalControls.attach, pill.attach),
    mic: micCfg,
    send: sendCfg,
    // A switching control uses one common box. Preserve old configurations
    // by preferring the send dimensions when the two legacy buttons differ.
    mic_send: hasLegacyShape
      ? mergeObjects(canonicalControls.mic_send, combinedCfg)
      : mergeObjects(combinedCfg, canonicalControls.mic_send),
  };
  delete controls.voice;

  let rows;
  if (hasLegacyShape) {
    rows = emptyRows();
    const layout = pill.layout || {};
    placeLegacyControl(rows, layout.textarea || '1,2', 'textarea');
    placeLegacyControl(rows, layout.stats || '2,2', 'stats');
    placeLegacyControl(rows, layout.buttons || '1,3', legacyButtonName(legacyButtons));
    if (pill.attach?.enabled !== false) {
      placeLegacyControl(rows, layout.attach || '2,3', 'attach');
    }
  } else if (Array.isArray(pill.rows)) {
    rows = normalizeRows(pill.rows);
  } else {
    rows = normalizeRows([]);
  }

  return { ...pill, rows: finalizeRows(rows), controls };
}

/** Lists enabled controls in row order for renderer visibility decisions. */
export function configuredRowControls(config) {
  const names = [];
  for (const row of config?.rows || []) {
    for (const zone of ZONES) {
      for (const name of row?.[zone]?.controls || []) {
        if (config?.controls?.[name]?.enabled !== false) names.push(name);
      }
      for (const name of row?.[zone]?.carousel || []) {
        if (config?.controls?.[name]?.enabled !== false) names.push(name);
      }
    }
  }
  return names;
}

/** Clears hidden/display state when a configured control becomes visible. */
function showElement(el) {
  if (!el) return;
  el.hidden = false;
  el.style.removeProperty('display');
}

/** Forces a control hidden even when author CSS overrides the hidden attribute. */
function hideElement(el) {
  if (!el) return;
  el.hidden = true;
  el.style.setProperty('display', 'none', 'important');
}

/** Applies configured box and icon dimensions without inventing defaults. */
function sizeControl(el, cfg = {}) {
  if (!el) return;
  const containerSize = cfg.container_size || cfg.box_size;
  const elementSize = cfg.element_size || cfg.size;
  if (containerSize && containerSize !== 'auto') {
    el.style.setProperty('width', containerSize, 'important');
    el.style.setProperty('height', containerSize, 'important');
  }
  if (elementSize) {
    const icon = el.querySelector('svg') || el.querySelector('i');
    if (icon) {
      icon.style.setProperty('width', elementSize, 'important');
      icon.style.setProperty('height', elementSize, 'important');
    }
  }
}

let _lockedControlPopover = null;
let _lockedControlPopoverCleanup = null;

/** Removes the active lock popover and its resize/scroll listeners. */
function _closeLockedControlPopover() {
  _lockedControlPopoverCleanup?.();
  _lockedControlPopoverCleanup = null;
  _lockedControlPopover?.remove();
  _lockedControlPopover = null;
}

/** Keeps the registration prompt within the viewport near its triggering control. */
function _positionLockedControlPopover(popover, anchor) {
  const rect = anchor.getBoundingClientRect();
  const padding = 8;
  const gap = 8;
  const width = popover.offsetWidth;
  const height = popover.offsetHeight;
  const right = rect.right + gap;
  const left = rect.left - width - gap;
  const preferredLeft = right + width <= window.innerWidth - padding
    ? right
    : (left >= padding ? left : rect.left + (rect.width - width) / 2);
  const x = Math.max(padding, Math.min(preferredLeft, window.innerWidth - width - padding));
  const y = Math.max(padding, Math.min(
    rect.top + (rect.height - height) / 2,
    window.innerHeight - height - padding,
  ));
  popover.style.left = `${x}px`;
  popover.style.top = `${y}px`;
}

/** Replaces the prior lock prompt and routes its CTA to the login overlay. */
export function showLockedControlPopover(anchor, cfg = {}) {
  _closeLockedControlPopover();
  const popover = document.createElement('div');
  popover.className = 'chat-control-lock-popover';
  popover.setAttribute('role', 'dialog');
  const message = document.createElement('div');
  message.className = 'chat-control-lock-popover-message';
  if (cfg.locked_feature) {
    const disabled = document.createElement('div');
    disabled.className = 'chat-control-lock-popover-line';
    const feature = document.createElement('strong');
    feature.textContent = cfg.locked_feature;
    disabled.append(feature, ' feature is disabled for anonymous users');
    const description = document.createElement('div');
    description.className = 'chat-control-lock-popover-line';
    description.textContent = cfg.locked_description || 'Register to unlock the full features of the app.';
    message.append(disabled, description);
  } else {
    message.textContent = cfg.locked_message || 'Register to unlock this feature.';
  }
  popover.appendChild(message);
  const cta = document.createElement('button');
  cta.type = 'button';
  cta.className = 'chat-control-lock-popover-cta';
  cta.textContent = cfg.locked_cta || 'Register';
  cta.addEventListener('click', () => {
    _closeLockedControlPopover();
    showLeftOverlay();
  });
  popover.appendChild(cta);
  document.body.appendChild(popover);
  // Shared resize/scroll callback repositions this singleton prompt.
  const reposition = () => _positionLockedControlPopover(popover, anchor);
  reposition();
  window.addEventListener('resize', reposition);
  document.addEventListener('scroll', reposition, true);
  // Keep listener removal paired with the active popup's lifetime.
  _lockedControlPopoverCleanup = () => {
    window.removeEventListener('resize', reposition);
    document.removeEventListener('scroll', reposition, true);
  };
  _lockedControlPopover = popover;
  setTimeout(() => document.addEventListener('click', _closeLockedControlPopover, { once: true }), 0);
}

/** Keeps a capture-phase lock guard bound once while reading the latest lock config. */
function applyControlLock(el, cfg = {}) {
  if (!el) return;
  el._chatControlLockConfig = cfg;
  const locked = cfg.locked === true;
  el.classList.toggle('tier-locked', locked);
  if (locked) {
    el.setAttribute('aria-disabled', 'true');
    el.title = cfg.locked_feature
      ? `${cfg.locked_feature} feature is disabled for anonymous users.`
      : (cfg.locked_message || 'Register to unlock this feature.');
  } else {
    el.removeAttribute('aria-disabled');
  }
  if (el._chatControlLockBound) return;
  el._chatControlLockBound = true;
  el.addEventListener('click', (event) => {
    const liveCfg = el._chatControlLockConfig || {};
    if (liveCfg.locked !== true) return;
    event.preventDefault();
    event.stopImmediatePropagation();
    showLockedControlPopover(el, liveCfg);
  }, true);
}

/** Sets the pill class matching the configured combined/individual action controls. */
function setButtonMode(pillEl, names) {
  pillEl.classList.remove(...MODE_CLASSES);
  const hasCombined = names.includes('mic_send');
  const hasMic = names.includes('mic');
  const hasSend = names.includes('send');
  if (hasCombined) pillEl.classList.add('chat-pill-mic-send');
  else if (hasMic && hasSend) pillEl.classList.add('chat-pill-distinct-buttons');
  else if (hasMic) pillEl.classList.add('chat-pill-mic-only');
  else if (hasSend) pillEl.classList.add('chat-pill-send-only');
  else pillEl.classList.add('chat-pill-no-action');
}

// Stat types → element lookup. 'ctx' and 'ctx-max' share the same element;
// the renderer picks the presentation (compact number vs current/max) from
// whichever entry is active in the config.
const STATS_TYPE_EL = {
  'token-bar': '#chat-token-bar, .chat-token-bar',
  ctx: '#chat-model-ctx, .chat-model-ctx',
  'ctx-max': '#chat-model-ctx, .chat-model-ctx',
  cost: '#chat-cost, .chat-cost',
};

/**
 * Resolve chat.json → controls.stats.visible into the live set of stat
 * chips. Each entry is either a plain string ('token-bar', 'ctx', 'ctx-max',
 * 'cost') or an object { type, decimals } — decimals overrides the per-type
 * default (ctx: 1 below 1M / 2 above; cost: 2, rounded up).
 *
 * The resolved list [{ type, decimals, el }] is stashed on the stats container
 * as `_statsConfig` so the renderers (chat-activity.js for the panel,
 * chat-surface.js for the widget/embed) can read which types are enabled and
 * what decimals to use. Elements for disabled types are hidden; enabled ones
 * are value-gated by their renderers (hidden while their value is zero).
 */
/** Resolves configured stat types to unique DOM nodes and stores renderer metadata. */
function applyStatsConfig(statsEl, cfg = {}) {
  if (!statsEl || !Array.isArray(cfg.visible)) return;
  const resolved = [];
  const seenEls = new Set();
  for (const raw of cfg.visible) {
    const entry = (typeof raw === 'string') ? { type: raw }
      : (raw && typeof raw === 'object' && typeof raw.type === 'string') ? { ...raw } : null;
    if (!entry) continue;
    const selector = STATS_TYPE_EL[entry.type];
    if (!selector) continue;
    const el = statsEl.querySelector(selector);
    if (!el || seenEls.has(el)) continue;   // drop dupes sharing one element (ctx/ctx-max)
    seenEls.add(el);
    resolved.push({
      type: entry.type,
      decimals: (typeof entry.decimals === 'number' && Number.isFinite(entry.decimals)) ? entry.decimals : null,
      el,
    });
  }
  // Stash on the canonical stats container (the passed element may be the row
  // wrapper; the container is .chat-pill-stats itself).
  const container = statsEl.classList.contains('chat-pill-stats')
    ? statsEl
    : statsEl.querySelector('.chat-pill-stats');
  if (container) container._statsConfig = resolved;
  statsEl._statsConfig = resolved;
  for (const selector of new Set(Object.values(STATS_TYPE_EL))) {
    const el = statsEl.querySelector(selector);
    if (!el) continue;
    const enabled = seenEls.has(el);
    el.hidden = !enabled;
    if (enabled) {
      // Clear any inline display a previous disabled-pass left behind —
      // otherwise the inline 'none' beats the CSS flex rule and the stat
      // never comes back after being re-enabled. Renderers re-hide on zero.
      el.style.display = '';
    } else {
      // Inline display beats the author CSS display rules (e.g.
      // .chat-token-bar { display:flex }) that would otherwise override the
      // hidden attribute. Enabled stats keep their renderer-controlled display.
      el.style.display = 'none';
    }
  }
}

// Per-pill ownership keeps remounts independent and bounds carousel/pending work.
const pillLayouts = new WeakMap();

/** Disconnects owned observers and cancels deferred moves before a panel remount. */
export function disposeChatPillLayout(pillEl) {
  const state = pillLayouts.get(pillEl);
  if (!state) return;
  state.disposed = true;
  clearTimeout(state.timer);
  pillEl.removeEventListener('focusout', state.onBlur);
  for (const zone of state.zones) zone.observer?.disconnect();
  pillLayouts.delete(pillEl);
}

/** Adopts the boot rows so initial focus does not require disconnecting the editor. */
function createPillLayout(els) {
  const pillEl = els.pill;
  let rows = pillEl.querySelector(':scope > .chat-pill-layout-rows');
  if (!rows) {
    rows = document.createElement('div');
    rows.className = 'chat-pill-layout-rows';
    const preview = pillEl.querySelector(':scope > #chat-preview-bar');
    pillEl.insertBefore(rows, preview?.nextSibling || pillEl.firstChild);
  }
  const zones = [];
  for (let index = 0; index < 2; index++) {
    let row = rows.querySelector(`[data-pill-row="${index}"]`);
    if (!row) {
      row = document.createElement('div'); row.className = 'chat-pill-layout-row';
      row.dataset.pillRow = String(index); rows.appendChild(row);
    }
    for (const name of ZONES) {
      let element = row.querySelector(`.chat-pill-layout-zone-${name}`);
      if (!element) {
        element = document.createElement('div');
        element.className = `chat-pill-layout-zone chat-pill-layout-zone-${name}`;
        row.appendChild(element);
      }
      zones.push({element, strip: null, observer: null, update: null});
    }
  }
  const state = {zones, pending: null, timer: null, disposed: false};
  // Natural focus departure applies the latest deferred configuration after other handlers settle.
  state.onBlur = () => {
    clearTimeout(state.timer);
    state.timer = setTimeout(() => {
      const pending = state.pending;
      if (!state.disposed && pending && pillEl.isConnected) applyChatPillLayout(pending.els, pending.pill);
    }, 0);
  };
  pillEl.addEventListener('focusout', state.onBlur);
  pillLayouts.set(pillEl, state);
  return state;
}

/** Creates one carousel owner per zone; retained zones retain listeners and observers. */
function enableCarousel(zone) {
  const element = zone.element;
  element.classList.add('chat-pill-zone-carousel');
  const left = document.createElement('button'), right = document.createElement('button');
  left.type = right.type = 'button';
  left.className = 'chat-pill-zone-chev left'; right.className = 'chat-pill-zone-chev right';
  left.setAttribute('aria-label', 'Scroll left'); right.setAttribute('aria-label', 'Scroll right');
  left.innerHTML = '&#10094;'; right.innerHTML = '&#10095;';
  const strip = document.createElement('div'); strip.className = 'chat-pill-zone-carousel-strip';
  element.append(left, strip, right);
  zone.strip = strip; zone.chevrons = [left, right];
  const update = () => {
    const overflow = strip.scrollWidth - strip.clientWidth > 1;
    element.classList.toggle('has-overflow', overflow);
    left.classList.toggle('visible', overflow && strip.scrollLeft > 1);
    right.classList.toggle('visible', overflow && strip.scrollLeft < strip.scrollWidth - strip.clientWidth - 1);
  };
  // Retained click/scroll listeners read current strip geometry rather than stale layout values.
  const step = () => Math.max(60, Math.floor(strip.clientWidth * .6));
  left.addEventListener('click', () => strip.scrollBy({left: -step(), behavior: 'smooth'}));
  right.addEventListener('click', () => strip.scrollBy({left: step(), behavior: 'smooth'}));
  strip.addEventListener('scroll', update, {passive: true});
  zone.update = update;
  if (typeof ResizeObserver !== 'undefined') {
    zone.observer = new ResizeObserver(update); zone.observer.observe(strip);
  }
}

/** Moves only changed controls; moveBefore preserves focus/selection on connected nodes. */
function placeControl(parent, node, before = null) {
  if (node === before || (node.parentNode === parent && node.nextSibling === before)) return;
  if (parent.moveBefore && parent.isConnected && node.isConnected) parent.moveBefore(node, before);
  else parent.insertBefore(node, before);
}

/** Reconciles configured rows and styles without blur/refocus; unsafe focused moves wait for natural blur. */
export function applyChatPillLayout(els, rawPill = {}) {
  const pill = normalizeChatPill(rawPill);
  const pillEl = els?.pill;
  if (!pillEl) return pill;
  const state = pillLayouts.get(pillEl) || createPillLayout(els);
  const controls = {textarea: els.input, stats: els.stats, mic_send: els.pillButtons,
    mic: els.mic, send: els.send, attach: els.attach, stop: els.stop, continue: els.continue};
  const names = configuredRowControls(pill);
  const desired = new Map();
  const zoneNames = state.zones.map((zone, index) => {
    const cfg = pill.rows[Math.floor(index / 3)][ZONES[index % 3]];
    return (cfg.carousel || cfg.controls).filter(name => names.includes(name) && controls[name]);
  });
  for (let index = 0; index < state.zones.length; index++) {
    for (const name of zoneNames[index]) desired.set(controls[name], index);
  }
  if (names.includes('mic_send')) {
    desired.set(els.mic, 'buttons'); desired.set(els.send, 'buttons');
  }
  const focused = document.activeElement;
  // A focused editor may stay focused after OS dismissal. Never restore focus automatically.
  // Without moveBefore, defer any structural operation that disconnects/hides that editor.
  const unsafeMove = [...new Set(Object.values(controls).filter(Boolean))].some(node => {
    if (node !== focused && !node.contains(focused)) return false;
    const target = desired.get(node);
    if (target === undefined) return true;
    if (target === 'buttons') return node.parentNode !== els.pillButtons;
    const zone = state.zones[target];
    const carousel = Boolean(pill.rows[Math.floor(target / 3)][ZONES[target % 3]].carousel);
    const siblings = zoneNames[target].map(name => controls[name]).filter(el => desired.get(el) === target);
    const next = siblings[siblings.indexOf(node) + 1] || null;
    return node.parentNode !== (carousel ? zone.strip : zone.element) || node.nextSibling !== next;
  });
  const deferStructure = unsafeMove && typeof pillEl.moveBefore !== 'function';
  state.pending = deferStructure ? {els, pill: rawPill} : null;
  pillEl.classList.remove('chat-pill-boot');
  pillEl.classList.add('chat-pill-row-layout');
  for (let index = 0; index < state.zones.length; index++) {
    const zone = state.zones[index], cfg = pill.rows[Math.floor(index / 3)][ZONES[index % 3]];
    zone.element.style.alignItems = cfg.align || '';
    zone.element.style.padding = cfg.padding || '';
    if (!deferStructure && cfg.carousel && !zone.strip) enableCarousel(zone);
  }
  if (!deferStructure) {
    setButtonMode(pillEl, names);
    for (const node of new Set(Object.values(controls).filter(Boolean))) {
      if (!desired.has(node)) {
        if (pillEl.contains(node) && node.parentNode !== pillEl) placeControl(pillEl, node);
        hideElement(node);
      } else showElement(node);
    }
    if (names.includes('mic_send')) {
      placeControl(els.pillButtons, els.mic, els.pillButtons.firstChild);
      placeControl(els.pillButtons, els.send);
    }
    for (let index = 0; index < state.zones.length; index++) {
      const zone = state.zones[index], cfg = pill.rows[Math.floor(index / 3)][ZONES[index % 3]];
      const parent = cfg.carousel ? zone.strip : zone.element;
      // Reverse insertion retains nodes already at their correct position.
      let before = null;
      for (const name of [...zoneNames[index]].reverse()) {
        const node = controls[name];
        if (desired.get(node) !== index) continue;
        placeControl(parent, node, before); before = node;
      }
    }
    for (let index = 0; index < state.zones.length; index++) {
      const zone = state.zones[index], cfg = pill.rows[Math.floor(index / 3)][ZONES[index % 3]];
      if (!cfg.carousel && zone.strip) {
        zone.observer?.disconnect(); zone.observer = null;
        zone.strip.remove(); for (const button of zone.chevrons) button.remove();
        zone.strip = null; zone.update = null;
        zone.element.classList.remove('chat-pill-zone-carousel', 'has-overflow');
      }
      zone.update?.();
    }
  }

  const textareaCfg = pill.controls.textarea || {};
  if (els.input) {
    if (textareaCfg.min_height) {
      els.input.style.minHeight = textareaCfg.min_height;
      els.input.removeAttribute('rows');
    }
    if (textareaCfg.max_height) els.input.style.maxHeight = textareaCfg.max_height;
    if (textareaCfg.font_size) {
      els.input.style.setProperty('--chat-pill-font-size', textareaCfg.font_size);
    }
    // padding is applied as inline-style so it overrides the CSS default
    // (8px 8px 2px 0). Authored in data/config/chat.json.
    if (textareaCfg.padding) els.input.style.padding = textareaCfg.padding;
  }
  applyStatsConfig(els.stats, pill.controls.stats);
  applyControlLock(els.attach, pill.controls.attach);
  sizeControl(els.attach, pill.controls.attach);
  sizeControl(els.stop, pill.controls.stop);
  sizeControl(els.continue, pill.controls.continue);
  sizeControl(els.mic, names.includes('mic_send') ? pill.controls.mic_send : pill.controls.mic);
  sizeControl(els.send, names.includes('mic_send') ? pill.controls.mic_send : pill.controls.send);

  // Clear stale inline display values left by older/static shell versions.
  pillEl.style.removeProperty('display');

  return pill;
}
