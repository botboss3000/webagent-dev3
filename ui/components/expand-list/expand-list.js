// Expand list: the shared, framework-free list behind the chat header chip panels (Tasks,
// History, Broker, Main Notes, Artifacts, Actions). Each row shows a status icon, its title,
// badges and a pill; opening it reveals its description, any custom content and its steps
// on a dashed connector line. A step opens to its details and, when editable, its status
// icon ticks it done or open. Finished rows are marked by icon and pill only, never struck
// through. Expansion state survives update(), and an icon whose status changed plays a short
// pop (none under reduced motion, see CSS).
// Statuses: completed, in-progress, pending, need-help, failed, cancelled. A row's
// `content(host)` adds its own controls; a `lazy` row gets an empty slot its owner fills from
// onExpand, only when opened.
import {requestTouchVibration} from '../../shared/vibrate/vibrate.js';

const ICONS = {completed: 'CircleCheck', 'in-progress': 'CircleDotDashed', 'need-help': 'CircleAlert', failed: 'CircleX', cancelled: 'CircleX', pending: 'Circle'};
const LABELS = {completed: 'Done', 'in-progress': 'In progress', pending: 'Pending', 'need-help': 'Blocked', failed: 'Failed', cancelled: 'Cancelled'};
const finished = status => status === 'completed' || status === 'cancelled';
const el = (tag, cls, text) => { const n = document.createElement(tag); if (cls) n.className = cls; if (text != null) n.textContent = text; return n; };

/** A Lucide status icon, or a plain dot while the icon library has not loaded. */
function statusIcon(status, name) {
  const node = window.lucide?.icons?.[name || ICONS[status] || 'Circle'];
  const icon = node ? window.lucide.createElement(node) : el('span', '', '•');
  icon.setAttribute('aria-hidden', 'true');
  return icon;
}

/**
 * Returns `{element, update(tasks)}`. Each task is `{id, title, status, icon?, pill?, badges?,
 * description?, notes?, content?, lazy?, subtasks: [{id, title, status, description?, tags?, content?}]}`;
 * `content(host)` appends custom DOM (buttons, links, forms) on each render; `icon`
 * is a Lucide icon name replacing the status icon and `pill` replaces the status pill's text
 * (false hides it). Callbacks are optional: `canEdit(task)` enables `onToggle(task, subtask,
 * done)`, `onRemove(task, subtask)` and `onAdd(task, title)`; `defaultOpen(task)` says whether
 * a task starts open (unfinished ones by default); `onExpand(task, open, slot)` runs when a
 * lazy task opens or closes, with its content slot (kept across update(), so fill it once
 * and clear `slot.dataset.loaded` to reload next time); `onHold(task, event)` runs on
 * long-press or right-click of a task row. `label` names the list for assistive technology.
 */
export function createExpandList({canEdit = () => false, onToggle, onRemove, onAdd, defaultOpen = task => !finished(task.status),
  onExpand, onHold, label = 'Tasks'} = {}) {
  const root = el('ul', 'expand-list'); root.setAttribute('aria-label', label);
  const expanded = new Set(), seen = new Set(), openDetails = new Set(), lastStatus = new Map();
  /** Opens or closes in place, so the collapse transitions instead of being rebuilt open. */
  const flip = (set, key, trigger, panel) => {
    const open = !set.has(key); open ? set.add(key) : set.delete(key);
    trigger.setAttribute('aria-expanded', String(open)); panel.dataset.open = String(open);
  };
  const slots = new Map();
  let tasks = [];
  /** Calls onExpand for a lazy task; an open slot loads once until its owner clears `loaded`. */
  function expandLazy(task, open) {
    const slot = slots.get(task.id);
    if (!task.lazy || !slot || !onExpand) return;
    if (open && slot.dataset.loaded) return;
    if (open) slot.dataset.loaded = '1';
    onExpand(task, open, slot);
  }
  /** Long-press (touch) or right-click on `node` runs onHold; the click that ends a hold is swallowed. */
  function holdable(node, task) {
    let timer = 0, held = false;
    node.addEventListener('pointerdown', e => { if (e.button) return; held = false; clearTimeout(timer);
      timer = setTimeout(() => { held = true; requestTouchVibration(); onHold(task, e); }, 500); });
    for (const type of ['pointerup', 'pointerleave', 'pointercancel']) node.addEventListener(type, () => clearTimeout(timer));
    node.addEventListener('contextmenu', e => { e.preventDefault(); clearTimeout(timer); held = true; onHold(task, e); });
    node.addEventListener('click', e => { if (held) { held = false; e.stopImmediatePropagation(); } }, true);
  }

  function iconButton(status, key, title, run, name) {
    const changed = lastStatus.has(key) && lastStatus.get(key) !== status;
    lastStatus.set(key, status);
    const b = el(run ? 'button' : 'span', 'xl-icon'); b.dataset.status = status;
    if (changed) b.classList.add('is-changed');
    b.append(statusIcon(status, name));
    if (run) {
      b.type = 'button'; b.setAttribute('aria-label', title);
      b.onclick = e => { e.stopPropagation(); requestTouchVibration(e); run(); };
    } else b.title = LABELS[status] || status;
    return b;
  }
  function subtaskRow(task, sub, editable, index) {
    const key = task.id + '/' + sub.id, open = openDetails.has(key);
    const row = el('li', 'xl-sub'); row.dataset.status = sub.status;
    if (!lastStatus.has(key)) { row.classList.add('is-new'); row.style.setProperty('--i', index); }
    const line = el('div', 'xl-sub-line');
    const toggle = editable && onToggle && (sub.status !== 'cancelled' && sub.status !== 'failed');
    line.append(iconButton(sub.status, key, (sub.status === 'completed' ? 'Reopen ' : 'Mark done ') + sub.title,
      toggle ? () => onToggle(task, sub, sub.status !== 'completed') : null));
    const hasDetail = !!(sub.description || sub.tags?.length || sub.content || sub.status !== 'completed' && sub.status !== 'pending');
    const name = el(hasDetail ? 'button' : 'span', 'xl-sub-title', sub.title);
    if (hasDetail) {
      name.type = 'button'; name.setAttribute('aria-expanded', String(open));
      name.onclick = e => { e.stopPropagation(); requestTouchVibration(e); flip(openDetails, key, name, row.querySelector('.xl-collapse')); };
    }
    line.append(name);
    if (editable && onRemove && sub.status !== 'cancelled') {
      const remove = el('button', 'xl-remove', '×'); remove.type = 'button'; remove.setAttribute('aria-label', 'Remove ' + sub.title);
      remove.onclick = e => { e.stopPropagation(); requestTouchVibration(e); onRemove(task, sub); };
      line.append(remove);
    }
    if (line.firstChild.classList.contains('is-changed')) row.classList.add('is-changed');
    row.append(line);
    if (hasDetail) {
      const detail = el('div', 'xl-collapse'); detail.dataset.open = String(open);
      const inner = el('div', 'xl-sub-detail');
      if (!['completed', 'pending'].includes(sub.status)) inner.append(el('span', 'xl-pill', LABELS[sub.status] || sub.status));
      if (sub.description) inner.append(el('p', '', sub.description));
      if (sub.tags?.length) { const tags = el('div', 'xl-tags'); for (const t of sub.tags) tags.append(el('span', 'xl-tag', t)); inner.append(tags); }
      sub.content?.(inner);
      detail.append(inner); row.append(detail);
    }
    return row;
  }
  function taskItem(task) {
    if (!seen.has(task.id)) { seen.add(task.id); if (defaultOpen(task)) expanded.add(task.id); }
    const open = expanded.has(task.id), editable = canEdit(task);
    const item = el('li', 'xl-task'); item.dataset.status = task.status; item.dataset.id = task.id;
    const body = el('div', 'xl-collapse');
    const head = el('button', 'xl-head'); head.type = 'button'; head.setAttribute('aria-expanded', String(open));
    head.onclick = e => { e.stopPropagation(); requestTouchVibration(e); flip(expanded, task.id, head, body); expandLazy(task, expanded.has(task.id)); };
    if (onHold) holdable(head, task);
    const chevron = el('span', 'xl-chevron', window.lucide?.icons ? null : '›');
    if (window.lucide?.icons) chevron.append(window.lucide.createElement(window.lucide.icons.ChevronRight));
    const meta = el('span', 'xl-meta');
    for (const b of task.badges || []) meta.append(el('span', 'xl-tag', b));
    if (task.pill !== false) meta.append(el('span', 'xl-pill', task.pill || LABELS[task.status] || task.status));
    head.append(iconButton(task.status, task.id, '', null, task.icon), el('span', 'xl-title', task.title), meta, chevron);
    if (head.firstChild.classList.contains('is-changed')) item.classList.add('is-changed');
    item.append(head);
    body.dataset.open = String(open);
    const inner = el('div', 'xl-body');
    if (task.description) inner.append(el('p', 'xl-description', task.description));
    if (editable && task.notes) inner.append(el('p', 'xl-notes', task.notes));
    if (task.content) { const host = el('div', 'xl-content'); task.content(host); inner.append(host); }
    const subs = el('ul', 'xl-subs');
    (task.subtasks || []).forEach((sub, i) => subs.append(subtaskRow(task, sub, editable, i)));
    if (subs.children.length) inner.append(subs);
    if (task.lazy) {
      if (!slots.has(task.id)) slots.set(task.id, el('div', 'xl-slot'));
      inner.append(slots.get(task.id));
    }
    if (editable && onAdd) {
      const form = el('form', 'xl-add');
      const input = el('input', 'xl-add-input'); input.type = 'text'; input.placeholder = 'Add a step'; input.maxLength = 240;
      input.setAttribute('aria-label', 'Add a step to ' + task.title);
      const add = el('button', 'xl-add-button', 'Add'); add.type = 'submit';
      form.onsubmit = e => { e.preventDefault(); requestTouchVibration(e); onAdd(task, input.value.trim()); };
      form.append(input, add); inner.append(form);
    }
    body.append(inner); item.append(body);
    return item;
  }
  function render() {
    root.replaceChildren(...tasks.map(taskItem));
    for (const task of tasks) if (expanded.has(task.id)) expandLazy(task, true);
  }
  return {
    element: root,
    /** Replaces the tasks, keeping which tasks and subtasks are open. */
    update(next) { tasks = next || []; render(); }
  };
}
