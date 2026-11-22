'use strict';
// Administrator Host Admin: approve or deny host actions held by the root host helper, open
// time-limited allow-all windows for one requester, and read recent decisions. The helper
// itself runs approved actions and never lets allow-all cover boot, disk or no-undo actions.
// Root-equivalent approvals and opening a window need the host approval passphrase; where the
// host's window_openers leaves out "app", windows are opened only over SSH and this page
// shows that command and the time left instead.
// Production releases requested by agents or the operator wait here too; only this page
// approves them, and the build broker then builds and publishes the release.

const UNDO = {exact: 'Exact undo', best_effort: 'Best-effort undo', none: 'No undo'};
const RELEASE_STATES = {awaiting_approval: 'Waiting for approval', approved: 'Approved', running: 'Running',
  published: 'Published', failed: 'Failed', denied: 'Denied', interrupted: 'Interrupted'};
const WINDOWS = [[0, 'End'], [900, '15 min'], [3600, '1 hour'], [14400, '4 hours'], [86400, '24 hours']];

function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

function when(seconds) {
  return new Date(seconds * 1000).toLocaleString([], {dateStyle: 'medium', timeStyle: 'short'});
}

// "3 h 20 min left" until the epoch-seconds time `until`.
function left(until) {
  const minutes = Math.max(0, Math.round((until - Date.now() / 1000) / 60));
  return (minutes >= 60 ? Math.floor(minutes / 60) + ' h ' : '') + (minutes % 60) + ' min left';
}

function passphraseInput() {
  const input = el('input');
  input.type = 'password';
  input.autocomplete = 'off';
  input.placeholder = 'Host approval passphrase';
  input.setAttribute('aria-label', 'Host approval passphrase');
  return input;
}

function who(requester) {
  if (requester.channel === 'ssh') return 'SSH key ' + (requester.identity || '?');
  if (!requester.session && requester.user) return 'File Manager, administrator ' + requester.user;
  return 'App session ' + String(requester.session || '?').slice(0, 8);
}

export function start(mount) {
  const badges = ['tab-button-host-admin', 'mobile-nav-host-admin']
    .map(id => document.getElementById(id)).filter(Boolean)
    .map(target => {
      const badge = el('span', 'host-badge');
      badge.hidden = true;
      target.append(badge);
      return badge;
    });
  let me = null;
  let timer = null;

  function setPending(count) {
    for (const badge of badges) {
      badge.hidden = !count;
      badge.textContent = count > 99 ? '99+' : String(count);
    }
  }

  // `shown` takes the error text when `button` cannot (a checkbox); a failed checkbox flips back.
  // A request that needs the passphrase is never sent with an empty one: the helper counts
  // every wrong entry towards its 15-minute lock.
  async function post(path, body, button, shown) {
    if ('passphrase' in body && !body.passphrase) {
      if (button.type === 'checkbox') button.checked = !button.checked;
      (shown || button).textContent = 'Type the host approval passphrase first';
      return;
    }
    button.disabled = true;
    try {
      const response = await fetch(path, {
        method: 'POST', credentials: 'same-origin',
        headers: {'Content-Type': 'application/json', 'x-csrf-token': me.csrf_token},
        body: JSON.stringify(body),
      });
      if (!response.ok) throw Error((await response.text()) || 'Request failed');
      await load();
    } catch (error) {
      button.disabled = false;
      if (button.type === 'checkbox') button.checked = !button.checked;
      (shown || button).textContent = error.message.slice(0, 80);
    }
  }

  function card(request, pending, appOpens) {
    const item = el('li', 'host-item');
    item.dataset.undo = request.undo;
    item.dataset.state = request.state;
    const top = el('div', 'host-top');
    top.append(el('strong', 'host-action', request.action), el('span', 'host-pill', pending ? UNDO[request.undo] || request.undo : request.state));
    const preview = el('pre', 'host-preview', request.preview);
    const meta = el('span', 'host-meta', who(request.requester) + ' · ' + when(request.created_at)
      + (pending ? (request.expires_at ? ' · expires ' + when(request.expires_at) : '') : request.decided_by ? ' · by ' + request.decided_by : ''));
    item.append(top, preview, meta);
    if (pending && request.boot_disk) item.append(el('span', 'host-error', 'Boot or disk change: a mistake can stop the server booting.'));
    if (request.error) item.append(el('span', 'host-error', request.error));
    if (pending) {
      const actions = el('div', 'host-actions');
      const approve = el('button', 'host-approve', 'Approve');
      const deny = el('button', '', 'Deny');
      // Boot and disk changes, File Manager access outside the checkout and root-equivalent
      // actions need the host approval passphrase, set at the server console. So does opening a
      // window from here.
      const needsPass = request.boot_disk || request.passphrase || request.root;
      const canWindow = appOpens && request.allow_key && request.undo !== 'none' && !request.boot_disk && !request.passphrase;
      let passphrase = null;
      if (needsPass || canWindow) {
        passphrase = passphraseInput();
        actions.append(passphrase);
      }
      // File Manager grants last the time set here: 1 h by default, at most 24 h.
      let hours = null;
      if (request.action === 'files_grant' || request.action === 'terminal_grant') {
        hours = el('input');
        hours.type = 'number';
        hours.inputMode = 'decimal';
        hours.min = '0.25';
        hours.max = '24';
        hours.step = '0.25';
        hours.value = '1';
        hours.className = 'host-hours';
        hours.setAttribute('aria-label', 'Hours of access');
        const label = el('label', 'host-meta');
        label.append(hours, document.createTextNode(' h'));
        actions.append(label);
      }
      for (const [button, decision] of [[approve, 'approve'], [deny, 'deny']]) {
        button.type = 'button';
        button.onclick = () => post('/api/v3/admin/host/requests/' + encodeURIComponent(request.id) + '/decision',
          decision === 'approve' && needsPass
            ? {decision, passphrase: passphrase.value, ...(hours ? {seconds: Math.round(Number(hours.value) * 3600)} : {})}
            : {decision}, button);
      }
      actions.append(approve, deny);
      if (canWindow) {
        const select = el('select');
        select.setAttribute('aria-label', 'Allow-all window for this requester');
        for (const [seconds, label] of WINDOWS.slice(1)) select.append(new Option('Allow all for ' + label, String(seconds)));
        const open = el('button', '', 'Open window');
        open.type = 'button';
        open.onclick = () => post('/api/v3/admin/host/allow', {requester: request.allow_key, seconds: Number(select.value), passphrase: passphrase.value}, open);
        actions.append(select, open);
      }
      item.append(actions);
    }
    return item;
  }

  // A release card in the Builds page's task-card form: status glyph, title and owner, state
  // line, then one row per stage with its time, the normal range from earlier published
  // releases and its state.
  const RELEASE_STAGES = [['export', 'Export'], ['credential_guard', 'Credential guard'], ['build', 'Stable build and tests'],
    ['installers', 'Installers'], ['push', 'Push release'], ['dev_tag', 'Tag dev commit'], ['upload', 'Upload installers']];
  const minutes = seconds => (seconds / 60).toFixed(1);

  function normalRange(name, history) {
    const times = history.map(r => r.stages?.[name]).filter(s => s?.started_at && s.finished_at)
      .map(s => s.finished_at - s.started_at).sort((a, b) => a - b);
    return times.length ? minutes(times[0]) + '–' + minutes(times[times.length - 1]) : 'no history';
  }

  function stageRows(release, history) {
    const table = el('div', 'tp-stages');
    table.setAttribute('role', 'table');
    const head = el('div', 'tp-stage tp-stage-head');
    head.setAttribute('role', 'row');
    for (const [label, cell] of [['#', 'tp-sid'], ['Stage', 'tp-sname'], ['Time', 'tp-time'], ['Normal', 'tp-normal'], ['Status', 'tp-state']]) head.append(el('span', cell, label));
    table.append(head);
    const stopped = ['failed', 'interrupted'].includes(release.status);
    RELEASE_STAGES.forEach(([name, label], at) => {
      const stage = (release.stages || {})[name];
      const state = !stage ? 'upcoming' : stage.finished_at ? 'done' : stopped ? 'failed' : 'running';
      // A stopped stage ran until the release ended; only a running one counts up to now.
      const end = stage?.finished_at || (stopped ? release.finished_at : null) || Date.now() / 1000;
      const seconds = !stage ? null : end - stage.started_at;
      const row = el('div', 'tp-stage tp-stage-' + state);
      row.setAttribute('role', 'row');
      // where: the sidekick, or the main server (with the reason) when it could not build there.
      const where = stage?.where ? ' (' + stage.where + ')' : '';
      row.append(el('span', 'tp-sid', String(at + 1)), el('span', 'tp-sname', label + where),
        el('span', 'tp-time', seconds == null ? '—' : minutes(seconds) + ' min'),
        el('span', 'tp-normal', normalRange(name, history)), el('span', 'tp-state', state === 'failed' ? 'stopped' : state));
      table.append(row);
    });
    return table;
  }

  function releaseCard(release, history) {
    const waiting = release.status === 'awaiting_approval';
    const live = ['approved', 'running'].includes(release.status);
    const tone = release.status === 'published' ? 'success' : ['failed', 'interrupted'].includes(release.status) ? 'danger'
      : live ? 'progress' : waiting ? 'pending' : 'idle';
    const card = el('li', 'build-card task-panel host-release');
    card.dataset.status = tone;
    card.dataset.active = String(live || waiting);
    const head = el('header', 'tp-head');
    const glyph = el('span', 'tp-glyph' + (live ? ' tp-spin' : ''), tone === 'success' ? '✓' : tone === 'danger' ? '!' : '●');
    glyph.setAttribute('role', 'img');
    glyph.setAttribute('aria-label', RELEASE_STATES[release.status] || release.status);
    const titles = el('div', 'tp-titles');
    titles.append(el('strong', 'tp-name', 'Production release v' + release.version),
      el('span', 'tp-meta', 'Requested by ' + release.requested_by + ' · ' + when(release.requested_at)
        + (release.decided_by ? ' · decided by ' + release.decided_by : '')));
    const side = el('div', 'tp-side');
    side.append(el('span', 'tp-pos', release.bump + ' bump'), el('span', 'tp-ab', 'dev ' + String(release.commit || '').slice(0, 10)));
    head.append(glyph, titles, side);
    card.append(head);
    const sequence = el('div', 'tp-seq');
    if (live) sequence.append(el('span', 'tp-label', 'Building now'));
    const stageName = RELEASE_STAGES.find(([name]) => name === release.stage)?.[1];
    sequence.append(el('span', '', (RELEASE_STATES[release.status] || release.status) + (live && stageName ? ' · ' + stageName : '')));
    card.append(sequence);
    const begun = Object.values(release.stages || {}).map(s => s.started_at).filter(Boolean);
    if (begun.length) {
      const total = (release.finished_at || Date.now() / 1000) - Math.min(...begun);
      card.append(el('p', 'tp-counter', (live ? 'running for ' : 'finished in ') + minutes(total) + ' min'));
    }
    if (release.notes) card.append(el('p', 'tp-notes', release.notes));
    if (!waiting && release.status !== 'denied') card.append(stageRows(release, history));
    const installers = release.installers || {};
    if (installers.files?.length) card.append(el('p', 'tp-notes', 'Installers: ' + installers.files.join(', ')));
    for (const problem of [release.error, installers.error, installers.apk].filter(Boolean)) card.append(el('p', 'host-error', problem));
    if (waiting) {
      const actions = el('div', 'host-actions');
      const approve = el('button', 'host-approve', 'Approve release');
      const deny = el('button', '', 'Deny');
      for (const [button, decision] of [[approve, 'approve'], [deny, 'deny']]) {
        button.type = 'button';
        button.onclick = () => post('/api/v3/admin/production/' + encodeURIComponent(release.id) + '/decision', {decision}, button);
      }
      actions.append(approve, deny);
      card.append(actions);
    }
    return card;
  }

  function releases(page) {
    const box = el('section', 'host-releases');
    box.append(el('h2', '', 'Production releases'));
    if (!page) {
      box.append(el('p', 'host-empty', 'Production release status is unavailable.'));
      return box;
    }
    box.append(el('p', 'host-help', 'Approving builds the stable app from the requested dev commit, then pushes it as one release commit to the public repository. Published version: '
      + (page.published_version ? 'v' + page.published_version : 'none yet') + '.'));
    const list = el('ul', 'host-list');
    const history = page.releases.filter(r => r.status === 'published');
    for (const release of page.releases.slice(0, 5)) list.append(releaseCard(release, history));
    box.append(page.releases.length ? list : el('p', 'host-empty', 'No production releases yet.'));
    return box;
  }

  // Page-wide allow-all: every requester's ordinary actions run without a card until it ends,
  // for example overnight work. Boot, disk and no-undo actions still wait here. Opening needs the
  // passphrase; ending never does. Without "app" in window_openers it opens only over SSH.
  function allowAll(windows, appOpens) {
    const box = el('div', 'host-allow-all');
    const until = windows['*'];
    const status = el('span', 'host-meta', until
      ? 'On until ' + when(until) + ' (' + left(until) + '). Boot, disk and no-undo actions still ask.'
      : appOpens ? 'Off. Turn on for unattended work, such as overnight tasks.' : 'Off.');
    if (appOpens) {
      const label = el('label', 'host-allow-toggle');
      const toggle = el('input');
      toggle.type = 'checkbox';
      toggle.checked = !!until;
      label.append(toggle, el('span', '', 'Allow all requesters'));
      const select = el('select');
      select.setAttribute('aria-label', 'Allow-all duration');
      for (const [seconds, text] of WINDOWS.slice(1)) select.append(new Option('for ' + text, String(seconds)));
      select.value = '86400';
      select.hidden = !!until;
      const passphrase = passphraseInput();
      passphrase.hidden = !!until;
      toggle.onchange = () => post('/api/v3/admin/host/allow', toggle.checked
        ? {requester: '*', seconds: Number(select.value), passphrase: passphrase.value}
        : {requester: '*', seconds: 0}, toggle, status);
      box.append(label, select, passphrase, status);
    } else {
      const ssh = el('p', 'host-help', 'On this host, windows are opened on the server over SSH: ');
      ssh.append(el('code', '', 'sudo buildctl host allow <hours>'));
      box.append(ssh, status);
      if (until) {
        const end = el('button', '', 'End now');
        end.type = 'button';
        end.onclick = () => post('/api/v3/admin/host/allow', {requester: '*', seconds: 0}, end);
        box.append(end);
      }
    }
    const others = Object.entries(windows).filter(([key]) => key !== '*');
    if (others.length) box.append(el('span', 'host-meta', 'Per-requester windows: ' + others
      .map(([key, end]) => (key.startsWith('ssh:') ? 'SSH key ' + key.slice(4) : 'app session ' + key.slice(0, 8)) + ' until ' + when(end) + ' (' + left(end) + ')').join(', ')));
    return box;
  }

  function hasGrants(page) {
    return Object.keys(page.file_grants || {}).length + Object.keys(page.terminal_grants || {}).length > 0;
  }

  // File Manager outside the checkout: live grants (revoke needs no passphrase) and the
  // newest changes the host file service made.
  function fileAccess(page) {
    const box = el('section', 'host-files');
    if (hasGrants(page)) box.classList.add('is-active');
    box.append(el('h2', '', 'File Manager outside the checkout'));
    const grants = [...Object.entries(page.file_grants || {}).map(([admin, until]) => ['files', admin, until]),
      ...Object.entries(page.terminal_grants || {}).map(([admin, until]) => ['terminal', admin, until])];
    box.append(el('p', 'host-help', grants.length
      ? 'File and terminal access both act as the operator account, like a login on this computer; anything needing root asks for its sudo password. App data and sockets stay closed to the File Manager.'
      : 'No administrator can browse outside the checkout or open a terminal. The File Manager asks here (or in its own prompt).'));
    const list = el('ul', 'host-list');
    for (const [kind, admin, until] of grants) {
      const item = el('li', 'host-item');
      item.append(el('strong', '', admin + (kind === 'terminal' ? ' · terminal' : ' · files')),
        el('span', 'host-meta', 'Until ' + when(until)));
      const revoke = el('button', '', 'Revoke');
      revoke.type = 'button';
      revoke.onclick = () => post('/api/v3/admin/host/files/revoke', {admin, kind}, revoke);
      const actions = el('div', 'host-actions');
      actions.append(revoke);
      item.append(actions);
      list.append(item);
    }
    if (grants.length) box.append(list);
    const log = page.file_log || [];
    if (log.length) {
      const details = el('details', 'host-files-log');
      details.append(el('summary', '', 'Recent file changes (' + log.length + ')'));
      const changes = el('ul', 'host-list');
      for (const entry of log) {
        changes.append(el('li', 'host-meta', when(entry.at) + ' · ' + entry.admin + ' · ' + entry.op + ' ' + entry.path
          + (entry.to ? ' → ' + entry.to : '') + (entry.bytes !== undefined ? ' (' + entry.bytes + ' bytes)' : '')));
      }
      details.append(changes);
      box.append(details);
    }
    return box;
  }

  function render(page, production) {
    const wrap = el('div', 'host-wrap');
    // A live File Manager approval: pin it to the top so Revoke is one tap away.
    const pinned = hasGrants(page);
    if (pinned) wrap.append(fileAccess(page));
    wrap.append(el('h2', '', 'Waiting for approval' + (page.pending.length ? ' (' + page.pending.length + ')' : '')));
    const appOpens = (page.window_openers || []).includes('app');
    wrap.append(el('p', 'host-help', 'Host actions from agents and SSH release keys wait here until you decide. Root-equivalent actions (file writes, reverts, broker installs, helper updates, package upgrades) need the host approval passphrase unless a window covers them. An allow-all window approves one requester\'s ordinary and root-equivalent actions until it ends; boot, disk and no-undo actions always ask.'));
    wrap.append(allowAll(page.windows || {}, appOpens));
    const pending = el('ul', 'host-list');
    for (const request of page.pending) pending.append(card(request, true, appOpens));
    wrap.append(page.pending.length ? pending : el('p', 'host-empty', 'Nothing is waiting.'));
    wrap.append(releases(production));
    if (!pinned) wrap.append(fileAccess(page));
    wrap.append(el('h2', '', 'Recent'));
    const recent = el('ul', 'host-list');
    for (const request of page.recent) recent.append(card(request, false));
    wrap.append(page.recent.length ? recent : el('p', 'host-empty', 'No host actions yet.'));
    mount.replaceChildren(wrap);
  }

  async function load() {
    const response = await fetch('/api/v3/admin/host/requests', {credentials: 'same-origin', cache: 'no-store'});
    if (!response.ok) throw Error('Host helper unavailable');
    const page = await response.json();
    // A broker that cannot answer must not hide the host requests.
    const production = await fetch('/api/v3/admin/production', {credentials: 'same-origin', cache: 'no-store'})
      .then(reply => reply.ok ? reply.json() : null).catch(() => null);
    const waiting = production ? production.releases.filter(r => r.status === 'awaiting_approval').length : 0;
    setPending(page.pending.length + waiting);
    render(page, production);
  }

  async function syncIdentity() {
    clearInterval(timer);
    timer = null;
    setPending(0);
    mount.replaceChildren();
    const response = await fetch('/api/v3/auth/me', {credentials: 'same-origin'});
    me = response.ok ? await response.json() : null;
    if (!me?.is_admin) return;
    const refresh = () => {
      // Never wipe a passphrase being typed.
      if ([...mount.querySelectorAll('input[type=password]')].some(input => input.value)) return Promise.resolve();
      return load().catch(error => { mount.textContent = error.message; });
    };
    await refresh();
    timer = setInterval(refresh, 15000);
  }
  window.addEventListener('v3-identity-changed', () => { void syncIdentity(); });
  void syncIdentity();
}
