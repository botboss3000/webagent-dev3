// Two-line session row block shared by the chat session dropdown, its trigger and the
// Agents → Sessions title column. Flag labels and colour roles come from the session
// list's `flags` catalog (config/engine/gates/closing_status.json).
import {chipCounts} from '../../elements/activity-group/index.js';

let catalog = new Map();
/** Adopt the flag catalog the session list returned. */
export function setFlagCatalog(flags) {
  if (Array.isArray(flags) && flags.length) catalog = new Map(flags.map(flag => [flag.key, flag]));
}
// Run states that keep a run going; while one holds, the row shows live activity.
const LIVE = ['running', 'queued', 'waiting_approval', 'waiting_gate', 'waiting_external', 'recovery', 'ready', 'resuming'];
/** Whether the session's newest run is still live. */
export const isLive = item => LIVE.includes(item?.run_status);
/** The flag key a row's chip shows: a queued or approval-waiting run, else the undismissed settled flag. */
export function rowFlag(item) {
  if (item?.run_status === 'queued') return 'queued';
  if (item?.run_status === 'waiting_approval') return 'awaiting_approval';
  if (isLive(item)) return '';
  return item?.flag && !item.flag_dismissed ? item.flag : '';
}
/** Formats an age in seconds as 45s, 12m, 3h, 2d, 5w, then a short date. */
export function elapsed(ts) {
  if (!ts) return '';
  const seconds = Math.max(0, Math.floor(Date.now() / 1000 - ts));
  for (const [unit, size, step] of [['s',60,1], ['m',3600,60], ['h',86400,3600], ['d',604800,86400], ['w',2419200,604800]]) {
    if (seconds < size) return Math.floor(seconds / step) + unit;
  }
  return new Date(ts * 1000).toLocaleDateString(undefined, {month:'short', day:'numeric'});
}
/** Formats a running run's duration as 12s, 1m 12s or 1h 4m. */
export function duration(ts) {
  if (!ts) return '';
  const seconds = Math.max(0, Math.floor(Date.now() / 1000 - ts));
  if (seconds < 60) return seconds + 's';
  if (seconds < 3600) return Math.floor(seconds / 60) + 'm ' + (seconds % 60) + 's';
  return Math.floor(seconds / 3600) + 'h ' + Math.floor(seconds % 3600 / 60) + 'm';
}
/** Refresh every ticking time label under `root`, writing only changed text. */
export function tickTimes(root) {
  for (const node of root.querySelectorAll('[data-ts]')) {
    const ts = Number(node.dataset.ts), value = node.dataset.clock === 'run' ? duration(ts) : elapsed(ts);
    if (node.textContent !== value) node.textContent = value;
  }
}
const make = (cls, text = '') => Object.assign(document.createElement('span'), {className:cls, textContent:text});
/** Build an empty two-line block: title and flag on top, note and time below. */
export function createBlock() {
  const block = make('srow-block');
  const top = make('srow-top'), bottom = make('srow-bottom');
  const note = make('srow-note');
  note.append(make('srow-note-text'));
  top.append(make('session-row-title-label'), make('srow-flag'));
  bottom.append(note, make('srow-time'));
  block.append(top, bottom);
  return block;
}
/** Fill a block from one session row; `title` overrides the label text. */
export function fillBlock(block, item, title = item?.title || 'New Session') {
  const label = block.querySelector('.session-row-title-label');
  if (label && label.textContent !== title) label.textContent = title;
  const key = rowFlag(item), spec = catalog.get(key);
  const flag = block.querySelector('.srow-flag');
  flag.hidden = !key;
  flag.textContent = spec?.label || key.replace(/_/g, ' ');
  flag.dataset.tone = spec?.tone || 'neutral';
  flag.title = spec?.tooltip || flag.textContent;
  const live = isLive(item);
  const text = live ? chipCounts(item?.activity_counts || {}) : item?.notification || '';
  const note = block.querySelector('.srow-note-text');
  if (note.textContent !== text) note.textContent = text;
  note.parentElement.title = text;
  const time = block.querySelector('.srow-time');
  const ts = live ? item?.run_started_at : item?.flag_set_at || item?.last_active_at;
  time.dataset.clock = live ? 'run' : 'since';
  if (ts) time.dataset.ts = ts; else delete time.dataset.ts;
  time.textContent = !ts ? '' : live ? duration(ts) : elapsed(ts);
  time.title = live ? 'Run time' : ts ? (item?.flag_set_at ? 'Flag set ' : 'Last active ') + new Date(ts * 1000).toLocaleString() : '';
  block.classList.toggle('srow-live', live);
  return block;
}
/** One block's HTML, for renderers that build rows as strings. */
export const blockHTML = (item, title) => fillBlock(createBlock(), item, title).outerHTML;
/** The Mark Dismissed / Mark Active session update for a row with a settled flag, or null. */
export function flagToggle(item) {
  if (!item?.flag || !item.flag_run_id || isLive(item)) return null;
  const dismissed = !!item.flag_dismissed;
  return {title:dismissed ? 'Mark Active' : 'Mark Dismissed', glyph:dismissed ? 'bell-ring' : 'bell-off',
    changes:{flag_dismissed:!dismissed, flag_run_id:item.flag_run_id}};
}
/** Slide an overflowing note to its end and back once (desktop hover). */
function reveal(note) {
  const text = note.querySelector('.srow-note-text');
  const shift = note.clientWidth - text.scrollWidth;
  if (shift >= 0 || matchMedia('(prefers-reduced-motion: reduce)').matches) return;
  text.style.setProperty('--srow-shift', shift + 'px');
  text.classList.remove('srow-reveal'); void text.offsetWidth; text.classList.add('srow-reveal');
  text.addEventListener('animationend', () => text.classList.remove('srow-reveal'), {once:true});
}
/** Hover reveal and sideways drag scrolling for every note under `root`. A sideways drag
 *  never opens the row or its long-press menu; a vertical drag stays a list scroll. */
export function bindNotes(root, signal) {
  let drag = null;
  root.addEventListener('pointerover', event => {
    const note = event.target.closest?.('.srow-note');
    if (note && event.pointerType === 'mouse' && !note.contains(event.relatedTarget)) reveal(note);
  }, {signal});
  root.addEventListener('pointerdown', event => {
    const note = event.target.closest?.('.srow-note'); if (!note) return;
    const text = note.querySelector('.srow-note-text');
    drag = {note, text, id:event.pointerId, x:event.clientX, y:event.clientY, max:Math.min(0, note.clientWidth - text.scrollWidth), active:false};
  }, {signal});
  root.addEventListener('pointermove', event => {
    if (!drag || event.pointerId !== drag.id) return;
    const dx = event.clientX - drag.x, dy = event.clientY - drag.y;
    if (!drag.active) {
      if (Math.abs(dy) > 8 && Math.abs(dy) >= Math.abs(dx)) { drag = null; return; }
      if (Math.abs(dx) < 8 || drag.max === 0) return;
      drag.active = true; drag.text.classList.remove('srow-reveal'); drag.text.classList.add('srow-dragging');
    }
    drag.text.style.transform = `translateX(${Math.max(drag.max, Math.min(0, dx))}px)`;
  }, {signal});
  const end = event => {
    if (!drag || event.pointerId !== drag.id) return;
    const {text, active} = drag; drag = null;
    if (!active) return;
    text.classList.remove('srow-dragging'); text.style.transform = '';
    // The release after a drag is not a row click; a later click is left alone.
    const until = Date.now() + 400;
    root.addEventListener('click', stop => { if (Date.now() < until) { stop.preventDefault(); stop.stopImmediatePropagation(); } }, {capture:true, once:true, signal});
  };
  root.addEventListener('pointerup', end, {signal});
  root.addEventListener('pointercancel', end, {signal});
}
