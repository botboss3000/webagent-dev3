// Storage panel, opened from the Sessions footer's storage meter: usage against the account
// limit, bulk cleanup (preview, then a second click to delete permanently) and the saved
// automatic cleanup that runs whenever storage reaches its limit. The server owns the rules
// (pinned sessions and main chats are never touched); this only collects the choices.
const MIB = 1024 * 1024;
const ACTIONS = [
  {kind: 'archived_sessions', label: 'Delete archived sessions', days: 0, min: 0,
    help: 'Archived for at least this many days (0 = every archived session).'},
  {kind: 'inactive_sessions', label: 'Delete sessions not used recently', days: 90, min: 1,
    help: 'Unused for at least this many days. Pinned sessions and main chats are kept.'},
  {kind: 'session_files', label: 'Remove files only', days: 30, min: 1,
    help: 'Files attached in sessions unused this many days. The sessions and messages stay.'},
  {kind: 'workspace_folders', label: 'Clear browser and Sandbox folders', days: 14, min: 1,
    help: 'Folders of sessions unused this many days. Their saved sign-ins and work files go.'},
];

function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}
export function formatMB(bytes) {
  const mb = Number(bytes) / MIB;
  return (mb >= 100 ? Math.round(mb) : Math.round(mb * 10) / 10) + ' MB';
}
/** Whole-number percent of the limit; over 100 when usage passed it. */
export function usedPercent(status) {
  if (!status || !Number.isSafeInteger(status.used_bytes) || !Number.isSafeInteger(status.limit_bytes) || status.limit_bytes <= 0) return null;
  return Math.floor(status.used_bytes * 100 / status.limit_bytes);
}
function relativeDays(seconds) {
  const days = Math.floor((Date.now() / 1000 - Number(seconds)) / 86400);
  return days < 1 ? 'today' : days === 1 ? '1 day ago' : days + ' days ago';
}

export function openStoragePanel({request, onChanged = () => {}, documentObject = document}) {
  documentObject.querySelector('.storage-panel-backdrop')?.remove();
  const backdrop = el('div', 'storage-panel-backdrop');
  const panel = el('div', 'storage-panel');
  panel.setAttribute('role', 'dialog');
  panel.setAttribute('aria-modal', 'true');
  panel.setAttribute('aria-label', 'Storage');
  backdrop.append(panel);
  const close = () => { backdrop.remove(); documentObject.removeEventListener('keydown', onKey); };
  const onKey = event => { if (event.key === 'Escape') close(); };
  documentObject.addEventListener('keydown', onKey);
  backdrop.addEventListener('click', event => { if (event.target === backdrop) close(); });

  const head = el('div', 'storage-panel-head');
  const closeButton = el('button', 'storage-panel-close', '×');
  closeButton.type = 'button';
  closeButton.setAttribute('aria-label', 'Close');
  closeButton.onclick = close;
  head.append(el('h2', '', 'Storage'), closeButton);
  const usage = el('div', 'storage-panel-usage');
  const usageText = el('p', 'storage-panel-usage-text', 'Loading…');
  const bar = el('div', 'storage-panel-bar');
  const fill = el('div', 'storage-panel-fill');
  bar.append(fill);
  usage.append(usageText, bar);

  const rows = el('div', 'ac-list storage-panel-actions');
  const controls = new Map();
  for (const action of ACTIONS) {
    const row = el('label', 'ac-ability-row storage-panel-row');
    const check = el('input');
    check.type = 'checkbox';
    const days = el('input', 'storage-panel-days');
    days.type = 'number';
    days.min = String(action.min);
    days.max = '3650';
    days.value = String(action.days);
    days.setAttribute('aria-label', action.label + ': days');
    const daysWrap = el('span', 'storage-panel-days-wrap');
    daysWrap.append(days, el('span', '', 'days'));
    const text = el('span', 'storage-panel-row-text');
    text.append(el('strong', '', action.label), el('span', 'storage-panel-help', action.help));
    row.append(check, text, daysWrap);
    rows.append(row);
    controls.set(action.kind, {check, days});
    check.addEventListener('change', () => resetConfirm());
    days.addEventListener('input', () => resetConfirm());
  }

  const largestHead = el('h3', '', 'Largest sessions');
  const largest = el('div', 'ac-list storage-panel-largest');
  largest.append(el('p', 'storage-panel-help', 'Loading…'));
  const picked = new Set();

  const preview = el('div', 'storage-panel-preview');
  preview.setAttribute('aria-live', 'polite');
  const buttons = el('div', 'storage-panel-buttons');
  const previewButton = el('button', 'storage-panel-button', 'Preview');
  const deleteButton = el('button', 'storage-panel-button storage-panel-danger', 'Delete permanently');
  const saveButton = el('button', 'storage-panel-button', 'Save as auto-cleanup');
  for (const button of [previewButton, deleteButton, saveButton]) button.type = 'button';
  buttons.append(previewButton, deleteButton, saveButton);
  const auto = el('div', 'storage-panel-auto');

  panel.append(head, usage, el('h3', '', 'Clean up'), rows, largestHead, largest, preview, buttons, auto);
  documentObject.body.append(backdrop);
  closeButton.focus();

  function selected({saved = false} = {}) {
    const actions = [];
    for (const action of ACTIONS) {
      const {check, days} = controls.get(action.kind);
      if (!check.checked) continue;
      const value = Math.max(action.min, Math.min(3650, Math.floor(Number(days.value) || 0)));
      actions.push(value > 0 ? {kind: action.kind, days: value} : {kind: action.kind});
    }
    if (!saved && picked.size) actions.push({kind: 'sessions', ids: [...picked]});
    return actions;
  }
  let armed = null;
  function resetConfirm() {
    clearTimeout(armed);
    armed = null;
    deleteButton.textContent = 'Delete permanently';
    deleteButton.classList.remove('armed');
  }
  function showError(error) {
    preview.replaceChildren(el('p', 'storage-panel-error', error.message || String(error)));
  }
  function describe(result) {
    const p = result.preview;
    const parts = [];
    if (p.delete) parts.push(p.delete + ' sessions deleted');
    if (p.files) parts.push('files removed from ' + p.files + ' sessions');
    if (p.folders) parts.push(p.folders + ' folders cleared');
    return parts.length ? parts.join(', ') + ' — about ' + formatMB(p.estimated_bytes) : 'Nothing matches these choices.';
  }

  async function loadStatus() {
    try {
      const status = await request('/storage');
      const percent = usedPercent(status);
      if (percent === null) {
        usageText.textContent = formatMB(status?.used_bytes || 0) + ' used · no limit';
        fill.style.width = '0%';
      } else {
        usageText.textContent = formatMB(status.used_bytes) + ' of ' + formatMB(status.limit_bytes) + ' used (' + percent + '%)'
          + (status.full ? ' — full: new runs wait until you free space' : '');
        fill.style.width = Math.min(100, percent) + '%';
        panel.dataset.storageState = status.full || percent >= 100 ? 'full' : percent >= (status.warning_percent || 80) ? 'warn' : 'ok';
      }
      const policy = status?.auto_cleanup;
      auto.replaceChildren();
      if (Array.isArray(policy) && policy.length) {
        const names = policy.map(item => {
          const known = ACTIONS.find(action => action.kind === item.kind);
          return (known ? known.label : item.kind) + (item.days ? ' (' + item.days + ' days)' : '');
        });
        const off = el('button', 'storage-panel-link', 'Turn off');
        off.type = 'button';
        off.onclick = async () => {
          try { await request('/storage/cleanup-policy', 'PUT', {actions: null}); await loadStatus(); }
          catch (error) { showError(error); }
        };
        auto.append(el('span', '', 'Auto-cleanup is on. When storage reaches its limit it runs: ' + names.join('; ') + '. '), off);
      } else {
        auto.append(el('span', '', 'Auto-cleanup is off. Choose actions above and save them to run them whenever storage reaches its limit.'));
      }
    } catch (error) {
      usageText.textContent = 'Storage usage is unavailable';
    }
  }

  async function loadLargest() {
    try {
      const data = await request('/storage/sessions');
      largest.replaceChildren();
      if (!data.sessions?.length) { largest.append(el('p', 'storage-panel-help', 'No sessions to clean up.')); return; }
      for (const session of data.sessions) {
        const row = el('label', 'ac-ability-row storage-panel-row');
        const check = el('input');
        check.type = 'checkbox';
        check.checked = picked.has(session.id);
        check.addEventListener('change', () => {
          if (check.checked) picked.add(session.id); else picked.delete(session.id);
          resetConfirm();
        });
        const text = el('span', 'storage-panel-row-text');
        text.append(el('strong', '', session.title || 'Untitled session'),
          el('span', 'storage-panel-help', formatMB(session.bytes) + ' · ' + (session.status === 'archived' ? 'archived · ' : '') + 'used ' + relativeDays(session.last_active_at)));
        row.append(check, text);
        largest.append(row);
      }
    } catch (error) {
      largest.replaceChildren(el('p', 'storage-panel-error', 'Session sizes are unavailable'));
    }
  }

  previewButton.onclick = async () => {
    resetConfirm();
    const actions = selected();
    if (!actions.length) { preview.replaceChildren(el('p', 'storage-panel-help', 'Choose at least one action or session.')); return; }
    try {
      const result = await request('/storage/cleanup', 'POST', {actions, dry_run: true});
      const list = el('ul', 'storage-panel-list');
      for (const session of result.preview.sessions.slice(0, 10)) list.append(el('li', '', (session.title || 'Untitled session') + ' · ' + formatMB(session.bytes)));
      preview.replaceChildren(el('p', '', 'Would do: ' + describe(result) + '.'), list);
    } catch (error) { showError(error); }
  };
  deleteButton.onclick = async () => {
    const actions = selected();
    if (!actions.length) { preview.replaceChildren(el('p', 'storage-panel-help', 'Choose at least one action or session.')); return; }
    if (!armed) {
      deleteButton.textContent = 'Click again to delete — this cannot be undone';
      deleteButton.classList.add('armed');
      armed = setTimeout(resetConfirm, 6000);
      return;
    }
    resetConfirm();
    deleteButton.disabled = true;
    try {
      const result = await request('/storage/cleanup', 'POST', {actions});
      picked.clear();
      const failed = result.report.failed?.length ? ' ' + result.report.failed.length + ' could not be removed.' : '';
      preview.replaceChildren(el('p', '', 'Done: ' + result.summary + '. Now ' + formatMB(result.used_bytes_after) + ' used.' + failed));
      onChanged();
      await Promise.all([loadStatus(), loadLargest()]);
    } catch (error) { showError(error); }
    finally { deleteButton.disabled = false; }
  };
  saveButton.onclick = async () => {
    const actions = selected({saved: true});
    if (!actions.length) { preview.replaceChildren(el('p', 'storage-panel-help', 'Choose at least one action above to save (hand-picked sessions are not saved).')); return; }
    try {
      await request('/storage/cleanup-policy', 'PUT', {actions});
      preview.replaceChildren(el('p', '', 'Saved. These actions run whenever storage reaches its limit, and you get a notification listing what was removed.'));
      await loadStatus();
    } catch (error) { showError(error); }
  };

  void loadStatus();
  void loadLargest();
  return {close};
}
