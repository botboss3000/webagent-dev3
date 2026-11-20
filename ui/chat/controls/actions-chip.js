// Actions chip: a header session-tab chip for the action ledger's records of this chat
// (protected or critical tool calls such as publishing). It appears once the chat has any,
// counts uncertain ones, and opens a panel where the chat's owner marks an uncertain action
// done or failed after checking its real outcome. Others see the list read-only.
import {requestTouchVibration} from '../../shared/vibrate/vibrate.js';
import {renderActions} from '../../shared/actions/actions-list.js';

const el = (tag, cls, text) => { const n = document.createElement(tag); if (cls) n.className = cls; if (text != null) n.textContent = text; return n; };

/** Mounts the chip into the header session-tabs row and returns its session lifecycle. */
export function createActionsChip({session = () => '', request, canResolve = () => true} = {}) {
  const tabs = document.getElementById('chat-sub-header');
  const chip = el('button', 'sub-chip chip-actions'); chip.type = 'button'; chip.id = 'chat-actions-chip';
  chip.setAttribute('aria-haspopup', 'dialog'); chip.setAttribute('aria-expanded', 'false');
  let actions = [], popup = null, loading = 0;
  const path = () => '/sessions/' + encodeURIComponent(session()) + '/actions';

  function close() { popup?.remove(); popup = null; chip.setAttribute('aria-expanded', 'false'); }
  document.addEventListener('pointerdown', e => { if (popup && !popup.contains(e.target) && !chip.contains(e.target)) close(); }, true);
  function open() {
    const box = el('div', 'sub-chip-menu actions-panel wa-scroll-gutter'); box.setAttribute('role', 'dialog'); box.setAttribute('aria-label', 'Session actions');
    renderActions(box, actions, {changed: refresh,
      resolve: canResolve() ? (id, state) => request(path() + '/' + encodeURIComponent(id) + '/resolve', 'POST', {state}) : null});
    popup?.remove(); popup = box; document.body.append(box); chip.setAttribute('aria-expanded', 'true');
    const r = chip.getBoundingClientRect();
    box.style.top = (r.bottom + 4) + 'px';
    if (!matchMedia('(max-width: 600px)').matches) box.style.left = Math.max(4, Math.min(r.left, innerWidth - box.offsetWidth - 8)) + 'px';
  }
  function render() {
    if (!actions.length) { chip.remove(); close(); return; }
    const wrap = tabs?.querySelector('#chat-sub-scroll-wrap');
    if (wrap && chip.parentNode !== wrap) wrap.append(chip);
    const uncertain = actions.filter(a => a.state === 'uncertain').length;
    const glyph = el('span', 'chip-status'); glyph.innerHTML = '<i data-lucide="shield-check"></i>';
    chip.dataset.uncertain = String(uncertain > 0);
    chip.replaceChildren(glyph, el('span', '', 'Actions'), el('span', 'actions-chip-meta', uncertain ? uncertain + ' uncertain' : String(actions.length)));
    chip.title = 'Recorded actions: ' + actions.length + (uncertain ? ', ' + uncertain + ' uncertain' : '');
    chip.setAttribute('aria-label', chip.title);
    window.lucide?.createIcons();
  }
  async function refresh() {
    const id = session(), ticket = ++loading;
    if (!id) return;
    try {
      const data = await request(path());
      if (ticket !== loading || id !== session()) return;
      actions = data.actions || []; render(); if (popup) open();
    } catch { /* The chip stays as it was; the panel retries on the next refresh. */ }
  }
  chip.addEventListener('click', e => { e.stopPropagation(); requestTouchVibration(e); if (popup) close(); else open(); });
  return {
    /** Loads the open chat's actions. */
    open() { actions = []; render(); void refresh(); },
    /** Reloads after the agent replies: a finished call may have recorded or settled one. */
    refresh,
    disconnect() { loading++; actions = []; close(); render(); }
  };
}
