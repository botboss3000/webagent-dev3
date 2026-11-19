// Agent configuration is distinct from capability-level views; Rust enforces every save.
import { buildConfigRow } from './ability-row-controls/ability-row-controls.js';
import { createToggleRow, createSelectRow, createNumberRow, createLongTextRow } from '../../../../shared/config-rows/config-rows.js';
const node = (tag, cls = '', text = '') => {
  const el = document.createElement(tag); el.className = cls; el.textContent = text; return el;
};
const row = (name, control, hint = '') => buildConfigRow({ name, desc: hint, descTitle: hint, control });
const field = (name, value = '', type = 'text', maxLength = 2048) => {
  const input = node('input', 'ac-input'); input.type = type; input.value = value;
  input.maxLength = maxLength; input.setAttribute('aria-label', name); return input;
};
const message = error => error?.status === 409
  ? 'Configuration changed. Reload abilities before saving again.'
  : 'Configuration could not be saved. Check the fields and your agent authority.';
// Save one settings change in place; the returned revision is kept for the next save
// instead of re-reading the whole page. The row shows the spinner, check or error.
async function saveSettings({ ability, data, request, scope, status, settings, notice }) {
  try {
    const saved = await request(scope + '/abilities/' + encodeURIComponent(ability.id) + '/settings', 'PUT',
      { expected_revision: data.revision, settings });
    data.revision = saved.revision;
    data.settings = saved.settings || data.settings;
    status.textContent = notice;
  } catch (error) {
    status.textContent = message(error);
    throw new Error(message(error));
  }
}
// One revision covers a whole settings object: each change sends every value, and changes
// queue so each save carries the revision the previous one returned.
function wholeObjectSaver({ values, check, saving, notice, ...context }) {
  let queue = Promise.resolve();
  return key => value => {
    const run = queue.then(async () => {
      const previous = values[key];
      values[key] = value;
      const problem = check(values);
      if (problem) { values[key] = previous; context.status.textContent = problem; throw new Error(problem); }
      context.status.textContent = saving;
      try { await saveSettings({ ...context, settings: { ...values }, notice }); }
      catch (error) { values[key] = previous; throw error; }
    });
    queue = run.catch(() => {});
    return run;
  };
}
// Agent settings are edited at the agent's own (Operator) level; other levels are read-only.
const editable = levelId => !levelId || levelId === 'operator';
const settingsSpec = {
  git_control: ['notify_repo_changes', 'Notify repository changes'],
};
function buildOptimizationControls({ ability, data, request, scope, levelId, status }) {
  const panel = node('div', 'ac-list v3-administrator-settings');
  const enabled = editable(levelId);
  panel.append(node('p', 'ac-hint', enabled
    ? 'Defaults for explicit /optimize requests. Command overrides apply to that request only. Experiments produce a candidate for review.'
    : 'Optimization settings are read-only while viewing another capability level.'));
  const help = node('div', 'ac-list');
  help.append(node('p', 'ac-ability-name', 'How to use Optimization'));
  help.append(node('p', 'ac-hint',
    'Start a chat with this agent and send /optimize followed by a code optimization goal. The account and agent need permission for Optimization and Codebase Admin; permitted Discoverable abilities activate automatically.'));
  const example = node('pre', 'ac-hint', '/optimize Optimize for reduced RAM usage.');
  example.style.whiteSpace = 'pre-wrap'; example.style.overflowWrap = 'anywhere';
  help.append(example);
  help.append(node('p', 'ac-hint',
    'You can use the command without flags: it uses the saved settings on this page. Select Memory priority to favor lower peak RAM. Resource limits are the benchmark environment’s budget, not the desired resource use of the optimized code.'));
  help.append(node('p', 'ac-hint',
    'Cold builds may exceed the default 1024 MiB RAM budget. Use 4096 MiB for a cold build and increase Total runtime if needed. Optional flags override only one request and do not change saved settings:'));
  const override = node('pre', 'ac-hint',
    '/optimize --priority=memory --ram_mb=4096 --cpu_cores=2 --total_seconds=1800 Reduce RAM usage in the agent loop.');
  override.style.whiteSpace = 'pre-wrap'; override.style.overflowWrap = 'anywhere';
  help.append(override);
  help.append(node('p', 'ac-hint',
    'Each experiment runs the full correctness suite, then predefined v3 workloads: Quick measures 3 tests, Standard 6, and Extended 10. Your text describes the optimization goal; sample tasks such as “update this page” are not replayed as benchmark workloads.'));
  help.append(node('p', 'ac-hint',
    'Review the measured results and candidate changes before requesting a separate release. Prompt and skill optimization, and automatic candidate deployment, are excluded.'));
  panel.append(help);
  const current = { priority: 'balanced', attempts: 3, total_seconds: 900, profiling: 'both',
    cpu_cores: 1, ram_mb: 1024, coverage: 'standard', max_slowdown_percent: 0,
    max_memory_regression_percent: 0, allow_dependencies: false, ...(data.settings?.[ability.id] || {}) };
  const numbers = new Set();
  const values = {};
  const save = wholeObjectSaver({ ability, data, request, scope, status, values,
    saving: 'Saving Optimization settings…', notice: 'Optimization settings saved.',
    check: all => [...numbers].some(key => !Number.isInteger(all[key]))
      ? 'Enter integers within the displayed limits before saving.' : '' });
  // Rows are direct children of the panel: the page lifts them into the ability's list.
  for (const [key, title, choices] of [
    ['priority', 'Optimization priority', ['balanced', 'speed', 'memory']],
    ['profiling', 'Profiling', ['none', 'cpu', 'allocations', 'both']],
    ['coverage', 'Workload coverage', ['quick', 'standard', 'extended']],
  ]) {
    values[key] = createSelectRow(panel, { title, value: current[key], disabled: !enabled, onSave: save(key),
      options: choices.map(value => [value, value[0].toUpperCase() + value.slice(1)]) }).getValue();
  }
  for (const [key, title, min, max] of [
    ['attempts', 'Candidate attempts', 1, 10], ['total_seconds', 'Total runtime (seconds)', 1, 1800],
    ['cpu_cores', 'CPU cores', 1, 2], ['ram_mb', 'RAM limit (MiB)', 128, 4096],
    ['max_slowdown_percent', 'Maximum slowdown (%)', 0, 25],
    ['max_memory_regression_percent', 'Maximum memory regression (%)', 0, 25],
  ]) {
    numbers.add(key);
    values[key] = createNumberRow(panel, { title, value: current[key], min, max, disabled: !enabled,
      onSave: save(key) }).getValue();
  }
  values.allow_dependencies = createToggleRow(panel, { title: 'Dependency changes',
    hint: 'Permission and sandbox restrictions still apply.', value: current.allow_dependencies === true,
    disabled: !enabled, onSave: save('allow_dependencies') }).getValue();
  return panel;
}

// Abilities whose manifest settings save as one object through the generic validator.
const manifestSettings = new Set(['agent_management', 'browser_extension', 'browser_control', 'web_search']);
// The Default Agent reaches all owned sessions and may delegate unless its owner changes it.
const defaultAgentSettings = { session_scope: 'owned', allow_delegation: true };
function buildManifestSettings({ ability, data, request, scope, levelId, status }) {
  const declared = Array.isArray(ability.config?.settings) ? ability.config.settings : [];
  if (!declared.length) return null;
  const panel = node('div', 'ac-list v3-administrator-settings');
  const enabled = editable(levelId);
  panel.append(node('p', 'ac-hint', enabled
    ? 'Agent settings save as soon as you change them.'
    : 'Agent settings are read-only while viewing another capability level.'));
  const saved = data.settings?.[ability.id] || {};
  const fallback = data.agent_id === 'shared_default' ? defaultAgentSettings : {};
  const value = item => item.key in saved ? saved[item.key] : item.key in fallback ? fallback[item.key] : item.default;
  const values = {};
  const save = wholeObjectSaver({ ability, data, request, scope, status, values,
    saving: 'Saving agent settings…', notice: 'Agent settings saved.',
    check: all => declared.some(item => item.type === 'number' && !Number.isInteger(all[item.key]))
      ? 'Use a whole number within each setting\'s range.' : '' });
  for (const item of declared) {
    const options = { title: item.label, hint: item.hint || '', disabled: !enabled, onSave: save(item.key) };
    if (item.type === 'boolean') {
      values[item.key] = createToggleRow(panel, { ...options, value: value(item) === true }).getValue();
    } else if (item.type === 'select') {
      values[item.key] = createSelectRow(panel, { ...options, value: value(item),
        options: (item.options || []).map(choice => [choice.value, choice.label]) }).getValue();
    } else if (item.type === 'text') {
      values[item.key] = String(value(item) ?? '');
      createLongTextRow(panel, { ...options, value: values[item.key], maxLength: item.max_length || 2000 });
    } else {
      values[item.key] = createNumberRow(panel, { ...options, value: value(item), min: item.min, max: item.max,
        step: item.step || 1 }).getValue();
    }
  }
  return panel;
}
export function buildAdministratorControls({ ability, data, repository, request, scope,
  levelId, status, rerender }) {
  if (manifestSettings.has(ability.id) && data.authority?.can_manage === true) {
    return buildManifestSettings({ ability, data, request, scope, levelId, status });
  }
  if (ability.id === 'optimization' && data.authority?.can_manage === true) {
    return buildOptimizationControls({ ability, data, request, scope, levelId, status });
  }
  const spec = settingsSpec[ability.id];
  if (!spec || data.authority?.can_manage !== true) return null;
  const panel = node('div', 'ac-list v3-administrator-settings');
  const enabled = editable(levelId);
  panel.append(node('p', 'ac-hint', enabled
    ? 'Agent settings.'
    : 'Agent settings are read-only while viewing another capability level.'));
  const [key, title] = spec;
  createToggleRow(panel, { title, value: data.settings?.[ability.id]?.[key] === true, disabled: !enabled,
    onSave: async next => {
      status.textContent = 'Saving agent settings…';
      await saveSettings({ ability, data, request, scope, status, settings: { [key]: next },
        notice: 'Agent settings saved.' });
    } });
  panel.append(node('p', 'ac-hint', 'Repository connections belong to this agent. Only the connection owner can edit it. Tokens are write-only.'));
  if (repository?.load_failed || !repository || repository.agent_id !== data.agent_id) {
    panel.append(node('p', 'ac-hint', 'Repository connections unavailable. Reload abilities to retry.'));
    return panel;
  }
  const connections = Array.isArray(repository.connections) ? repository.connections.slice(0, 8) : [];
  const addConnection = (connection = null) => {
    const form = node('div', 'ac-list');
    const editable = enabled && repository.can_manage === true && (!connection || connection.can_edit === true);
    const id = field('Connection ID', connection?.id || '', 'text', 80);
    id.pattern = '[A-Za-z0-9_-]{1,80}'; id.required = true; id.disabled = !!connection || !editable;
    const url = field('HTTPS repository URL', connection?.url || '', 'url');
    url.required = true; url.placeholder = 'https://example.com/team/repository';
    const username = field('Repository username', connection?.username || 'git', 'text', 128);
    // A private token is never populated from a server response or retained after submission.
    const token = field('New repository token', '', 'password', 512);
    token.autocomplete = 'new-password'; token.placeholder = 'Leave empty to keep the existing token';
    const revoked = field('Revoke connection', '', 'checkbox'); revoked.checked = connection?.revoked === true;
    for (const input of [url, username, token, revoked]) input.disabled = !editable;
    form.append(node('p', 'ac-hint', connection
      ? connection.id + ' · ' + (connection.revoked ? 'Revoked' : connection.configured
        ? connection.authenticated ? 'Authenticated' : 'Public repository' : 'Unconfigured')
      : 'New connection (maximum 8 per agent)'));
    form.append(row('Connection ID', id), row('HTTPS repository URL', url,
      'No embedded credentials, query or fragment.'), row('Username', username),
      row('New token', token, 'Use a repository-scoped token. Changing URL or username clears the saved credential unless replaced.'),
      row('Revoked', revoked));
    const submit = node('button', 'ac-btn ac-btn-sm', connection ? 'Save connection' : 'Add connection');
    submit.type = 'button'; submit.disabled = !editable;
    submit.onclick = async () => {
      if (!editable || submit.disabled) return;
      const secret = token.value; token.value = '';
      let parsed;
      try { parsed = new URL(url.value); } catch { /* Reject locally without reflecting a credential-bearing URL. */ }
      if (!/^[A-Za-z0-9_-]{1,80}$/.test(id.value) || !parsed || parsed.protocol !== 'https:' ||
          parsed.username || parsed.password || parsed.search || parsed.hash || parsed.pathname === '/' ||
          parsed.pathname.includes('%') || url.value.trim() !== url.value ||
          /[\x00-\x20\x7f]/.test(url.value) ||
          !username.value || username.value.length > 128 || /[^\x21-\x7e]|['"\\@:]/.test(username.value) ||
          (secret && (secret.length < 8 || secret.length > 512 || /[^\x21-\x7e]/.test(secret)))) {
        status.textContent = 'Use a connection ID, a clean HTTPS repository URL, a valid username and an 8–512 character token if supplied.';
        return;
      }
      submit.disabled = true;
      for (const input of [id, url, username, token, revoked]) input.disabled = true;
      status.textContent = 'Saving repository connection…';
      try {
        await request(scope + '/repository-connections/' + encodeURIComponent(id.value), 'PUT', {
          expected_revision: repository.revision, url: url.value, username: username.value,
          ...(secret ? { token: secret } : {}), revoked: revoked.checked,
        });
        await rerender({ administratorNotice: 'Repository connection saved.' });
      } catch (error) {
        status.textContent = message(error); submit.disabled = !editable;
        for (const input of [url, username, token, revoked]) input.disabled = !editable;
        id.disabled = !!connection || !editable;
      } finally { token.value = ''; }
    };
    form.append(row(connection ? 'Update connection' : 'Create connection', submit)); panel.append(form);
  };
  for (const connection of connections) addConnection(connection);
  if (connections.length < 8) addConnection();
  else panel.append(node('p', 'ac-hint', 'This agent has reached its 8-connection limit.'));
  return panel;
}
