// Tasks chip: a header session-tab chip for the session task list the agent keeps as it
// works (sections of steps, a plan's checklist first). The list arrives with the session
// snapshot; the chip shows done/total steps and opens a panel where the owner ticks, adds
// and removes steps (each edit reaches the agent with its next step) and reads the History
// of revisions. Everyone in the chat sees the list; those who can send in it edit.
import {requestTouchVibration} from '../../shared/vibrate/vibrate.js';
import {createTaskPlan} from '../../components/tasks/task-plan.js';

const el = (tag, cls, text) => { const n = document.createElement(tag); if (cls) n.className = cls; if (text != null) n.textContent = text; return n; };

/** Done and total steps over every section, cancelled steps left out. */
export function taskCounts(list) {
  let done = 0, total = 0;
  for (const section of list?.sections || []) {
    for (const step of section.steps || []) {
      if (step.status === 'cancelled') continue;
      total += 1; if (step.status === 'done') done += 1;
    }
  }
  return {done, total};
}

/**
 * Mounts the chip into the header session-tabs row and returns its session lifecycle.
 * `session()` names the open chat, `request(path, method, body)` calls the API and
 * `note(text)` reports an error; `canEdit()` says whether the caller may send in the chat
 * (others see the list read-only).
 */
export function createTasksChip({session = () => '', request, note = () => {}, canEdit = () => true} = {}) {
  const tabs = document.getElementById('chat-sub-header');
  const chip = el('button', 'sub-chip chip-tasks'); chip.type = 'button'; chip.id = 'chat-tasks-chip';
  chip.setAttribute('aria-haspopup', 'dialog'); chip.setAttribute('aria-expanded', 'false');
  let state = null, popup = null, view = 'tasks', busy = false;
  const path = () => '/sessions/' + encodeURIComponent(session()) + '/tasks';

  function close() { popup?.remove(); popup = null; chip.setAttribute('aria-expanded', 'false'); }
  /** Places a body-mounted popup under the chip; phone widths get a full-width panel (CSS). */
  function show(node) {
    popup?.remove(); popup = node; document.body.append(node); chip.setAttribute('aria-expanded', 'true');
    const r = chip.getBoundingClientRect();
    node.style.top = (r.bottom + 4) + 'px';
    if (!matchMedia('(max-width: 600px)').matches) node.style.left = Math.max(4, Math.min(r.left, innerWidth - node.offsetWidth - 8)) + 'px';
  }
  document.addEventListener('pointerdown', e => { if (popup && !popup.contains(e.target) && !chip.contains(e.target)) close(); }, true);

  /** Sends one edit of section `id`; a stale revision reloads the list. */
  async function edit(id, steps) {
    if (busy || !state) return;
    busy = true;
    try {
      await request(path(), 'POST', {request_id: crypto.randomUUID(), expected_revision: state.revision, section: {id}, steps});
      const fresh = await request(path());
      state = {revision: fresh.revision, list: fresh.list}; render(); if (popup) open();
    } catch (error) {
      note(error.message);
      try { const fresh = await request(path()); state = {revision: fresh.revision, list: fresh.list}; render(); if (popup) open(); } catch {}
    } finally { busy = false; }
  }
  function button(cls, text, label, run) {
    const b = el('button', cls, text); b.type = 'button'; if (label) b.setAttribute('aria-label', label);
    b.onclick = e => { e.stopPropagation(); requestTouchVibration(e); run(e); };
    return b;
  }
  const SUB = {open: 'pending', in_progress: 'in-progress', blocked: 'need-help', done: 'completed', cancelled: 'cancelled'};
  /** A section as a task-plan task: its steps become subtasks, outcomes their details. */
  function planTask(section) {
    const steps = section.steps || [];
    const status = section.status === 'done' ? 'completed' : section.status === 'cancelled' ? 'cancelled'
      : steps.some(s => s.status === 'blocked') ? 'need-help'
      : steps.some(s => s.status === 'in_progress' || s.status === 'done') ? 'in-progress' : 'pending';
    return {id: section.id, title: section.title, status, section, badges: [section.origin === 'user' ? 'Your request' : 'Agent'],
      description: section.summary, notes: section.notes,
      subtasks: steps.map(step => ({id: step.id, title: step.label, status: SUB[step.status] || 'pending', description: step.outcome}))};
  }
  const plan = createTaskPlan({
    label: 'Session tasks',
    canEdit: task => task.section.status === 'open' && canEdit(),
    onToggle: (task, sub, done) => edit(task.id, [{id: sub.id, status: done ? 'done' : 'open'}]),
    onRemove: (task, sub) => edit(task.id, [{id: sub.id, status: 'cancelled'}]),
    onAdd: (task, label) => { if (label.length < 3) note('A step needs at least 3 characters'); else void edit(task.id, [{label}]); }
  });
  async function historyView(box) {
    const list = el('ol', 'task-history');
    box.append(list);
    try {
      const data = await request(path());
      for (const entry of data.history || []) {
        const item = el('li', 'task-history-entry');
        const when = entry.updated_at ? new Date(entry.updated_at * 1000).toLocaleString([], {dateStyle: 'short', timeStyle: 'short'}) : '';
        item.append(el('small', 'task-history-meta', ['Revision ' + entry.revision, entry.by === 'user' ? 'You' : 'Agent', when].filter(Boolean).join(' · ')));
        const lines = el('ul', 'task-history-changes');
        for (const line of entry.changes || []) lines.append(el('li', '', String(line)));
        item.append(lines); list.append(item);
      }
      if (!list.children.length) list.append(el('li', 'task-history-empty', 'No revisions yet.'));
    } catch (error) { list.append(el('li', 'task-history-empty', error.message)); }
  }
  function open() {
    const box = el('div', 'sub-chip-menu task-panel'); box.setAttribute('role', 'dialog'); box.setAttribute('aria-label', 'Session tasks');
    const switcher = el('div', 'task-tabs'); switcher.setAttribute('role', 'tablist');
    for (const [id, label] of [['tasks', 'Tasks'], ['history', 'History']]) {
      const tab = button('task-tab', label, '', () => { view = id; open(); });
      tab.setAttribute('role', 'tab'); tab.setAttribute('aria-selected', String(view === id));
      switcher.append(tab);
    }
    box.append(switcher);
    if (view === 'history') void historyView(box);
    else {
      const sections = [...(state?.list?.sections || [])].sort((a, b) => (a.status === 'open' ? 0 : 1) - (b.status === 'open' ? 0 : 1));
      plan.update(sections.map(planTask)); box.append(plan.element);
    }
    show(box);
  }
  function render() {
    const sections = state?.list?.sections || [];
    if (!sections.length) { chip.remove(); close(); return; }
    const wrap = tabs?.querySelector('#chat-sub-scroll-wrap');
    if (wrap && chip.parentNode !== wrap) wrap.append(chip);
    const {done, total} = taskCounts(state.list);
    const glyph = el('span', 'chip-status'); glyph.innerHTML = '<i data-lucide="list-checks"></i>';
    chip.replaceChildren(glyph, el('span', '', 'Tasks'), el('span', 'tasks-chip-meta', done + '/' + total));
    chip.title = 'Session tasks: ' + done + ' of ' + total + ' steps done';
    chip.setAttribute('aria-label', chip.title);
    window.lucide?.createIcons();
  }
  chip.addEventListener('click', e => { e.stopPropagation(); requestTouchVibration(e); if (popup) close(); else { view = 'tasks'; open(); } });
  return {
    /** Takes the session snapshot's task list (`null` when there is none or it is not the caller's). */
    set(value) {
      const next = value && value.list ? {revision: value.revision || 0, list: value.list} : null;
      const changed = next?.revision !== state?.revision;
      state = next; render();
      if (changed && popup && !busy) open();
    },
    disconnect() { state = null; close(); render(); }
  };
}
