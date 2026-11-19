'use strict';
import {createSubpage} from '../../../../shared/subpage-list/subpage-list.js';
import {createConfigList, createNumberRow, createSelectRow, createToggleRow} from '../../../../shared/config-rows/config-rows.js';

const scopeOptions = [
  ['selected', 'Selected tools'],
  ['all_eligible', 'All eligible read tools'],
];

function staleMessage(error) {
  const message = error?.message || String(error);
  return /revision|stale|reload before saving/i.test(message)
    ? 'Identity changed elsewhere. Reload this page before saving.'
    : 'Save failed: ' + message;
}

// A read-only preview of the saved lifetime/budget policy: nothing here saves until the
// experimental strategy returns, so every row is disabled.
function renderExperimentalPolicy(host, agent, available, enabled) {
  const hint = document.createElement('p');
  hint.className = 'ac-hint';
  hint.textContent = available
    ? 'Under development · Experimental — disabled. Saved lifetime and budget values are preserved for a later strategy revision.'
    : 'Under development · Experimental — backend update pending. Saved settings below are read-only; the running lifetime/budget policy is unchanged.';
  host.append(hint);

  const policy = agent.tool_output_policy || agent.tool_output_defaults || {};
  const eligible = Array.isArray(agent.tool_output_eligible_tools)
    ? agent.tool_output_eligible_tools : [];
  enabled.checked = Boolean(policy.enabled);
  enabled.disabled = true;
  // Switches inside the page stay out of the category header.

  createSelectRow(createConfigList(host, {label: 'Retention scope setting'}), {title: 'Retention scope', hint: 'Choose which eligible read tools can have older results shortened.',
    options: scopeOptions, value: policy.scope || 'all_eligible', disabled: true});
  const toolHint = document.createElement('p');
  toolHint.className = 'ac-hint';
  toolHint.textContent = 'Selected tools: this list is used only for Selected tools scope. Only eligible read tools are available.';
  host.append(toolHint);
  const selected = new Set(Array.isArray(policy.tools) ? policy.tools : []);
  const tools = createConfigList(host, {label: 'Selected tools'});
  for (const name of eligible) {
    createToggleRow(tools, {title: name, value: selected.has(name), disabled: true}).control.dataset.toolOutputTool = name;
  }

  const list = createConfigList(host, {label: 'Lifetime and budget settings'});
  const numberFields = [
    ['lifetime_calls', 'Lifetime in model calls', 1, 1000,
      'How many model calls an ordinary result stays in active context before it is shortened.'],
    ['ordinary_chars', 'Ordinary result characters', 1024, 262144,
      'Character budget for ordinary eligible results.'],
    ['promoted_chars', 'Promoted result characters', 1024, 262144,
      'Character budget for manually promoted results and results promoted by an edit.'],
  ];
  for (const [key, title, min, max, hint] of numberFields) {
    createNumberRow(list, {title, hint, value: policy[key], min, max, disabled: true});
  }
  const optionFields = [
    ['refresh_on_recall', 'Refresh lifetime when recalled',
      'A successful recall starts a fresh retention window.'],
    ['promote_on_edit', 'Promote results used for edits',
      'Results used by a successful edit receive the promoted character budget.'],
  ];
  for (const [key, title, hint] of optionFields) {
    createToggleRow(list, {title, hint, value: Boolean(policy[key]), disabled: true});
  }

  const status = document.createElement('p');
  status.className = 'ac-hint';
  status.setAttribute('role', 'status');
  status.setAttribute('aria-live', 'polite');
  status.textContent = available
    ? 'Lifetime/budget eviction is disabled independently of duplicate-result suppression.'
    : 'Backend update pending. These saved values do not change the active runtime policy.';
  host.append(status);
}

export function renderToolOutputPolicy(host, agent, persist) {
  const parent = host;
  const row = createSubpage(host, {id:'duplicate-tool-results', title:'Duplicate tool results', toggleLabel:'Suppress identical tool results'});
  host = row.body;
  const help = document.createElement('p');
  help.className = 'ac-hint';
  help.textContent = 'Included tools still execute and save their complete results. Exact unchanged repeats reference a full result in context. Choose included tools on Abilities. Changes apply to future results at the next model call.';
  const enabled = row.toggle;
  enabled.id = 'identity-tool-result-deduplication-enabled';
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
  host.append(help, status);
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
  const retention = createSubpage(parent, {id:'lifetime-budget-retention', title:'Lifetime/budget retention', toggleLabel:'Enable retention'});
  retention.toggle.id = 'identity-tool-output-enabled';
  const experimental = document.createElement('fieldset');
  experimental.disabled = true;
  experimental.style.cssText = 'border:0;padding:0;margin:0;min-width:0';
  retention.body.append(experimental);
  const policy = structuredClone(agent.tool_output_policy || agent.tool_output_defaults || {});
  if (available) policy.enabled = false;
  renderExperimentalPolicy(experimental, {...agent, tool_output_policy: policy}, available, retention.toggle);
}
