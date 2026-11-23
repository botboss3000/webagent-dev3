'use strict';
// Administrator section of the Notifications page: security alerts raised under the live
// admin-alerts.json, with that file's event rules and delivery channels shown read-only.
// Served only to signed-in administrators; the page passes its own list helpers in.

const LABELS = {
  login_failed: 'Failed sign-ins',
  login_blocked: 'Sign-in blocks',
  rate_limited: 'Rate-limited requests',
  host_approval: 'Host actions waiting for approval',
  supply_chain_fixable: 'New fixable high/critical dependency findings',
  supply_chain_failed: 'Dependency scan failed',
  supply_chain_stale: 'Dependency scan stale',
};
const CHANNELS = {in_app: 'In-app (this page)', ntfy: 'ntfy push', email: 'Email', twilio: 'SMS (Twilio)'};

function minutes(seconds) {
  const value = Math.ceil(seconds / 60);
  return value >= 60 && value % 60 === 0 ? (value / 60) + ' h' : value + ' min';
}

export function renderAlerts(page, {el, header, list}) {
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
