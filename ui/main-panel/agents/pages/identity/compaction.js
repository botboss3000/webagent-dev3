'use strict';
import {createSubpage} from '../../../../shared/subpage-list/subpage-list.js';

// Identity owns agent thresholds; the app-wide JSON retains hard safety limits.
export function renderCompaction(host, agent, persist, request) {
  const row = createSubpage(host, {id:'context-compaction', title:'Context compaction'});
  host = row.body;
  const hint = document.createElement('p');
  hint.className = 'ac-hint';
  hint.textContent = 'Older completed exchanges are summarized when the estimated whole model request reaches the trigger. The summary runs in the background while the agent keeps working; the agent stops to wait for it only at the ceiling. Each summary is added as a new block, and earlier blocks stay unchanged. The newest tail stays verbatim when a safe boundary permits. Changes apply at the next model call.';
  const fallback = document.createElement('p');
  fallback.className = 'ac-hint';
  fallback.textContent = 'Context-limit fallback: when the System model writes the summaries and the history since the last summary would outgrow its input budget, compaction starts earlier at that budget. Your saved trigger stays unchanged. Each System summary chunk must fit that budget; if even the required instructions and minimum summary request cannot fit, the run pauses with a budget error.';
  const estimate = document.createElement('p');
  estimate.className = 'ac-hint';
  estimate.textContent = 'Token sizing is estimated, so actual provider counts can differ. Set per-model input and output budgets on Primary Models.';
  const scope = document.createElement('p');
  scope.className = 'ac-hint';
  scope.textContent = 'The trigger counts instructions, tool definitions, history and the verbatim tail together. Only eligible older history is summarized. For example, at a 150,000-token trigger, 50,000 tokens of instructions, tools and retained recent history leave about 100,000 tokens eligible for compaction.';
  // What every model call resends (instructions, skills, Main Notes, tool schemas), so the
  // trigger can be set above it with room left for history.
  const fixed = document.createElement('p');
  fixed.className = 'ac-hint';
  fixed.textContent = 'Measuring the fixed context…';
  const room = document.createElement('p');
  room.className = 'ac-hint';
  room.setAttribute('role', 'status');
  host.append(hint, scope, fixed, room, fallback, estimate);
  let fixedTokens = null;
  const checkRoom = () => {
    const trigger = Number(inputs.get('trigger_tokens')?.value);
    const tail = Number(inputs.get('tail_tokens')?.value);
    if (fixedTokens === null || !Number.isFinite(trigger) || !Number.isFinite(tail)) {
      room.textContent = '';
      return;
    }
    const needed = fixedTokens + tail + agent.compaction_defaults.summary_tokens;
    room.textContent = trigger >= needed
      ? 'About ' + (trigger - fixedTokens).toLocaleString() + ' tokens of the trigger are left for history.'
      : 'The trigger is below the fixed context plus the verbatim tail and summary (' +
        needed.toLocaleString() + ' tokens): background summaries cannot start, and only the ceiling folds.';
  };
  if (request) {
    request('/agents/' + encodeURIComponent(agent.id) + '/context-composition').then((data) => {
      const part = (name) => Number(data?.breakdown?.[name]?.tokens) || 0;
      fixedTokens = Number(data?.tokens) || 0;
      fixed.textContent = 'Fixed context on every call (estimated): ' +
        fixedTokens.toLocaleString() + ' tokens (prompt ' + part('system_prompt').toLocaleString() +
        ', skills ' + part('skills').toLocaleString() + ', tools ' +
        part('tools').toLocaleString() + ').';
      checkRoom();
    }).catch(() => { fixed.textContent = 'The fixed context size is unavailable.'; });
  } else {
    fixed.hidden = true;
  }
  const fields = [
    ['trigger_tokens', 'Summary trigger', 'Estimated total request tokens, including instructions, tools and the verbatim tail'],
    ['tail_tokens', 'Verbatim tail', 'Recent estimated tokens to keep word for word'],
    ['ceiling_tokens', 'Ceiling', 'Size at which the agent stops to finish the summary. Below the trigger, the trigger is used. The model’s context limit caps both'],
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
    // An absent ceiling means the trigger.
    input.value = String(agent.compaction_thresholds[key] ?? agent.compaction_defaults[key] ??
      agent.compaction_thresholds.trigger_tokens);
    input.readOnly = !agent.can_manage;
    labelEl.htmlFor = input.id;
    const help = document.createElement('p');
    help.className = 'ac-hint';
    help.textContent = description;
    wrap.append(labelEl, input, help);
    host.append(wrap);
    inputs.set(key, input);
    input.addEventListener('input', checkRoom);
  }
  // Same model reuses the running request's cached prefix; Claude and OpenCode bridges answer from a
  // fork of the chat's session, Muse uses System.
  const summarizerWrap = document.createElement('div');
  summarizerWrap.className = 'ac-cfg-field';
  const summarizerLabel = document.createElement('label');
  summarizerLabel.className = 'ac-label';
  summarizerLabel.textContent = 'Summarizer';
  const summarizer = document.createElement('select');
  summarizer.className = 'ac-input';
  summarizer.id = 'identity-compaction-summarizer';
  summarizer.disabled = !agent.can_manage;
  for (const [value, text] of [['self', 'Same model'], ['system', 'System model']]) {
    const option = document.createElement('option');
    option.value = value;
    option.textContent = text;
    summarizer.append(option);
  }
  summarizer.value = agent.compaction_thresholds.summarizer || agent.compaction_defaults.summarizer || 'system';
  summarizerLabel.htmlFor = summarizer.id;
  const summarizerHelp = document.createElement('p');
  summarizerHelp.className = 'ac-hint';
  summarizerHelp.textContent = 'Same model writes the summary from the agent’s own request, which reuses its provider cache. Claude and OpenCode bridges answer from a fork of the chat’s session when their client supports it; Muse, and any summary that fails, use the System model.';
  summarizerWrap.append(summarizerLabel, summarizer, summarizerHelp);
  host.append(summarizerWrap);
  const inherited = document.createElement('p');
  inherited.className = 'ac-hint';
  const describe = () => {
    inherited.textContent = agent.compaction_valid === false
      ? 'Saved thresholds conflict with current app limits. Update them before the next model call.'
      : agent.compaction_overridden ? 'Agent override saved.'
      : 'Using app defaults: ' + agent.compaction_defaults.trigger_tokens.toLocaleString() +
        ' trigger, ' + agent.compaction_defaults.tail_tokens.toLocaleString() + ' tail, ' +
        (agent.compaction_defaults.ceiling_tokens ?? agent.compaction_defaults.trigger_tokens).toLocaleString() +
        ' ceiling.';
  };
  describe();
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
    thresholds.summarizer = summarizer.value;
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
      describe();
      reset.disabled = !agent.compaction_overridden;
      status.textContent = 'Saved. New model calls use these thresholds.';
    } catch (error) {
      status.textContent = 'Save failed: ' + error.message;
    } finally {
      save.disabled = false;
    }
  });
  // Drops this agent's override so later changes to the app defaults reach it again.
  const reset = document.createElement('button');
  reset.type = 'button';
  reset.className = 'ac-btn ac-btn-ghost';
  reset.textContent = 'Use app defaults';
  reset.disabled = !agent.compaction_overridden;
  reset.addEventListener('click', async () => {
    reset.disabled = true;
    status.textContent = 'Saving…';
    try {
      agent = await persist({reset_compaction_thresholds: true});
      for (const [key, input] of inputs) {
        input.value = String(agent.compaction_thresholds[key] ?? agent.compaction_defaults[key] ??
          agent.compaction_thresholds.trigger_tokens);
      }
      summarizer.value = agent.compaction_thresholds.summarizer || agent.compaction_defaults.summarizer || 'system';
      describe();
      status.textContent = 'Saved. New model calls use the app defaults.';
    } catch (error) {
      status.textContent = 'Reset failed: ' + error.message;
    } finally {
      reset.disabled = !agent.compaction_overridden;
    }
  });
  const actions = document.createElement('div');
  actions.className = 'ac-actions';
  actions.append(save, reset);
  host.append(actions, status);
}
