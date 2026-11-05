// Broker chip: a header session-tab chip (beside Spawns) for the broker tasks this chat's
// builds and checks ran. The engine records each job's stage snapshot while it follows
// the job (crates/modules/engine/src/runtime/build_tasks.rs); the chip loads the task list
// on session open and on build events, and a task's card loads only when opened. While a
// card is open its counters tick in the browser and it re-reads the task every 15 s.
import {requestTouchVibration} from '../../shared/vibrate.js';

const ACTIVE = ['queued','running','needs_decision'];
const FAILED = ['failed','stale','cancelled','uncertain','interrupted','needs_decision'];
const PAGE = 10, REFRESH_MS = 15000, HOLD_MS = 500, STALE_SECONDS = 60;

/** Minutes with one decimal, counted in whole tenths (0.1 every 6 s). */
export function minutes(seconds) {
  return (Math.floor(Math.max(0, seconds || 0) / 6) / 10).toFixed(1);
}

/** The chip or row icon for a task row: spinner, check, error or warning. */
export function taskIcon(task) {
  if (task?.warning) return 'warning';
  if (FAILED.includes(task?.status)) return 'error';
  if (ACTIVE.includes(task?.status)) return 'running';
  return task?.status === 'completed' ? 'done' : 'running';
}

/** Seconds a running counter shows now: the snapshot's value plus time since it was read. */
export function liveSeconds(base, snapshotAt, serverOffset, nowMs = Date.now()) {
  // A browser clock behind the snapshot never counts backwards.
  return (base || 0) + Math.max(0, nowMs / 1000 + serverOffset - (snapshotAt || 0));
}

/** A running stage's verdict at `seconds`, flipping to slow as soon as it passes p90. */
export function stageVerdict(stage, seconds) {
  const [p50, p90] = stage?.normal || [];
  if (p90 == null) return 'no baseline';
  return seconds > p90 ? 'slow' : seconds > p50 ? 'slower than median' : 'on track';
}

/** Time left, counting down from the snapshot; past 0 the job is past its normal range. */
export function remaining(range, elapsedSinceSnapshot) {
  if (!Array.isArray(range)) return null;
  const [low, high] = range.map(value => value - elapsedSinceSnapshot);
  return high <= 0 ? 'past normal' : minutes(Math.max(0, low)) + '–' + minutes(high) + ' min left';
}

const ICONS = {running:'loader-circle', done:'circle-check', error:'circle-alert', warning:'triangle-alert'};
const LABELS = {queued:'Queued', running:'Running', needs_decision:'Needs a decision', completed:'Completed', failed:'Failed',
  stale:'Stale', cancelled:'Cancelled', uncertain:'Uncertain — needs reconciliation', interrupted:'Interrupted'};

const el = (tag, cls, text) => { const n = document.createElement(tag); if (cls) n.className = cls; if (text != null) n.textContent = text; return n; };
const icon = state => { const s = el('span', 'chip-status broker-icon broker-' + state); s.innerHTML = `<i data-lucide="${ICONS[state]}"></i>`; return s; };
const draw = () => window.lucide?.createIcons();

// Long-press (touch) or right-click opens a context menu; a plain click does `tap`.
function pressable(node, tap, hold) {
  let timer = null, held = false;
  node.addEventListener('pointerdown', e => { if (e.button) return; held = false; clearTimeout(timer);
    timer = setTimeout(() => { held = true; try { requestTouchVibration(); } catch {} hold(e); }, HOLD_MS); });
  for (const type of ['pointerup', 'pointerleave', 'pointercancel']) node.addEventListener(type, () => clearTimeout(timer));
  node.addEventListener('contextmenu', e => { e.preventDefault(); clearTimeout(timer); held = true; hold(e); });
  node.addEventListener('click', e => { e.stopPropagation(); if (held) { held = false; return; } tap(e); });
}

function stageRows(snapshot, age, slowNow) {
  const table = el('div', 'broker-stages'); table.setAttribute('role', 'table');
  const head = el('div', 'broker-stage broker-stage-head'); head.setAttribute('role', 'row');
  for (const label of ['#', 'Stage', 'Time', 'Normal (min)', 'Status']) head.append(el('span', '', label));
  table.append(head);
  const rows = [...(snapshot.finished_stages || []).map(s => [s, 'done']), ...(snapshot.running_stages || []).map(s => [s, 'running']), ...(snapshot.upcoming_stages || []).map(s => [s, 'upcoming'])];
  for (const [stage, state] of rows) {
    const row = el('div', 'broker-stage broker-stage-' + state); row.setAttribute('role', 'row');
    const time = el('span', 'broker-time');
    if (state === 'running') { time.dataset.base = stage.seconds || 0; slowNow.push([stage, row]); }
    time.textContent = state === 'upcoming' ? '—' : minutes(state === 'running' ? (stage.seconds || 0) + age : stage.seconds) + ' min';
    const normal = stage.normal ? minutes(stage.normal[0]) + '–' + minutes(stage.normal[1]) : (stage.samples ? '' : 'no history');
    row.append(el('span', 'broker-id', stage.id || ''), el('span', 'broker-name', stage.stage), time, el('span', 'broker-normal', normal),
      el('span', 'broker-state', state === 'running' ? stageVerdict(stage, (stage.seconds || 0) + age) : state));
    table.append(row);
  }
  return table;
}

/** One task's card: its newest build (or newest job) with stages and Background, and the
 * other jobs under Advanced. `hold(job)` opens a job's menu on long-press; `pageLink` adds a
 * link to the task's Broker card page (ui/broker-card.html). Returns the
 * element, a `tick` that recomputes the counters, and whether the job is still active. */
export function renderCard(task, detail, hold = () => {}, {pageLink = false} = {}) {
  const box = el('div', 'sub-chip-menu broker-card'); box.setAttribute('role', 'dialog'); box.setAttribute('aria-label', 'Broker task ' + (task.name || task.task_id));
  const jobs = detail.task.jobs || [];
  const main = jobs.find(job => job.kind === 'build') || jobs[0];
  const checks = jobs.filter(job => job !== main);
  const offset = (detail.now || Date.now() / 1000) - Date.now() / 1000;
  const head = el('div', 'broker-card-head');
  head.append(icon(taskIcon(detail.task)), el('strong', 'broker-card-title', task.name || task.task_id));
  box.append(head);
  if (!main) { box.append(el('p', 'broker-note', 'No job yet.')); return {box, tick() {}}; }
  const snapshot = main.snapshot || {}, live = ACTIVE.includes(main.status);
  const age = live ? liveSeconds(0, main.snapshot_at, offset) : 0;
  const job = el('div', 'broker-job');
  const title = el('div', 'broker-job-title', (main.kind === 'check' ? 'Check ' + (main.check || '') : 'Build') + ' ' + main.job_id.slice(0, 8) + ' · ' + (LABELS[main.status] || main.status));
  pressable(title, () => {}, () => hold(main));
  const line = el('div', 'broker-summary');
  job.append(title, line);
  if (detail.task.warning === 'broker_unresponsive') job.append(el('p', 'broker-warning', 'Broker not responding — the job’s state is unknown.'));
  else if (detail.task.warning === 'stalled') job.append(el('p', 'broker-warning', 'Stalled — no progress for ' + minutes(snapshot.idle_seconds) + ' min.'));
  const behind = (snapshot.queued_behind || []).map(o => o.task_id + (o.session ? ' (session ' + o.session + ')' : ''));
  if (main.status === 'queued' && behind.length) job.append(el('p', 'broker-note', 'Queued behind ' + behind.join(', ')));
  const slow = [];
  if (snapshot.finished_stages || snapshot.running_stages) job.append(stageRows(snapshot, age, slow));
  if ((snapshot.background || []).length) {
    const bg = el('div', 'broker-background'); bg.append(el('strong', '', 'Background'));
    for (const work of snapshot.background) bg.append(el('div', '', work.name + ' · ' + (work.status || 'running') + ' · ' + minutes(work.seconds) + ' min' + (work.result ? ' (' + work.result + ')' : '')));
    job.append(bg);
  }
  const stale = el('p', 'broker-stale'); job.append(stale);
  box.append(job);
  if (checks.length) {
    const advanced = el('details', 'broker-advanced'); advanced.append(el('summary', '', 'Advanced · ' + checks.length + ' more job' + (checks.length === 1 ? '' : 's')));
    for (const other of checks) {
      const row = el('div', 'broker-check');
      const seconds = other.snapshot?.job_seconds;
      row.append(icon(taskIcon(other)), el('span', '', (other.kind === 'check' ? (other.check || 'check') : 'build') + ' ' + other.job_id.slice(0, 8)),
        el('span', 'broker-normal', (LABELS[other.status] || other.status) + (seconds != null ? ' · ' + minutes(seconds) + ' min' : '')));
      pressable(row, () => {}, () => hold(other));
      advanced.append(row);
    }
    box.append(advanced);
  }
  if (pageLink) {
    const link = el('a', 'broker-page-link', 'Task progress page');
    link.href = '/v3-ui/broker-card.html?task_id=' + encodeURIComponent(task.task_id);
    box.append(link);
  }
  // Recomputes every counter from the snapshot and the time since it was read.
  function tick() {
    const since = live ? liveSeconds(0, main.snapshot_at, offset) : 0;
    const parts = [];
    if (snapshot.job_seconds != null) parts.push('Elapsed ' + minutes(snapshot.job_seconds + since) + ' min');
    if (snapshot.job_normal) parts.push('normal ' + minutes(snapshot.job_normal[0]) + '–' + minutes(snapshot.job_normal[1]));
    const left = live ? remaining(snapshot.remaining_seconds, since) : null; if (left) parts.push(left);
    line.textContent = parts.join(' · ');
    for (const [stage, row] of slow) {
      const seconds = (stage.seconds || 0) + since;
      row.querySelector('.broker-time').textContent = minutes(seconds) + ' min';
      const verdict = stageVerdict(stage, seconds);
      row.querySelector('.broker-state').textContent = verdict;
      row.classList.toggle('broker-slow', verdict === 'slow');
    }
    stale.textContent = live && since > STALE_SECONDS ? 'Updated ' + minutes(since) + ' min ago' : '';
  }
  tick();
  return {box, tick, live};
}

/** Mounts the chip into the header session-tabs row and returns its session lifecycle. */
export function createBrokerChip({request, note = () => {}, family = () => []}) {
  const tabs = document.getElementById('chat-sub-header');
  const chip = document.createElement('button');
  chip.type = 'button'; chip.id = 'chat-broker-chip'; chip.className = 'sub-chip chip-broker';
  chip.setAttribute('aria-haspopup', 'dialog'); chip.setAttribute('aria-expanded', 'false');
  let sid = '', tasks = [], popup = null, cardTimer = null, tickTimer = null, epoch = 0, pending = null;

  const path = (id, rest = '') => '/sessions/' + encodeURIComponent(id) + '/build-tasks' + rest;

  function close() {
    clearInterval(cardTimer); clearInterval(tickTimer); cardTimer = tickTimer = null;
    popup?.remove(); popup = null; chip.setAttribute('aria-expanded', 'false');
  }
  /** Places a body-mounted popup under the chip; phone widths get a full-width panel (CSS). */
  function show(node) {
    close(); popup = node; document.body.append(node); chip.setAttribute('aria-expanded', 'true');
    const r = chip.getBoundingClientRect();
    node.style.top = (r.bottom + 4) + 'px';
    if (!matchMedia('(max-width: 600px)').matches) node.style.left = Math.max(4, Math.min(r.left, innerWidth - node.offsetWidth - 8)) + 'px';
    draw();
    const outside = e => { if (!popup || popup.contains(e.target) || chip.contains(e.target)) return; document.removeEventListener('pointerdown', outside, true); close(); };
    document.addEventListener('pointerdown', outside, true);
  }


  function renderChip() {
    tabs?.toggleAttribute('data-broker', tasks.length > 0);
    if (!tasks.length) { chip.remove(); close(); return; }
    const wrap = tabs?.querySelector('#chat-sub-scroll-wrap');
    if (wrap && chip.parentNode !== wrap) wrap.append(chip);
    const state = taskIcon(tasks[0]);
    chip.replaceChildren(icon(state), document.createTextNode('Broker'));
    chip.title = 'Broker: ' + (tasks[0].name || tasks[0].task_id) + ' — ' + (LABELS[tasks[0].status] || tasks[0].status || 'waiting');
    chip.setAttribute('aria-label', chip.title);
    draw();
  }

  async function refresh() {
    if (!sid) return;
    const turn = ++epoch, ids = [{id: sid, label: ''}, ...family(sid)];
    try {
      const lists = await Promise.all(ids.map(({id, label}) => request(path(id)).then(data => (data.tasks || []).map(task => ({...task, sid: id, spawn: label}))).catch(() => [])));
      if (turn !== epoch) return;
      tasks = lists.flat().sort((a, b) => (b.updated_at || 0) - (a.updated_at || 0));
      renderChip();
    } catch (error) { note('Broker tasks unavailable: ' + error.message); }
  }

  // Coalesce bursts of build events into one read.
  function soon() { clearTimeout(pending); pending = setTimeout(() => void refresh(), 500); }

  function menu(items) {
    const box = el('div', 'sub-chip-menu broker-menu'); box.setAttribute('role', 'menu');
    for (const {label, glyph, run, confirm} of items) {
      const row = el('button', 'family-menu-item'); row.type = 'button'; row.setAttribute('role', 'menuitem');
      row.innerHTML = `<span class="chip-status"><i data-lucide="${glyph}"></i></span>`;
      const text = el('span', 'family-menu-text', label); row.append(text);
      let armed = false;
      row.onclick = async e => {
        e.stopPropagation();
        // Hard-to-undo actions ask once more on the same row.
        if (confirm && !armed) { armed = true; text.textContent = confirm; row.classList.add('broker-confirm'); return; }
        row.disabled = true;
        try { await run(); } catch (error) { note(error.message); } finally { close(); }
      };
      box.append(row);
    }
    show(box);
  }

  async function taskMenu(task) {
    let detail = null;
    try { detail = await request(path(task.sid, '/' + encodeURIComponent(task.task_id))); } catch (error) { note(error.message); return; }
    const jobs = detail.task.jobs || [], running = jobs.some(job => ACTIVE.includes(job.status));
    const items = [];
    if (detail.can_control && !detail.task.abandoned && jobs[0]?.status !== 'completed')
      items.push({label: 'Cancel task', glyph: 'octagon-x', confirm: 'Tap again: stop its build and discard unreleased edits',
        run: async () => { const out = await request(path(task.sid, '/' + encodeURIComponent(task.task_id) + '/cancel'), 'POST', {}); note(out.state === 'abandoned' ? 'Task cancelled' : out.reason || 'Stop requested'); await refresh(); }});
    items.push({label: 'Copy Broker ID', glyph: 'copy', run: () => navigator.clipboard.writeText(task.task_id)});
    if (jobs[0]) items.push({label: 'Open in Builds page', glyph: 'external-link', run: () => { location.href = '/app?tab=builds&job_id=' + encodeURIComponent(jobs[0].job_id); }});
    if (!running) items.push({label: 'Clear from this session', glyph: 'eye-off',
      run: async () => { await request(path(task.sid, '/' + encodeURIComponent(task.task_id) + '/clear'), 'POST', {}); await refresh(); }});
    menu(items);
  }

  function jobMenu(task, job, canControl) {
    const items = [];
    if (canControl && ACTIVE.includes(job.status))
      items.push({label: 'Stop job', glyph: 'square', confirm: 'Tap again: stop this job and keep the task',
        run: async () => { await request(path(task.sid, '/' + encodeURIComponent(task.task_id) + '/jobs/' + encodeURIComponent(job.job_id) + '/stop'), 'POST', {}); note('Stop requested'); }});
    items.push({label: 'Copy Job ID', glyph: 'copy', run: () => navigator.clipboard.writeText(job.job_id)});
    menu(items);
  }

  function chipMenu() {
    if (tasks.length === 1) return void taskMenu(tasks[0]);
    if (!tasks.some(task => !ACTIVE.includes(task.status))) return;
    menu([{label: 'Clear finished jobs', glyph: 'eye-off', run: async () => {
      await Promise.all([...new Set(tasks.map(task => task.sid))].map(id => request(path(id, '/clear'), 'POST', {}))); await refresh(); }}]);
  }

  function taskRow(task) {
    const row = el('button', 'family-menu-item broker-row'); row.type = 'button'; row.setAttribute('role', 'menuitem');
    const text = el('span', 'family-menu-text');
    text.append(el('span', '', task.name || task.task_id), el('small', '', [task.spawn, LABELS[task.status] || task.status].filter(Boolean).join(' · ')));
    row.append(icon(taskIcon(task)), text);
    pressable(row, () => void openCard(task), () => void taskMenu(task));
    return row;
  }

  function openList() {
    const box = el('div', 'sub-chip-menu family-spawn-menu broker-list'); box.setAttribute('role', 'menu'); box.setAttribute('aria-label', 'Broker tasks');
    let shown = 0;
    const more = () => { for (const task of tasks.slice(shown, shown + PAGE)) box.append(taskRow(task)); shown = Math.min(tasks.length, shown + PAGE); draw(); };
    more();
    // Load the next ten as the list scrolls to its end.
    box.addEventListener('scroll', () => { if (shown < tasks.length && box.scrollTop + box.clientHeight >= box.scrollHeight - 24) more(); });
    show(box);
  }



  async function openCard(task) {
    const opened = sid;
    let detail;
    try { detail = await request(path(task.sid, '/' + encodeURIComponent(task.task_id))); } catch (error) { note(error.message); return; }
    if (sid !== opened) return;
    let view = renderCard(task, detail, job => jobMenu(task, job, detail.can_control), {pageLink: detail.can_control});
    show(view.box);
    tickTimer = setInterval(view.tick, 1000);
    if (view.live) cardTimer = setInterval(async () => {
      if (popup !== view.box) return;
      const open = view.box.querySelector('details')?.open;
      try {
        const next = await request(path(task.sid, '/' + encodeURIComponent(task.task_id)));
        if (popup !== view.box) return;
        const fresh = renderCard(task, next, job => jobMenu(task, job, next.can_control), {pageLink: next.can_control});
        if (open) fresh.box.querySelector('details')?.setAttribute('open', '');
        // Replace in place: same position, timers moved to the fresh card.
        fresh.box.style.top = view.box.style.top; fresh.box.style.left = view.box.style.left;
        clearInterval(tickTimer); view.box.replaceWith(fresh.box); popup = fresh.box; view = fresh; draw();
        tickTimer = setInterval(fresh.tick, 1000);
        if (!fresh.live) { clearInterval(cardTimer); cardTimer = null; void refresh(); }
      } catch {}
    }, REFRESH_MS);
  }

  pressable(chip, () => { if (popup) return close(); if (tasks.length === 1) void openCard(tasks[0]); else openList(); }, chipMenu);

  return {
    open(id) { close(); sid = id; tasks = []; renderChip(); void refresh(); },
    /** Re-reads the list after a published chat item that concerns a build. */
    published(item) { if (sid && /build_work|:build:/.test(JSON.stringify(item))) soon(); },
    refresh,
    disconnect() { close(); sid = ''; tasks = []; ++epoch; clearTimeout(pending); renderChip(); }
  };
}
