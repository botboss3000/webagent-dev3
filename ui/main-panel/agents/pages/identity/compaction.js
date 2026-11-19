'use strict';
import {createSubpage} from '../../../../shared/subpage-list/subpage-list.js';
import {createConfigForm, createConfigList, createNumberRow, createSelectRow, createToggleRow} from '../../../../shared/config-rows/config-rows.js';

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
    const trigger = Number(rows.get('trigger_tokens')?.control.value);
    const tail = Number(rows.get('tail_tokens')?.control.value);
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
  // An absent ceiling means the trigger.
  const saved = (key) => agent.compaction_thresholds[key] ?? agent.compaction_defaults[key] ?? agent.compaction_thresholds.trigger_tokens;
  const summarizerValue = () => agent.compaction_thresholds.summarizer || agent.compaction_defaults.summarizer || 'system';
  // Fold by tasks: 'verbatim' folds by the verbatim tail; a number keeps that many finished tasks.
  const foldValue = () => agent.compaction_thresholds.fold_tasks_retained == null ? 'verbatim' : String(agent.compaction_thresholds.fold_tasks_retained);
  // Which session states keep pinging, with their defaults; dismissing a session's flag stops it.
  const keepStates = [
    ['completed', 'While completed', 'The last run finished normally.', true],
    ['needs_input', 'While waiting for you', 'The agent asked for input or an approval.', true],
    ['stopped', 'While stopped', 'The run was stopped.', false],
    ['error', 'After an error', 'The run failed or paused on an error.', false],
  ];
  const keepValue = (key) => {
    const keep = agent.compaction_thresholds.keep_alive;
    if (key === 'keep_alive_enabled') return Boolean(keep?.enabled);
    if (key === 'keep_alive_limit_minutes') return keep?.limit_minutes ?? 240;
    const state = keepStates.find(([name]) => 'keep_alive_' + name === key);
    return keep ? Boolean(keep[state[0]]) : state[3];
  };
  // Managers save the thresholds together with one explicit Save; others see them read-only.
  const manage = agent.can_manage;
  const status = document.createElement('p');
  status.className = 'ac-hint';
  status.setAttribute('role', 'status');
  const form = manage ? createConfigForm(host, {saveLabel:'Save compaction thresholds', resetLabel:'Use app defaults',
    onSave: async (values) => {
      status.textContent = '';
      const thresholds = {};
      for (const [key] of fields) {
        const value = Number(values[key]);
        if (!Number.isSafeInteger(value) || value < 1 || value > 1000000) throw Error('Enter whole numbers from 1 to 1,000,000.');
        thresholds[key] = value;
      }
      thresholds.summarizer = values.summarizer;
      if (values.fold_boundary !== 'verbatim') thresholds.fold_tasks_retained = Number(values.fold_boundary);
      const timeout = Number(values.cache_timeout_minutes);
      if (!Number.isSafeInteger(timeout) || timeout < 0 || timeout > 1440) throw Error('Enter a cache timeout from 0 to 1,440 minutes.');
      if (timeout > 0) thresholds.cache_timeout_minutes = timeout;
      // Absent keep-alive means off, so only a switched-on one is saved.
      if (values.keep_alive_enabled) {
        const limit = Number(values.keep_alive_limit_minutes);
        if (!Number.isSafeInteger(limit) || limit < 1 || limit > 240) throw Error('Enter a keep-alive limit from 1 to 240 minutes.');
        thresholds.keep_alive = {enabled:true, limit_minutes:limit};
        for (const [key] of keepStates) thresholds.keep_alive[key] = Boolean(values['keep_alive_' + key]);
      }
      if (thresholds.tail_tokens + agent.compaction_defaults.summary_tokens >= thresholds.trigger_tokens) {
        throw Error('Trigger must exceed the tail plus the summary target (' +
          agent.compaction_defaults.summary_tokens.toLocaleString() + ').');
      }
      agent = await persist({compaction_thresholds: thresholds});
      describe();
      reset.disabled = !agent.compaction_overridden;
      status.textContent = 'Saved. New model calls use these thresholds.';
    },
    // Drops this agent's override so later changes to the app defaults reach it again.
    onReset: async () => {
      reset.disabled = true;
      status.textContent = '';
      try {
        agent = await persist({reset_compaction_thresholds: true});
      } catch (error) {
        reset.disabled = !agent.compaction_overridden;
        throw Error('Reset failed: ' + error.message);
      }
      for (const [key, row] of rows) { row.setValue(key === 'summarizer' ? summarizerValue() : key === 'fold_boundary' ? 'verbatim' : key === 'cache_timeout_minutes' ? 0 : key.startsWith('keep_alive_') ? keepValue(key) : saved(key)); form.set(key, row.getValue(), true); }
      describe();
      checkRoom();
      reset.disabled = !agent.compaction_overridden;
      status.textContent = 'Saved. New model calls use the app defaults.';
    }}) : null;
  const list = createConfigList(host, {label:'Compaction thresholds'});
  const rows = new Map();
  for (const [key, title, hint] of fields) {
    const row = createNumberRow(list, {title, hint, value:saved(key), min:1, max:1000000, disabled:!manage, key, form});
    // Typing counts as an edit at once, so Save is ready before the field is left.
    row.control.addEventListener('input', () => { form?.set(key, Number(row.control.value)); checkRoom(); });
    rows.set(key, row);
  }
  // Same model reuses the running request's cached prefix; Claude and OpenCode bridges answer from a
  // fork of the chat's session, Muse uses System.
  rows.set('summarizer', createSelectRow(list, {title:'Summarizer', options:[['self', 'Same model'], ['system', 'System model']],
    value:summarizerValue(), disabled:!manage, key:'summarizer', form,
    hint:'Same model writes the summary from the agent’s own request, which reuses its provider cache. Claude and OpenCode bridges answer from a fork of the chat’s session when their client supports it; Muse, and any summary that fails, use the System model.'}));
  // Fold by tasks: a task starting folds the finished tasks behind those kept verbatim.
  rows.set('fold_boundary', createSelectRow(list, {title:'Fold boundary', options:[
    ['verbatim', 'Verbatim limit'], ['0', 'Tasks: current task'], ['1', 'Tasks: current + 1'],
    ['2', 'Tasks: current + 2'], ['3', 'Tasks: current + 3']],
    value:foldValue(), disabled:!manage, key:'fold_boundary', form,
    hint:'Verbatim limit folds the oldest history once the request reaches the trigger, keeping the verbatim tail. Tasks folds whole finished tasks from the session task list: when a new task starts (its first step begins), the finished tasks behind the ones kept are summarized, cut at the first message after each was closed. A task still being discussed or planned does not fold the one before it. The trigger and ceiling still apply as a safety limit.'}));
  // Zero leaves the timeout to each model's provider file (60 minutes unless it sets one).
  rows.set('cache_timeout_minutes', createNumberRow(list, {title:'Cache timeout (minutes)',
    hint:'How long the model provider keeps this agent’s prompt cache. After this much idle time the next call reorders the context first: skills and Main Notes changes kept beside summary blocks move into the instructions. 0 uses each model’s own setting (60 minutes unless its provider file sets one).',
    value:agent.compaction_thresholds.cache_timeout_minutes ?? 0, min:0, max:1440, disabled:!manage, key:'cache_timeout_minutes', form}));
  // Experimental keep-alive: pings resend the last request shortly before the cache timeout.
  // One expandable row: its switch turns keep-alive on, clicking the row opens its settings.
  const keepList = createConfigList(host, {label:'Cache keep-alive (experimental)'});
  const keepItem = document.createElement('div');
  keepItem.className = 'ac-row';
  keepList.append(keepItem);
  rows.set('keep_alive_enabled', createToggleRow(keepItem, {title:'Keep cache warm (experimental)',
    hint:'Shortly before the cache timeout, resend the session’s last request with a one-word question so the provider keeps it cached. Each ping is a small extra model call; its answer is not saved. Claude and OpenCode bridges answer from a fork of the chat’s session. Opening a session before its cache times out also earns it one ping, even past the limit or in a state switched off here.',
    value:keepValue('keep_alive_enabled'), disabled:!manage, key:'keep_alive_enabled', form}));
  const keepHead = keepItem.querySelector('.ac-ability-row');
  const keepChevron = document.createElement('span');
  keepChevron.className = 'ac-row-chevron ac-row-chevron--left';
  keepChevron.innerHTML = '<svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><polyline points="9 18 15 12 9 6"/></svg>';
  keepHead.prepend(keepChevron);
  keepHead.tabIndex = 0;
  keepHead.setAttribute('role', 'button');
  keepHead.setAttribute('aria-expanded', 'false');
  const keepBody = document.createElement('div');
  keepBody.className = 'ac-ability-body';
  keepItem.append(keepBody);
  const toggleKeep = () => keepHead.setAttribute('aria-expanded', String(keepItem.classList.toggle('expanded')));
  keepHead.addEventListener('click', event => { if (!event.target.closest('.conn-toggle-wrap, input, button')) toggleKeep(); });
  keepHead.addEventListener('keydown', event => {
    if (event.target === keepHead && (event.key === 'Enter' || event.key === ' ')) { event.preventDefault(); toggleKeep(); }
  });
  rows.set('keep_alive_limit_minutes', createNumberRow(keepBody, {title:'Stop after (minutes)',
    hint:'Stop pinging this long after the session’s last real model call (at most 240).',
    value:keepValue('keep_alive_limit_minutes'), min:1, max:240, disabled:!manage, key:'keep_alive_limit_minutes', form}));
  for (const [key, title, hint] of keepStates) {
    rows.set('keep_alive_' + key, createToggleRow(keepBody, {title, hint,
      value:keepValue('keep_alive_' + key), disabled:!manage, key:'keep_alive_' + key, form}));
  }
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
  if (!form) return;
  const reset = [...form.element.querySelectorAll('button')].find(button => button.textContent === 'Use app defaults');
  reset.disabled = !agent.compaction_overridden;
  form.attach();
  host.append(status);
}
