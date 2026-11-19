'use strict';
import {schedulerPanel} from '../builds/scheduler.js';

// Operator tasks carry their provenance at the end of the summary:
// "[origin=claude-code; session=<session name>; session_id=<id>]". Only the session name is shown.
// Task IDs and session names come from task summaries; they are rendered as text only.
export function provenance(summary) {
  const text = String(summary || '');
  const match = /\s*\[([^\]]*\borigin=[^\]]*)\]\s*$/.exec(text);
  const fields = Object.fromEntries((match?.[1] || '').split(';')
    .map(part => part.split('=')).filter(part => part.length > 1)
    .map(([key, ...value]) => [key.trim(), value.join('=').trim()]));
  // Session names are not stored on the server; fall back to the short session ID.
  const session = fields.session || (fields.session_id ? fields.session_id.slice(0, 8) : '');
  return {text: match ? text.slice(0, match.index) : text, origin: fields.origin || '', session};
}
// The broker reports local operators as "t480" or "t480/<harness>"; local owners also show their session name.
const owner = (origin, session) => String(origin || '').startsWith('t480')
  ? origin + (session ? ' session “' + session + '”' : '')
  : origin || '—';
// A phase or status name with underscores turned into spaces.
const phaseLabel = value => String(value || 'waiting').replaceAll('_', ' ');
// A Unix timestamp in seconds as the reader's local date and time.
const when = value => Number.isFinite(value) ? new Date(value * 1000).toLocaleString() : '—';
// A start and end time as a duration in minutes, or a dash when the start is unknown.
const duration = (start, end) => Number.isFinite(start)
  ? (Math.max(0, (Number.isFinite(end) ? end : Date.now() / 1000) - start) / 60).toFixed(1) + ' min'
  : '—';
const deploymentPhases = [
  'capturing_rollback', 'stopping_loop', 'stopping_api',
  'renaming_api', 'creating_api', 'starting_api',
  'renaming_loop', 'creating_loop', 'starting_loop',
  'checking_api', 'checking_loop'
];
// The ordered build phases a job of this operation passes through.
const phasesFor = job => [
  ...(job.operation === 'build' || job.operation === 'build_deploy'
    ? ['building_api', 'building_loop'] : []),
  ...(['deploy', 'build_deploy', 'ui_deploy'].includes(job.operation)
    ? ['indexing_source'] : []),
  ...(job.operation === 'deploy' || job.operation === 'build_deploy'
    ? deploymentPhases : []),
  ...(job.operation === 'ui_deploy' ? ['switching_ui', 'checking_ui'] : []),
  ...(job.isolated ? ['promoting_checkout'] : []),
  'finished'
];
const stoppedStatuses = new Set(['failed', 'cancelled', 'stale', 'uncertain', 'interrupted']);
// Whether a job is still running, queued or waiting for a decision.
const isActive = job => job.status !== 'completed' && !stoppedStatuses.has(job.status);
// The tone, glyph and words that describe a job's status.
const stateFor = job => job.status === 'needs_decision'
  ? {tone: 'danger', icon: '⚑', label: 'Decision required · source synchronization'}
  : job.status === 'completed'
  ? {tone: 'success', icon: '⚑', label: 'Complete'}
  : stoppedStatuses.has(job.status)
    ? {tone: 'danger', icon: '⚑', label: 'Incomplete · ' + phaseLabel(job.status)}
    : {tone: 'progress', icon: '●', label: 'In progress · ' + phaseLabel(job.status)};

const FAILED = ['failed', 'stale', 'cancelled', 'uncertain', 'interrupted', 'needs_decision'];
const STALE_SECONDS = 60;
const DETAIL_TTL_MS = 10000;
const LABELS = {queued:'Queued', running:'Running', needs_decision:'Needs a decision', completed:'Completed', failed:'Failed',
  stale:'Stale', cancelled:'Cancelled', uncertain:'Uncertain — needs reconciliation', interrupted:'Interrupted'};

// Minutes with one decimal, counted in whole tenths (0.1 every 6 s).
function minutes(seconds) {
  return (Math.floor(Math.max(0, seconds || 0) / 6) / 10).toFixed(1);
}

// Seconds a running counter shows now: the snapshot's value plus the time since it was read.
function liveSeconds(base, snapshotAt, serverOffset, nowMs = Date.now()) {
  // A browser clock behind the snapshot never counts backwards.
  return (base || 0) + Math.max(0, nowMs / 1000 + serverOffset - (snapshotAt || 0));
}

// A running stage's verdict at these seconds, flipping to slow as soon as it passes p90.
function stageVerdict(stage, seconds) {
  const [p50, p90] = stage?.normal || [];
  if (p90 == null) return 'no baseline';
  return seconds > p90 ? 'slow' : seconds > p50 ? 'slower than median' : 'on track';
}

// The time left in a job's normal range, counted down from its snapshot.
function remaining(range, elapsedSinceSnapshot) {
  if (!Array.isArray(range)) return null;
  const [low, high] = range.map(value => value - elapsedSinceSnapshot);
  return high <= 0 ? 'past normal' : minutes(Math.max(0, low)) + '–' + minutes(high) + ' min left';
}

// Stage rows built from a job's build phases when the broker has no stage timings for it.
function phaseStages(job) {
  const phases = phasesFor(job).filter(phase => phase !== 'finished');
  const at = phases.indexOf(job.phase);
  const cut = job.status === 'completed' ? phases.length : at >= 0 ? at : isActive(job) ? 0 : phases.length;
  const stage = (phase, index) => ({id: String(index + 1), stage: phaseLabel(phase), seconds: null, normal: null, samples: 0});
  const rows = phases.map(stage);
  if (job.status === 'queued') return {finished_stages: [], upcoming_stages: rows};
  return isActive(job) || job.status !== 'completed' && at >= 0
    ? {finished_stages: rows.slice(0, cut), running_stages: rows.slice(cut, cut + 1), upcoming_stages: rows.slice(cut + 1)}
    : {finished_stages: rows.slice(0, cut), upcoming_stages: rows.slice(cut)};
}

// The tasks behind the admin job rows, with the newest job of each kept as its main job.
function groupTasks(jobs) {
  const groups = new Map();
  for (const job of jobs) {
    const key = job.task_id || job.job_id;
    if (!groups.has(key)) groups.set(key, {key, task_id: job.task_id || '', jobs: []});
    groups.get(key).jobs.push(job);
  }
  const list = [...groups.values()].map(group => {
    group.jobs.sort((a, b) => (b.created_at || 0) - (a.created_at || 0));
    group.main = group.jobs[0];
    group.others = group.jobs.slice(1);
    group.active = isActive(group.main);
    group.created = group.main.created_at || 0;
    return group;
  });
  return list.sort((a, b) => taskRank(a) - taskRank(b)
    || (taskRank(a) === 3 ? b.created - a.created : a.created - b.created));
}

// The queue order: running, then queued, then needs a decision, then finished newest first.
function taskRank(group) {
  const status = group.main.status;
  return status === 'running' ? 0 : status === 'queued' ? 1 : status === 'needs_decision' ? 2 : 3;
}

// A task's main job, its snapshot and the time counted since that snapshot was read.
function jobProgress(group, detail) {
  const main = group.main;
  const offset = (detail?.now || Date.now() / 1000) - Date.now() / 1000;
  const job = (detail?.task?.jobs || []).find(item => item.job_id === main.job_id);
  const live = isActive(main);
  const age = live && job?.snapshot_at ? liveSeconds(0, job.snapshot_at, offset) : 0;
  return {job, snapshot: job?.snapshot || {}, live, age, offset, snapshotAt: job?.snapshot_at};
}

// How long a task has been building or finished in, from its start time or its snapshot.
function taskElapsed(group, detail) {
  const main = group.main;
  const progress = jobProgress(group, detail);
  const start = main.started_at || main.created_at;
  if (Number.isFinite(start)) {
    const end = Number.isFinite(main.finished_at) ? main.finished_at : Date.now() / 1000 + progress.offset;
    return Math.max(0, end - start);
  }
  return ((progress.snapshot.job_seconds || 0) + progress.age) || 0;
}

// How long one job of the task took, or a dash when it never started.
function jobSeconds(job) {
  const start = job.started_at || job.created_at;
  if (!Number.isFinite(start)) return null;
  return Math.max(0, (Number.isFinite(job.finished_at) ? job.finished_at : Date.now() / 1000) - start);
}

export function mountBuilds(panel, onStatus = () => {}) {
  let stopped = false;
  let loading = false;
  let snapshot = {jobs: [], containers: {}};
  const dismissedKey = 'v3-builds-dismissed-job-ids';
  const readDismissed = () => {
    try { return new Set(JSON.parse(localStorage.getItem(dismissedKey) || '[]')); }
    catch { return new Set(); }
  };
  let dismissed = readDismissed();
  let activeOnly = false;
  const enabledStatuses = new Map();
  const cancelRequests = new Map();
  const cancelMessages = new Map();
  const cancelling = new Set();
  const details = new Map();
  const detailAt = new Map();
  const detailPending = new Set();
  let tickers = [];
  let panelNodes = [];
  let stripSync = () => {};
  let flashTimer = null;
  const filters = document.createElement('div');
  filters.className = 'build-filters';
  filters.setAttribute('aria-label', 'Filter builds by status');
  const clear = document.createElement('select');
  clear.setAttribute('aria-label', 'Clear finished builds from this view');
  for (const [value, label] of [['completed', 'Clear completed'], ['failed-cancelled', 'Clear failed & cancelled'], ['all', 'Clear all finished']]) {
    const option = document.createElement('option');
    option.value = value;
    option.textContent = label;
    clear.append(option);
  }
  const clearButton = document.createElement('button');
  clearButton.type = 'button';
  clearButton.textContent = 'Clear';
  const reset = document.createElement('button');
  reset.type = 'button';
  reset.textContent = 'Reset view';
  const controls = document.createElement('div');
  controls.className = 'build-clear-controls';
  controls.append(clear, clearButton, reset);
  panel.replaceChildren();
  const update = document.createElement('button');
  update.type = 'button';
  update.className = 'build-update';
  update.textContent = '↻';
  update.setAttribute('aria-label', 'Refresh builds');
  update.title = 'Refresh builds';
  const status = document.createElement('p');
  status.className = 'build-summary';
  status.setAttribute('role', 'status');
  const spinner = document.createElement('span');
  spinner.className = 'build-loading-spinner';
  spinner.setAttribute('aria-hidden', 'true');
  status.append(spinner, 'Loading builds…');
  const list = document.createElement('div');
  list.className = 'build-list';
  list.setAttribute('aria-label', 'Development builds');
  const leftovers = document.createElement('details');
  leftovers.className = 'build-retained';
  leftovers.setAttribute('aria-label', 'Retained tasks needing investigation');
  const leftoverSummary = document.createElement('summary');
  leftoverSummary.className = 'build-retained-summary';
  const leftoverTitle = document.createElement('h3');
  leftoverTitle.textContent = 'Retained tasks needing investigation';
  const leftoverCount = document.createElement('span');
  leftoverCount.className = 'build-retained-count';
  const leftoverChevron = document.createElement('span');
  leftoverChevron.className = 'build-retained-chevron';
  leftoverChevron.setAttribute('aria-hidden', 'true');
  leftoverChevron.textContent = '▸';
  const leftoverList = document.createElement('div');
  leftoverList.className = 'build-retained-list';
  leftoverSummary.append(leftoverTitle, leftoverCount, leftoverChevron);
  leftovers.append(leftoverSummary, leftoverList);
  leftovers.hidden = true;
  // Blocker-board notes sessions left on each other's tasks; hidden when there are none.
  const board = document.createElement('details');
  board.className = 'build-retained';
  board.setAttribute('aria-label', 'Board notes');
  const boardSummary = document.createElement('summary');
  boardSummary.className = 'build-retained-summary';
  const boardTitle = document.createElement('h3');
  boardTitle.textContent = 'Board notes';
  const boardCount = document.createElement('span');
  boardCount.className = 'build-retained-count';
  const boardChevron = leftoverChevron.cloneNode(true);
  const boardList = document.createElement('div');
  boardList.className = 'build-retained-list';
  boardSummary.append(boardTitle, boardCount, boardChevron);
  board.append(boardSummary, boardList);
  board.hidden = true;
  const reviewed = new Set();
  const reviewErrors = new Map();
  const synchronization = document.createElement('div');
  synchronization.setAttribute('role', 'status');
  const toolbar = document.createElement('div');
  toolbar.className = 'build-toolbar';
  toolbar.append(update, filters, controls);
  const scheduler = schedulerPanel();
  // The scheduler's own resource sentence is replaced by the tiles at the top of the page.
  scheduler.root.querySelector(':scope > p').hidden = true;
  const resources = document.createElement('section');
  resources.className = 'bq-resources';
  resources.setAttribute('aria-label', 'Build host resources');
  // The three secondary sections share one bordered list of identical disclosure rows.
  const more = document.createElement('div');
  more.className = 'bq-more';
  more.append(...scheduler.root.querySelectorAll(':scope > details.build-retained'), board, leftovers);
  panel.append(toolbar, resources, status, synchronization, list, scheduler.root, more);

  // Draws the scheduler mode, free memory, reserve, pressure and disk use as small tiles at the top.
  function renderResources() {
    const mib = bytes => Number.isFinite(bytes) ? (bytes / 1024 ** 3).toFixed(1) + ' GiB' : '—';
    const res = snapshot.resources || {};
    const host = res.host || {};
    const tone = value => !Number.isFinite(value) ? '' : value >= 80 ? 'danger' : value >= 40 ? 'warning' : 'success';
    const percent = value => Number.isFinite(value) ? value + '%' : '—';
    // The Docker guard starts deleting at 80% disk use.
    const diskTone = value => !Number.isFinite(value) ? '' : value >= 90 ? 'danger' : value >= 80 ? 'warning' : 'success';
    const tiles = [
      ['Scheduler', snapshot.scheduler?.mode || 'unavailable', ''],
      ['Available RAM', mib(host.available), ''],
      ['Host reserve', mib(res.reserve), ''],
      ['Memory pressure', percent(host.pressure?.memory), tone(host.pressure?.memory)],
      ['CPU pressure', percent(host.pressure?.cpu), tone(host.pressure?.cpu)],
      ['Disk used', percent(host.disk_percent), diskTone(host.disk_percent)]
    ];
    resources.replaceChildren();
    for (const [label, value, level] of tiles) {
      const tile = document.createElement('div');
      tile.className = 'bq-res';
      if (level) tile.dataset.tone = level;
      const term = document.createElement('span');
      term.className = 'bq-res-label';
      term.textContent = label;
      const amount = document.createElement('span');
      amount.className = 'bq-res-value';
      amount.textContent = value;
      tile.append(term, amount);
      resources.append(tile);
    }
  }

  const fact = (facts, label, value) => {
    const item = document.createElement('div');
    item.className = 'build-fact';
    const term = document.createElement('dt');
    term.textContent = label;
    const description = document.createElement('dd');
    description.textContent = String(value ?? '—');
    item.append(term, description);
    facts.append(item);
  };
  function showSynchronization(target, report) {
    target.replaceChildren();
    if (!report?.total && report?.status !== 'unavailable') return;
    const title = document.createElement('p');
    title.textContent = report.status === 'unavailable'
      ? 'Source synchronization could not be checked.'
      : report.total + ' source files differ from the deployed release; ' + report.affected + ' touched by this build, ' + report.promotion_conflicts + ' promotion conflicts.';
    const guidance = document.createElement('p');
    guidance.textContent = report.guidance || '';
    const files = document.createElement('ul');
    for (const file of report.files || []) {
      const row = document.createElement('li');
      row.textContent = file.path + ' · ' + file.kind + (file.promotion_conflict ? ' · promotion conflict' : '');
      files.append(row);
    }
    target.append(title, guidance, files);
    if (report.next_offset != null) {
      const more = document.createElement('p');
      more.textContent = 'More files are available in the agent preflight report.';
      target.append(more);
    }
  }
  // The task name: the broker task name once read, otherwise the job summary without its provenance.
  function groupName(group) {
    const detail = details.get(group.task_id);
    const origin = provenance(group.main.summary);
    return detail?.task?.name || origin.text || group.task_id || group.main.job_id.slice(0, 8);
  }
  // How many tasks sit before and after this one, naming the queue ahead for a queued task.
  function aheadText(group, index, total, jobSnapshot) {
    const ahead = index, behind = total - index - 1;
    if (group.main.status === 'queued' && (jobSnapshot.queued_behind || []).length) {
      const names = jobSnapshot.queued_behind
        .map(other => other.name || other.task_id || String(other.job_id || '').slice(0, 8)).join(', ');
      return ahead + ' ahead: ' + names + ' · ' + behind + ' behind';
    }
    return ahead + ' ahead · ' + behind + ' behind';
  }
  // One transparent chevron over a faded queue edge, present only while that side can scroll.
  function chevronButton(glyph, label, side) {
    const button = document.createElement('button');
    button.type = 'button';
    button.className = 'qchev qchev-' + side;
    button.textContent = glyph;
    button.setAttribute('aria-label', label);
    button.title = label;
    return button;
  }
  // One chip of the queue strip: its place, status dot, task name and elapsed time.
  function queueChip(group, index, total) {
    const chip = document.createElement('button');
    chip.type = 'button';
    chip.className = 'qchip';
    chip.dataset.tone = stateFor(group.main).tone;
    const number = document.createElement('span');
    number.className = 'qchip-num';
    number.textContent = '#' + (index + 1);
    const dot = document.createElement('span');
    dot.className = 'qdot';
    dot.setAttribute('aria-hidden', 'true');
    const name = document.createElement('span');
    name.className = 'qchip-name';
    name.textContent = groupName(group);
    const time = document.createElement('span');
    time.className = 'qchip-time';
    time.textContent = minutes(taskElapsed(group, details.get(group.task_id))) + ' min';
    chip.append(number, dot, name, time);
    chip.setAttribute('aria-label', 'Task ' + (index + 1) + ' of ' + total + ': ' + groupName(group)
      + ', ' + (LABELS[group.main.status] || group.main.status));
    chip.addEventListener('click', () => revealPanel(group.key));
    return chip;
  }
  // The broker card's stage table: finished, running and upcoming rows with times and verdicts.
  function stageTable(jobSnapshot, age, failed, slowRows) {
    const table = document.createElement('div');
    table.className = 'tp-stages';
    table.setAttribute('role', 'table');
    const head = document.createElement('div');
    head.className = 'tp-stage tp-stage-head';
    head.setAttribute('role', 'row');
    for (const [label, cellClass] of [['#', 'tp-sid'], ['Stage', 'tp-sname'], ['Time', 'tp-time'], ['Normal', 'tp-normal'], ['Status', 'tp-state']]) {
      const cell = document.createElement('span');
      cell.className = cellClass;
      cell.textContent = label;
      head.append(cell);
    }
    table.append(head);
    // A failed job's unfinished stages show where it stopped, in red.
    const rows = [...(jobSnapshot.finished_stages || []).map(stage => [stage, 'done']),
      ...(jobSnapshot.running_stages || []).map(stage => [stage, failed ? 'failed' : 'running']),
      ...(jobSnapshot.upcoming_stages || []).map(stage => [stage, 'upcoming'])];
    for (const [stage, state] of rows) {
      const row = document.createElement('div');
      row.className = 'tp-stage tp-stage-' + state;
      row.setAttribute('role', 'row');
      const time = document.createElement('span');
      time.className = 'tp-time';
      if (state === 'running') slowRows.push([stage, row]);
      time.textContent = state === 'upcoming' || stage.seconds == null ? '—'
        : minutes(state === 'running' ? (stage.seconds || 0) + age : stage.seconds) + ' min';
      const normal = stage.normal
        ? minutes(stage.normal[0]) + '–' + minutes(stage.normal[1])
        : (stage.samples ? '' : 'no history');
      const id = document.createElement('span');
      id.className = 'tp-sid';
      id.textContent = stage.id || '';
      const name = document.createElement('span');
      name.className = 'tp-sname';
      // where: the broker sidekick ran this stage ("sidekick", or "sidekick → server" after a fallback).
      name.textContent = stage.stage + (stage.where ? ' (' + stage.where + ')' : '');
      const normalCell = document.createElement('span');
      normalCell.className = 'tp-normal';
      normalCell.textContent = normal;
      const stateCell = document.createElement('span');
      stateCell.className = 'tp-state';
      stateCell.textContent = state === 'running' ? stageVerdict(stage, (stage.seconds || 0) + age) : state;
      row.append(id, name, time, normalCell, stateCell);
      table.append(row);
    }
    return table;
  }
  // The queue-compatibility check kept from the previous card, as one compact block.
  function preflightSection(report) {
    const section = document.createElement('section');
    section.className = 'tp-preflight';
    section.setAttribute('aria-label', 'Queue compatibility');
    const title = document.createElement('h4');
    title.textContent = 'Queue compatibility · ' + phaseLabel(report.status);
    const note = document.createElement('p');
    note.textContent = 'Checked before compilation. Shared builds are rechecked at admission, with up to eight tasks per build.';
    section.append(title, note);
    const peers = document.createElement('ul');
    for (const peer of report.peers || []) {
      const row = document.createElement('li');
      row.append(document.createTextNode(peer.job_id + ' · ' + peer.reason));
      if (peer.paths?.length) {
        const files = document.createElement('div');
        files.textContent = (peer.conflict_count ? 'Conflicting files: ' : 'Overlapping files: ')
          + peer.paths.join(', ') + (peer.paths_truncated ? ' (more files omitted)' : '');
        row.append(files);
      }
      peers.append(row);
    }
    section.append(peers);
    if (!(report.peers || []).length && report.status === 'checked') {
      const empty = document.createElement('p');
      empty.textContent = 'No other pending tasks to compare.';
      section.append(empty);
    }
    if (report.truncated || report.status === 'unavailable') {
      const partial = document.createElement('p');
      partial.textContent = report.status === 'unavailable'
        ? 'Source comparison unavailable; admission will check again.'
        : 'Partial queue preview; more comparisons are available on other job cards.';
      section.append(partial);
    }
    return section;
  }
  // The full build-phase list with each step's state and time.
  function phaseList(job) {
    const phases = document.createElement('ol');
    phases.className = 'build-phases';
    phases.setAttribute('aria-label', 'Build phases and current step');
    const sequence = phasesFor(job);
    const currentIndex = sequence.indexOf(job.phase);
    const history = new Map((job.phase_history || []).map(item => [item.phase, item.at]));
    for (const [index, phase] of sequence.entries()) {
      const row = document.createElement('li');
      const completed = history.has(phase) && (index < currentIndex || job.status === 'completed');
      const stoppedHere = phase === job.phase && stoppedStatuses.has(job.status);
      const current = phase === job.phase && !stoppedHere && job.status !== 'completed';
      row.className = stoppedHere ? 'stopped' : current ? 'current' : completed ? 'completed' : 'upcoming';
      if (current || stoppedHere) row.setAttribute('aria-current', 'step');
      const state = stoppedHere ? 'stopped here' : current ? 'current step' : completed ? 'complete' : 'upcoming';
      row.textContent = phaseLabel(phase) + ' · ' + state
        + (history.has(phase) ? ' · ' + when(history.get(phase)) : '');
      phases.append(row);
    }
    return phases;
  }
  // The task's older jobs, each with its kind, short ID, state and duration.
  function otherJobs(group) {
    const box = document.createElement('div');
    box.className = 'tp-jobs';
    const title = document.createElement('h4');
    title.textContent = 'Other jobs of this task';
    box.append(title);
    for (const job of group.others) {
      const row = document.createElement('div');
      row.className = 'tp-job';
      row.dataset.tone = stateFor(job).tone;
      const kind = document.createElement('span');
      kind.className = 'tp-job-kind';
      kind.textContent = phaseLabel(job.operation || (job.check ? 'check ' + job.check : 'job'));
      const id = document.createElement('span');
      id.className = 'tp-job-id';
      id.textContent = String(job.job_id || '').slice(0, 8);
      const state = document.createElement('span');
      state.className = 'tp-job-state';
      state.textContent = LABELS[job.status] || job.status;
      const seconds = jobSeconds(job);
      const time = document.createElement('span');
      time.className = 'tp-job-time';
      time.textContent = seconds == null ? '—' : minutes(seconds) + ' min';
      row.append(kind, id, state, time);
      box.append(row);
    }
    return box;
  }
  // Everything the old build card carried: facts, containers, sync, phases, log, stop and other jobs.
  function advancedSection(group, main, ownerText, open) {
    const advanced = document.createElement('details');
    advanced.className = 'tp-advanced';
    advanced.dataset.advanced = '';
    if (open) advanced.open = true;
    const summary = document.createElement('summary');
    summary.textContent = 'Advanced · job details';
    advanced.append(summary);
    const facts = document.createElement('dl');
    facts.className = 'build-facts';
    if (main.phase !== 'finished') fact(facts, 'Current phase', phaseLabel(main.phase));
    fact(facts, 'Rust tests', main.rust_tests?.status || 'not started');
    fact(facts, 'Test result', main.rust_tests?.summary || 'Awaiting a confirmed test result');
    fact(facts, 'Started', when(main.started_at || main.created_at));
    fact(facts, 'Elapsed', duration(main.created_at, main.finished_at));
    fact(facts, 'Job ID', main.job_id);
    fact(facts, 'Task ID', main.task_id || '—');
    fact(facts, 'Owner', ownerText);
    fact(facts, 'Input digest', main.inputs_sha256 || '—');
    for (const role of ['api', 'loop']) {
      const container = snapshot.containers?.[role] || {};
      fact(facts, 'Current ' + role.toUpperCase() + ' container',
        [container.state, container.health].filter(Boolean).join(' / ') || 'Unavailable');
    }
    advanced.append(facts);
    const sourceStatus = document.createElement('div');
    showSynchronization(sourceStatus, main.synchronization);
    advanced.append(sourceStatus);
    if (main.reason) {
      const reason = document.createElement('p');
      reason.className = 'build-reason';
      reason.textContent = main.reason;
      advanced.append(reason);
    }
    if (main.queue_preflight) advanced.append(preflightSection(main.queue_preflight));
    const phaseTitle = document.createElement('h4');
    phaseTitle.textContent = 'Build phases';
    advanced.append(phaseTitle, phaseList(main));
    if (main.log) {
      const logTitle = document.createElement('h4');
      logTitle.textContent = 'Sanitized build log excerpt';
      const log = document.createElement('pre');
      log.className = 'tp-log';
      log.textContent = main.log;
      advanced.append(logTitle, log);
    }
    const actions = document.createElement('div');
    actions.className = 'tp-actions';
    const cancellation = main.cancellation || {};
    if (cancellation.available
      || (cancellation.requested && !stoppedStatuses.has(main.status) && main.status !== 'completed')) {
      const cancel = document.createElement('button');
      cancel.type = 'button';
      cancel.textContent = cancellation.requested ? 'Stop requested'
        : cancellation.mode === 'queued' ? 'Cancel queued job'
        : cancellation.mode === 'deployment_stop' ? 'Request deployment stop' : 'Stop build';
      cancel.disabled = cancellation.requested || cancelling.has(main.job_id);
      cancel.addEventListener('click', async () => {
        const affected = cancellation.affected_job_ids || [main.job_id];
        const warning = (affected.length > 1 ? 'This stops all ' + affected.length + ' jobs in the shared build. ' : '')
          + (cancellation.mode === 'deployment_stop' ? 'Deployment effects have started. Stopping may leave an uncertain release requiring reconciliation. ' : '')
          + 'Request stop?';
        if (!window.confirm(warning)) return;
        const requestId = cancelRequests.get(main.job_id) || crypto.randomUUID();
        cancelRequests.set(main.job_id, requestId);
        cancelling.add(main.job_id);
        cancelMessages.set(main.job_id, 'Requesting stop…');
        render();
        try {
          const identity = await fetch('/api/v3/auth/me', {credentials: 'same-origin', cache: 'no-store'});
          if (!identity.ok) throw Error('Sign in required');
          const me = await identity.json();
          if (!me.is_admin || stopped) throw Error('Administrator access required');
          const response = await fetch('/api/v3/admin/builds/' + encodeURIComponent(main.job_id) + '/cancel', {
            method: 'POST', credentials: 'same-origin',
            headers: {'Content-Type': 'application/json', 'x-csrf-token': me.csrf_token},
            body: JSON.stringify({request_id: requestId, expected_job_ids: affected})
          });
          const result = await response.json();
          if (!response.ok) throw Error(result.error || 'Stop request unavailable (' + response.status + ')');
          cancelMessages.set(main.job_id, 'Stop result: ' + phaseLabel(result.disposition));
        } catch (error) {
          cancelMessages.set(main.job_id, error.message + '. Update status before retrying the same request.');
        } finally {
          cancelling.delete(main.job_id);
          if (!stopped) { render(); void load(); }
        }
      });
      actions.append(cancel);
    }
    if (cancelMessages.has(main.job_id)) {
      const note = document.createElement('p');
      note.setAttribute('role', 'status');
      note.textContent = cancelMessages.get(main.job_id);
      actions.append(note);
    }
    if (actions.childNodes.length) advanced.append(actions);
    if (group.others.length) advanced.append(otherJobs(group));
    return advanced;
  }
  // One task's panel: live stages and counters, with every receipt detail inside Advanced.
  function taskPanel(group, index, total, firstActiveIndex, openKeys, linkedId) {
    const main = group.main;
    const detail = details.get(group.task_id);
    const progress = jobProgress(group, detail);
    const jobSnapshot = progress.snapshot;
    const live = progress.live;
    const ageNow = () => (progress.snapshotAt ? liveSeconds(0, progress.snapshotAt, progress.offset) : 0);
    const elapsedNow = () => taskElapsed(group, detail);
    const origin = provenance(main.summary);
    const ownerText = owner(main.origin || origin.origin, origin.session);
    const tone = stateFor(main).tone;
    const card = document.createElement('article');
    card.className = 'build-card task-panel';
    card.dataset.status = tone;
    card.dataset.active = String(live);
    card.dataset.task = group.key;
    if (group.jobs.some(job => job.job_id === linkedId)) card.classList.add('linked');
    if (index === 0 && live) card.classList.add('tp-accent');
    const head = document.createElement('header');
    head.className = 'tp-head';
    const glyph = document.createElement('span');
    glyph.className = 'tp-glyph' + (main.status === 'running' ? ' tp-spin' : '');
    glyph.textContent = tone === 'success' ? '✓' : tone === 'danger' ? '!' : '●';
    glyph.setAttribute('role', 'img');
    glyph.setAttribute('aria-label', stateFor(main).label);
    const titles = document.createElement('div');
    titles.className = 'tp-titles';
    const name = document.createElement('strong');
    name.className = 'tp-name';
    name.textContent = groupName(group);
    const meta = document.createElement('span');
    meta.className = 'tp-meta';
    meta.textContent = (group.task_id || main.job_id) + ' · ' + ownerText;
    titles.append(name, meta);
    const side = document.createElement('div');
    side.className = 'tp-side';
    const position = document.createElement('span');
    position.className = 'tp-pos';
    position.textContent = (index + 1) + ' of ' + total;
    const ahead = document.createElement('span');
    ahead.className = 'tp-ab';
    ahead.textContent = aheadText(group, index, total, jobSnapshot);
    side.append(position, ahead);
    head.append(glyph, titles, side);
    card.append(head);
    const sequence = document.createElement('div');
    sequence.className = 'tp-seq';
    if (index === firstActiveIndex) {
      const flag = document.createElement('span');
      flag.className = 'tp-label';
      flag.textContent = main.status === 'running' ? 'Building now' : 'Next';
      sequence.append(flag);
    }
    const stateWord = document.createElement('span');
    stateWord.textContent = LABELS[main.status] || main.status;
    sequence.append(stateWord);
    if (live && main.phase && main.phase !== 'finished') {
      sequence.append(document.createTextNode(' · ' + phaseLabel(main.phase)));
    }
    card.append(sequence);
    const counter = document.createElement('p');
    counter.className = 'tp-counter';
    if (!live) counter.textContent = 'finished in ' + minutes(elapsedNow()) + ' min';
    card.append(counter);
    const slowRows = [];
    if (jobSnapshot.finished_stages || jobSnapshot.running_stages || jobSnapshot.upcoming_stages) {
      card.append(stageTable(jobSnapshot, live ? ageNow() : 0, FAILED.includes(main.status), slowRows));
    } else {
      card.append(stageTable(phaseStages(main), 0, FAILED.includes(main.status), []));
      const pending = document.createElement('p');
      pending.className = 'tp-pending';
      pending.textContent = group.task_id && !detailAt.has(group.task_id)
        ? 'Stage timings are loading from the broker…'
        : 'No stage timings recorded; stages follow the build phases.';
      card.append(pending);
    }
    if ((jobSnapshot.background || []).length) {
      const background = document.createElement('div');
      background.className = 'tp-background';
      const title = document.createElement('strong');
      title.textContent = 'Background';
      background.append(title);
      for (const work of jobSnapshot.background) {
        const row = document.createElement('div');
        row.textContent = work.name + ' · ' + (work.status || 'running') + ' · ' + minutes(work.seconds) + ' min'
          + (work.result ? ' (' + work.result + ')' : '');
        background.append(row);
      }
      card.append(background);
    }
    const stale = document.createElement('p');
    stale.className = 'tp-stale';
    card.append(stale);
    card.append(advancedSection(group, main, ownerText, openKeys.has(group.key)));
    if (live) {
      const draw = () => {
        const age = ageNow();
        const parts = ['Elapsed ' + minutes(elapsedNow()) + ' min'];
        if (jobSnapshot.job_normal) {
          parts.push('normal ' + minutes(jobSnapshot.job_normal[0]) + '–' + minutes(jobSnapshot.job_normal[1]));
        }
        const left = remaining(jobSnapshot.remaining_seconds, age);
        if (left) parts.push(left);
        counter.textContent = parts.join(' · ');
        for (const [stage, row] of slowRows) {
          const seconds = (stage.seconds || 0) + age;
          row.querySelector('.tp-time').textContent = minutes(seconds) + ' min';
          const verdict = stageVerdict(stage, seconds);
          row.querySelector('.tp-state').textContent = verdict;
          row.classList.toggle('tp-slow', verdict === 'slow');
        }
        stale.textContent = age > STALE_SECONDS ? 'Updated ' + minutes(age) + ' min ago' : '';
      };
      draw();
      tickers.push(draw);
    }
    return card;
  }
  // Brings a task panel into view and briefly outlines it after its queue chip is tapped.
  function revealPanel(key) {
    const entry = panelNodes.find(item => item.key === key);
    if (!entry) return;
    entry.node.scrollIntoView({block: 'start', behavior: 'smooth'});
    entry.node.classList.add('qflash');
    clearTimeout(flashTimer);
    flashTimer = setTimeout(() => entry.node.classList.remove('qflash'), 1500);
  }
  // Reads each task's broker detail, keeping the last good copy for ten seconds per task.
  async function loadDetails(groups) {
    const now = Date.now();
    const stale = groups.map(group => group.task_id).filter(id => id
      && (!detailAt.has(id) || now - detailAt.get(id) > DETAIL_TTL_MS) && !detailPending.has(id));
    if (!stale.length) return;
    for (const id of stale) detailPending.add(id);
    let changed = false;
    await Promise.all(stale.map(async id => {
      try {
        const response = await fetch('/api/v3/builds/tasks/' + encodeURIComponent(id), {cache: 'no-store'});
        if (response.ok) {
          const data = await response.json();
          if (data?.task) { details.set(id, data); changed = true; }
        }
      } catch {
        // A failed read keeps the panel working from the admin job row.
      } finally {
        detailAt.set(id, Date.now());
        detailPending.delete(id);
      }
    }));
    if (changed && !stopped) render();
  }
  function render() {
    scheduler.update(snapshot);
    renderResources();
    // Keep the disclosure node intact so polling never resets its open state.
    leftoverList.replaceChildren();
    const notices = Array.isArray(snapshot.leftovers) ? snapshot.leftovers : [];
    leftovers.hidden = !notices.length;
    leftoverCount.textContent = String(notices.length);
    for (const notice of notices) {
      const card = document.createElement('article');
      card.className = 'build-retained-notice';
      const text = document.createElement('p');
      text.textContent = notice.notification;
      const detail = document.createElement('p');
      detail.textContent = 'Source: ' + notice.origin + ' · Task: ' + notice.task_id +
        (Number.isSafeInteger(notice.retained_bytes) ? ' · Retained: ' + (notice.retained_bytes / 1024 ** 2).toFixed(1) + ' MiB' : '');
      card.append(text, detail);
      if (notice.session_id) {
        const link = document.createElement('a');
        link.href = '/app?session_id=' + encodeURIComponent(notice.session_id);
        link.textContent = 'Open originating conversation';
        card.append(link);
      }
      if (notice.job_id) {
        const link = document.createElement('a');
        link.href = '/app?tab=builds&job_id=' + encodeURIComponent(notice.job_id);
        link.textContent = 'Open build receipt';
        card.append(link);
      }
      const button = document.createElement('button');
      button.type = 'button';
      button.textContent = 'Mark reviewed';
      button.title = 'Clear this reminder after investigation. Retained work remains intact.';
      button.disabled = reviewed.has(notice.id);
      button.addEventListener('click', async () => {
        reviewed.add(notice.id); reviewErrors.delete(notice.id); render();
        try {
          const identity = await fetch('/api/v3/auth/me', {credentials: 'same-origin', cache: 'no-store'});
          if (!identity.ok) throw Error('Sign in required');
          const me = await identity.json();
          if (!me.is_admin || stopped) throw Error('Administrator access required');
          const response = await fetch('/api/v3/admin/builds/leftovers/' + encodeURIComponent(notice.id) + '/review', {
            method: 'POST', credentials: 'same-origin', headers: {'x-csrf-token': me.csrf_token}
          });
          if (!response.ok) throw Error('Review could not be saved; refresh before retrying.');
        } catch (error) { reviewErrors.set(notice.id, error.message); }
        finally { reviewed.delete(notice.id); if (!stopped) { render(); void load(); } }
      });
      card.append(button);
      if (reviewErrors.has(notice.id)) {
        const error = document.createElement('p'); error.setAttribute('role', 'status');
        error.textContent = reviewErrors.get(notice.id); card.append(error);
      }
      leftoverList.append(card);
    }
    renderBoard(snapshot.board);
    showSynchronization(synchronization, snapshot.synchronization);
    const jobs = snapshot.jobs;
    const groups = groupTasks(jobs);
    const statuses = [...new Set(groups.map(group => String(group.main.status || 'unknown')))].sort();
    for (const statusName of statuses) if (!enabledStatuses.has(statusName)) enabledStatuses.set(statusName, true);
    for (const statusName of [...enabledStatuses.keys()]) if (!statuses.includes(statusName)) enabledStatuses.delete(statusName);
    filters.replaceChildren();
    const addChip = (key, title, enabled, toggle) => {
      const chip = document.createElement('button');
      chip.type = 'button';
      chip.className = 'build-filter-chip';
      chip.dataset.filter = key;
      chip.setAttribute('aria-pressed', String(enabled));
      const flag = document.createElement('span');
      flag.setAttribute('aria-hidden', 'true');
      flag.textContent = '⚑';
      chip.append(flag, document.createTextNode(title));
      chip.addEventListener('click', () => {
        toggle();
        render();
        // Rendering refreshes the chips; retain keyboard focus on the same filter.
        [...filters.children].find(item => item.dataset.filter === key)?.focus?.();
      });
      filters.append(chip);
    };
    addChip('active', 'Active', activeOnly, () => { activeOnly = !activeOnly; });
    for (const statusName of statuses) {
      const title = phaseLabel(statusName);
      addChip(statusName, title[0].toUpperCase() + title.slice(1),
        enabledStatuses.get(statusName) !== false,
        () => { enabledStatuses.set(statusName, enabledStatuses.get(statusName) === false); });
    }
    const persistDismissed = () => { try { localStorage.setItem(dismissedKey, JSON.stringify([...dismissed])); } catch {} };
    clearButton.onclick = () => {
      const mode = clear.value;
      for (const group of groups) for (const job of group.jobs) {
        const terminal = job.status === 'completed' || stoppedStatuses.has(job.status);
        const match = mode === 'completed' ? job.status === 'completed'
          : mode === 'failed-cancelled' ? ['failed', 'cancelled'].includes(job.status) : terminal;
        if (match) dismissed.add(job.job_id);
      }
      persistDismissed(); render();
    };
    reset.onclick = () => {
      activeOnly = false;
      dismissed.clear(); persistDismissed();
      for (const statusName of statuses) enabledStatuses.set(statusName, true);
      render();
    };
    const linkedId = new URL(location.href).searchParams.get('job_id');
    const visible = groups.filter(group => !group.jobs.every(job => dismissed.has(job.job_id))
      && (!activeOnly || group.active)
      && enabledStatuses.get(String(group.main.status || 'unknown')) !== false);
    status.hidden = !snapshot.busy;
    status.textContent = snapshot.busy ? 'A development build is active.' : '';
    // Keep each panel's Advanced state and the strip position across a poll.
    const openKeys = new Set();
    for (const node of list.querySelectorAll('.task-panel')) {
      if (node.querySelector('details[data-advanced]')?.open) openKeys.add(node.dataset.task);
    }
    const previousStrip = list.querySelector('.qstrip');
    const keepScroll = previousStrip ? previousStrip.scrollLeft : 0;
    list.replaceChildren();
    tickers = [];
    panelNodes = [];
    if (!visible.length) {
      status.hidden = false;
      status.textContent = activeOnly ? 'No active builds match the filters.'
        : jobs.length ? 'No builds match the filters.' : 'No development builds recorded.';
      return;
    }
    if (linkedId && !jobs.some(job => job.job_id === linkedId)) {
      const notice = document.createElement('p');
      notice.className = 'build-missing';
      notice.textContent = 'The linked job is outside the recent build list.';
      list.append(notice);
    }
    const running = visible.filter(group => group.main.status === 'running').length;
    const queued = visible.filter(group => group.main.status === 'queued').length;
    const decisions = visible.filter(group => group.main.status === 'needs_decision').length;
    const finished = visible.length - running - queued - decisions;
    const counts = [
      running ? running + ' building' : '',
      queued ? queued + ' queued' : '',
      decisions ? decisions + (decisions === 1 ? ' needs a decision' : ' need a decision') : '',
      finished ? finished + ' finished' : ''
    ].filter(Boolean).join(' · ') || 'idle';
    const head = document.createElement('div');
    head.className = 'bq-head';
    const heading = document.createElement('h2');
    heading.textContent = 'Build queue · ' + visible.length + ' task' + (visible.length === 1 ? '' : 's');
    const countLine = document.createElement('span');
    countLine.className = 'bq-counts';
    countLine.textContent = counts;
    head.append(heading, countLine);
    list.append(head);
    const firstActiveIndex = visible.findIndex(group => group.active);
    const aheadLine = document.createElement('p');
    aheadLine.className = 'bq-ahead';
    if (firstActiveIndex >= 0) {
      const first = visible[firstActiveIndex];
      const firstSnapshot = jobProgress(first, details.get(first.task_id)).snapshot;
      aheadLine.textContent = 'First in the queue: ' + groupName(first) + ' · '
        + aheadText(first, firstActiveIndex, visible.length, firstSnapshot);
    } else {
      aheadLine.textContent = 'No build is running · ' + visible.length + ' task' + (visible.length === 1 ? '' : 's') + ' in the queue.';
    }
    list.append(aheadLine);
    const stripWrap = document.createElement('div');
    stripWrap.className = 'qstrip-wrap';
    const strip = document.createElement('div');
    strip.className = 'qstrip';
    strip.setAttribute('aria-label', 'Build queue');
    const leftChevron = chevronButton('‹', 'Scroll the queue left', 'left');
    const rightChevron = chevronButton('›', 'Scroll the queue right', 'right');
    leftChevron.addEventListener('click', () => strip.scrollBy({left: -160, behavior: 'smooth'}));
    rightChevron.addEventListener('click', () => strip.scrollBy({left: 160, behavior: 'smooth'}));
    visible.forEach((group, index) => strip.append(queueChip(group, index, visible.length)));
    stripWrap.append(leftChevron, strip, rightChevron);
    list.append(stripWrap);
    // Fades only the edge that still has content, and shows only the chevron that can still move.
    stripSync = () => {
      const max = strip.scrollWidth - strip.clientWidth;
      const left = strip.scrollLeft > 1;
      const right = max > 1 && strip.scrollLeft < max - 1;
      leftChevron.classList.toggle('visible', left);
      rightChevron.classList.toggle('visible', right);
      strip.classList.toggle('fade-left', left);
      strip.classList.toggle('fade-right', right);
    };
    strip.addEventListener('scroll', stripSync, {passive: true});
    requestAnimationFrame(() => { strip.scrollLeft = keepScroll; stripSync(); });
    visible.forEach((group, index) => {
      const node = taskPanel(group, index, visible.length, firstActiveIndex, openKeys, linkedId);
      panelNodes.push({key: group.key, node});
      list.append(node);
    });
    void loadDetails(groups);
  }
  // Each open note: its text, then which task it is on, who left it and when.
  function renderBoard(feed) {
    const notes = Array.isArray(feed?.notes) ? feed.notes : [];
    boardList.replaceChildren();
    board.hidden = !notes.length;
    boardCount.textContent = String(notes.length + (feed?.more || 0));
    for (const note of [...notes].reverse()) {
      const card = document.createElement('article');
      card.className = 'build-retained-notice';
      const text = document.createElement('p');
      text.textContent = note.text;
      const detail = document.createElement('p');
      // A session's note on its own task names the task once.
      const from = note.from_task && note.from_task !== note.to_task ? ' from ' + note.from_task : '';
      const who = note.author + (note.session ? ' (' + note.session + ')' : '');
      const when = Number.isFinite(note.at) ? new Date(note.at * 1000).toLocaleString() : '';
      detail.textContent = 'On ' + note.to_task + from + ' · ' + who + (note.verified ? '' : ' · unverified') +
        (when ? ' · ' + when : '');
      card.append(text, detail);
      boardList.append(card);
    }
    if (feed?.more) {
      const rest = document.createElement('p');
      rest.textContent = feed.more + ' older notes not shown.';
      boardList.append(rest);
    }
  }
  async function load() {
    if (loading || stopped || document.hidden) return;
    loading = true;
    update.disabled = true;
    try {
      const response = await fetch('/api/v3/admin/builds', {credentials: 'same-origin', cache: 'no-store'});
      if (!response.ok) throw Error('Build status unavailable (' + response.status + ')');
      const data = await response.json();
      if (!Array.isArray(data.jobs)) throw Error('Build status response is invalid');
      if (!stopped) { snapshot = data; onStatus(data.jobs.filter(isActive).length); render(); }
    } catch (error) {
      if (!stopped) { leftovers.hidden = true; onStatus(0); status.hidden = false; status.textContent = error.message; }
    } finally {
      loading = false;
      update.disabled = false;
    }
  }
  update.addEventListener('click', () => { void load(); });
  void load();
  const timer = setInterval(load, 5000);
  const ticker = setInterval(() => { for (const draw of tickers) draw(); }, 1000);
  const onResize = () => stripSync();
  window.addEventListener('resize', onResize);
  return () => {
    stopped = true;
    scheduler.destroy();
    clearInterval(timer);
    clearInterval(ticker);
    clearTimeout(flashTimer);
    window.removeEventListener('resize', onResize);
    onStatus(0);
  };
}
