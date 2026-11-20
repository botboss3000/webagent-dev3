// Artifacts chip: a header session-tab chip for what the agent produced in this chat (the
// agreed plan first). Artifacts are kept with the session (its session_artifacts rows) and
// arrive with the session snapshot; the chip lists them as expand-list rows, and opening a
// row builds its read-only card under it. A UI element card (kind ui_element) also runs the
// agent's saved element in a sandboxed frame (artifact-element.js), only once its row opens.
// The user's own uploads are not artifacts.
import {requestTouchVibration} from '../../shared/vibrate/vibrate.js';
import {createExpandList} from '../../components/expand-list/expand-list.js';
import {elementFrame} from './artifact-element.js';

const KINDS = {plan: 'Plan', ui_element: 'Element'};
const ICONS = {plan: 'ClipboardList', ui_element: 'AppWindow'};
const el = (tag, cls, text) => { const n = document.createElement(tag); if (cls) n.className = cls; if (text != null) n.textContent = text; return n; };

/** The session's artifacts that can be shown, newest first. */
export function artifactRows(items) {
  return (Array.isArray(items) ? items : [])
    .filter(row => typeof row?.id === 'string' && row.id)
    .sort((a, b) => (b.updated_at || 0) - (a.updated_at || 0));
}

/**
 * One artifact as a card: its title, then each section as a label over its text; a UI
 * element card then shows the element (`frame(row)` builds it). `inline` renders it inside a
 * list row, without the title the row already shows.
 */
export function artifactCard(row, frame = null, {inline = false} = {}) {
  const box = el('div', inline ? 'artifact-card artifact-card-inline' : 'sub-chip-menu artifact-card');
  if (!inline) box.setAttribute('role', 'dialog');
  box.setAttribute('aria-label', 'Artifact ' + (row.title || row.id));
  if (!inline) box.append(el('strong', 'artifact-card-title', row.title || 'Artifact'));
  for (const section of (Array.isArray(row.sections) ? row.sections : []).slice(0, 20)) {
    if (typeof section?.value !== 'string' || !section.value) continue;
    const part = el('section', 'artifact-section');
    part.append(el('div', 'artifact-label', String(section.label || '')), el('p', 'artifact-text', section.value));
    box.append(part);
  }
  if (row.kind === 'ui_element' && frame) { box.classList.add('artifact-card-element'); box.append(frame(row)); }
  return box;
}

/**
 * Mounts the chip into the header session-tabs row and returns its session lifecycle.
 * `session()` names the open chat and `send(text)` sends the user's message, for UI
 * element cards.
 */
export function createArtifactsChip({session = () => '', send = async () => {}} = {}) {
  const tabs = document.getElementById('chat-sub-header');
  const chip = el('button', 'sub-chip chip-artifacts'); chip.type = 'button'; chip.id = 'chat-artifacts-chip';
  chip.setAttribute('aria-haspopup', 'dialog'); chip.setAttribute('aria-expanded', 'false');
  let rows = [], popup = null;

  function close() { popup?.remove(); popup = null; chip.setAttribute('aria-expanded', 'false'); }
  /** Places a body-mounted popup under the chip; phone widths get a full-width panel (CSS). */
  function show(node) {
    close(); popup = node; document.body.append(node); chip.setAttribute('aria-expanded', 'true');
    const r = chip.getBoundingClientRect();
    node.style.top = (r.bottom + 4) + 'px';
    if (!matchMedia('(max-width: 600px)').matches) node.style.left = Math.max(4, Math.min(r.left, innerWidth - node.offsetWidth - 8)) + 'px';
    const outside = e => { if (!popup || popup.contains(e.target) || chip.contains(e.target)) return; document.removeEventListener('pointerdown', outside, true); close(); };
    document.addEventListener('pointerdown', outside, true);
  }
  function openList() {
    const box = el('div', 'sub-chip-menu family-spawn-menu artifact-list wa-scroll-gutter'); box.setAttribute('aria-label', 'Artifacts');
    show(box);
    const list = createExpandList({label: 'Artifacts', defaultOpen: () => rows.length === 1,
      onExpand: (item, open, slot) => {
        if (!open) { delete slot.dataset.loaded; slot.replaceChildren(); return; }
        slot.replaceChildren(artifactCard(item.row, element => elementFrame(element, session(), text => { close(); return send(text); }), {inline: true}));
      }});
    list.update(rows.map(row => ({id: row.id, title: row.title || row.id, row, lazy: true, status: 'pending', icon: ICONS[row.kind] || 'FileText',
      pill: KINDS[row.kind] || row.kind || false,
      badges: row.updated_at ? [new Date(row.updated_at * 1000).toLocaleString([], {dateStyle: 'short', timeStyle: 'short'})] : []})));
    box.append(list.element);
  }
  function render() {
    tabs?.toggleAttribute('data-artifacts', rows.length > 0);
    if (!rows.length) { chip.remove(); close(); return; }
    const wrap = tabs?.querySelector('#chat-sub-scroll-wrap');
    if (wrap && chip.parentNode !== wrap) wrap.append(chip);
    const glyph = el('span', 'chip-status'); glyph.innerHTML = '<i data-lucide="layers"></i>';
    chip.replaceChildren(glyph, el('span', '', 'Artifacts'), el('span', 'artifacts-chip-meta', String(rows.length)));
    chip.title = rows.length === 1 ? 'Artifact: ' + (rows[0].title || 'Artifact') : rows.length + ' artifacts';
    chip.setAttribute('aria-label', chip.title);
    window.lucide?.createIcons();
  }
  chip.addEventListener('click', e => {
    e.stopPropagation(); requestTouchVibration(e);
    if (popup) return close();
    openList();
  });
  return {
    /** Takes the session snapshot's artifact list. */
    set(items) {
      const next = artifactRows(items);
      const changed = JSON.stringify(next.map(row => [row.id, row.revision])) !== JSON.stringify(rows.map(row => [row.id, row.revision]));
      rows = next; render();
      if (changed && popup) close();
    },
    disconnect() { rows = []; close(); render(); }
  };
}
