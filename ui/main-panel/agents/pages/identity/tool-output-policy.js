'use strict';
import {requestTouchVibration} from '../../../../shared/vibrate.js';

const scopeOptions = [
  ['code_index', 'Code Index tools'],
  ['selected', 'Selected tools'],
  ['all_eligible', 'All eligible read tools'],
];

function createField(host, label, helpText) {
  const wrap = document.createElement('div');
  wrap.className = 'ac-cfg-field';
  const labelEl = document.createElement('label');
  labelEl.className = 'ac-label';
  labelEl.textContent = label;
  const help = document.createElement('p');
  help.className = 'ac-hint';
  help.textContent = helpText;
  wrap.append(labelEl);
  host.append(wrap);
  return {wrap, labelEl, help};
}

function staleMessage(error) {
  const message = error?.message || String(error);
  return /revision|stale|reload before saving/i.test(message)
    ? 'Identity changed elsewhere. Reload this page before saving.'
    : 'Save failed: ' + message;
}

function renderExperimentalPolicy(host, agent, persist, available) {
  const title = document.createElement('h3');
  title.textContent = 'Lifetime/budget retention';
  const hint = document.createElement('p');
  hint.className = 'ac-hint';
  hint.textContent = available
    ? 'Under development · Experimental — disabled. Saved lifetime and budget values are preserved for a later strategy revision.'
    : 'Under development · Experimental — backend update pending. Saved settings below are read-only; the running lifetime/budget policy is unchanged.';
  host.append(title, hint);

  const canManage = false;
  let policy = structuredClone(agent.tool_output_policy || agent.tool_output_defaults || {});
  const eligible = Array.isArray(agent.tool_output_eligible_tools)
    ? agent.tool_output_eligible_tools : [];

  const enabledField = createField(host, 'Enable retention',
    'When off, existing results remain unchanged. You can still save the other settings for later.');
  const enabled = document.createElement('input');
  enabled.type = 'checkbox';
  enabled.id = 'identity-tool-output-enabled';
  enabled.checked = Boolean(policy.enabled);
  enabled.disabled = !canManage;
  enabledField.labelEl.htmlFor = enabled.id;
  enabledField.wrap.append(enabled);

  const scopeField = createField(host, 'Retention scope',
    'Choose which eligible read tools can have older results shortened.');
  const scope = document.createElement('select');
  scope.className = 'ac-input';
  scope.id = 'identity-tool-output-scope';
  scope.disabled = !canManage;
  for (const [value, label] of scopeOptions) {
    const option = document.createElement('option');
    option.value = value;
    option.textContent = label;
    scope.append(option);
  }
  scopeField.labelEl.htmlFor = scope.id;
  scopeField.wrap.append(scope, scopeField.help);

  const toolsField = createField(host, 'Selected tools',
    'This list is used only for Selected tools scope. Only eligible read tools are available.');
  const toolList = document.createElement('div');
  toolList.style.cssText = 'display:flex;flex-direction:column;gap:6px;padding:4px 0';
  const toolChecks = new Map();
  for (const name of eligible) {
    const row = document.createElement('label');
    row.className = 'ac-label';
    row.style.cssText = 'display:flex;align-items:center;gap:8px';
    const check = document.createElement('input');
    check.type = 'checkbox';
    check.value = name;
    check.dataset.toolOutputTool = name;
    check.disabled = !canManage;
    const text = document.createElement('span');
    text.textContent = name;
    row.append(check, text);
    toolList.append(row);
    toolChecks.set(name, check);
  }
  toolsField.wrap.append(toolList, toolsField.help);

  const numberFields = [
    ['lifetime_calls', 'Lifetime in model calls', 1, 1000,
      'How many model calls an ordinary result stays in active context before it is shortened.'],
    ['ordinary_chars', 'Ordinary result characters', 1024, 262144,
      'Character budget for ordinary eligible results.'],
    ['promoted_chars', 'Promoted result characters', 1024, 262144,
      'Character budget for manually promoted results and results promoted by an edit.'],
  ];
  const numbers = new Map();
  for (const [key, label, min, max, description] of numberFields) {
    const field = createField(host, label, description);
    const input = document.createElement('input');
    input.type = 'number';
    input.className = 'ac-input';
    input.min = String(min);
    input.max = String(max);
    input.step = '1';
    input.id = 'identity-tool-output-' + key;
    input.disabled = !canManage;
    field.labelEl.htmlFor = input.id;
    field.wrap.append(input, field.help);
    numbers.set(key, input);
  }

  const optionFields = [
    ['refresh_on_recall', 'Refresh lifetime when recalled',
      'A successful recall starts a fresh retention window.'],
    ['promote_on_edit', 'Promote results used for edits',
      'Results used by a successful edit receive the promoted character budget.'],
  ];
  const options = new Map();
  for (const [key, label, description] of optionFields) {
    const field = createField(host, label, description);
    const input = document.createElement('input');
    input.type = 'checkbox';
    input.id = 'identity-tool-output-' + key;
    input.disabled = !canManage;
    field.labelEl.htmlFor = input.id;
    field.wrap.append(input, field.help);
    options.set(key, input);
  }

  const status = document.createElement('p');
  status.className = 'ac-hint';
  status.setAttribute('role', 'status');
  status.setAttribute('aria-live', 'polite');
  host.append(status);

  const fill = (saved) => {
    agent = saved;
    policy = structuredClone(saved.tool_output_policy || saved.tool_output_defaults || {});
    enabled.checked = Boolean(policy.enabled);
    scope.value = policy.scope || 'code_index';
    const selected = new Set(Array.isArray(policy.tools) ? policy.tools : []);
    for (const [name, check] of toolChecks) check.checked = selected.has(name);
    for (const [key, input] of numbers) input.value = String(policy[key]);
    for (const [key, input] of options) input.checked = Boolean(policy[key]);
    reset.disabled = !canManage || !saved.tool_output_overridden;
  };

  const save = document.createElement('button');
  save.type = 'button';
  save.className = 'ac-btn ac-btn-ghost';
  save.textContent = 'Save tool output settings';
  save.disabled = !canManage;
  save.addEventListener('click', async () => {
    if (!canManage || save.disabled) return;
    const next = {
      enabled: enabled.checked,
      scope: scope.value,
      tools: [...toolChecks].filter(([, check]) => check.checked).map(([name]) => name),
      refresh_on_recall: options.get('refresh_on_recall').checked,
      promote_on_edit: options.get('promote_on_edit').checked,
    };
    for (const [key, input] of numbers) {
      const value = Number(input.value);
      if (!Number.isSafeInteger(value) || value < Number(input.min) || value > Number(input.max)) {
        status.textContent = 'Enter whole numbers from ' + Number(input.min).toLocaleString() +
          ' to ' + Number(input.max).toLocaleString() + '.';
        return;
      }
      next[key] = value;
    }
    if (next.scope === 'selected' && next.tools.length === 0) {
      status.textContent = 'Choose at least one tool for Selected tools scope.';
      return;
    }
    save.disabled = true;
    reset.disabled = true;
    status.textContent = 'Saving…';
    try {
      const saved = await persist({tool_output_policy: next});
      fill(saved);
      status.textContent = 'Saved. New model calls use these settings.';
    } catch (error) {
      status.textContent = staleMessage(error);
    } finally {
      save.disabled = !canManage;
      reset.disabled = !canManage || !agent.tool_output_overridden;
    }
  });
  const reset = document.createElement('button');
  reset.type = 'button';
  reset.className = 'ac-btn ac-btn-ghost';
  reset.textContent = 'Reset to app defaults';
  reset.disabled = !canManage || !agent.tool_output_overridden;
  reset.addEventListener('click', async () => {
    if (!canManage || reset.disabled) return;
    save.disabled = true;
    reset.disabled = true;
    status.textContent = 'Resetting…';
    try {
      const saved = await persist({reset_tool_output_policy: true});
      fill(saved);
      status.textContent = 'Reset to app defaults. New model calls use these values.';
    } catch (error) {
      status.textContent = staleMessage(error);
      reset.disabled = !canManage || !agent.tool_output_overridden;
    } finally {
      save.disabled = !canManage;
    }
  });
  if (canManage) host.append(save, reset);
  if (!canManage) status.textContent = available
    ? 'Lifetime/budget eviction is disabled independently of duplicate-result suppression.'
    : 'Backend update pending. These saved values do not change the active runtime policy.';
  fill(agent);
}


export function renderToolOutputPolicy(host, agent, persist) {
  const title = document.createElement('h3');
  title.textContent = 'Duplicate tool results';
  const {wrap, labelEl, help} = createField(host, 'Suppress identical tool results',
    'Included tools still execute and save their complete results. Exact unchanged repeats reference a full result in context. Choose included tools on Abilities. Changes apply to future results at the next model call.');
  host.insertBefore(title, wrap);
  const enabled = document.createElement('input');
  enabled.type = 'checkbox';
  enabled.id = 'identity-tool-result-deduplication-enabled';
  wrap.style.webkitTapHighlightColor = 'transparent';
  enabled.addEventListener('click', event => {
    if (!enabled.disabled) requestTouchVibration(event);
  });
  labelEl.htmlFor = enabled.id;
  const available = typeof agent.tool_result_deduplication?.enabled === 'boolean';
  let savedEnabled = available && agent.tool_result_deduplication.enabled;
  enabled.checked = savedEnabled;
  enabled.disabled = !agent.can_manage || !available;
  const status = document.createElement('p');
  status.className = 'ac-hint';
  status.setAttribute('role', 'status');
  status.setAttribute('aria-live', 'polite');
  if (!available) status.textContent = 'Duplicate-result configuration is unavailable: backend update pending. No suppression setting can be saved yet.';
  else if (!agent.can_manage) status.textContent = 'Read-only: only an agent manager can change this setting.';
  wrap.append(enabled, help, status);
  enabled.addEventListener('change', async () => {
    if (!available || !agent.can_manage || enabled.disabled) {
      enabled.checked = savedEnabled;
      return;
    }
    enabled.disabled = true;
    status.textContent = 'Saving…';
    try {
      const saved = await persist({tool_result_deduplication: {enabled: enabled.checked}});
      savedEnabled = saved.tool_result_deduplication.enabled;
      status.textContent = 'Saved. Future results use this setting; existing references stay unchanged.';
    } catch (error) {
      status.textContent = staleMessage(error);
    } finally {
      enabled.checked = savedEnabled;
      enabled.disabled = !agent.can_manage || !available;
    }
  });
  const experimental = document.createElement('fieldset');
  experimental.disabled = true;
  experimental.style.cssText = 'border:0;padding:0;margin:0;min-width:0';
  host.append(experimental);
  const policy = structuredClone(agent.tool_output_policy || agent.tool_output_defaults || {});
  if (available) policy.enabled = false;
  renderExperimentalPolicy(experimental, {...agent, tool_output_policy: policy}, persist, available);
}
