'use strict';
// Administrator Host Admin: approve or deny host actions held by the root host helper, open
// time-limited allow-all windows for one requester, and read recent decisions. The helper
// itself runs approved actions and never lets allow-all cover boot, disk or no-undo actions.
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

function who(requester) {
  if (requester.channel === 'ssh') return 'SSH key ' + (requester.identity || '?');
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

  async function post(path, body, button) {
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
      button.textContent = error.message.slice(0, 80);
    }
  }

  function card(request, pending) {
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
      // Boot and disk changes need the host approval passphrase, set at the server console.
      let passphrase = null;
      if (request.boot_disk) {
        passphrase = el('input');
        passphrase.type = 'password';
        passphrase.autocomplete = 'off';
        passphrase.placeholder = 'Host approval passphrase';
        passphrase.setAttribute('aria-label', 'Host approval passphrase');
        actions.append(passphrase);
      }
      for (const [button, decision] of [[approve, 'approve'], [deny, 'deny']]) {
        button.type = 'button';
        button.onclick = () => post('/api/v3/admin/host/requests/' + encodeURIComponent(request.id) + '/decision',
          decision === 'approve' && passphrase ? {decision, passphrase: passphrase.value} : {decision}, button);
      }
      actions.append(approve, deny);
      if (request.allow_key && request.undo !== 'none' && !request.boot_disk) {
        const select = el('select');
        select.setAttribute('aria-label', 'Allow-all window for this requester');
        for (const [seconds, label] of WINDOWS.slice(1)) select.append(new Option('Allow all for ' + label, String(seconds)));
        const open = el('button', '', 'Open window');
        open.type = 'button';
        open.onclick = () => post('/api/v3/admin/host/allow', {requester: request.allow_key, seconds: Number(select.value)}, open);
        actions.append(select, open);
      }
      item.append(actions);
    }
    return item;
  }

  function releaseCard(release) {
    const waiting = release.status === 'awaiting_approval';
    const item = el('li', 'host-item');
    item.dataset.state = waiting ? 'pending' : release.status;
    const top = el('div', 'host-top');
    top.append(el('strong', 'host-action', 'Production release v' + release.version),
      el('span', 'host-pill', RELEASE_STATES[release.status] || release.status));
    item.append(top);
    if (release.notes) item.append(el('pre', 'host-preview', release.notes));
    const stage = release.status === 'running' && release.stage ? ' · stage ' + release.stage.replace('_', ' ') : '';
    item.append(el('span', 'host-meta', 'Requested by ' + release.requested_by + ' · ' + when(release.requested_at)
      + ' · ' + release.bump + ' bump from dev commit ' + String(release.commit || '').slice(0, 10) + stage
      + (release.decided_by ? ' · decided by ' + release.decided_by : '')));
    if (release.error) item.append(el('span', 'host-error', release.error));
    if (waiting) {
      const actions = el('div', 'host-actions');
      const approve = el('button', 'host-approve', 'Approve release');
      const deny = el('button', '', 'Deny');
      for (const [button, decision] of [[approve, 'approve'], [deny, 'deny']]) {
        button.type = 'button';
        button.onclick = () => post('/api/v3/admin/production/' + encodeURIComponent(release.id) + '/decision', {decision}, button);
      }
      actions.append(approve, deny);
      item.append(actions);
    }
    return item;
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
    for (const release of page.releases.slice(0, 5)) list.append(releaseCard(release));
    box.append(page.releases.length ? list : el('p', 'host-empty', 'No production releases yet.'));
    return box;
  }

  // Page-wide allow-all: every requester's ordinary actions run without a card until it ends,
  // for example overnight work. Boot, disk and no-undo actions still wait here.
  function allowAll(windows) {
    const box = el('div', 'host-allow-all');
    const until = windows['*'];
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
    toggle.onchange = () => post('/api/v3/admin/host/allow', {requester: '*', seconds: toggle.checked ? Number(select.value) : 0}, toggle);
    box.append(label, select, el('span', 'host-meta', until
      ? 'On until ' + when(until) + '. Boot, disk and no-undo actions still ask.'
      : 'Off. Turn on for unattended work, such as overnight tasks.'));
    const others = Object.entries(windows).filter(([key]) => key !== '*');
    if (others.length) box.append(el('span', 'host-meta', 'Per-requester windows: ' + others
      .map(([key, end]) => (key.startsWith('ssh:') ? 'SSH key ' + key.slice(4) : 'app session ' + key.slice(0, 8)) + ' until ' + when(end)).join(', ')));
    return box;
  }

  function render(page, production) {
    const wrap = el('div', 'host-wrap');
    wrap.append(el('h2', '', 'Waiting for approval' + (page.pending.length ? ' (' + page.pending.length + ')' : '')));
    wrap.append(el('p', 'host-help', 'Host actions from agents and SSH release keys wait here until you decide. An allow-all window approves one requester\'s ordinary actions until it ends; boot, disk and no-undo actions always ask.'));
    wrap.append(allowAll(page.windows || {}));
    const pending = el('ul', 'host-list');
    for (const request of page.pending) pending.append(card(request, true));
    wrap.append(page.pending.length ? pending : el('p', 'host-empty', 'Nothing is waiting.'));
    wrap.append(releases(production));
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
    const refresh = () => load().catch(error => { mount.textContent = error.message; });
    await refresh();
    timer = setInterval(refresh, 15000);
  }
  window.addEventListener('v3-identity-changed', () => { void syncIdentity(); });
  void syncIdentity();
}
