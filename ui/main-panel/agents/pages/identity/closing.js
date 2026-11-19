import {createSubpage} from '../../../../shared/subpage-list/subpage-list.js';
import {createConfigForm, createConfigList, createLongTextRow, createToggleRow} from '../../../../shared/config-rows/config-rows.js';
// Final response: whether every message becomes a chat bubble, and which run-ending parts the
// agent records. Guidance about publishing lives in each execution mode's per-turn guidance; the
// flag vocabulary and word limit stay app-wide in the closing gate configuration.
export function renderClosing(content, agent, persist) {
  if (!agent.closing_message) return;
  const {body} = createSubpage(content, {id:'final-response', title:'Final response'});
  const protocol = agent.closing_protocol || {};
  const flags = Array.isArray(protocol.flags) ? protocol.flags.join(', ') : 'the configured flag vocabulary';
  let saved = {...agent.closing_message};
  let promoteAll = (agent.message_visibility || {}).mode === 'auto';
  let busy = false;
  // Saves run one after another, so a switch flipped while another saves is not lost.
  let queue = Promise.resolve();
  const commit = change => { const task = queue.then(() => save1(change)); queue = task.catch(() => {}); return task; };
  async function save1(change) {
    busy = true;
    paint();
    try {
      const result = await persist(change);
      if (result.closing_message) saved = {...result.closing_message};
      if (result.message_visibility) promoteAll = result.message_visibility.mode === 'auto';
    } finally {
      busy = false;
      paint();
    }
  }
  // The closing turn runs when either part is on.
  const closing = () => commit({closing_message: {...saved,
    require_flag: flag.getValue(), require_notification: note.getValue(), enabled: flag.getValue() || note.getValue()}});
  const list = createConfigList(body, {label:'Final response settings'});
  // Each switch saves on its own; these stay on the page, not in the category header.
  const toggle = (key, title, hint, onSave) => {
    const row = createToggleRow(list, {title, hint, disabled:!agent.can_manage, onSave});
    row.control.dataset.finalKey = key;
    return row;
  };
  const promote = toggle('promote_all', 'Promote all messages',
    'Off: the agent shows working messages with the publish_main_message tool. On: every message becomes a chat bubble.',
    on => commit({message_visibility: {mode: on ? 'auto' : 'manual'}}));
  const flag = toggle('require_flag', 'Run completion flag',
    'The agent records a private flag for how the run ended (one of ' + flags + ').', closing);
  const note = toggle('require_notification', 'Run completion statement',
    'The agent records a short notification or next step (about ' + (protocol.notification_target_words || 5) + ' words).', closing);
  // The guidance saves with its own button and keeps the other saved closing settings.
  const form = createConfigForm(body, {onSave: values => commit({closing_message: {...saved, notification_guidance: values.notification_guidance}})});
  const guidance = createLongTextRow(list, {title:'Notification guidance', value:saved.notification_guidance || '',
    placeholder:'Optional guidance for the private short notification.', minHeight:80, disabled:!agent.can_manage, key:'notification_guidance', form});
  form.attach();
  // While a save runs the switches stay where the user put them; they repaint from the
  // saved values once it returns (or fails).
  function paint() {
    const on = saved.enabled !== false;
    if (!busy) {
      promote.setValue(promoteAll);
      flag.setValue(on && saved.require_flag !== false);
      note.setValue(on && saved.require_notification !== false);
    }
    // The row and bar set their own display, so the hidden attribute alone would not hide them.
    guidance.row.style.display = form.element.style.display = note.getValue() ? '' : 'none';
  }
  paint();
}
