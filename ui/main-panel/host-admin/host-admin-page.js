'use strict';
// Administrator Host Admin: approve or deny host actions held by the root host helper, open
// time-limited allow-all windows for one requester, and read recent decisions. The helper
// itself runs approved actions and never lets allow-all cover boot, disk or no-undo actions.

const UNDO = {exact: 'Exact undo', best_effort: 'Best-effort undo', none: 'No undo'};
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
      + (pending ? ' · expires ' + when(request.expires_at) : request.decided_by ? ' · by ' + request.decided_by : ''));
    item.append(top, preview, meta);
    if (request.error) item.append(el('span', 'host-error', request.error));
    if (pending) {
      const actions = el('div', 'host-actions');
      const approve = el('button', 'host-approve', 'Approve');
      const deny = el('button', '', 'Deny');
      for (const [button, decision] of [[approve, 'approve'], [deny, 'deny']]) {
        button.type = 'button';
        button.onclick = () => post('/api/v3/admin/host/requests/' + encodeURIComponent(request.id) + '/decision', {decision}, button);
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

  function render(page) {
    const wrap = el('div', 'host-wrap');
    wrap.append(el('h2', '', 'Waiting for approval' + (page.pending.length ? ' (' + page.pending.length + ')' : '')));
    wrap.append(el('p', 'host-help', 'Host actions from agents and SSH release keys wait here until you decide. An allow-all window approves one requester\'s ordinary actions until it ends; boot, disk and no-undo actions always ask.'));
    const pending = el('ul', 'host-list');
    for (const request of page.pending) pending.append(card(request, true));
    wrap.append(page.pending.length ? pending : el('p', 'host-empty', 'Nothing is waiting.'));
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
    setPending(page.pending.length);
    render(page);
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
