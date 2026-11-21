'use strict';

// Identity → Chat customization: this agent's own chat.json values (its chat overlay, merged
// over the app's chat.json for this agent's chats). The composer box turns a request in plain
// words into the matching settings and changes; the rows below show every described chat
// setting for one surface at a time (Common, Desktop, Mobile, Widget), each marked with where
// its value comes from and, when this agent overrides it, a Use app value action. Advanced
// lists every remaining chat.json value, drawn from its JSON type, saved through the same
// overlay (PATCH /agents/{id}/chat-config/chat).
import { createSubpage } from '../../../../shared/subpage-list/subpage-list.js';
import { createConfigList, createNumberRow, createSelectRow, createTextRow, createToggleRow }
  from '../../../../shared/config-rows/config-rows.js';
import { SURFACES, createComposerBox, createSettingRow, createSettingsStore, highlight, settingTitle }
  from '../../../../shared/config-rows/settings-page.js';
import { requestTouchVibration } from '../../../../shared/vibrate/vibrate.js';

const CHAT = 'config/ui/chat/chat.json';
const node = (tag, className = '', text = '') => {
  const element = document.createElement(tag);
  if (className) element.className = className;
  if (text) element.textContent = text;
  return element;
};
const words = text => text.replace(/[_-]+/g, ' ').trim();
const capital = text => text.charAt(0).toUpperCase() + text.slice(1);
// Area headings: a chat setting id's second part ("chat.<area>.…").
const AREAS = {chat_header: 'Header', composer: 'Message box', messages: 'Texts', message_bubbles: 'Message bubbles',
  message_gutters: 'Message buttons', gutter_presentation: 'Message buttons', message_headings: 'Message headings',
  message_spacing: 'Spacing', fade: 'Fades', pill_fade: 'Fades', idle_footer: 'Footer', active_footer: 'Footer',
  glimmer: 'Glow', screen_display: 'Screen', main_notes: 'Header'};
const area = id => { const key = id.split('.')[1] || ''; return AREAS[key] || capital(words(key)); };
const button = (label, onClick) => {
  const element = node('button', 'ac-btn ac-btn-sm', label);
  element.type = 'button';
  element.style.webkitTapHighlightColor = 'transparent';
  element.addEventListener('click', event => { requestTouchVibration(event); onClick(element); });
  return element;
};

export function renderChatCustomization(stage, request, {agentId, canManage}) {
  const {body} = createSubpage(stage, {id: 'chat-customization', title: 'Chat customization'});
  const page = 'agent:' + agentId;
  body.append(node('p', 'ac-hint', 'How chats with this agent look and behave. Values you change here apply to this '
    + 'agent only; everything else follows the app’s chat settings. Common is the shared default; Desktop, '
    + 'Mobile and Widget override it on those surfaces.'));
  const status = node('p', 'ac-hint');
  status.setAttribute('role', 'status');
  const composerHost = node('div');
  const surfaceHost = node('div');
  const lists = node('div');
  const advanced = node('details', 'stp-unavailable');
  advanced.append(node('summary', 'ac-hint', 'Advanced: every chat value'));
  body.append(composerHost, surfaceHost, status, lists, advanced);
  const store = createSettingsStore(request, page);
  let surface = 'common';
  const rows = new Map();

  const draw = () => {
    rows.clear();
    lists.replaceChildren();
    const chat = store.settings.filter(s => s.owner === CHAT && !s.unavailable
      && (s.params?.surface || 'common') === surface);
    const areas = new Map();
    for (const setting of chat) {
      const name = area(setting.id);
      if (!areas.has(name)) areas.set(name, []);
      areas.get(name).push(setting);
    }
    for (const [name, settings] of areas) {
      lists.append(node('div', 'ac-label stp-area', name));
      const list = createConfigList(lists, {label: name});
      for (const setting of settings) {
        const row = createSettingRow(list, {...setting, value: setting.value},
          {onSave: canManage ? store.save(setting) : undefined, title: settingTitle(setting, false)});
        if (!row) continue;
        if (!canManage) row.setDisabled(true);
        rows.set(setting.id, row.row);
        if (canManage && setting.source === 'agent') {
          row.row.querySelector('.ac-ability-label')?.append(button('Use app value', async element => {
            element.disabled = true;
            try { await store.reset(setting); draw(); } catch (error) { status.textContent = error.message; element.disabled = false; }
          }));
        }
      }
    }
    if (!chat.length) lists.append(node('p', 'ac-hint', 'No chat settings are described for this surface yet.'));
  };
  const reload = async () => { await store.load(); draw(); };

  const surfaceRow = createSelectRow(createConfigList(surfaceHost, {label: 'Surface'}), {title: 'Surface',
    hint: 'Which chat these rows change.', options: SURFACES, value: surface,
    onSave: async value => { surface = value; draw(); }});
  surfaceRow.control.dataset.field = 'chat-surface';

  if (canManage) {
    createComposerBox(composerHost, {request, page, placeholder: 'e.g. "bigger message text and hide the header on phones"',
      onOpen: (id, chosen) => {
        if (chosen && chosen !== surface) { surface = chosen; surfaceRow.setValue(chosen); draw(); }
        const row = rows.get(id);
        if (row) highlight(row);
      },
      onApplied: reload});
    const resetAll = button('Reset all to app settings', async element => {
      if (!confirm('Remove all of this agent’s chat overrides? Its chats go back to the app’s settings.')) return;
      element.disabled = true;
      try {
        const overlay = await request('/agents/' + encodeURIComponent(agentId) + '/chat-config/chat');
        await request('/agents/' + encodeURIComponent(agentId) + '/chat-config/chat', 'DELETE',
          {expected_revision: overlay.revision});
        await reload();
        status.textContent = 'Chat settings reset to the app’s.';
      } catch (error) {
        status.textContent = error.message;
      } finally {
        element.disabled = false;
      }
    });
    body.append(resetAll);
  }
  advanced.addEventListener('toggle', () => { if (advanced.open && !advanced.dataset.drawn) void drawAdvanced(); });

  // Every scalar chat.json value, one lazily drawn section per top-level part.
  async function drawAdvanced() {
    advanced.dataset.drawn = '1';
    const path = '/agents/' + encodeURIComponent(agentId) + '/chat-config/chat';
    let doc;
    try { doc = await request(path); } catch (error) { advanced.append(node('p', 'ac-hint', error.message)); return; }
    let revision = doc.revision;
    const own = (pointer, overlay = doc.overlay) => pointer.split('/').slice(1)
      .reduce((value, key) => value && typeof value === 'object' ? value[key] : undefined, overlay) !== undefined;
    let queue = Promise.resolve();
    const patch = (pointer, value) => {
      const task = queue.then(async () => {
        const nested = pointer.split('/').slice(1).reverse().reduce((inner, key) => ({[key]: inner}), value);
        const saved = await request(path, 'PATCH', {patch: nested, expected_revision: revision});
        revision = saved.revision;
        doc.overlay = saved.overlay;
      });
      queue = task.catch(() => {});
      return task;
    };
    const leaves = (value, pointer, out) => {
      if (value && typeof value === 'object' && !Array.isArray(value)) {
        for (const [key, child] of Object.entries(value)) {
          if (key !== '_instructions' && key !== 'description') leaves(child, pointer + '/' + key, out);
        }
      } else if (!Array.isArray(value)) {
        out.push([pointer, value]);
      }
      return out;
    };
    advanced.append(node('p', 'ac-hint', 'Raw values from chat.json, by their key names. Changes apply to this agent only.'));
    for (const [part, value] of Object.entries(doc.effective || {})) {
      if (!value || typeof value !== 'object' || Array.isArray(value)) continue;
      const section = node('details', 'stp-unavailable');
      section.append(node('summary', 'ac-hint', capital(words(part))));
      advanced.append(section);
      section.addEventListener('toggle', () => {
        if (!section.open || section.dataset.drawn) return;
        section.dataset.drawn = '1';
        const list = createConfigList(section, {label: part});
        for (const [pointer, current] of leaves(value, '/' + part, [])) {
          const title = pointer.split('/').slice(2).map(words).join(' › ');
          const hint = own(pointer) ? 'This agent’s value' : '';
          const onSave = canManage ? next => patch(pointer, next) : undefined;
          const disabled = !canManage;
          if (typeof current === 'boolean') createToggleRow(list, {title, hint, value: current, disabled, onSave});
          else if (typeof current === 'number') createNumberRow(list, {title, hint, value: current, disabled, onSave,
            step: Number.isInteger(current) ? 1 : 0.01});
          else createTextRow(list, {title, hint, value: current ?? '', disabled, placeholder: current == null ? 'none' : '', onSave});
        }
      });
    }
  }

  void reload().catch(error => { status.textContent = 'Chat settings unavailable: ' + error.message; });
}
