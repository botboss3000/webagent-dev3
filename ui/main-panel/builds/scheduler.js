'use strict';

const mib = bytes => Number.isFinite(bytes) ? (bytes / 1024 ** 2).toFixed(1) + ' MiB' : '—';
const label = value => String(value || 'waiting').replaceAll('_', ' ');

/** Keep logical tasks visible independently of dismissible build receipt cards. */
// @code-index {"version":1,"feature":"controlled-build-scheduler","aliases":["parallel builds","source ownership","compiler parking"],"purpose":"Builds task/resource controls; indirect Config: build-service/scheduler-defaults.json -> mode and resource thresholds via protected broker snapshot. Touch feedback is page-owned: ui/vibrate.json -> enabled, duration_ms.","span_lines":80,"configuration":{"status":"configured","references":[{"path":"build-service/scheduler-defaults.json","pointers":["/mode","/orphan_after_seconds","/reserve_bytes","/reserve_percent","/max_phases","/max_parked","/memory_pressure_percent"]},{"path":"ui/vibrate.json","pointers":["/enabled","/duration_ms"]}],"opportunities":[]}}
export function schedulerPanel() {
  const root = document.createElement('section');
  root.className = 'build-scheduler';
  root.setAttribute('aria-label', 'Source ownership and build resources');
  const summary = document.createElement('p');
  const message = document.createElement('p');
  message.setAttribute('role', 'status');
  // Collapsed by default; render() replaces only the list so the open state survives polling.
  const tasks = document.createElement('details');
  tasks.className = 'build-scheduler-tasks build-retained';
  const taskSummary = document.createElement('summary');
  taskSummary.className = 'build-retained-summary';
  const taskHeading = document.createElement('h3'); taskHeading.textContent = 'Unfinished source tasks';
  const taskCount = document.createElement('span'); taskCount.className = 'build-retained-count';
  const taskChevron = document.createElement('span'); taskChevron.className = 'build-retained-chevron';
  taskChevron.setAttribute('aria-hidden', 'true'); taskChevron.textContent = '▸';
  taskSummary.append(taskHeading, taskCount, taskChevron);
  const taskList = document.createElement('div'); taskList.className = 'build-retained-list';
  tasks.append(taskSummary, taskList);
  const phases = document.createElement('div');
  phases.className = 'build-scheduler-phases';
  root.append(summary, message, tasks, phases);
  let snapshot = {}, stopped = false, busy = false, offset = 0, updateRevision = 0;
  const requests = new Map();

  // Page-owned feedback uses ui/vibrate.json -> enabled, duration_ms.

  /** Authenticate each closed scheduler action and retain its exact retry ID on uncertain failure. */
  async function send(args) {
    const meResponse = await fetch('/api/v3/auth/me', {credentials: 'same-origin', cache: 'no-store'});
    if (!meResponse.ok) throw Error('Sign in required');
    const me = await meResponse.json();
    if (!me.is_admin || stopped) throw Error('Administrator access required');
    const key = JSON.stringify(args);
    const mutable = !['reservations', 'resources', 'takeover_preview', 'create'].includes(args.action);
    const body = {...args};
    if (mutable) {
      if (!requests.has(key)) requests.set(key, crypto.randomUUID());
      body.request_id = requests.get(key);
    }
    const response = await fetch('/api/v3/admin/builds/scheduler', {
      method: 'POST', credentials: 'same-origin',
      headers: {'Content-Type': 'application/json', 'x-csrf-token': me.csrf_token},
      body: JSON.stringify(body)
    });
    const result = await response.json();
    if (!response.ok) throw Error(result.error || 'Scheduler request failed; refresh before retrying.');
    requests.delete(key);
    return result;
  }
  /** Keep controls disabled during a mutation, then refresh ownership and resource revisions. */
  async function perform(action) {
    if (busy || stopped) return;
    busy = true; message.textContent = 'Applying scheduler action…'; render();
    try { await action(); if (!stopped) message.textContent = 'Saved. Updating current status…'; }
    catch (error) { if (!stopped) message.textContent = error.message; }
    finally {
      busy = false;
      if (!stopped) {
        try {
          const response = await fetch('/api/v3/admin/builds', {credentials: 'same-origin', cache: 'no-store'});
          if (response.ok) snapshot = await response.json();
          if (offset) snapshot.scheduler = await send({action: 'reservations', offset, limit: 20});
        } catch {}
        render();
      }
    }
  }
  /** Register one enabled action without granting authority from its UI state. */
  function button(parent, text, action, disabled = false) {
    const control = document.createElement('button');
    control.type = 'button'; control.textContent = text; control.disabled = busy || disabled;
    control.addEventListener('click', () => { void perform(action); });
    parent.append(control); return control;
  }
  /** Render untrusted coordination metadata as text, never executable markup. */
  function paragraph(parent, text) {
    const p = document.createElement('p'); p.textContent = text; parent.append(p);
  }
  /** Bind owner actions to the visible task's exact identity, revision and epoch. */
  function taskArgs(task, action, extra = {}) {
    return {action, task_id: task.label, task_key: task.id, expected_revision: task.revision, expected_epoch: task.epoch, ...extra};
  }
  /** Show unfinished code separately from execution history and gate phase controls by state. */
  function render() {
    const ownership = snapshot.scheduler || {};
    const resources = snapshot.resources || {};
    const host = resources.host || {};
    summary.textContent = 'Scheduler: ' + (ownership.mode || 'unavailable') +
      ' · Available RAM: ' + mib(host.available) + ' · Host reserve: ' + mib(resources.reserve) +
      ' · Memory pressure: ' + (host.pressure?.memory ?? '—') + '%' +
      ' · CPU pressure: ' + (host.pressure?.cpu ?? '—') + '%';
    taskCount.textContent = String(ownership.total || 0);
    taskList.replaceChildren();
    paragraph(taskList, 'A successful build keeps its reservation until the owning agent verifies acceptance and explicitly completes the task.');
    for (const task of ownership.tasks || []) {
      const card = document.createElement('article'); card.className = 'build-card';
      const title = document.createElement('h4'); title.textContent = task.label + ' · ' + label(task.state); card.append(title);
      paragraph(card, 'Generation ' + task.generation + ' · Revision ' + task.revision + ' · Epoch ' + task.epoch);
      paragraph(card, 'Reserved: ' + ((task.scopes || []).join(', ') || 'No source claimed'));
      if (task.requested?.length) paragraph(card, 'Waiting for: ' + task.requested.join(', '));
      const controls = document.createElement('div'); controls.className = 'build-scheduler-controls';
      if (task.owned_by_caller) {
        button(controls, task.state === 'orphaned' ? 'Recover ownership' : 'Heartbeat',
          () => send(taskArgs(task, task.state === 'orphaned' ? 'recover' : 'heartbeat')));
        button(controls, 'Continue corrections', () => send(taskArgs(task, 'continue')), !['owned', 'validating'].includes(task.state));
        button(controls, 'Hand off', async () => {
          const target = window.prompt('Existing empty followup task ID');
          if (target) await send(taskArgs(task, 'handoff', {target_task_id: target}));
        }, !['owned', 'validating', 'orphaned'].includes(task.state));
        button(controls, 'Verify and complete', async () => {
          if (task.state === 'validating') { await send(taskArgs(task, 'complete')); return; }
          const jobId = window.prompt('Latest synchronized release job ID');
          if (!jobId) return;
          const commit = window.prompt('Exact verified release commit (40 hexadecimal characters)');
          if (!commit) return;
          const check = window.prompt('Acceptance check you performed successfully');
          if (!check) return;
          const accepted = await send(taskArgs(task, 'acceptance', {job_id: jobId, commit, acceptance: [{check, passed: true}]}));
          await send(taskArgs({...task, revision: accepted.revision, epoch: accepted.epoch}, 'complete'));
        }, !['owned', 'validating'].includes(task.state));
      } else paragraph(card, 'The owning agent completes acceptance. Administrator takeover is available after execution and effects are reconciled.');
      button(controls, 'Administrator takeover', async () => {
        const preview = await send({action: 'takeover_preview', task_id: task.label, task_key: task.id});
        if (preview.jobs.some(j => ['queued', 'running', 'uncertain', 'needs_decision'].includes(j.status)))
          throw Error('Stop safe jobs and reconcile uncertain publication effects before takeover.');
        const reason = window.prompt('Reason for administrator takeover (required)');
        if (!reason) return;
        if (!window.confirm('Transfer ' + task.label + ' to a new followup task? Previous writers will be fenced.')) return;
        const target = 'followup-' + crypto.randomUUID();
        await send({action: 'create', task_id: target, summary: 'Administrator followup for ' + task.label});
        await send(taskArgs({...task, revision: preview.task.revision, epoch: preview.task.epoch}, 'takeover',
          {target_task_id: target, reason}));
      }, !['owned', 'validating', 'orphaned'].includes(task.state));
      card.append(controls); taskList.append(card);
    }
    const pages = document.createElement('div'); pages.className = 'build-scheduler-controls';
    button(pages, 'Previous source tasks', async () => { offset = Math.max(0, offset - 20); }, !offset);
    button(pages, 'Next source tasks', async () => { offset = ownership.next_offset; }, ownership.next_offset == null);
    paragraph(pages, 'Showing ' + (ownership.offset || 0) + '–' + ((ownership.offset || 0) + (ownership.tasks || []).length) + ' of ' + (ownership.total || 0) + ' unfinished tasks');
    taskList.append(pages);
    for (const legacy of ownership.legacy || []) paragraph(taskList, legacy.task_id + ' · Existing task needs ownership classification');
    phases.replaceChildren();
    const phaseHeading = document.createElement('h3'); phaseHeading.textContent = 'Measured build phases'; phases.append(phaseHeading);
    for (const phase of Object.values(resources.phases || {})) {
      const card = document.createElement('article'); card.className = 'build-card';
      const title = document.createElement('h4'); title.textContent = label(phase.phase) + ' · ' + label(phase.state); card.append(title);
      const link = document.createElement('a'); link.href = '/app?tab=builds&job_id=' + encodeURIComponent(phase.job_id);
      link.textContent = phase.job_id; card.append(link);
      paragraph(card, 'RAM ' + mib(phase.metrics?.memory) + ' · Peak ' + mib(phase.metrics?.peak_memory) +
        ' · Swap ' + mib(phase.metrics?.swap) + ' · CPU ' + (phase.metrics?.cpu_percent ?? '—') + '%');
      paragraph(card, 'Admission estimate ' + mib(phase.estimate) + ' · Parked ' + Number(phase.paused_seconds || 0).toFixed(1) + ' s · Priority ' + (phase.priority || 0));
      if (phase.reason) paragraph(card, phase.reason);
      const controls = document.createElement('div'); controls.className = 'build-scheduler-controls';
      const args = action => ({action, job_id: phase.job_id, phase_id: phase.phase_id, expected_revision: phase.revision || 0});
      const parkable = ['compile_test', 'compile_release', 'plugin_compile'].includes(phase.phase);
      if (phase.state === 'running' && parkable) button(controls, 'Pause compiler', () => send(args('pause')));
      if (['parked', 'waiting_intervention'].includes(phase.state)) button(controls, 'Resume when safe', () => send(args('resume')));
      if (['running', 'parked', 'waiting_intervention'].includes(phase.state)) {
        button(controls, 'Lower priority', () => send({...args('priority'), priority: phase.priority - 1}), phase.priority <= -2);
        button(controls, 'Raise priority', () => send({...args('priority'), priority: phase.priority + 1}), phase.priority >= 2);
      }
      if (phase.state === 'recovery_required') button(controls, 'Reconcile stopped phase', async () => {
        if (window.confirm('Remove this exact interrupted compiler container? Source ownership remains held.')) await send(args('reconcile'));
      });
      card.append(controls); phases.append(card);
    }
    if (host.controller_error) paragraph(phases, host.controller_error);
  }
  return {
    root,
    async update(value) {
      const revision = ++updateRevision;
      if (stopped) return;
      if (offset) {
        try { value.scheduler = await send({action: 'reservations', offset, limit: 20}); }
        catch (error) { if (!stopped) message.textContent = error.message; }
      }
      if (!stopped && revision === updateRevision) { snapshot = value; render(); }
    },
    destroy() { stopped = true; }
  };
}
