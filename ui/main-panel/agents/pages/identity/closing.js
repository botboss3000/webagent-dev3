import {createSettingsRow} from './settings-row.js';
'use strict';
// Agent-local closing style. Rust retains the fixed flag and notification contract.
export function renderClosing(content, agent, persist) {
  const settings = agent.closing_message;
  if (!settings) return;
  const {body:section} = createSettingsRow(content, {id:'final-response', title:'Final response'});
  section.classList.add('agent-closing-message-settings');
  const hint = document.createElement('p');
  hint.className = 'ac-hint';
  hint.textContent = 'The agent chooses a completion flag and a short private notification when it finishes. The final reply is promoted automatically. Notification guidance is optional.';
  section.append(hint);
  const list = document.createElement('div');
  list.className = 'ac-list agent-closing-message-list';
  section.append(list);
  const fixed = document.createElement('div');
  fixed.className = 'ac-ability-row agent-closing-message-row';
  const fixedLabel = document.createElement('div');
  fixedLabel.className = 'ac-ability-label';
  const fixedName = document.createElement('div');
  fixedName.className = 'ac-ability-name';
  fixedName.textContent = 'Fixed closing protocol';
  const fixedDesc = document.createElement('div');
  fixedDesc.className = 'ac-ability-desc';
  const protocol = agent.closing_protocol || {};
  const flags = Array.isArray(protocol.flags) ? protocol.flags.join(', ') : 'the configured flag vocabulary';
  fixedDesc.textContent = 'Choose one of ' + flags + ' and a notification of at most ' + (protocol.notification_max_words || 5) + ' words before the final reply.';
  fixedLabel.append(fixedName, fixedDesc);
  fixed.append(fixedLabel);
  list.append(fixed);
  const fields = [
    ['notification_guidance', 'Notification guidance', 'Optional guidance for the private short notification.'],
  ];
  let saved = { ...settings };
  for (const [key, labelText, placeholder] of fields) {
    const row = document.createElement('div');
    row.className = 'ac-ability-row agent-closing-message-row';
    const label = document.createElement('label');
    label.className = 'agent-closing-message-guidance';
    const name = document.createElement('span');
    name.textContent = labelText;
    const input = document.createElement('textarea');
    input.className = 'ac-input';
    input.rows = 3;
    input.value = saved[key] || '';
    input.placeholder = placeholder;
    input.disabled = !agent.can_manage;
    label.append(name, input);
    const save = document.createElement('button');
    save.type = 'button';
    save.className = 'ac-btn ac-btn-ghost';
    save.textContent = 'Save';
    save.disabled = !agent.can_manage;
    const status = document.createElement('span');
    status.setAttribute('role', 'status');
    status.setAttribute('aria-live', 'polite');
    save.addEventListener('click', async () => {
      const next = { ...saved, [key]: input.value };
      input.disabled = true;
      save.disabled = true;
      status.textContent = 'Saving…';
      try {
        const result = await persist({ closing_message: next });
        saved = { ...result.closing_message };
        input.value = saved[key] || '';
        status.textContent = 'Saved';
      } catch (error) {
        input.value = saved[key] || '';
        status.textContent = 'Save failed: ' + error.message;
      } finally {
        input.disabled = !agent.can_manage;
        save.disabled = !agent.can_manage;
      }
    });
    row.append(label, save, status);
    list.append(row);
  }
}
