// Actions list: the action ledger's recorded tool calls (protected or critical tools such as
// publishing or pushing) with their state. An uncertain action may or may not have taken
// effect; whoever may settle it checks the real outcome and marks it done or failed. Shared
// by the chat's Actions chip, the agent's Artifacts and Members pages and App → Actions.
// Text-only DOM: tool arguments are untrusted.
import {requestTouchVibration} from '../vibrate/vibrate.js';

const el = (tag, cls, text) => { const n = document.createElement(tag); if (cls) n.className = cls; if (text != null) n.textContent = text; return n; };
export const STATE = {prepared: 'Prepared', running: 'Running', completed: 'Done', confirmed_failed: 'Failed', uncertain: 'Uncertain'};
const when = seconds => seconds ? new Date(seconds * 1000).toLocaleString([], {dateStyle: 'short', timeStyle: 'short'}) : '';

/** Uncertain actions first, then newest first. */
export function sortActions(actions) {
  return [...actions].sort((a, b) => (a.state === 'uncertain' ? 0 : 1) - (b.state === 'uncertain' ? 0 : 1) || (b.updated_at || b.created_at || 0) - (a.updated_at || a.created_at || 0));
}

/**
 * Fills `box` with one row per action. `resolve(id, state)` settles an uncertain action
 * (omit it for a read-only list); `changed()` runs after a resolution so the caller reloads.
 */
export function renderActions(box, actions, {resolve, changed = () => {}, showSession = false, empty = 'No recorded actions yet.'} = {}) {
  box.replaceChildren();
  if (!actions.length) { box.append(el('p', 'actions-empty', empty)); return; }
  const list = el('ul', 'actions-list');
  for (const action of sortActions(actions)) {
    const row = el('li', 'actions-row'); row.dataset.state = action.state || '';
    const head = el('div', 'actions-row-head');
    head.append(el('strong', 'actions-tool', action.tool || 'tool'), el('span', 'actions-state', STATE[action.state] || action.state || ''));
    if (action.critical) head.append(el('span', 'actions-critical', 'Critical'));
    row.append(head);
    const meta = [when(action.updated_at || action.created_at), action.attempt ? 'Attempt ' + action.attempt : '', showSession && action.session_id ? 'Chat ' + String(action.session_id).slice(0, 8) : '', action.resolved_by ? 'Marked by ' + action.resolved_by : ''].filter(Boolean).join(' · ');
    if (meta) row.append(el('small', 'actions-meta', meta));
    if (action.arguments && typeof action.arguments === 'object') {
      const details = el('details', 'actions-args'); details.append(el('summary', '', 'Arguments'));
      details.append(el('pre', 'actions-args-body', JSON.stringify(action.arguments, null, 2).slice(0, 4000)));
      row.append(details);
    }
    if (action.state === 'uncertain') {
      row.append(el('p', 'actions-hint', 'This may or may not have taken effect. Check the real outcome, then mark it.'));
      if (resolve) {
        const buttons = el('div', 'actions-buttons');
        for (const [state, label] of [['completed', 'Mark done'], ['confirmed_failed', 'Mark failed']]) {
          const button = el('button', 'actions-btn', label); button.type = 'button';
          button.onclick = async event => {
            requestTouchVibration(event);
            const text = state === 'completed' ? 'Mark this action done? The agent will treat it as finished and not repeat it.' : 'Mark this action failed? The agent may then try it again.';
            if (!window.confirm(text)) return;
            for (const b of buttons.querySelectorAll('button')) b.disabled = true;
            try { await resolve(action.id, state); changed(); }
            catch (error) { row.append(el('p', 'actions-error', error.message)); for (const b of buttons.querySelectorAll('button')) b.disabled = false; }
          };
          buttons.append(button);
        }
        row.append(buttons);
      }
    }
    list.append(row);
  }
  box.append(list);
}

/** Adds the list's stylesheet once, for pages that do not load the chat styles. */
function ensureStyles() {
  if (document.querySelector('link[data-actions-list]')) return;
  const link = document.createElement('link'); link.rel = 'stylesheet'; link.dataset.actionsList = '';
  link.href = new URL('./actions-list.css', import.meta.url).href; document.head.append(link);
}

/** Loads `path` (an `{actions}` response) into `box` and keeps it reloadable. */
export async function mountActions(box, request, path, {resolvePath, showSession = false} = {}) {
  ensureStyles();
  const status = el('p', 'actions-empty', 'Loading…'); status.setAttribute('role', 'status'); box.replaceChildren(status);
  const load = async () => {
    try {
      const data = await request(path);
      renderActions(box, data.actions || [], {showSession, changed: load,
        resolve: resolvePath ? (id, state) => request(resolvePath(id), 'POST', {state}) : null});
      return data.actions || [];
    } catch (error) { status.textContent = 'Actions unavailable: ' + error.message; box.replaceChildren(status); return []; }
  };
  return load();
}
