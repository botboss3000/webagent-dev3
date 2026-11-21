'use strict';

// Described settings as shared config rows, from the configuration catalog
// (GET /api/v3/config/settings?page=). The setting's type picks the control: boolean → switch,
// enum → dropdown, integer/number → stepper with its range and unit, length → width plus px/%,
// string → text. Each row saves alone through POST /api/v3/config/apply with the revision it was
// drawn from, so the server's validators, ranges and permission checks decide every save.
// The composer box asks the configuration composer what a request means and lists every
// matching setting and every change it found, each change with its own Apply.
// Used by App → All Settings (mountSettingsPage) and an agent's Identity → Chat customization.

import { createSubpage, createSubpageList } from '../subpage-list/subpage-list.js';
import { createConfigForm, createConfigList, createLengthRow, createLongTextRow, createNumberRow, createSelectRow,
  createTextRow, createToggleRow } from './config-rows.js';
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
const GENERIC = new Set(['enabled', 'mode', 'max', 'min', 'seconds', 'minutes', 'limit', 'width', 'value', 'default',
  'visible', 'label', 'top', 'bottom', 'intensity', 'margin_bottom', 'pill_width', 'pill_radius', 'line_height',
  'bg_light', 'bg_dark', 'background_opacity', 'new_session', 'existing_session']);
const UNIT_SUFFIX = /_(px|mb|kb|ms|tokens|seconds|minutes)$/;
// Id words read better spelled out.
const SPELLED = {bg_light: 'background (light mode)', bg_dark: 'background (dark mode)'};
export const SURFACES = [['common', 'Common'], ['desktop', 'Desktop'], ['mobile', 'Mobile'], ['widget', 'Widget']];
// Where a chat value comes from (the composer's `source`).
const SOURCES = {agent: 'This agent’s value', app: 'App value', common: 'Same as Common'};

const words = text => text.replace(/[_-]+/g, ' ').trim();
const capital = text => text.charAt(0).toUpperCase() + text.slice(1);
/** "chat.limits.attachments.max_file_size_mb" → "Max file size"; "x.main_notes.enabled" → "Main notes enabled".
 *  Parameters are appended unless withParams is false. */
export function settingTitle(setting, withParams = true) {
  const parts = setting.id.split('.');
  let last = parts.at(-1).replace(UNIT_SUFFIX, '');
  if (GENERIC.has(last) && parts.length > 2) {
    // "glimmer.agent.enabled" → "Glimmer agent enabled": agent/user alone don't say what.
    const owner = ['agent', 'user'].includes(parts.at(-2)) && parts.length > 3 ? parts.at(-3) + ' ' : '';
    last = owner + parts.at(-2) + ' ' + (SPELLED[last] || last);
  }
  const params = withParams ? Object.values(setting.params || {}).map(words).join(', ') : '';
  return capital(words(last)) + (params ? ' (' + params + ')' : '');
}
/** "config/models/models-runtime.json" → "Models runtime"; the agent and user stores keep plain names. */
export function groupTitle(owner) {
  if (owner === 'agent_metadata') return 'Agent';
  if (owner === 'user_profile') return 'Profile';
  return capital(words(owner.split('/').pop().replace(/\.json$/, '')));
}
export const settingKey = setting => setting.id + JSON.stringify(setting.params || {});
const shown = value => value == null ? 'none' : typeof value === 'object'
  ? (value.width != null ? value.width + (value.unit || '') : JSON.stringify(value)) : String(value);

/** Draw one setting as the row its type calls for; onSave(value) saves it. null when no row fits.
 *  title replaces the generated title; the hint is the description plus where the value comes from. */
export function createSettingRow(list, setting, {onSave, agents = [], title = settingTitle(setting)} = {}) {
  const hint = [setting.description || '', SOURCES[setting.source] || ''].filter(Boolean).join(' · ');
  const value = setting.value;
  switch (setting.type) {
    case 'boolean':
      return createToggleRow(list, {title, hint, value: !!value, onSave});
    case 'enum':
      return createSelectRow(list, {title, hint, value: String(value ?? ''),
        options: (setting.enum || []).map(choice => [choice, capital(words(String(choice)))]), onSave});
    case 'integer':
    case 'number':
      // Arrows move by 1 unless the setting holds fractions (a ratio like 0.5).
      return createNumberRow(list, {title, hint, value: value ?? '', min: setting.min, max: setting.max,
        step: [setting.min, setting.max, value].every(n => n == null || Number.isInteger(n)) ? 1 : 0.1,
        unit: setting.unit, onSave});
    case 'length':
      return createLengthRow(list, {title, hint, value, min: setting.min, max: setting.max, onSave});
    case 'string':
      // The field sits under the title so long titles stay readable on phones.
      return createTextRow(list, {title, hint, value: value ?? '', maxLength: setting.max_length, stacked: true,
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

/** The `key: text.` sentence an object's `_instructions` gives for one of its keys. */
function instruction(object, key) {
  const text = typeof object?._instructions === 'string' ? object._instructions : '';
  const at = text.search(new RegExp('(?:^|[.;]\\s+)' + key.replace(/[.*+?^${}()|[\]\\]/g, '\\$&') + ':'));
  if (at < 0) return '';
  const rest = text.slice(at).replace(/^[.;]?\s*/, '').slice(key.length + 1).trim();
  const next = rest.search(/\.\s+[a-z_][a-z0-9_]*(?:\s*\/\s*[a-z_][a-z0-9_]*)?:/);
  return (next < 0 ? rest : rest.slice(0, next + 1)).trim();
}

/** Every scalar value under `value` as a row picked by its JSON type (true/false → switch,
 *  number → stepper, text → text box, long text → editor). Arrays are left out and null values
 *  are shown read-only. onSave(pointer, value) saves one; own(pointer) labels values this layer
 *  sets itself. Hints come from each object's `_instructions`. */
export function createRawRows(list, value, pointer, {onSave, own = () => false, disabled = false, root = pointer}) {
  for (const [key, child] of Object.entries(value || {})) {
    if (key === '_instructions' || key === 'description' || Array.isArray(child)) continue;
    const path = pointer + '/' + key.replace(/~/g, '~0').replace(/\//g, '~1');
    if (child && typeof child === 'object') {
      createRawRows(list, child, path, {onSave, own, disabled, root});
      continue;
    }
    const title = path.slice(root.length + 1).split('/').map(words).join(' \u203a ');
    const hint = [instruction(value, key), own(path) ? 'Set here' : ''].filter(Boolean).join(' \u00b7 ');
    const save = disabled || !onSave ? undefined : next => onSave(path, next);
    const off = disabled || !onSave;
    if (typeof child === 'boolean') createToggleRow(list, {title, hint, value: child, disabled: off, onSave: save});
    else if (typeof child === 'number') createNumberRow(list, {title, hint, value: child, disabled: off, onSave: save,
      step: Number.isInteger(child) ? 1 : 0.01});
    else if (typeof child === 'string' && (child.length > 80 || child.includes('\n'))) {
      createLongTextRow(list, {title, hint, value: child, disabled: off, onSave: save});
    } else createTextRow(list, {title, hint, value: child ?? '', placeholder: child == null ? 'none' : '',
      disabled: off || child == null, stacked: true, onSave: save});
  }
}

/** A copy of `document` with `pointer` set to `value`. */
export function withValue(document, pointer, value) {
  const copy = structuredClone(document);
  const keys = pointer.split('/').slice(1).map(key => key.replace(/~1/g, '/').replace(/~0/g, '~'));
  let node = copy;
  for (const key of keys.slice(0, -1)) node = node[key] ??= {};
  node[keys.at(-1)] = value;
  return copy;
}

/** Every live file under config/ (JSON and Markdown), grouped by folder, one lazily drawn
 *  section per file. JSON files show each value as a row and the whole file as text; Markdown
 *  files show their text. Saves go through PUT /admin/config-files/{path}: the owner's
 *  validator, a fence on the bytes read and the broker's write all apply. */
async function drawRawFiles(host, request, onSaved) {
  const {files = []} = await request('/admin/config-files');
  host.append(node('p', 'ac-hint', 'Every config file, by key name. Each save is checked by that file\u2019s own '
    + 'validator where it has one and only replaces the file if nobody changed it meanwhile.'));
  const folders = new Map();
  for (const path of files) {
    const folder = path.split('/').slice(1, -1).join(' / ') || 'config';
    if (!folders.has(folder)) folders.set(folder, []);
    folders.get(folder).push(path);
  }
  for (const [folder, paths] of folders) {
    host.append(node('div', 'ac-label stp-area', capital(words(folder))));
    for (const path of paths) {
      const section = node('details', 'stp-unavailable');
      section.append(node('summary', 'ac-hint', path.split('/').pop()));
      host.append(section);
      section.addEventListener('toggle', () => {
        if (section.open && !section.dataset.drawn) { section.dataset.drawn = '1'; void drawFile(section, path); }
      });
    }
  }
  async function drawFile(section, path) {
    const url = '/admin/config-files/' + path.split('/').map(encodeURIComponent).join('/');
    const body = node('div');
    section.append(body);
    let file;
    try { file = await request(url); } catch (error) { body.append(node('p', 'ac-hint', error.message)); return; }
    let queue = Promise.resolve();
    const put = text => {
      const task = queue.then(async () => {
        const saved = await request(url, 'PUT', {text, expected_digest: file.digest});
        file = {...file, text, digest: saved.digest};
        await onSaved?.();
      });
      queue = task.catch(() => {});
      return task;
    };
    const redraw = () => { body.replaceChildren(); draw(); };
    const draw = () => {
      const json = path.endsWith('.json');
      let config = null;
      if (json) {
        try { config = JSON.parse(file.text); } catch { body.append(node('p', 'ac-hint', 'This file is not valid JSON; fix it as text.')); }
      }
      if (config && typeof config === 'object') {
        createRawRows(createConfigList(body, {label: path}), config, '', {onSave: async (pointer, value) => {
          await put(JSON.stringify(withValue(config, pointer, value), null, 2));
          config = JSON.parse(file.text);
        }});
      }
      // The whole file as text, saved with one button: lists and Markdown are edited here.
      const raw = node('details', 'stp-unavailable');
      raw.open = !json;
      raw.append(node('summary', 'ac-hint', json ? 'Edit as text' : 'Text'));
      body.append(raw);
      const form = createConfigForm(raw, {saveLabel: 'Save file', onSave: async values => { await put(values.text); redraw(); }});
      createLongTextRow(createConfigList(raw, {label: path + ' text'}), {title: path.split('/').pop(), value: file.text,
        minHeight: 160, maxHeight: 520, key: 'text', form});
      form.attach();
    };
    draw();
  }
}

/** A request helper with the signed-in user's CSRF token, for pages mounted without one. */
export async function makeRequest() {
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

/** One page's settings and their saves. Saves run one at a time: each save's new revision (a
 *  file digest, the agent's revision or its chat overlay revision) is read back before the next
 *  save sends its own. */
export function createSettingsStore(request, page) {
  const path = '/config/settings?page=' + encodeURIComponent(page);
  const revisions = new Map();
  let queue = Promise.resolve();
  const store = {
    settings: [],
    async load() {
      store.settings = (await request(path)).settings;
      for (const setting of store.settings) revisions.set(settingKey(setting), setting.revision);
      return store.settings;
    },
    // body is {value} or {reset: true}.
    send(setting, body) {
      const task = queue.then(async () => {
        try {
          await request('/config/apply', 'POST', {page, setting: setting.id, params: setting.params,
            revision: revisions.get(settingKey(setting)), ...body});
        } finally {
          await store.load().catch(() => {});
        }
      });
      queue = task.catch(() => {});
      return task;
    },
    save: setting => value => store.send(setting, {value}),
    reset: setting => store.send(setting, {reset: true}),
  };
  return store;
}

/** The composer box: a request in plain words → every matching setting and every change found.
 *  onOpen(id, surface) shows a setting's row; a change's Apply saves it, then onApplied(). */
export function createComposerBox(host, {request, page, onOpen, onApplied, placeholder}) {
  const form = node('form', 'stp-search');
  form.setAttribute('role', 'search');
  const field = createTextField({label: 'Describe a change or find a setting',
    placeholder: placeholder || 'e.g. "bigger chat text"', maxLength: 300});
  const find = node('button', 'ac-btn', 'Find');
  find.type = 'submit';
  find.style.webkitTapHighlightColor = 'transparent';
  form.append(field.element, find);
  const status = node('p', 'ac-hint');
  status.setAttribute('role', 'status');
  const results = node('div', 'stp-results');
  host.append(form, status, results);
  const button = (label, kind, onClick) => {
    const element = node('button', 'ac-btn ac-btn-sm' + (kind ? ' ac-btn-' + kind : ''), label);
    element.type = 'button';
    element.style.webkitTapHighlightColor = 'transparent';
    element.addEventListener('click', async event => { requestTouchVibration(event); await onClick(element); });
    return element;
  };
  const describe = (result, id) => (result.settings || []).find(setting => setting.id === id);
  // One answer (a whole request's, or one change of several) as a result card.
  const draw = (result, answer) => {
    const surface = answer.params?.surface;
    const titleOf = id => settingTitle({id, params: surface ? {surface} : {}});
    const card = node('div', 'stp-result');
    if (answer.outcome === 'card') {
      const review = answer.review || {};
      card.append(node('div', 'stp-result-title', titleOf(answer.setting)),
        node('div', 'ac-hint', describe(result, answer.setting)?.description || ''));
      if (review.unavailable) {
        card.append(node('div', 'ac-hint', review.unavailable));
      } else if (review.proposed == null) {
        card.append(node('div', 'ac-hint', 'Now ' + shown(review.current) + '. Say a value, or open the row to set one.'));
      } else {
        card.append(node('div', 'stp-change', shown(review.current) + ' → ' + shown(review.proposed)));
        if (review.out_of_range) card.append(node('div', 'ac-hint', 'Out of range: ' + review.out_of_range));
      }
      const actions = node('div', 'cfr-actions');
      if (review.proposed != null && !review.out_of_range && !review.unavailable) {
        actions.append(button('Apply', 'primary', async element => {
          element.disabled = true;
          try {
            await request('/config/apply', 'POST', {page, setting: answer.setting, value: review.proposed,
              params: answer.params, revision: review.revision});
            element.textContent = 'Applied';
            await onApplied?.();
          } catch (error) {
            element.disabled = false;
            card.append(node('div', 'cfr-error', error.message));
          }
        }));
      }
      actions.append(button('Open', '', () => onOpen?.(answer.setting, surface)));
      card.append(actions);
    } else if (answer.outcome === 'pick') {
      card.append(node('div', 'stp-result-title', answer.needs ? 'Which value? Open the row to set it.' : 'Matching settings'));
      const list = node('div', 'cfr-actions');
      for (const id of answer.candidates || []) list.append(button(titleOf(id), '', () => onOpen?.(id, surface)));
      card.append(list);
    } else if (answer.outcome === 'other_page') {
      card.append(node('div', 'ac-hint', titleOf(answer.setting) + ' is set on another page (' + (answer.reason || 'not here') + ').'));
    } else if (answer.outcome === 'out_of_range') {
      card.append(node('div', 'ac-hint', titleOf(answer.setting) + ': out of range, ' + answer.limit + '.'));
    } else {
      card.append(node('div', 'ac-hint', 'No setting matched. Try other words, or name the setting.'));
    }
    results.append(card);
  };
  form.addEventListener('submit', async event => {
    event.preventDefault();
    requestTouchVibration(event);
    const text = field.getValue().trim();
    if (!text) return;
    find.disabled = true;
    status.textContent = 'Finding…';
    results.replaceChildren();
    try {
      const result = await request('/config/propose', 'POST', {text, page});
      status.textContent = result.outcome === 'multiple' ? (result.parts.length + ' changes found') : '';
      for (const answer of result.outcome === 'multiple' ? result.parts : [result]) draw(result, answer);
    } catch (error) {
      status.textContent = 'Search unavailable: ' + error.message;
    } finally {
      find.disabled = false;
    }
  });
  return {element: form};
}

/** Highlight a row and bring it into view. */
export function highlight(row) {
  row.scrollIntoView({block: 'center', behavior: 'smooth'});
  row.classList.add('stp-match');
  setTimeout(() => row.classList.remove('stp-match'), 2400);
}

/** Mount the All Settings page for `page` ('app', 'agent:<id>' or 'user') into host; the app's
 *  also lists every raw config value. */
export async function mountSettingsPage(host, {page}) {
  const generation = (host._settingsGeneration || 0) + 1;
  host._settingsGeneration = generation;
  host.replaceChildren();
  const status = node('p', 'ac-hint', 'Loading settings…');
  status.setAttribute('role', 'status');
  host.append(status);
  let request, store;
  try {
    request = await makeRequest();
    store = createSettingsStore(request, page);
    await store.load();
  } catch (error) {
    if (host._settingsGeneration === generation) status.textContent = 'Settings unavailable: ' + error.message;
    return;
  }
  if (host._settingsGeneration !== generation) return;
  status.textContent = '';
  const composer = node('div');
  host.insertBefore(composer, status);

  let agents = [];
  if (store.settings.some(setting => setting.type === 'agent_id' && !setting.unavailable)) {
    agents = (await request('/agents').catch(() => ({}))).agents || [];
  }
  // One category per settings file, and per chat surface for chat settings.
  const groups = new Map();
  const unavailable = [];
  for (const setting of store.settings) {
    if (setting.unavailable) { unavailable.push(setting); continue; }
    // Settings stored on the agent or the account itself have no owner file.
    const owner = setting.owner || (page === 'user' ? 'user_profile' : 'agent_metadata');
    const surface = setting.params?.surface;
    const id = (owner + (surface ? '-' + surface : '')).replace(/[^a-z0-9]+/gi, '-').replace(/^-|-$/g, '').toLowerCase();
    const title = groupTitle(owner) + (surface ? ' · ' + (SURFACES.find(([s]) => s === surface)?.[1] || surface) : '');
    if (!groups.has(id)) groups.set(id, {id, title, settings: []});
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
      const row = createSettingRow(list, setting, {onSave: store.save(setting), agents,
        title: settingTitle(setting, !setting.params?.surface)});
      if (row) rows.set(settingKey(setting), {row: row.row, group: group.id});
      else unavailable.push({...setting, unavailable: 'This kind of setting is changed in chat'});
    }
  }
  register.present();
  if (unavailable.length) {
    const note = node('details', 'stp-unavailable');
    note.append(node('summary', 'ac-hint', unavailable.length + (unavailable.length === 1 ? ' more setting' : ' more settings')
      + ' can’t be changed on this page'));
    const items = node('ul', 'ac-hint');
    for (const setting of unavailable) items.append(node('li', '', settingTitle(setting) + ': ' + setting.unavailable));
    note.append(items);
    host.append(note);
  }
  if (page === 'app') {
    const raw = node('details', 'stp-unavailable');
    raw.append(node('summary', 'ac-hint', 'Advanced: every config file (JSON and Markdown)'));
    host.append(raw);
    raw.addEventListener('toggle', () => {
      if (raw.open && !raw.dataset.drawn) {
        raw.dataset.drawn = '1';
        void drawRawFiles(raw, request, () => store.load()).catch(error => raw.append(node('p', 'ac-hint', error.message)));
      }
    });
  }
  createComposerBox(composer, {request, page,
    onOpen: (id, surface) => {
      const match = rows.get(settingKey({id, params: surface ? {surface} : {}}))
        || [...rows.entries()].find(([k]) => k.startsWith(id + '{'))?.[1];
      if (!match) return;
      register.open(match.group);
      setTimeout(() => highlight(match.row), 260);
    },
    onApplied: () => mountSettingsPage(host, {page})});
}
