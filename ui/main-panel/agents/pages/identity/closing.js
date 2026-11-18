import {createSettingsRow} from '../../../../shared/settings-register/settings-register.js';
'use strict';
// Final response: whether every message becomes a chat bubble, and which run-ending parts the
// agent records. Guidance about publishing lives in each execution mode's per-turn guidance; the
// flag vocabulary and word limit stay app-wide in the closing gate configuration.
export function renderClosing(content, agent, persist) {
  if (!agent.closing_message) return;
  const {body:section} = createSettingsRow(content, {id:'final-response', title:'Final response'});
  section.classList.add('agent-closing-message-settings');
  const protocol = agent.closing_protocol || {};
  const flags = Array.isArray(protocol.flags) ? protocol.flags.join(', ') : 'the configured flag vocabulary';
  const switchRow = (key, title, help) => {
    const label = document.createElement('label');
    label.className = 'ac-label agent-final-response-switch';
    label.style.cssText = 'display:flex;align-items:flex-start;justify-content:space-between;gap:12px;-webkit-tap-highlight-color:transparent';
    const copy = document.createElement('span');
    const name = document.createElement('strong');
    name.textContent = title;
    const hint = document.createElement('span');
    hint.className = 'ac-hint';
    hint.style.display = 'block';
    hint.textContent = help;
    copy.append(name, hint);
    const wrap = document.createElement('span');
    wrap.className = 'conn-toggle-wrap ac-ability-toggle-wrap';
    wrap.dataset.inlineSwitch = '';
    const input = document.createElement('input');
    input.type = 'checkbox';
    input.className = 'conn-toggle';
    input.dataset.finalKey = key;
    input.setAttribute('aria-label', title);
    const track = document.createElement('span');
    track.className = 'conn-toggle-track';
    track.setAttribute('aria-hidden', 'true');
    wrap.append(input, track);
    label.append(copy, wrap);
    return {label, input};
  };
  const promote = switchRow('promote_all', 'Promote all messages',
    'Off: the agent shows working messages with the publish_main_message tool. On: every message becomes a chat bubble.');
  const flag = switchRow('require_flag', 'Run completion flag',
    'The agent records a private flag for how the run ended (one of ' + flags + ').');
  const note = switchRow('require_notification', 'Run completion statement',
    'The agent records a short notification or next step (about ' + (protocol.notification_target_words || 5) + ' words).');
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
  section.append(promote.label, flag.label, note.label, guidanceLabel, save, status);

  let saved = {...agent.closing_message};
  let promoteAll = (agent.message_visibility || {}).mode === 'auto';
  let busy = false;
  // While a save runs the switches stay where the user put them; they repaint from the
  // saved values once it returns (or fails).
  const paint = () => {
    const on = saved.enabled !== false;
    if (!busy) {
      promote.input.checked = promoteAll;
      flag.input.checked = on && saved.require_flag !== false;
      note.input.checked = on && saved.require_notification !== false;
      guidance.value = saved.notification_guidance || '';
    }
    // Saves are queued, so the switches stay usable while one runs.
    const editable = agent.can_manage;
    promote.input.disabled = flag.input.disabled = note.input.disabled = !editable;
    guidanceLabel.hidden = save.hidden = !note.input.checked;
    guidance.disabled = save.disabled = !editable;
  };
  // Saves run one after another, so a switch flipped while another saves is not lost.
  let queue = Promise.resolve();
  const commit = change => { queue = queue.then(() => save1(change)); return queue; };
  async function save1(change) {
    busy = true;
    paint();
    status.textContent = 'Saving…';
    try {
      const result = await persist(change);
      if (result.closing_message) saved = {...result.closing_message};
      if (result.message_visibility) promoteAll = result.message_visibility.mode === 'auto';
      status.textContent = 'Saved';
    } catch (error) {
      status.textContent = 'Save failed: ' + error.message;
    } finally {
      busy = false;
      paint();
    }
  }
  // The closing turn runs when either part is on.
  const closing = (requireFlag, requireNote) => ({closing_message: {...saved,
    require_flag: requireFlag, require_notification: requireNote, enabled: requireFlag || requireNote}});
  paint();
  promote.input.addEventListener('change', () => void commit({message_visibility: {mode: promote.input.checked ? 'auto' : 'manual'}}));
  flag.input.addEventListener('change', () => void commit(closing(flag.input.checked, note.input.checked)));
  note.input.addEventListener('change', () => void commit(closing(flag.input.checked, note.input.checked)));
  save.addEventListener('click', () => void commit({closing_message: {...saved, notification_guidance: guidance.value}}));
}
