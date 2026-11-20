'use strict';

// All Settings: every described setting of one page (the app, one agent, or the signed-in
// user), drawn from the configuration catalog (GET /api/v3/config/settings?page=) as shared
// config rows. The setting's type picks the control: boolean → switch, enum → dropdown,
// integer/number → stepper with its range and unit, length → width plus px/%, string → text.
// Each row saves alone through POST /api/v3/config/apply with the revision it was drawn
// from, so the server's validators, ranges and permission checks decide every save.
// A search box asks the configuration composer which setting a request means and opens it.

import { createSubpage, createSubpageList } from '../subpage-list/subpage-list.js';
import { createConfigList, createLengthRow, createNumberRow, createSelectRow, createTextRow,
  createToggleRow } from './config-rows.js';
import { createTextField } from '../controls/controls.js';
import { requestTouchVibration } from '../vibrate/vibrate.js';

const node = (tag, className = '', text = '') => {
  const element = document.createElement(tag);
  if (className) element.className = className;
  if (text) element.textContent = text;
  return element;
};
const SLIDERS = '<path d="M4 7h16"/><circle cx="9" cy="7" r="2.5"/><path d="M4 17h16"/><circle cx="15" cy="17" r="2.5"/>';
// Id segments too generic to name a row alone ("enabled" names nothing without its subject).
const GENERIC = new Set(['enabled', 'mode', 'max', 'min', 'seconds', 'minutes', 'limit', 'width', 'value', 'default']);
const UNIT_SUFFIX = /_(px|mb|kb|ms|tokens|seconds|minutes)$/;

const words = text => text.replace(/[_-]+/g, ' ').trim();
const capital = text => text.charAt(0).toUpperCase() + text.slice(1);
/** "chat.limits.attachments.max_file_size_mb" → "Max file size"; "x.main_notes.enabled" → "Main notes enabled". */
export function settingTitle(setting) {
  const parts = setting.id.split('.');
  let last = parts.at(-1).replace(UNIT_SUFFIX, '');
  if (GENERIC.has(last) && parts.length > 2) last = parts.at(-2) + ' ' + last;
  const params = Object.values(setting.params || {}).map(words).join(', ');
  return capital(words(last)) + (params ? ' (' + params + ')' : '');
}
/** "config/models/models-runtime.json" → "Models runtime"; the agent and user stores keep plain names. */
export function groupTitle(owner) {
  if (owner === 'agent_metadata') return 'Agent';
  if (owner === 'user_profile') return 'Profile';
  return capital(words(owner.split('/').pop().replace(/\.json$/, '')));
}
const key = setting => setting.id + JSON.stringify(setting.params || {});

/** Draw one setting as the row its type calls for; onSave(value) saves it. null when no row fits. */
export function createSettingRow(list, setting, {onSave, agents = []} = {}) {
  const title = settingTitle(setting);
  const hint = setting.description || '';
  const value = setting.value;
  switch (setting.type) {
    case 'boolean':
      return createToggleRow(list, {title, hint, value: !!value, onSave});
    case 'enum':
      return createSelectRow(list, {title, hint, value: String(value ?? ''),
        options: (setting.enum || []).map(choice => [choice, capital(words(String(choice)))]), onSave});
    case 'integer':
    case 'number':
      return createNumberRow(list, {title, hint, value: value ?? '', min: setting.min, max: setting.max,
        step: setting.type === 'integer' ? 1 : 0.01, unit: setting.unit, onSave});
    case 'length':
      return createLengthRow(list, {title, hint, value, min: setting.min, max: setting.max, onSave});
    case 'string':
      return createTextRow(list, {title, hint, value: value ?? '', maxLength: setting.max_length,
        onSave: next => {
          if (!next.trim()) return Promise.reject(Error('Enter some text'));
          return onSave(next);
        }});
    case 'agent_id':
      if (!agents.length) return null;
      return createSelectRow(list, {title, hint, value: String(value ?? ''),
        options: agents.map(agent => [agent.id, agent.name || agent.id]), onSave});
    default:
      return null;
  }
}

async function makeRequest() {
  const me = await (await fetch('/api/v3/auth/me', {credentials: 'same-origin', cache: 'no-store'})).json();
  return async (path, method = 'GET', body) => {
    const response = await fetch('/api/v3' + path, {method, credentials: 'same-origin', cache: 'no-store',
      headers: {'Content-Type': 'application/json', ...(method === 'GET' ? {} : {'x-csrf-token': me.csrf_token})},
      ...(body === undefined ? {} : {body: JSON.stringify(body)})});
    const data = await response.json().catch(() => ({}));
    if (!response.ok) throw Object.assign(Error(data.error || data.message || 'Request failed (' + response.status + ')'),
      {status: response.status});
    return data;
  };
}

/** Mount the All Settings page for `page` ('app', 'agent:<id>' or 'user') into host. */
export async function mountSettingsPage(host, {page}) {
  const generation = (host._settingsGeneration || 0) + 1;
  host._settingsGeneration = generation;
  host.replaceChildren();
  const status = node('p', 'ac-hint', 'Loading settings…');
  status.setAttribute('role', 'status');
  host.append(status);
  let request, data;
  try {
    request = await makeRequest();
    data = await request('/config/settings?page=' + encodeURIComponent(page));
  } catch (error) {
    if (host._settingsGeneration === generation) status.textContent = 'Settings unavailable: ' + error.message;
    return;
  }
  if (host._settingsGeneration !== generation) return;
  status.textContent = '';

  // Search: the composer names the setting a request means; its row opens and is highlighted.
  const search = node('form', 'stp-search');
  search.setAttribute('role', 'search');
  const field = createTextField({label: 'Find a setting', placeholder: 'Find a setting, e.g. "bigger chat text"', maxLength: 300});
  const find = node('button', 'ac-btn', 'Find');
  find.type = 'submit';
  find.style.webkitTapHighlightColor = 'transparent';
  search.append(field.element, find);
  host.insertBefore(search, status);

  // Rows save one at a time: each save's new revision (a file digest or the agent's
  // revision) is read back before the next save sends its own.
  const revisions = new Map(data.settings.map(setting => [key(setting), setting.revision]));
  let queue = Promise.resolve();
  const refresh = async () => {
    const fresh = await request('/config/settings?page=' + encodeURIComponent(page));
    for (const setting of fresh.settings) revisions.set(key(setting), setting.revision);
  };
  const saver = setting => value => {
    const task = queue.then(async () => {
      try {
        await request('/config/apply', 'POST', {page, setting: setting.id, value, params: setting.params,
          revision: revisions.get(key(setting))});
      } finally {
        await refresh().catch(() => {});
      }
    });
    queue = task.catch(() => {});
    return task;
  };
  let agents = [];
  if (data.settings.some(setting => setting.type === 'agent_id' && !setting.unavailable)) {
    agents = (await request('/agents').catch(() => ({}))).agents || [];
  }

  const groups = new Map();
  const unavailable = [];
  for (const setting of data.settings) {
    if (setting.unavailable) { unavailable.push(setting); continue; }
    const id = setting.owner.replace(/[^a-z0-9]+/gi, '-').replace(/^-|-$/g, '').toLowerCase();
    if (!groups.has(id)) groups.set(id, {id, title: groupTitle(setting.owner), settings: []});
    groups.get(id).settings.push(setting);
  }
  if (!groups.size) {
    status.textContent = 'No settings can be changed here yet.';
    return;
  }
  const categories = [...groups.values()].map(group => ({id: group.id, icon: SLIDERS,
    hint: group.settings.length + (group.settings.length === 1 ? ' setting' : ' settings')}));
  const register = createSubpageList(host, {label: 'All settings', categories});
  const rows = new Map();
  for (const group of groups.values()) {
    const section = createSubpage(register.stage, {id: group.id, title: group.title});
    const list = createConfigList(section.body, {label: group.title});
    for (const setting of group.settings) {
      const row = createSettingRow(list, setting, {onSave: saver(setting), agents});
      if (row) rows.set(setting.id, {row: row.row, group: group.id});
      else unavailable.push({...setting, unavailable: 'This kind of setting is changed in chat'});
    }
  }
  register.present();
  if (unavailable.length) {
    const note = node('details', 'stp-unavailable');
    note.append(node('summary', 'ac-hint', unavailable.length + ' more settings can’t be changed on this page'));
    const items = node('ul', 'ac-hint');
    for (const setting of unavailable) items.append(node('li', '', settingTitle(setting) + ': ' + setting.unavailable));
    note.append(items);
    host.append(note);
  }

  search.addEventListener('submit', async event => {
    event.preventDefault();
    requestTouchVibration(event);
    const text = field.getValue().trim();
    if (!text) return;
    find.disabled = true;
    status.textContent = 'Finding…';
    try {
      const result = await request('/config/propose', 'POST', {text, page});
      const match = (result.settings || []).map(setting => rows.get(setting.id)).find(Boolean);
      if (!match) { status.textContent = 'No setting on this page matches that.'; return; }
      status.textContent = '';
      register.open(match.group);
      setTimeout(() => {
        match.row.scrollIntoView({block: 'center', behavior: 'smooth'});
        match.row.classList.add('stp-match');
        setTimeout(() => match.row.classList.remove('stp-match'), 2400);
      }, 260);
    } catch (error) {
      status.textContent = 'Search unavailable: ' + error.message;
    } finally {
      find.disabled = false;
    }
  });
}
