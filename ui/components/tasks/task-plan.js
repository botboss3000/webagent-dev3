// Task plan: a reusable, framework-free list of tasks with expandable subtasks. Each task
// row shows a status icon, its title, badges and a status pill; opening it reveals its
// description and subtasks on a dashed connector line. A subtask opens to its details and,
// when editable, its status icon ticks it done or open. Expansion state survives update(),
// and an icon whose status changed plays a short pop (none under reduced motion, see CSS).
// Statuses: completed, in-progress, pending, need-help, failed, cancelled.
import {requestTouchVibration} from '../../shared/vibrate/vibrate.js';

const ICONS = {completed: 'CircleCheck', 'in-progress': 'CircleDotDashed', 'need-help': 'CircleAlert', failed: 'CircleX', cancelled: 'CircleX', pending: 'Circle'};
const LABELS = {completed: 'Done', 'in-progress': 'In progress', pending: 'Pending', 'need-help': 'Blocked', failed: 'Failed', cancelled: 'Cancelled'};
const finished = status => status === 'completed' || status === 'cancelled';
const el = (tag, cls, text) => { const n = document.createElement(tag); if (cls) n.className = cls; if (text != null) n.textContent = text; return n; };

/** A Lucide status icon, or a plain dot while the icon library has not loaded. */
function statusIcon(status) {
  const node = window.lucide?.icons?.[ICONS[status] || 'Circle'];
  const icon = node ? window.lucide.createElement(node) : el('span', '', '•');
  icon.setAttribute('aria-hidden', 'true');
  return icon;
}

/**
 * Returns `{element, update(tasks)}`. Each task is `{id, title, status, badges?, description?,
 * notes?, subtasks: [{id, title, status, description?, tags?}]}`. Callbacks are optional:
 * `canEdit(task)` enables `onToggle(task, subtask, done)`, `onRemove(task, subtask)` and
 * `onAdd(task, title)`; `label` names the list for assistive technology.
 */
export function createTaskPlan({canEdit = () => false, onToggle, onRemove, onAdd, label = 'Tasks'} = {}) {
  const root = el('ul', 'task-plan'); root.setAttribute('aria-label', label);
  const expanded = new Set(), seen = new Set(), openDetails = new Set(), lastStatus = new Map();
  /** Opens or closes in place, so the collapse transitions instead of being rebuilt open. */
  const flip = (set, key, trigger, panel) => {
    const open = !set.has(key); open ? set.add(key) : set.delete(key);
    trigger.setAttribute('aria-expanded', String(open)); panel.dataset.open = String(open);
  };
  let tasks = [];

  function iconButton(status, key, title, run) {
    const changed = lastStatus.has(key) && lastStatus.get(key) !== status;
    lastStatus.set(key, status);
    const b = el(run ? 'button' : 'span', 'plan-icon'); b.dataset.status = status;
    if (changed) b.classList.add('is-changed');
    b.append(statusIcon(status));
    if (run) {
      b.type = 'button'; b.setAttribute('aria-label', title);
      b.onclick = e => { e.stopPropagation(); requestTouchVibration(e); run(); };
    } else b.title = LABELS[status] || status;
    return b;
  }
  function subtaskRow(task, sub, editable, index) {
    const key = task.id + '/' + sub.id, open = openDetails.has(key);
    const row = el('li', 'plan-sub'); row.dataset.status = sub.status;
    if (!lastStatus.has(key)) { row.classList.add('is-new'); row.style.setProperty('--i', index); }
    const line = el('div', 'plan-sub-line');
    const toggle = editable && onToggle && (sub.status !== 'cancelled' && sub.status !== 'failed');
    line.append(iconButton(sub.status, key, (sub.status === 'completed' ? 'Reopen ' : 'Mark done ') + sub.title,
      toggle ? () => onToggle(task, sub, sub.status !== 'completed') : null));
    const hasDetail = !!(sub.description || sub.tags?.length || sub.status !== 'completed' && sub.status !== 'pending');
    const name = el(hasDetail ? 'button' : 'span', 'plan-sub-title', sub.title);
    if (hasDetail) {
      name.type = 'button'; name.setAttribute('aria-expanded', String(open));
      name.onclick = e => { e.stopPropagation(); requestTouchVibration(e); flip(openDetails, key, name, row.querySelector('.plan-collapse')); };
    }
    line.append(name);
    if (editable && onRemove && sub.status !== 'cancelled') {
      const remove = el('button', 'plan-remove', '×'); remove.type = 'button'; remove.setAttribute('aria-label', 'Remove ' + sub.title);
      remove.onclick = e => { e.stopPropagation(); requestTouchVibration(e); onRemove(task, sub); };
      line.append(remove);
    }
    if (line.firstChild.classList.contains('is-changed')) row.classList.add('is-changed');
    row.append(line);
    if (hasDetail) {
      const detail = el('div', 'plan-collapse'); detail.dataset.open = String(open);
      const inner = el('div', 'plan-sub-detail');
      if (!['completed', 'pending'].includes(sub.status)) inner.append(el('span', 'plan-pill', LABELS[sub.status] || sub.status));
      if (sub.description) inner.append(el('p', '', sub.description));
      if (sub.tags?.length) { const tags = el('div', 'plan-tags'); for (const t of sub.tags) tags.append(el('span', 'plan-tag', t)); inner.append(tags); }
      detail.append(inner); row.append(detail);
    }
    return row;
  }
  function taskItem(task) {
    if (!seen.has(task.id)) { seen.add(task.id); if (!finished(task.status)) expanded.add(task.id); }
    const open = expanded.has(task.id), editable = canEdit(task);
    const item = el('li', 'plan-task'); item.dataset.status = task.status;
    const body = el('div', 'plan-collapse');
    const head = el('button', 'plan-head'); head.type = 'button'; head.setAttribute('aria-expanded', String(open));
    head.onclick = e => { e.stopPropagation(); requestTouchVibration(e); flip(expanded, task.id, head, body); };
    const chevron = el('span', 'plan-chevron', window.lucide?.icons ? null : '›');
    if (window.lucide?.icons) chevron.append(window.lucide.createElement(window.lucide.icons.ChevronRight));
    const meta = el('span', 'plan-meta');
    for (const b of task.badges || []) meta.append(el('span', 'plan-tag', b));
    meta.append(el('span', 'plan-pill', LABELS[task.status] || task.status));
    head.append(iconButton(task.status, task.id, '', null), el('span', 'plan-title', task.title), meta, chevron);
    if (head.firstChild.classList.contains('is-changed')) item.classList.add('is-changed');
    item.append(head);
    body.dataset.open = String(open);
    const inner = el('div', 'plan-body');
    if (task.description) inner.append(el('p', 'plan-description', task.description));
    if (editable && task.notes) inner.append(el('p', 'plan-notes', task.notes));
    const subs = el('ul', 'plan-subs');
    (task.subtasks || []).forEach((sub, i) => subs.append(subtaskRow(task, sub, editable, i)));
    if (subs.children.length) inner.append(subs);
    if (editable && onAdd) {
      const form = el('form', 'plan-add');
      const input = el('input', 'plan-add-input'); input.type = 'text'; input.placeholder = 'Add a step'; input.maxLength = 240;
      input.setAttribute('aria-label', 'Add a step to ' + task.title);
      const add = el('button', 'plan-add-button', 'Add'); add.type = 'submit';
      form.onsubmit = e => { e.preventDefault(); requestTouchVibration(e); onAdd(task, input.value.trim()); };
      form.append(input, add); inner.append(form);
    }
    body.append(inner); item.append(body);
    return item;
  }
  function render() { root.replaceChildren(...tasks.map(taskItem)); }
  return {
    element: root,
    /** Replaces the tasks, keeping which tasks and subtasks are open. */
    update(next) { tasks = next || []; render(); }
  };
}
