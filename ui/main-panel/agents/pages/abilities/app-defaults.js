// App Default: administrators edit the app's own ability source files (tool definitions
// and skills here; manifests and groups follow) as drafts, then Save them as one release.
// The server owns the checks, drafts and the broker task; this page only shows and edits.
import { createRichTextEditor } from '../../../../shared/rich-text-editor/rich-text-editor.js';
import { advanceDeleteBtn, resetDeleteBtn } from '../../../../shared/models/delete-control.js';

export const APP_DEFAULT = 'app_default';
const DRAFTS = '/admin/app-defaults/drafts';
const LEVELS = ['operator', 'member', 'visitor'];
const POLICIES = [['', 'Not set (next role down)'], ['allow', 'Allow'], ['require_approval', 'Require approval'], ['deny', 'Deny']];
const TYPES = ['string', 'integer', 'number', 'boolean', 'array', 'object'];

const node = (tag, className = '', text = '') => {
  const element = document.createElement(tag);
  element.className = className;
  element.textContent = text;
  return element;
};
const field = (label, control, hint = '') => {
  const wrap = node('label', 'adf-field');
  wrap.append(node('span', 'adf-label', label), control);
  if (hint) wrap.append(node('span', 'ac-hint', hint));
  return wrap;
};
const select = (choices, value, label) => {
  const element = node('select', 'ac-input ac-config-sel');
  element.setAttribute('aria-label', label);
  for (const [id, name] of choices) {
    const option = node('option', '', name);
    option.value = id;
    element.append(option);
  }
  element.value = value ?? '';
  return element;
};
const text = (value, label, type = 'text') => {
  const input = node('input', 'ac-input');
  input.type = type;
  input.value = value ?? '';
  input.setAttribute('aria-label', label);
  return input;
};
const pretty = value => JSON.stringify(value, null, 2) + '\n';

/** The caller's draft state, or null when they are not an app administrator. */
export async function loadAppDefaults(request) {
  try { return await request(DRAFTS); } catch { return null; }
}

/** Shared state for one page: drafts, the bar to refresh, and debounced draft writes. */
export function createAppDefaults(request, initial) {
  const state = { drafts: initial, listeners: new Set(), timers: new Map(), pending: Promise.resolve() };
  const notify = () => { for (const listener of state.listeners) listener(state.drafts); };
  const refresh = async () => { state.drafts = await request(DRAFTS); notify(); return state.drafts; };
  // Writes run one at a time so each carries the revision the previous one returned.
  const write = (path, draft) => {
    state.pending = state.pending.then(async () => {
      try {
        const saved = await request(DRAFTS, 'POST', { path, draft, expected_revision: state.drafts.revision });
        state.drafts.revision = saved.revision;
        if (draft) state.drafts.files[path] = { draft, stale: false };
        else delete state.drafts.files[path];
        notify();
      } catch (error) {
        // Another tab or admin moved the revision: reread, then the next edit writes again.
        if (error.status === 409) await refresh();
        state.error?.(path + ': ' + error.message);
      }
    });
    return state.pending;
  };
  /** Draft `content` for path (or revert it when it equals the checkout), after a pause. */
  const edit = (path, content, original) => {
    clearTimeout(state.timers.get(path));
    state.timers.set(path, setTimeout(() => {
      state.timers.delete(path);
      write(path, content === original ? null : { content });
    }, 600));
  };
  const draftOf = path => state.drafts.files?.[path]?.draft || null;
  return { state, refresh, edit, write, revert: path => write(path, null), draftOf,
    onChange: listener => state.listeners.add(listener), request };
}

/** The bar beside the level selector: Save, the unsaved count and its list, Discard all and the release. */
export function buildAppDefaultsBar(app, { onStatus }) {
  const bar = node('div', 'adf-bar');
  const row = node('div', 'adf-bar-row');
  const save = node('button', 'ac-btn ac-btn-primary', 'Save');
  save.type = 'button';
  const count = node('button', 'ac-btn ac-btn-sm adf-count');
  count.type = 'button';
  count.setAttribute('aria-expanded', 'false');
  const discard = node('button', 'ac-btn ac-btn-sm adf-discard');
  discard.type = 'button';
  discard.title = 'Discard all drafts';
  discard.innerHTML = '<i data-lucide="trash-2"></i>';
  const note = text('', 'Optional note for this Save');
  note.placeholder = 'Optional note for the release';
  note.maxLength = 120;
  row.append(save, count, discard);
  const list = node('ul', 'adf-list');
  list.hidden = true;
  const release = node('div', 'adf-release');
  bar.append(row, note, list, release);
  const paint = drafts => {
    const paths = Object.keys(drafts.files || {});
    const releasing = drafts.release?.status === 'releasing';
    count.textContent = paths.length === 1 ? '1 unsaved edit' : paths.length + ' unsaved edits';
    count.disabled = !paths.length;
    discard.hidden = !paths.length;
    save.disabled = !paths.length || releasing;
    save.textContent = releasing ? 'Saving…' : 'Save';
    list.replaceChildren(...paths.map(path => {
      const item = node('li', '', path.replace(/^abilities\//, ''));
      if (drafts.files[path].stale) item.append(node('span', 'adf-stale', ' · changed on the server since you started; Save overwrites it'));
      return item;
    }));
    for (const other of drafts.others || []) {
      list.append(node('li', 'ac-hint', other.user + ' is drafting ' + other.paths.length + ' file(s)'));
    }
    release.replaceChildren();
    const job = drafts.release;
    if (job) {
      const link = node('a', '', 'Broker card');
      link.href = '/v3-ui/broker-card.html?task_id=' + encodeURIComponent(job.task_id);
      link.target = '_blank';
      link.rel = 'noopener';
      const label = job.status === 'releasing' ? 'Releasing your last Save. ' : 'Your last Save did not release: ' + (job.reason || 'see the Broker card') + '. Drafts are kept. ';
      release.append(node('span', job.status === 'releasing' ? '' : 'adf-failed', label), link);
    }
  };
  count.onclick = () => {
    list.hidden = !list.hidden;
    count.setAttribute('aria-expanded', String(!list.hidden));
  };
  save.onclick = async () => {
    save.disabled = true;
    await app.state.pending;
    try {
      const saved = await app.request('/admin/app-defaults/save', 'POST', { note: note.value, expected_revision: app.state.drafts.revision });
      note.value = '';
      onStatus('Save started a release. Track it on the Broker card.');
      app.state.drafts.revision = saved.revision;
    } catch (error) {
      onStatus('Save did not start: ' + error.message);
    }
    await app.refresh().catch(() => {});
  };
  discard.onclick = () => advanceDeleteBtn(discard, {
    armTitle: 'Click again to discard every draft',
    onConfirm: async () => {
      try {
        await app.state.pending;
        await app.request(DRAFTS, 'DELETE', { expected_revision: app.state.drafts.revision });
        onStatus('Drafts discarded.');
      } catch (error) {
        onStatus('Could not discard drafts: ' + error.message);
      }
      resetDeleteBtn(discard);
      discard.innerHTML = '<i data-lucide="trash-2"></i>';
      await app.refresh().catch(() => {});
      window.lucide?.createIcons({ nodes: [discard] });
      return true;
    },
  });
  app.onChange(paint);
  paint(app.state.drafts);
  // A running Save has no push source on this page: check its state while it runs and
  // the page is visible, patching only this bar.
  const timer = setInterval(() => {
    if (!bar.isConnected) { clearInterval(timer); return; }
    if (document.visibilityState === 'visible' && app.state.drafts.release?.status === 'releasing') app.refresh().catch(() => {});
  }, 20000);
  return bar;
}

/** Path of a tool's definition or skill file in the checkout. */
export const toolPath = (ability, tool, kind) =>
  'abilities/' + ability.group + '/' + ability.id + '/tools/' + tool + '/' + tool + (kind === 'skill' ? '.skill.md' : '.tool.json');

/** One tool's App Default panel: its tool.json as fields, its parameters, raw JSON and its skill. */
export function buildToolDefaultsPanel(app, ability, tool) {
  const panel = node('div', 'adf-panel');
  panel.append(node('div', 'adf-panel-title', tool.name));
  const body = node('div', 'adf-panel-body', 'Loading…');
  panel.append(body);
  const definitionPath = toolPath(ability, tool.name, 'definition');
  const skillPath = toolPath(ability, tool.name, 'skill');
  Promise.all([
    app.request('/admin/app-defaults/file?' + new URLSearchParams({ path: definitionPath })),
    app.request('/admin/app-defaults/file?' + new URLSearchParams({ path: skillPath })),
  ]).then(([definition, skill]) => {
    body.replaceChildren();
    if (!definition.exists) {
      body.append(node('p', 'ac-hint', 'This tool has no tool.json (it is built into the app), so it has no App Default file to edit.'));
      return;
    }
    const original = definition.content;
    let value;
    try {
      value = JSON.parse(app.draftOf(definitionPath)?.content ?? original);
    } catch {
      body.append(node('p', 'adf-failed', 'tool.json is not valid JSON; fix it in the raw editor below.'));
      value = null;
    }
    const raw = node('textarea', 'ac-input adf-raw');
    raw.spellcheck = false;
    raw.setAttribute('aria-label', tool.name + ' tool.json');
    const changed = () => {
      raw.value = pretty(value);
      app.edit(definitionPath, raw.value, original);
    };
    if (value) body.append(buildDefinitionForm(value, changed, tool.name));
    const rawToggle = node('button', 'ac-btn ac-btn-sm', 'Raw JSON');
    rawToggle.type = 'button';
    rawToggle.setAttribute('aria-expanded', 'false');
    const rawBox = node('div', 'adf-raw-box');
    rawBox.hidden = true;
    raw.value = value ? pretty(value) : (app.draftOf(definitionPath)?.content ?? original);
    const rawError = node('p', 'adf-failed');
    raw.oninput = () => {
      try {
        value = JSON.parse(raw.value);
        rawError.textContent = '';
        app.edit(definitionPath, raw.value, original);
      } catch (error) {
        rawError.textContent = 'Not valid JSON yet: ' + error.message;
      }
    };
    rawToggle.onclick = () => {
      rawBox.hidden = !rawBox.hidden;
      rawToggle.setAttribute('aria-expanded', String(!rawBox.hidden));
    };
    rawBox.append(node('p', 'ac-hint', 'The whole file. Fields the form does not show (owner tiers, role access, usage hint) live here.'), raw, rawError);
    body.append(rawToggle, rawBox);
    const skillOriginal = skill.content ?? '';
    const editor = createRichTextEditor({
      value: app.draftOf(skillPath)?.content ?? skillOriginal, label: tool.name + ' skill',
      placeholder: 'This tool has no skill yet; write one to add it.', minHeight: 100, maxHeight: 300,
      onChange: next => app.edit(skillPath, next.replace(/\n$/, '') + '\n', skillOriginal),
    });
    body.append(field('Tool skill', editor.element, 'Loads together with the tool. Arguments, results, limits and error recovery belong here.'));
  }).catch(error => {
    body.textContent = 'Could not load this tool: ' + error.message;
  });
  return panel;
}

/** Fields for the parts of a tool definition people edit most; everything stays in `value`. */
function buildDefinitionForm(value, changed, name) {
  const form = node('div', 'adf-form');
  const description = createRichTextEditor({
    value: value.description || '', label: name + ' description', minHeight: 44, maxHeight: 120,
    onChange: next => { value.description = next.trim(); changed(); },
  });
  form.append(field('Description', description.element, 'One sentence, at most 160 characters: it rides every model call.'));
  const effect = select([['read', 'Read: changes nothing'], ['write', 'Write: changes something']], value.effect, name + ' effect');
  effect.onchange = () => { value.effect = effect.value; changed(); };
  form.append(field('Effect', effect, 'Drives recovery, batches and the repeat guard; never access.'));
  const policies = node('div', 'adf-policies');
  for (const level of LEVELS) {
    const control = select(POLICIES, value.default_policy?.[level] || '', name + ' default for ' + level);
    control.onchange = () => {
      value.default_policy ||= {};
      if (control.value) value.default_policy[level] = control.value;
      else delete value.default_policy[level];
      changed();
    };
    policies.append(field(level[0].toUpperCase() + level.slice(1), control));
  }
  form.append(field('Default access by role', policies, 'Used when an agent level saves no permission for this tool.'));
  form.append(buildParametersForm(value, changed, name));
  return form;
}

/** One row per top-level argument; nested objects and anyOf are edited in raw JSON. */
function buildParametersForm(value, changed, name) {
  const wrap = node('div', 'adf-params');
  wrap.append(node('div', 'adf-label', 'Arguments'));
  const parameters = value.parameters ||= { type: 'object', properties: {} };
  const properties = parameters.properties || {};
  const required = () => (parameters.required ||= []);
  if (!Object.keys(properties).length) wrap.append(node('p', 'ac-hint', 'This tool takes no arguments.'));
  for (const [key, schema] of Object.entries(properties)) {
    const row = node('div', 'adf-param');
    const head = node('div', 'adf-param-head');
    head.append(node('code', '', key));
    const isRequired = node('input');
    isRequired.type = 'checkbox';
    isRequired.checked = (parameters.required || []).includes(key);
    isRequired.setAttribute('aria-label', key + ' required');
    isRequired.onchange = () => {
      const list = required().filter(item => item !== key);
      if (isRequired.checked) list.push(key);
      parameters.required = list;
      changed();
    };
    const requiredLabel = node('label', 'adf-inline');
    requiredLabel.append(isRequired, node('span', '', 'Required'));
    head.append(requiredLabel);
    row.append(head);
    if (schema.anyOf || (schema.type === 'object' && schema.properties)) {
      row.append(node('p', 'ac-hint', 'Several allowed forms or nested fields: edit this argument in raw JSON.'));
      wrap.append(row);
      continue;
    }
    const type = select(TYPES.map(t => [t, t]), Array.isArray(schema.type) ? schema.type[0] : schema.type, key + ' type');
    type.onchange = () => { schema.type = type.value; changed(); };
    const description = createRichTextEditor({
      value: schema.description || '', label: key + ' description', minHeight: 36, maxHeight: 100,
      onChange: next => { if (next.trim()) schema.description = next.trim(); else delete schema.description; changed(); },
    });
    row.append(field('Type', type), field('Description', description.element));
    const limits = node('div', 'adf-limits');
    const limit = (keyName, label) => {
      const input = text(schema[keyName], key + ' ' + label, 'number');
      input.onchange = () => {
        if (input.value === '') delete schema[keyName];
        else schema[keyName] = Number(input.value);
        changed();
      };
      limits.append(field(label, input));
    };
    const kind = Array.isArray(schema.type) ? schema.type[0] : schema.type;
    if (kind === 'string') { limit('minLength', 'Min length'); limit('maxLength', 'Max length'); }
    if (kind === 'integer' || kind === 'number') { limit('minimum', 'Minimum'); limit('maximum', 'Maximum'); }
    if (kind === 'array') { limit('minItems', 'Min items'); limit('maxItems', 'Max items'); }
    if (limits.childElementCount) row.append(limits);
    if (kind === 'string') {
      const options = text((schema.enum || []).join(', '), key + ' allowed values');
      options.onchange = () => {
        const values = options.value.split(',').map(item => item.trim()).filter(Boolean);
        if (values.length) schema.enum = values; else delete schema.enum;
        changed();
      };
      row.append(field('Allowed values', options, 'Comma separated; empty allows any text.'));
      const pattern = text(schema.pattern, key + ' pattern');
      pattern.onchange = () => { if (pattern.value) schema.pattern = pattern.value; else delete schema.pattern; changed(); };
      row.append(field('Pattern', pattern, 'A regular expression the text must match.'));
    }
    wrap.append(row);
  }
  wrap.append(node('p', 'ac-hint', 'Adding, removing or renaming an argument, or changing a type, needs a matching code change: a quick release refuses it.'));
  return wrap;
}

/** Path of an ability's manifest or skill, and of a group's manifest. */
export const abilityPath = (ability, file) => 'abilities/' + ability.group + '/' + ability.id + '/' + file;
export const skillPath = (ability, skillId) =>
  abilityPath(ability, 'skills/' + (skillId === 'core' ? ability.id : skillId) + '.skill.md');
export const groupPath = groupId => 'abilities/' + groupId + '/_group.json';

/** Read one file and draw `build(content, original)` into a panel once it arrives. */
function filePanel(app, title, path, build) {
  const panel = node('div', 'adf-panel');
  panel.append(node('div', 'adf-panel-title', title));
  const body = node('div', 'adf-panel-body', 'Loading…');
  panel.append(body);
  app.request('/admin/app-defaults/file?' + new URLSearchParams({ path })).then(file => {
    body.replaceChildren();
    if (!file.exists) {
      body.append(node('p', 'ac-hint', 'No ' + path.split('/').pop() + ' here: this part is built into the app.'));
      return;
    }
    build(body, app.draftOf(path)?.content ?? file.content, file.content);
  }).catch(error => { body.textContent = 'Could not load ' + path.split('/').pop() + ': ' + error.message; });
  return panel;
}

/** A JSON manifest: named fields drawn by `fields`, plus the whole file as raw JSON. */
function jsonPanel(app, title, path, fields) {
  return filePanel(app, title, path, (body, content, original) => {
    let value;
    try { value = JSON.parse(content); } catch { value = null; }
    const raw = node('textarea', 'ac-input adf-raw');
    raw.spellcheck = false;
    raw.setAttribute('aria-label', title + ' raw JSON');
    raw.value = value ? pretty(value) : content;
    const changed = () => { raw.value = pretty(value); app.edit(path, raw.value, original); };
    if (value) body.append(fields(value, changed));
    else body.append(node('p', 'adf-failed', 'Not valid JSON; fix it in the raw editor below.'));
    const rawError = node('p', 'adf-failed');
    raw.oninput = () => {
      try { value = JSON.parse(raw.value); rawError.textContent = ''; app.edit(path, raw.value, original); }
      catch (error) { rawError.textContent = 'Not valid JSON yet: ' + error.message; }
    };
    const toggle = node('button', 'ac-btn ac-btn-sm', 'Raw JSON');
    toggle.type = 'button';
    toggle.setAttribute('aria-expanded', 'false');
    const box = node('div', 'adf-raw-box');
    box.hidden = true;
    box.append(node('p', 'ac-hint', 'The whole file, including fields the form does not show.'), raw, rawError);
    toggle.onclick = () => { box.hidden = !box.hidden; toggle.setAttribute('aria-expanded', String(!box.hidden)); };
    body.append(toggle, box);
  });
}

/** Display fields shared by group and ability manifests: name, description, icon, color, order. */
function displayFields(value, changed, { nameKey, descKey, label }) {
  const form = node('div', 'adf-form');
  const name = text(value[nameKey], label + ' name');
  name.oninput = () => { value[nameKey] = name.value; changed(); };
  form.append(field('Name', name));
  const description = createRichTextEditor({
    value: value[descKey] || '', label: label + ' description', minHeight: 44, maxHeight: 140,
    onChange: next => { value[descKey] = next.trim(); changed(); },
  });
  form.append(field('Description', description.element));
  const row = node('div', 'adf-limits');
  row.append(field('Icon', iconPicker(value.icon || '', icon => { value.icon = icon; changed(); }), 'A Lucide icon name.'));
  const color = node('input', 'ac-input adf-color');
  color.type = 'color';
  color.value = /^#[0-9a-f]{6}$/i.test(value.color || '') ? value.color : '#888888';
  color.setAttribute('aria-label', label + ' color');
  color.oninput = () => { value.color = color.value; changed(); };
  row.append(field('Color', color));
  const order = text(value.order, label + ' order', 'number');
  order.onchange = () => { value.order = Number(order.value) || 0; changed(); };
  row.append(field('Order', order, 'Lower comes first.'));
  form.append(row);
  return form;
}

/** A searchable Lucide icon chooser: the current icon, a search box and matching icons to tap. */
function iconPicker(current, onPick) {
  const wrap = node('div', 'adf-icon');
  const head = node('div', 'adf-icon-head');
  const preview = node('span', 'adf-icon-preview');
  const name = node('code', '', current || 'none');
  const show = icon => {
    preview.innerHTML = icon ? '<i data-lucide="' + icon + '"></i>' : '';
    name.textContent = icon || 'none';
    window.lucide?.createIcons({ nodes: [preview] });
  };
  const search = text('', 'Search icons', 'search');
  search.placeholder = 'Search icons';
  const results = node('div', 'adf-icon-results');
  // Lucide's registry is keyed in PascalCase; pages name icons in kebab-case.
  const names = Object.keys(window.lucide?.icons || {}).map(key =>
    key.replace(/([a-z0-9])([A-Z])/g, '$1-$2').replace(/([A-Za-z])([0-9])/g, '$1-$2').toLowerCase());
  search.oninput = () => {
    const query = search.value.trim().toLowerCase();
    results.replaceChildren();
    if (!query) return;
    for (const icon of names.filter(icon => icon.includes(query)).slice(0, 40)) {
      const button = node('button', 'ac-btn ac-btn-sm adf-icon-choice');
      button.type = 'button';
      button.title = icon;
      button.setAttribute('aria-label', 'Use icon ' + icon);
      button.innerHTML = '<i data-lucide="' + icon + '"></i>';
      button.onclick = () => { show(icon); onPick(icon); results.replaceChildren(); search.value = ''; };
      results.append(button);
    }
    window.lucide?.createIcons({ nodes: [results] });
  };
  head.append(preview, name);
  wrap.append(head, search, results);
  show(current);
  return wrap;
}

/** A group's card: its _group.json name, description, icon, color and order. */
export function buildGroupDefaultsPanel(app, groupId, groupName) {
  return jsonPanel(app, groupName + ' group', groupPath(groupId), (value, changed) =>
    displayFields(value, changed, { nameKey: 'name', descKey: 'desc', label: groupName + ' group' }));
}

/** An ability's manifest, its skills (edit, add, delete) and nothing of its tools. */
export function buildAbilityDefaultsPanels(app, ability) {
  const panels = [];
  panels.push(jsonPanel(app, ability.name + ' manifest', abilityPath(ability, ability.id + '.json'), (value, changed) => {
    const form = displayFields(value, changed, { nameKey: 'display_name', descKey: 'description', label: ability.name });
    const summary = createRichTextEditor({
      value: value.skill_summary || '', label: ability.name + ' skill summary', minHeight: 44, maxHeight: 140,
      onChange: next => { if (next.trim()) value.skill_summary = next.trim(); else delete value.skill_summary; changed(); },
    });
    form.append(field('Skill summary', summary.element, 'Shown when the skill is offered on request.'));
    return form;
  }));
  // general_skills carries titles; selectable_skills may list the same skills by ID only.
  const skills = new Map();
  for (const entry of [...(ability.general_skills || (ability.has_skill ? [{ skill_id: 'core', title: 'Skill' }] : [])),
    ...(ability.selectable_skills || [])]) {
    const id = typeof entry === 'string' ? entry : entry.skill_id || 'core';
    if (!skills.has(id)) skills.set(id, typeof entry === 'string' ? id : entry.title || id);
  }
  for (const [id, title] of skills) panels.push(skillPanel(app, ability, id, title));
  panels.push(addSkillPanel(app, ability));
  return panels;
}

/** One skill file in the shared editor; skills other than the primary can be deleted. */
function skillPanel(app, ability, skillId, title) {
  const path = skillPath(ability, skillId);
  return filePanel(app, 'Skill: ' + (skillId === 'core' ? 'primary' : title), path, (body, content, original) => {
    const deleted = app.draftOf(path)?.delete === true;
    const editor = createRichTextEditor({
      value: deleted ? original : content, label: title + ' skill', minHeight: 120, maxHeight: 340,
      onChange: next => app.edit(path, next.replace(/\n$/, '') + '\n', original),
    });
    body.append(editor.element);
    if (skillId === 'core') return;
    const remove = node('button', 'ac-btn ac-btn-sm', deleted ? 'Keep this skill' : 'Delete this skill');
    remove.type = 'button';
    remove.onclick = () => {
      const now = app.draftOf(path)?.delete === true;
      app.write(path, now ? null : { delete: true });
      remove.textContent = now ? 'Delete this skill' : 'Keep this skill';
      body.classList.toggle('adf-deleted', !now);
    };
    body.classList.toggle('adf-deleted', deleted);
    body.append(remove, node('p', 'ac-hint', 'Deleting removes the file on Save; agents that load it by name lose it.'));
  });
}

/** Start a new skill: a file name and its text become a draft that creates the file on Save. */
function addSkillPanel(app, ability) {
  const panel = node('div', 'adf-panel');
  panel.append(node('div', 'adf-panel-title', 'Add a skill'));
  const name = text('', 'New skill name');
  name.placeholder = 'skill_name';
  const editor = createRichTextEditor({ value: '', label: 'New skill text', minHeight: 100, maxHeight: 300,
    placeholder: '---\nid: skill_name\ntitle: …\nwhen_to_use: …\n---\n\n# …' });
  const add = node('button', 'ac-btn ac-btn-sm', 'Add skill draft');
  add.type = 'button';
  const status = node('p', 'ac-hint', 'Lowercase letters, digits and underscores. It is created on Save.');
  add.onclick = () => {
    const id = name.value.trim();
    if (!/^[a-z0-9_]{1,60}$/.test(id) || id === ability.id) { status.textContent = 'Use lowercase letters, digits and underscores, other than the ability\'s own name.'; return; }
    const content = editor.getValue().trim();
    if (!content) { status.textContent = 'Write the skill text first.'; return; }
    app.write(skillPath(ability, id), { content: content + '\n' });
    status.textContent = 'Drafted ' + id + '.skill.md; it is created on Save.';
    name.value = '';
    editor.setValue('');
  };
  panel.append(field('File name', name), editor.element, add, status);
  return panel;
}

export const APP_DEFAULTS_CSS = '.adf-bar{display:flex;flex-direction:column;gap:8px;margin-top:10px}'
  + '.adf-bar-row{display:flex;flex-wrap:wrap;gap:8px;align-items:center}'
  + '.adf-list{margin:0;padding-left:18px;font-size:13px;color:var(--fg-2)}'
  + '.adf-release{font-size:13px;color:var(--fg-2)}'
  + '.adf-failed,.adf-stale{color:var(--warning)}'
  + '.adf-panel{border:var(--border-width) solid var(--border-soft);border-radius:var(--r-md,10px);padding:10px 12px;margin:8px 0}'
  + '.adf-panel-title{font-weight:600;margin-bottom:6px;overflow-wrap:anywhere}'
  + '.adf-form,.adf-panel-body{display:flex;flex-direction:column;gap:10px}'
  + '.adf-field{display:flex;flex-direction:column;gap:4px;min-width:0}'
  + '.adf-label{font-size:13px;font-weight:600;color:var(--fg-2)}'
  + '.adf-policies,.adf-limits{display:flex;flex-wrap:wrap;gap:8px}'
  + '.adf-policies>.adf-field,.adf-limits>.adf-field{flex:1 1 140px}'
  + '.adf-params{display:flex;flex-direction:column;gap:8px}'
  + '.adf-param{border-top:var(--border-width) solid var(--border-soft);padding-top:8px;display:flex;flex-direction:column;gap:8px}'
  + '.adf-param-head{display:flex;flex-wrap:wrap;gap:12px;align-items:center;justify-content:space-between}'
  + '.adf-inline{display:inline-flex;gap:6px;align-items:center;font-size:13px}'
  + '.adf-raw{width:100%;min-height:220px;font-family:var(--font-mono,monospace);font-size:12px;box-sizing:border-box}'
  + '.adf-raw-box{display:flex;flex-direction:column;gap:6px}'
  + '.adf-icon{display:flex;flex-direction:column;gap:6px;min-width:0}'
  + '.adf-icon-head{display:flex;gap:8px;align-items:center}'
  + '.adf-icon-preview{display:inline-flex;width:24px;height:24px;color:var(--fg-1)}'
  + '.adf-icon-results{display:flex;flex-wrap:wrap;gap:6px}'
  + '.adf-icon-choice{width:36px;height:36px;padding:0;display:inline-flex;align-items:center;justify-content:center}'
  + '.adf-color{width:64px;height:36px;padding:2px}'
  + '.adf-deleted .rte{opacity:.5}';
