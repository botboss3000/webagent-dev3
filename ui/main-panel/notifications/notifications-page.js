'use strict';
// Notifications: every signed-in user's own notifications (storage nearly full, what an
// automatic cleanup removed), then, for administrators, security alerts raised under the
// live admin-alerts.json with that file's event rules and delivery channels shown read-only.

const LABELS = {
  login_failed: 'Failed sign-ins',
  login_blocked: 'Sign-in blocks',
  rate_limited: 'Rate-limited requests',
  host_approval: 'Host actions waiting for approval',
};
const NOTICE_LABELS = {
  storage_level: 'Storage',
  storage_full: 'Storage full',
  storage_cleanup: 'Automatic cleanup',
};
const CHANNELS = {in_app: 'In-app (this page)', ntfy: 'ntfy push', email: 'Email', twilio: 'SMS (Twilio)'};

function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

function when(seconds) {
  return new Date(seconds * 1000).toLocaleString([], {dateStyle: 'medium', timeStyle: 'short'});
}

function minutes(seconds) {
  const value = Math.ceil(seconds / 60);
  return value >= 60 && value % 60 === 0 ? (value / 60) + ' h' : value + ' min';
}

export function start(mount) {
  const badges = ['tab-button-notifications', 'mobile-nav-notifications']
    .map(id => document.getElementById(id)).filter(Boolean)
    .map(target => {
      const badge = el('span', 'alerts-badge');
      badge.hidden = true;
      target.append(badge);
      return badge;
    });
  let me = null;
  let timer = null;

  function setUnread(count) {
    for (const badge of badges) {
      badge.hidden = !count;
      badge.textContent = count > 99 ? '99+' : String(count);
    }
  }

  async function markRead(path, body, button) {
    button.disabled = true;
    try {
      const response = await fetch(path, {
        method: 'POST', credentials: 'same-origin',
        headers: {'Content-Type': 'application/json', 'x-csrf-token': me.csrf_token},
        body: JSON.stringify(body),
      });
      if (!response.ok) throw Error('Could not mark as read');
      await load();
    } catch (error) {
      button.disabled = false;
      button.textContent = error.message;
    }
  }

  function list(items, path, label) {
    const list = el('ul', 'alerts-list');
    for (const item of items) {
      const row = el('li', 'alert-item');
      row.dataset.severity = item.severity;
      row.dataset.read = String(item.read_at !== null);
      const top = el('div', 'alert-top');
      top.append(el('span', 'alert-message', item.message));
      if (item.read_at === null) {
        const read = el('button', '', 'Mark read');
        read.type = 'button';
        read.onclick = () => markRead(path, {id: item.id}, read);
        top.append(read);
      }
      row.append(top, el('span', 'alert-meta', when(item.created_at) + ' · ' + label(item)));
      list.append(row);
    }
    return list;
  }

  function header(title, unread, path) {
    const head = el('div', 'alerts-head');
    head.append(el('h2', '', title + (unread ? ' (' + unread + ' unread)' : '')));
    if (unread) {
      const all = el('button', '', 'Mark all read');
      all.type = 'button';
      all.onclick = () => markRead(path, {}, all);
      head.append(all);
    }
    return head;
  }

  function renderNotices(page) {
    const section = el('section');
    section.append(header('Your notifications', page.unread, '/api/v3/notices/read'));
    section.append(page.notices.length
      ? list(page.notices, '/api/v3/notices/read', item => NOTICE_LABELS[item.kind] || item.kind)
      : el('p', 'alerts-empty', 'Nothing new. Storage warnings and automatic cleanups appear here.'));
    return section;
  }

  function renderAlerts(page) {
    const wrap = el('section');
    wrap.append(header('Administrator alerts', page.unread, '/api/v3/admin/alerts/read'));
    wrap.append(page.alerts.length
      ? list(page.alerts, '/api/v3/admin/alerts/read', item => item.severity + ' · ' + (LABELS[item.kind] || item.kind))
      : el('p', 'alerts-empty', 'No alerts in the last ' + page.config.retention_days + ' days.'));

    const rules = el('section');
    rules.append(el('h2', '', 'What raises an alert'),
      el('p', 'alerts-help', 'Set in config/notifications/admin-alerts.json (or ask the configuration assistant). Alerts are kept ' + page.config.retention_days + ' days.'));
    const ruleRows = el('div', 'alerts-rows');
    for (const [kind, rule] of Object.entries(page.config.events)) {
      const row = el('div', 'alerts-row');
      const pill = el('span', 'alerts-pill', rule.notify ? 'On' : 'Off');
      pill.dataset.on = String(rule.notify);
      row.append(el('strong', '', LABELS[kind] || kind),
        el('span', '', rule.threshold + ' within ' + minutes(rule.window_seconds) + ' · ' + rule.severity), pill);
      ruleRows.append(row);
    }
    rules.append(ruleRows);

    const channels = el('section');
    channels.append(el('h2', '', 'Delivery'));
    const channelRows = el('div', 'alerts-rows');
    for (const [name, channel] of Object.entries(page.config.channels)) {
      const row = el('div', 'alerts-row');
      const state = channel.available ? (channel.enabled ? 'On' : 'Off') : 'Not available yet';
      const pill = el('span', 'alerts-pill', state);
      pill.dataset.on = String(channel.enabled);
      row.append(el('strong', '', CHANNELS[name] || name), pill);
      channelRows.append(row);
    }
    channels.append(channelRows);
    return [wrap, rules, channels];
  }

  async function read(path) {
    const response = await fetch(path, {credentials: 'same-origin', cache: 'no-store'});
    if (!response.ok) throw Error('Notifications unavailable');
    return response.json();
  }

  async function load() {
    const [notices, alerts] = await Promise.all([
      read('/api/v3/notices'),
      me.is_admin ? read('/api/v3/admin/alerts') : null,
    ]);
    setUnread(notices.unread + (alerts ? alerts.unread : 0));
    const wrap = el('div', 'alerts-wrap');
    wrap.append(renderNotices(notices));
    if (alerts) wrap.append(...renderAlerts(alerts));
    mount.replaceChildren(wrap);
  }

  async function syncIdentity() {
    clearInterval(timer);
    timer = null;
    setUnread(0);
    mount.replaceChildren();
    const response = await fetch('/api/v3/auth/me', {credentials: 'same-origin'});
    me = response.ok ? await response.json() : null;
    if (!me?.csrf_token) return;
    const refresh = () => load().catch(error => { mount.textContent = error.message; });
    await refresh();
    timer = setInterval(refresh, 60000);
  }
  window.addEventListener('v3-identity-changed', () => { void syncIdentity(); });
  void syncIdentity();
}
