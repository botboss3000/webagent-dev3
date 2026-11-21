// Tasks chip: a header session-tab chip for the session task list the agent keeps as it
// works (sections of steps, a plan's checklist first). The list arrives with the session
// snapshot; the chip shows done/total steps and opens a panel where the owner ticks, adds
// and removes steps (each edit reaches the agent with its next step) and reads the History
// of revisions. It also lists the user requests (u7) the list must account for: unfiled
// ones, and set-aside ones with their reasons, which those who can send can bring back.
// Everyone in the chat sees the list; those who can send in it edit.
import {requestTouchVibration} from '../../shared/vibrate/vibrate.js';
import {createExpandList} from '../../components/expand-list/expand-list.js';

const el = (tag, cls, text) => { const n = document.createElement(tag); if (cls) n.className = cls; if (text != null) n.textContent = text; return n; };

/** Requests neither cited by a section or step nor set aside, and the set-aside ones. */
export function requestStates(list, requests) {
  const cited = new Set();
  for (const section of list?.sections || []) {
    for (const id of section.requests || []) cited.add(id);
    for (const step of section.steps || []) for (const id of step.requests || []) cited.add(id);
  }
  const aside = new Map((list?.set_aside || []).map(a => [a.request, a.reason]));
  const unfiled = [], setAside = [];
  for (const item of requests || []) {
    if (cited.has(item.id)) continue;
    if (aside.has(item.id)) setAside.push({...item, reason: aside.get(item.id)});
    else unfiled.push(item);
  }
  return {unfiled, setAside};
}

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

  const fromRead = fresh => ({revision: fresh.revision, list: fresh.list || {sections: []}, requests: fresh.requests || []});
  /** Sends one edit (a section's steps, or `restore` of set-aside requests); a stale revision reloads the list. */
  async function send(body) {
    if (busy || !state) return;
    busy = true;
    try {
      await request(path(), 'POST', {request_id: crypto.randomUUID(), expected_revision: state.revision, ...body});
      state = fromRead(await request(path())); render(); if (popup) open();
    } catch (error) {
      note(error.message);
      try { state = fromRead(await request(path())); render(); if (popup) open(); } catch {}
    } finally { busy = false; }
  }
  const edit = (id, steps) => send({section: {id}, steps});
  function button(cls, text, label, run) {
    const b = el('button', cls, text); b.type = 'button'; if (label) b.setAttribute('aria-label', label);
    b.onclick = e => { e.stopPropagation(); requestTouchVibration(e); run(e); };
    return b;
  }
  const SUB = {open: 'pending', in_progress: 'in-progress', blocked: 'need-help', done: 'completed', cancelled: 'cancelled'};
  /** A section as a expand-list row: its steps become subtasks, outcomes their details. */
  function planTask(section) {
    const steps = section.steps || [];
    const status = section.status === 'done' ? 'completed' : section.status === 'cancelled' ? 'cancelled'
      : steps.some(s => s.status === 'blocked') ? 'need-help'
      : steps.some(s => s.status === 'in_progress' || s.status === 'done') ? 'in-progress' : 'pending';
    const cites = ids => (ids || []).length ? ' (' + ids.join(', ') + ')' : '';
    return {id: section.id, title: section.title, status, section,
      badges: [section.origin === 'user' ? 'Your request' : 'Agent', ...(section.requests || [])],
      description: section.summary, notes: section.notes,
      subtasks: steps.map(step => ({id: step.id, title: step.label + cites(step.requests), status: SUB[step.status] || 'pending', description: step.outcome}))};
  }
  const plan = createExpandList({
    label: 'Session tasks',
    canEdit: task => task.section.status === 'open' && canEdit(),
    onToggle: (task, sub, done) => edit(task.id, [{id: sub.id, status: done ? 'done' : 'open'}]),
    onRemove: (task, sub) => edit(task.id, [{id: sub.id, status: 'cancelled'}]),
    onAdd: (task, label) => { if (label.length < 3) note('A step needs at least 3 characters'); else void edit(task.id, [{label}]); }
  });
  /** A History change line's status, read from its wording ("s2.3 done", "s2 added: …"). */
  const changeStatus = line => /\bdone\b/.test(line) ? 'completed' : /in_progress/.test(line) ? 'in-progress'
    : /\bblocked\b/.test(line) ? 'need-help' : /cancelled|removed/.test(line) ? 'cancelled' : 'pending';
  /** History: each revision a collapsible expand-list row (newest open), its changes as steps. */
  async function historyView(box) {
    try {
      const entries = (await request(path())).history || [];
      if (!entries.length) { box.append(el('p', 'task-history-empty', 'No revisions yet.')); return; }
      const log = createExpandList({label: 'Task list history', defaultOpen: task => task.id === 'r' + entries[0].revision});
      log.update(entries.map(entry => ({
        id: 'r' + entry.revision, title: 'Revision ' + entry.revision, status: 'pending', icon: 'History', pill: false,
        badges: [entry.by === 'user' ? 'You' : 'Agent', entry.updated_at ? new Date(entry.updated_at * 1000).toLocaleString([], {dateStyle: 'short', timeStyle: 'short'}) : ''].filter(Boolean),
        subtasks: (entry.changes || []).map((line, i) => ({id: String(i), title: String(line), status: changeStatus(String(line))}))
      })));
      box.append(log.element);
    } catch (error) { box.append(el('p', 'task-history-empty', error.message)); }
  }
  /** The requests block: unfiled requests, then set-aside ones with their reasons and Undo. */
  function requestsView() {
    const {unfiled, setAside} = requestStates(state?.list, state?.requests);
    if (!unfiled.length && !setAside.length) return null;
    const block = el('section', 'task-requests'); block.setAttribute('aria-label', 'Your requests');
    const row = (item, detail, action) => {
      const line = el('div', 'task-request');
      const text = el('div', 'task-request-text');
      text.append(el('span', 'task-request-id', item.id), el('span', '', ' ' + item.text));
      if (detail) text.append(el('div', 'task-request-detail', detail));
      line.append(text); if (action) line.append(action);
      return line;
    };
    if (unfiled.length) {
      block.append(el('h3', 'task-requests-head', 'Not in the task list'));
      for (const item of unfiled) block.append(row(item, ''));
    }
    if (setAside.length) {
      block.append(el('h3', 'task-requests-head', 'Set aside'));
      for (const item of setAside) {
        const undo = canEdit() ? button('task-request-undo', 'Undo', 'Put ' + item.id + ' back in the task list', () => void send({restore: [item.id]})) : null;
        block.append(row(item, item.reason, undo));
      }
    }
    return block;
  }
  function open() {
    const box = el('div', 'sub-chip-menu task-panel wa-scroll-gutter'); box.setAttribute('role', 'dialog'); box.setAttribute('aria-label', 'Session tasks');
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
      plan.update(sections.map(planTask)); if (sections.length) box.append(plan.element);
      const requests = requestsView(); if (requests) box.append(requests);
    }
    show(box);
  }
  function render() {
    const sections = state?.list?.sections || [];
    const {unfiled, setAside} = requestStates(state?.list, state?.requests);
    if (!sections.length && !unfiled.length && !setAside.length) { chip.remove(); close(); return; }
    const wrap = tabs?.querySelector('#chat-sub-scroll-wrap');
    if (wrap && chip.parentNode !== wrap) wrap.append(chip);
    const {done, total} = taskCounts(state.list);
    const glyph = el('span', 'chip-status'); glyph.innerHTML = '<i data-lucide="list-checks"></i>';
    const meta = el('span', 'tasks-chip-meta', done + '/' + total);
    chip.replaceChildren(glyph, el('span', '', 'Tasks'), meta);
    if (unfiled.length) chip.append(el('span', 'tasks-chip-unfiled', unfiled.length + ' unfiled'));
    chip.title = 'Session tasks: ' + done + ' of ' + total + ' steps done' + (unfiled.length ? ', ' + unfiled.length + ' requests not in the list' : '');
    chip.setAttribute('aria-label', chip.title);
    window.lucide?.createIcons();
  }
  chip.addEventListener('click', e => { e.stopPropagation(); requestTouchVibration(e); if (popup) close(); else { view = 'tasks'; open(); } });
  return {
    /** Takes the session snapshot's task list (`null` when there is none or it is not the caller's). */
    set(value) {
      const next = value && (value.list || value.requests?.length)
        ? {revision: value.revision || 0, list: value.list || {sections: []}, requests: value.requests || []} : null;
      const changed = next?.revision !== state?.revision || next?.requests?.length !== state?.requests?.length;
      state = next; render();
      if (changed && popup && !busy) open();
    },
    disconnect() { state = null; close(); render(); }
  };
}
