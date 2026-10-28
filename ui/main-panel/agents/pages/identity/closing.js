import {createSettingsRow} from './settings-row.js';
'use strict';
// Agent-local closing status: whether it runs, which parts it requires, and notification guidance.
// The flag vocabulary and word limit stay app-wide in the closing gate configuration.
export function renderClosing(content, agent, persist) {
  if (!agent.closing_message) return;
  const {body:section, toggle} = createSettingsRow(content, {id:'final-response', title:'Final response', toggleLabel:'Enable closing status'});
  section.classList.add('agent-closing-message-settings');
  const hint = document.createElement('p');
  hint.className = 'ac-hint';
  hint.textContent = 'When enabled, the agent records a private completion flag and/or a short notification when it finishes. The final reply is promoted automatically either way.';
  const protocol = agent.closing_protocol || {};
  const flags = Array.isArray(protocol.flags) ? protocol.flags.join(', ') : 'the configured flag vocabulary';
  const requirement = (key, text) => {
    const label = document.createElement('label');
    label.className = 'ac-label';
    label.style.webkitTapHighlightColor = 'transparent';
    const input = document.createElement('input');
    input.type = 'checkbox';
    input.dataset.closingKey = key;
    label.append(input, document.createTextNode(' ' + text));
    return {label, input};
  };
  const flag = requirement('require_flag', 'Require completion flag (one of ' + flags + ')');
  const note = requirement('require_notification', 'Require notification / next step (at most ' + (protocol.notification_max_words || 5) + ' words)');
  const guidanceLabel = document.createElement('label');
  guidanceLabel.className = 'agent-closing-message-guidance';
  const guidanceName = document.createElement('span');
  guidanceName.textContent = 'Notification guidance';
  const guidance = document.createElement('textarea');
  guidance.className = 'ac-input';
  guidance.rows = 3;
  guidance.placeholder = 'Optional guidance for the private short notification.';
  guidanceLabel.append(guidanceName, guidance);
  const save = document.createElement('button');
  save.type = 'button';
  save.className = 'ac-btn ac-btn-ghost';
  save.style.webkitTapHighlightColor = 'transparent';
  save.textContent = 'Save';
  const status = document.createElement('span');
  status.setAttribute('role', 'status');
  status.setAttribute('aria-live', 'polite');
  section.append(hint, flag.label, note.label, guidanceLabel, save, status);

  let saved = {...agent.closing_message};
  let busy = false;
  const paint = () => {
    toggle.checked = saved.enabled !== false;
    flag.input.checked = saved.require_flag !== false;
    note.input.checked = saved.require_notification !== false;
    guidance.value = saved.notification_guidance || '';
    const editable = agent.can_manage && !busy;
    toggle.disabled = !editable;
    flag.input.disabled = note.input.disabled = !editable || !toggle.checked;
    guidance.disabled = save.disabled = !editable || !toggle.checked || !note.input.checked;
  };
  async function commit(change) {
    if (busy) return;
    busy = true;
    paint();
    status.textContent = 'Saving…';
    try {
      const result = await persist({closing_message: {...saved, ...change}});
      saved = {...result.closing_message};
      status.textContent = 'Saved';
    } catch (error) {
      status.textContent = 'Save failed: ' + error.message;
    } finally {
      busy = false;
      paint();
    }
  }
  paint();
  toggle.addEventListener('click', event => { event.stopPropagation(); void commit({enabled: saved.enabled === false}); });
  flag.input.addEventListener('change', () => void commit({require_flag: flag.input.checked}));
  note.input.addEventListener('change', () => void commit({require_notification: note.input.checked}));
  save.addEventListener('click', () => void commit({notification_guidance: guidance.value}));
}
