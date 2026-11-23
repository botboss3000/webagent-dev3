'use strict';
// Notifications: every signed-in user's own notifications (storage nearly full, what an
// automatic cleanup removed), then, for administrators, the security alerts section from
// ../Administrator/notifications/alerts.js (served only to signed-in administrators).

const NOTICE_LABELS = {
  storage_level: 'Storage',
  storage_full: 'Storage full',
  storage_cleanup: 'Automatic cleanup',
};

function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

function when(seconds) {
  return new Date(seconds * 1000).toLocaleString([], {dateStyle: 'medium', timeStyle: 'short'});
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

  async function read(path) {
    const response = await fetch(path, {credentials: 'same-origin', cache: 'no-store'});
    if (!response.ok) throw Error('Notifications unavailable');
    return response.json();
  }

  async function load() {
    const [notices, alerts, section] = await Promise.all([
      read('/api/v3/notices'),
      me.is_admin ? read('/api/v3/admin/alerts') : null,
      // Missing for any sign-in the server does not treat as an administrator's own.
      me.is_admin ? import('../Administrator/notifications/alerts.js').catch(() => null) : null,
    ]);
    const shown = section ? alerts : null;
    setUnread(notices.unread + (shown ? shown.unread : 0));
    const wrap = el('div', 'alerts-wrap');
    wrap.append(renderNotices(notices));
    if (shown) wrap.append(...section.renderAlerts(shown, {el, header, list}));
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
