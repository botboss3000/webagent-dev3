// Actions list: the action ledger's recorded tool calls, as expand-list rows, (protected or critical tools such as
// publishing or pushing) with their state. An uncertain action may or may not have taken
// effect; whoever may settle it checks the real outcome and marks it done or failed. Shared
// by the chat's Actions chip, the agent's Artifacts and Members pages and App → Actions.
// Text-only DOM: tool arguments are untrusted.
import {requestTouchVibration} from '../vibrate/vibrate.js';
import {createExpandList} from '../../components/expand-list/expand-list.js';

const el = (tag, cls, text) => { const n = document.createElement(tag); if (cls) n.className = cls; if (text != null) n.textContent = text; return n; };
export const STATE = {prepared: 'Prepared', running: 'Running', completed: 'Done', confirmed_failed: 'Failed', uncertain: 'Uncertain'};
const when = seconds => seconds ? new Date(seconds * 1000).toLocaleString([], {dateStyle: 'short', timeStyle: 'short'}) : '';

/** Uncertain actions first, then newest first. */
export function sortActions(actions) {
  return [...actions].sort((a, b) => (a.state === 'uncertain' ? 0 : 1) - (b.state === 'uncertain' ? 0 : 1) || (b.updated_at || b.created_at || 0) - (a.updated_at || a.created_at || 0));
}

const STATUS = {prepared: 'pending', running: 'in-progress', completed: 'completed', confirmed_failed: 'failed', uncertain: 'need-help'};

/**
 * Fills `box` with an expand-list row per action; uncertain ones start open. `resolve(id,
 * state)` settles an uncertain action (omit it for a read-only list); `changed()` runs after a
 * resolution so the caller reloads.
 */
export function renderActions(box, actions, {resolve, changed = () => {}, showSession = false, showUser = false, empty = 'No recorded actions yet.'} = {}) {
  box.replaceChildren();
  if (!actions.length) { box.append(el('p', 'actions-empty', empty)); return; }
  const list = createExpandList({label: 'Recorded actions', defaultOpen: row => row.status === 'need-help'});
  list.update(sortActions(actions).map(action => ({
    id: action.id || String(action.created_at), title: action.tool || 'tool', status: STATUS[action.state] || 'pending',
    icon: action.critical ? 'ShieldAlert' : null, pill: STATE[action.state] || action.state || false,
    badges: [action.critical ? 'Critical' : '', when(action.updated_at || action.created_at)].filter(Boolean),
    description: [showUser && action.requester ? 'User ' + action.requester : '', action.attempt ? 'Attempt ' + action.attempt : '', showSession && action.session_id ? 'Chat ' + String(action.session_id).slice(0, 8) : '', action.resolved_by ? 'Marked by ' + action.resolved_by : ''].filter(Boolean).join(' · '),
    content: host => {
      if (action.arguments && typeof action.arguments === 'object') host.append(el('small', 'actions-label', 'Arguments'), el('pre', 'actions-args-body', JSON.stringify(action.arguments, null, 2).slice(0, 4000)));
      if (action.state !== 'uncertain') return;
      host.append(el('p', 'actions-hint', 'This may or may not have taken effect. Check the real outcome, then mark it.'));
      if (!resolve) return;
      const buttons = el('div', 'actions-buttons');
      for (const [state, label] of [['completed', 'Mark done'], ['confirmed_failed', 'Mark failed']]) {
        const button = el('button', 'actions-btn', label); button.type = 'button';
        button.onclick = async event => {
          requestTouchVibration(event);
          const text = state === 'completed' ? 'Mark this action done? The agent will treat it as finished and not repeat it.' : 'Mark this action failed? The agent may then try it again.';
          if (!window.confirm(text)) return;
          for (const b of buttons.querySelectorAll('button')) b.disabled = true;
          try { await resolve(action.id, state, action); changed(); }
          catch (error) { host.append(el('p', 'actions-error', error.message)); for (const b of buttons.querySelectorAll('button')) b.disabled = false; }
        };
        buttons.append(button);
      }
      host.append(buttons);
    }
  })));
  box.append(list.element);
}

/** Adds the list's stylesheet once, for pages that do not load the chat styles. */
function ensureStyles() {
  if (document.querySelector('link[data-actions-list]')) return;
  const link = document.createElement('link'); link.rel = 'stylesheet'; link.dataset.actionsList = '';
  link.href = new URL('./actions-list.css', import.meta.url).href; document.head.append(link);
}

/** Loads `path` (an `{actions}` response) into `box` and keeps it reloadable. */
export async function mountActions(box, request, path, {resolvePath, showSession = false, showUser = false, empty} = {}) {
  ensureStyles();
  const status = el('p', 'actions-empty', 'Loading…'); status.setAttribute('role', 'status'); box.replaceChildren(status);
  const load = async () => {
    try {
      const data = await request(path);
      renderActions(box, data.actions || [], {showSession, showUser, empty, changed: load,
        resolve: resolvePath ? (id, state, action) => request(resolvePath(id, action), 'POST', {state}) : null});
      return data.actions || [];
    } catch (error) { status.textContent = 'Actions unavailable: ' + error.message; box.replaceChildren(status); return []; }
  };
  return load();
}
