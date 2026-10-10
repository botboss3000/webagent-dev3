'use strict';

// Identity owns agent thresholds; the app-wide JSON retains hard safety limits.
export function renderCompaction(host, agent, persist) {
  const title = document.createElement('h3');
  title.textContent = 'Context compaction';
  const hint = document.createElement('p');
  hint.className = 'ac-hint';
  hint.textContent = 'Older completed exchanges are summarized when estimated history reaches the trigger. The newest tail stays verbatim when a safe boundary permits. Changes apply at the next model call.';
  const fallback = document.createElement('p');
  fallback.className = 'ac-hint';
  fallback.textContent = 'Context-limit fallback: if the summary trigger is higher than the System model’s effective input budget, compaction starts earlier at that budget, allowing for instructions and summary-request overhead. Your saved trigger stays unchanged. Each summary chunk must fit the System budget; if even the required instructions and minimum summary request cannot fit, the run pauses with a budget error.';
  const estimate = document.createElement('p');
  estimate.className = 'ac-hint';
  estimate.textContent = 'Token sizing is estimated, so actual provider counts can differ. Set per-model input and output budgets on Primary Models.';
  host.append(title, hint, fallback, estimate);
  const fields = [
    ['trigger_tokens', 'Summary trigger', 'Estimated history tokens before compaction'],
    ['tail_tokens', 'Verbatim tail', 'Recent estimated tokens to keep word for word'],
  ];
  const inputs = new Map();
  for (const [key, label, description] of fields) {
    const wrap = document.createElement('div');
    wrap.className = 'ac-cfg-field';
    const labelEl = document.createElement('label');
    labelEl.className = 'ac-label';
    labelEl.textContent = label;
    const input = document.createElement('input');
    input.type = 'number';
    input.min = '1';
    input.max = '1000000';
    input.step = '1';
    input.className = 'ac-input';
    input.id = 'identity-compaction-' + key;
    input.value = String(agent.compaction_thresholds[key]);
    input.readOnly = !agent.can_manage;
    labelEl.htmlFor = input.id;
    const help = document.createElement('p');
    help.className = 'ac-hint';
    help.textContent = description;
    wrap.append(labelEl, input, help);
    host.append(wrap);
    inputs.set(key, input);
  }
  const inherited = document.createElement('p');
  inherited.className = 'ac-hint';
  inherited.textContent = agent.compaction_valid === false
    ? 'Saved thresholds conflict with current app limits. Update them before the next model call.'
    : agent.compaction_overridden ? 'Agent override saved.'
    : 'Using app defaults: ' + agent.compaction_defaults.trigger_tokens.toLocaleString() +
      ' trigger, ' + agent.compaction_defaults.tail_tokens.toLocaleString() + ' tail.';
  host.append(inherited);
  if (!agent.can_manage) return;
  const status = document.createElement('p');
  status.className = 'ac-hint';
  status.setAttribute('role', 'status');
  const save = document.createElement('button');
  save.type = 'button';
  save.className = 'ac-btn ac-btn-ghost';
  save.textContent = 'Save compaction thresholds';
  save.addEventListener('click', async () => {
    const thresholds = {};
    for (const [key, input] of inputs) {
      const value = Number(input.value);
      if (!Number.isSafeInteger(value) || value < 1 || value > 1000000) {
        status.textContent = 'Enter whole numbers from 1 to 1,000,000.';
        return;
      }
      thresholds[key] = value;
    }
    if (thresholds.tail_tokens + agent.compaction_defaults.summary_tokens >= thresholds.trigger_tokens) {
      status.textContent = 'Trigger must exceed the tail plus the summary target (' +
        agent.compaction_defaults.summary_tokens.toLocaleString() + ').';
      return;
    }
    save.disabled = true;
    status.textContent = 'Saving…';
    try {
      const saved = await persist({compaction_thresholds: thresholds});
      agent = saved;
      inherited.textContent = 'Agent override saved.';
      status.textContent = 'Saved. New model calls use these thresholds.';
    } catch (error) {
      status.textContent = 'Save failed: ' + error.message;
    } finally {
      save.disabled = false;
    }
  });
  host.append(save, status);
}
