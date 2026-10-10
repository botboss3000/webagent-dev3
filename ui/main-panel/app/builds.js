'use strict';

const phaseLabel = value => String(value || 'waiting').replaceAll('_', ' ');
const when = value => Number.isFinite(value) ? new Date(value * 1000).toLocaleString() : '—';
const duration = (start, end) => Number.isFinite(start)
  ? (Math.max(0, (Number.isFinite(end) ? end : Date.now() / 1000) - start) / 60).toFixed(1) + ' min'
  : '—';
const deploymentPhases = [
  'capturing_rollback', 'stopping_loop', 'stopping_api',
  'renaming_api', 'creating_api', 'starting_api',
  'renaming_loop', 'creating_loop', 'starting_loop',
  'checking_api', 'checking_loop'
];
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
const isActive = job => job.status !== 'completed' && !stoppedStatuses.has(job.status);
const stateFor = job => job.status === 'needs_decision'
  ? {tone: 'danger', icon: '⚑', label: 'Decision required · source synchronization'}
  : job.status === 'completed'
  ? {tone: 'success', icon: '⚑', label: 'Complete'}
  : stoppedStatuses.has(job.status)
    ? {tone: 'danger', icon: '⚑', label: 'Incomplete · ' + phaseLabel(job.status)}
    : {tone: 'progress', icon: '●', label: 'In progress · ' + phaseLabel(job.status)};

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
  const header = document.createElement('div');
  header.className = 'build-header';
  const intro = document.createElement('div');
  const heading = document.createElement('h2');
  heading.textContent = 'Builds';
  const subtitle = document.createElement('p');
  subtitle.textContent = 'Development builds, source indexing, tests, and deployments';
  intro.append(heading, subtitle);
  const update = document.createElement('button');
  update.type = 'button';
  update.className = 'build-update';
  update.textContent = 'Update status';
  const status = document.createElement('p');
  status.className = 'build-summary';
  status.setAttribute('role', 'status');
  status.textContent = 'Loading build status…';
  const list = document.createElement('div');
  list.className = 'build-list';
  list.setAttribute('aria-label', 'Recent development builds');
  header.append(intro, update);
  const synchronization = document.createElement('div');
  synchronization.setAttribute('role', 'status');
  const toolbar = document.createElement('div');
  toolbar.className = 'build-toolbar';
  toolbar.append(filters, controls);
  panel.append(header, status, synchronization, toolbar, list);

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
  function render() {
    showSynchronization(synchronization, snapshot.synchronization);
    const jobs = snapshot.jobs;
    const statuses = [...new Set(jobs.map(job => String(job.status || 'unknown')))].sort();
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
      for (const job of jobs) {
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
    list.replaceChildren();
    // Keep resumable decision waits visible; finished and stopped receipts are inactive.
    const visibleJobs = jobs.filter(job => !dismissed.has(job.job_id)
      && (!activeOnly || isActive(job))
      && enabledStatuses.get(String(job.status || 'unknown')) !== false)
      .sort((a, b) => Number(isActive(b)) - Number(isActive(a)));
    if (!visibleJobs.length) {
      status.textContent = activeOnly ? 'No active builds match the filters.'
        : jobs.length ? 'No builds match the filters.' : 'No development builds recorded.';
      return;
    }
    status.textContent = snapshot.busy ? 'A development build is active.' : visibleJobs.length + ' recent builds';
    if (linkedId && !jobs.some(job => job.job_id === linkedId)) {
      const notice = document.createElement('p');
      notice.className = 'build-missing';
      notice.textContent = 'The linked job is outside the recent build list.';
      list.append(notice);
    }
    for (const job of visibleJobs) {
      const card = document.createElement('article');
      card.className = 'build-card';
      card.dataset.status = stateFor(job).tone;
      card.dataset.active = String(isActive(job));
      if (job.job_id === linkedId) card.classList.add('linked');
      const top = document.createElement('div');
      top.className = 'build-card-top';
      const title = document.createElement('h3');
      title.textContent = job.summary || job.task_id || job.job_id;
      const flag = document.createElement('span');
      flag.className = 'build-flag';
      const marker = document.createElement('span');
      marker.setAttribute('aria-hidden', 'true');
      marker.textContent = stateFor(job).icon;
      const label = document.createElement('span');
      label.textContent = stateFor(job).label;
      flag.append(marker, label);
      top.append(title, flag);
      card.append(top);
      const cancellation = job.cancellation || {};
      if (cancellation.available || (cancellation.requested && !stoppedStatuses.has(job.status) && job.status !== 'completed')) {
        const cancel = document.createElement('button');
        cancel.type = 'button';
        cancel.textContent = cancellation.requested ? 'Stop requested' :
          cancellation.mode === 'queued' ? 'Cancel queued job' :
          cancellation.mode === 'deployment_stop' ? 'Request deployment stop' : 'Stop build';
        cancel.disabled = cancellation.requested || cancelling.has(job.job_id);
        cancel.addEventListener('click', async () => {
          const affected = cancellation.affected_job_ids || [job.job_id];
          const warning = (affected.length > 1 ? 'This stops all ' + affected.length + ' jobs in the shared build. ' : '') +
            (cancellation.mode === 'deployment_stop' ? 'Deployment effects have started. Stopping may leave an uncertain release requiring reconciliation. ' : '') + 'Request stop?';
          if (!window.confirm(warning)) return;
          const requestId = cancelRequests.get(job.job_id) || crypto.randomUUID();
          cancelRequests.set(job.job_id, requestId);
          cancelling.add(job.job_id);
          cancelMessages.set(job.job_id, 'Requesting stop…');
          render();
          try {
            const identity = await fetch('/api/v3/auth/me', {credentials: 'same-origin', cache: 'no-store'});
            if (!identity.ok) throw Error('Sign in required');
            const me = await identity.json();
            if (!me.is_admin || stopped) throw Error('Administrator access required');
            const response = await fetch('/api/v3/admin/builds/' + encodeURIComponent(job.job_id) + '/cancel', {
              method: 'POST', credentials: 'same-origin',
              headers: {'Content-Type': 'application/json', 'x-csrf-token': me.csrf_token},
              body: JSON.stringify({request_id: requestId, expected_job_ids: affected})
            });
            const result = await response.json();
            if (!response.ok) throw Error(result.error || 'Stop request unavailable (' + response.status + ')');
            cancelMessages.set(job.job_id, 'Stop result: ' + phaseLabel(result.disposition));
          } catch (error) {
            cancelMessages.set(job.job_id, error.message + '. Update status before retrying the same request.');
          } finally {
            cancelling.delete(job.job_id);
            if (!stopped) { render(); void load(); }
          }
        });
        card.append(cancel);
      }
      if (cancelMessages.has(job.job_id)) {
        const note = document.createElement('p');
        note.setAttribute('role', 'status');
        note.textContent = cancelMessages.get(job.job_id);
        card.append(note);
      }
      if (job.queue_preflight) {
        const report = job.queue_preflight;
        const section = document.createElement('section');
        section.setAttribute('aria-label', 'Queue compatibility');
        const title = document.createElement('h4');
        title.textContent = 'Queue compatibility · ' + phaseLabel(report.status);
        const note = document.createElement('p');
        note.textContent = 'Checked before compilation. Shared builds are rechecked at admission, with up to eight tasks per build.';
        section.append(title, note);
        const peers = document.createElement('ul');
        for (const peer of report.peers || []) {
          const row = document.createElement('li');
          const link = document.createElement('a');
          link.href = '/app?tab=builds&job_id=' + encodeURIComponent(peer.job_id);
          link.textContent = peer.job_id;
          row.append(link, document.createTextNode(' · ' + peer.reason));
          if (peer.paths?.length) {
            const files = document.createElement('div');
            files.textContent = (peer.conflict_count ? 'Conflicting files: ' : 'Overlapping files: ') +
              peer.paths.join(', ') + (peer.paths_truncated ? ' (more files omitted)' : '');
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
        card.append(section);
      }
      const facts = document.createElement('dl');
      facts.className = 'build-facts';
      if (job.phase !== 'finished') fact(facts, 'Current phase', phaseLabel(job.phase));
      fact(facts, 'Rust tests', job.rust_tests?.status || 'not started');
      fact(facts, 'Test result', job.rust_tests?.summary || 'Awaiting a confirmed test result');
      fact(facts, 'Started', when(job.started_at || job.created_at));
      fact(facts, 'Elapsed', duration(job.created_at, job.finished_at));
      fact(facts, 'Job ID', job.job_id);
      fact(facts, 'Task ID', job.task_id || '—');
      fact(facts, 'Source', job.origin || '—');
      fact(facts, 'Input digest', job.inputs_sha256 || '—');
      for (const role of ['api', 'loop']) {
        const container = snapshot.containers?.[role] || {};
        fact(facts, 'Current ' + role.toUpperCase() + ' container',
          [container.state, container.health].filter(Boolean).join(' / ') || 'Unavailable');
      }
      card.append(facts);
      const sourceStatus = document.createElement('div');
      showSynchronization(sourceStatus, job.synchronization);
      card.append(sourceStatus);
      if (job.reason) {
        const reason = document.createElement('p');
        reason.className = 'build-reason';
        reason.textContent = job.reason;
        card.append(reason);
      }
      // Broker receipts own phase history. The known sequence only marks steps not yet reached.
      const phaseTitle = document.createElement('h4');
      phaseTitle.textContent = 'Build phases';
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
        row.textContent = phaseLabel(phase) + ' · ' + state +
          (history.has(phase) ? ' · ' + when(history.get(phase)) : '');
        phases.append(row);
      }
      card.append(phaseTitle, phases);
      if (job.log) {
        const log = document.createElement('details');
        const summary = document.createElement('summary');
        summary.textContent = 'Sanitized build log excerpt';
        const content = document.createElement('pre');
        content.className = 'build-log';
        content.textContent = job.log;
        log.append(summary, content);
        card.append(log);
      }
      list.append(card);
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
      if (!stopped) { onStatus(0); status.textContent = error.message; }
    } finally {
      loading = false;
      update.disabled = false;
    }
  }
  update.addEventListener('click', () => { void load(); });
  void load();
  const timer = setInterval(load, 5000);
  return () => { stopped = true; clearInterval(timer); onStatus(0); };
}
