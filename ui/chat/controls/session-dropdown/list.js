// V2 dropdown presentation/lifecycle, with independent Rust data authority.
// @code-index {"version":1,"feature":"chat-session-selector","aliases":["session dropdown","conversation selector","session list"],"purpose":"Session dropdown presentation and interactions. Config: ui/chat/controls/session-dropdown/session-dropdown.json -> common.trigger, common.menu, common.footer, common.gestures with desktop/mobile/widget overlays; adopted on page reload.","span_lines":40,"configuration":{"status":"configured","references":[{"path":"ui/chat/controls/session-dropdown/session-dropdown.json","pointers":["/common/trigger","/common/menu","/common/footer","/common/gestures","/desktop","/mobile","/widget"]}],"opportunities":[]}}
/** Renders active/recycled sessions, private search results, and revision-fenced management actions. Controller callbacks own navigation; client writes retain retry receipts, and teardown fences pending work. */
export function createSessionList(root, {guard, refresh, select, client, recycled = () => {}, screenSettings = () => null}) {
  const protectedSession=item=>item?.main_chat===true||item?.can_delete===false;
  let items = [], selected = '', config = {}, query = '', status = 'active';
  let hits = [], continuation = null, searchInfo = '', searching = false, epoch = 0, timer = 0, destroyed = false;
  let editing = null, checkboxMode = false, batchBusy = false;
  let hold = null, suppressClickUntil = 0;
  const checked = new Set();
  const menu = root.querySelector('.session-dropdown-menu');
  const abort = new AbortController(), timers = new Set(), receipts = new Map();
  // Lucide markup shared by the trigger-compatible row controls.
  const icon = name => `<i data-lucide="${name}" style="width:14px;height:14px"></i>`;
  // Builds an accessible icon button; optional actions stop row bubbling and use the caller guard.
  const button = (title, glyph, fn, cls = '') => {
    const b = document.createElement('button'); b.type = 'button'; b.className = cls;
    b.title = title; b.setAttribute('aria-label', title); b.innerHTML = icon(glyph);
    if (fn) b.onclick = e => { e.stopPropagation(); guard(fn)(e); };
    return b;
  };
  /** Tracks delayed UI work so teardown can cancel every pending callback. */
  function delay(fn, ms) { const t = setTimeout(() => { timers.delete(t); if (!destroyed) fn(); }, ms); timers.add(t); return t; }
  /** Formats activity timestamps for row labels without changing stored values. */
  function elapsed(ts) {
    if (!ts) return '';
    const seconds = Math.max(0, Math.floor(Date.now() / 1000 - ts));
    for (const [unit, size] of [['s',60], ['m',3600], ['h',86400], ['d',604800], ['w',2419200]]) {
      if (seconds < size) return Math.floor(seconds / ({s:1,m:60,h:3600,d:86400,w:604800}[unit])) + unit;
    }
    return new Date(ts * 1000).toLocaleDateString(undefined, {month:'short', day:'numeric'});
  }
  /** Builds the unread/activity status chip used by the trigger and list rows. */
  function statusNode(item) {
    const node = document.createElement('span');
    node.className = 'session-row-status ' + (item.is_unread ? 'session-status-unread' : 'session-status-idle');
    node.dataset.ts = item.last_active_at || '';
    node.textContent = elapsed(item.last_active_at);
    node.title = (item.is_unread ? `${item.unread_count} unread assistant messages · ` : '') +
      (item.last_active_at ? 'Last active ' + new Date(item.last_active_at * 1000).toLocaleString() : 'Activity unknown');
    return node;
  }
  /** Refreshes trigger label/status from the currently selected session. */
  function label() {
    const item = items.find(s => s.id === selected);
    const label = root.querySelector('#session-dropdown-label');
    if (label) label.textContent = item?.title || 'New Session';
    root.dataset.loaded = 'true';
    for (const id of ['icon','label','status','kebab','delete']) {
      const node = root.querySelector('#session-dropdown-' + id);
      node.hidden = config.trigger?.[{icon:'agent_icon',label:'session_name',kebab:'row_actions'}[id] || id] === false || (id === 'kebab' && config.trigger?.row_actions !== true) || (id === 'delete' && (config.trigger?.delete !== true || protectedSession(item)));
      node.style.display = node.hidden ? 'none' : '';
    }
    root.querySelector('#session-dropdown-icon').classList.toggle('session-row-agent-warning', !!item?.pinned && item?.main_chat !== true);
    root.querySelector('#session-dropdown-icon').classList.toggle('session-row-agent-main', item?.main_chat === true);
    const statusEl = root.querySelector('#session-dropdown-status');
    statusEl.replaceChildren(); if (item) statusEl.append(statusNode(item));
    root.querySelector('#session-dropdown-delete').disabled = !item || item.status === 'recycled' || protectedSession(item);
  }
  /** Sends revision-fenced client actions with stable retry IDs for uncertain responses. */
  async function mutate(item, action, changes = {}, deferred = false) {
    if (['recycle','purge'].includes(action)&&protectedSession(item)) throw Error('Main Chat cannot be deleted');
    const body = {expected_revision:item.metadata_revision, ...changes};
    const receiptKey = JSON.stringify([item.id, action, body]);
    if (!receipts.has(receiptKey)) receipts.set(receiptKey, crypto.randomUUID());
    body.client_request_id = receipts.get(receiptKey);
    try {
      const result = await client[action](item.id, body);
      receipts.delete(receiptKey);
      if (destroyed) return;
      if (action === 'recycle' && selected === item.id) recycled(item.id);
      if (result.session) items = items.map(s => s.id === item.id ? result.session : s);
      if (!deferred && !destroyed) {
        root._sessionDropdownController.setPopup(null);
        await refresh(); await loadScope();
      }
    } catch (error) {
      // Keep the exact receipt/body available for an uncertain-response retry.
      if (!deferred && !destroyed) { searchInfo = error.message || 'Session update failed'; render(); }
      throw error;
    }
  }
  // V2 checkbox management, backed by individually revision-fenced Rust writes.
  // A batch is not atomic: keep failed selections and their retry receipts.
  /** Recycles selected sessions sequentially; failed rows stay selected for retry. */
  async function recycleChecked() {
    if (batchBusy || destroyed) return;
    const targets = items.filter(item => checked.has(item.id)&&!protectedSession(item));
    if (!targets.length) return;
    batchBusy = true; render();
    const failures = []; let completed = 0;
    try {
      for (const item of targets) {
        if (destroyed) break;
        try { await mutate(item, 'recycle', {}, true); checked.delete(item.id); completed++; }
        catch (error) { failures.push({title:item.title || 'New Session', message:error.message || 'Recycle failed'}); }
      }
      if (!destroyed) {
        try { await refresh(); await loadScope(); }
        catch (error) { failures.push({title:'Refresh',message:error.message || 'List refresh failed'}); }
        searchInfo = `Recycled ${completed} of ${targets.length} selected sessions.` +
          (failures.length ? ' Not completed: ' + failures.map(f => `${f.title}: ${f.message}`).join('; ') : '');
      }
    } finally { batchBusy = false; if (!destroyed) render(); }
  }
  /** Updates one checkbox selection unless a batch operation is active. */
  function toggleChecked(id) {
    if (batchBusy||protectedSession(items.find(item=>item.id===id))) return;
    if (checked.has(id)) checked.delete(id); else checked.add(id);
    render();
  }
  // Capture row activation before the controller's normal session-opening path.
  for (const type of ['click','keydown']) menu.addEventListener(type, e => {
    if (!checkboxMode || status !== 'active' || e.target.closest('button,input')) return;
    const row = e.target.closest('.session-row');
    if (!row || (type === 'keydown' && !['Enter',' '].includes(e.key))) return;
    e.preventDefault(); e.stopImmediatePropagation(); toggleChecked(row.dataset.id);
    menu.querySelector(`[data-id="${CSS.escape(row.dataset.id)}"]`)?.focus();
  }, {capture:true,signal:abort.signal});
  /** Adds a timed two-step confirmation and guards asynchronous actions. */
  function confirmButton(b, text, fn) {
    let armed = false, busy = false, timeout = 0;
    const old = b.innerHTML, label = b.getAttribute('aria-label');
    // Purpose: Restore the confirmation button label and accessible name after timeout or completion.
    const reset = () => {
      armed = false; b.innerHTML = old;
      if (label == null) b.removeAttribute('aria-label'); else b.setAttribute('aria-label', label);
    };
    b.onclick = e => {
      e.stopPropagation(); if (busy || destroyed) return;
      if (!armed) {
        armed = true; b.textContent = text; b.setAttribute('aria-label', text);
        timeout = delay(reset, 3000); return;
      }
      clearTimeout(timeout); timers.delete(timeout);
      busy = true; b.disabled = true;
      guard(async () => { try { await fn(); } finally { busy = false; b.disabled = false; reset(); } })(e);
    };
  }
  /** Replaces a row title with an editor; Enter/blur commit and Escape cancels. */
  function rename(item, element) {
    if (!element || editing) return;
    const input = document.createElement('input'); input.className = 'session-row-title-input'; input.value = item.title;
    editing = input; element.replaceWith(input); input.focus(); input.select(); let done = false;
    // Purpose: Restore the row title editor and persist a changed non-empty title only when commit is true.
    const finish = commit => {
      if (done) return; done = true; editing = null; input.replaceWith(element);
      const title = input.value.trim(); label(); render();
      if (commit && title && title !== item.title) guard(() => mutate(item, 'patch', {title}))();
    };
    input.addEventListener('keydown', e => { e.stopPropagation(); if (e.key === 'Enter') { e.preventDefault(); finish(true); } if (e.key === 'Escape') { e.preventDefault(); finish(false); } });
    input.addEventListener('blur', () => finish(true));
  }
  /** Builds the row action popup and optional device display preferences; session mutations stay client-owned and display edits stay browser-local. Unsupported V3 actions remain visibly disabled. */
  function actions(item, anchor) {
    const popup = document.createElement('div'); popup.className = 'session-row-actions'; popup.dataset.source = 'session-list'; popup.dataset.id = item.id;
    const recycledActions = [
      ['restore','Restore session','archive-restore',() => mutate(item,'restore')],
      ['purge','Permanently delete session','trash-2',() => mutate(item,'purge')]
    ];
    for (const [key, title, glyph, fn] of (item.status === 'recycled' ? recycledActions : [
      ['pinning',item.pinned ? 'Unpin session' : 'Pin session',item.pinned ? 'pin-off' : 'pin',() => mutate(item,'patch',{pinned:!item.pinned})],
      ['auto_rename','Auto rename','wand-2',null],
      ['rename','Rename session','pencil',() => { root._sessionDropdownController.setPopup(null); rename(item, anchor.closest('.session-row')?.querySelector('.session-row-title') || root.querySelector('#session-dropdown-label')); }],
      ['open_in_widget','Open in widget','messages-square',null],
      ['refresh','Refresh session','refresh-cw',async () => { await refresh(); await loadScope(); }],
      ['start_scratchpad','Start a scratchpad','notepad-text',null],
      ['delete','Recycle session','trash-2',() => mutate(item,'recycle')]
    ])) {
      if (config.menu?.[key] === false || (['delete','purge'].includes(key)&&protectedSession(item))) continue;
      const b = button(title,glyph,fn,'session-row-action' + (key === 'delete' ? ' danger' : ''));
      b.append(document.createTextNode(' ' + title)); b.disabled = !fn;
      if (!fn) b.title = title + ' — unavailable in v3';
      if (['delete','restore','purge'].includes(key)) confirmButton(b, key === 'delete' ? 'Recycle?' : key === 'restore' ? 'Restore?' : 'Delete permanently?', fn);
      popup.append(b);
    }
    for (const [key,title,value] of [['agent_id','Agent',item.agent_id],['session_id','Session',item.id]]) {
      if (config.menu?.[key] === false) continue;
      const b = button(title,'copy',() => navigator.clipboard.writeText(value),'session-row-action session-row-id');
      b.append(document.createTextNode(` ${title}: ${value}`)); popup.append(b);
    }
    // Device display settings share the popup lifecycle; they never write session/server configuration.
    const displaySettings = config.menu?.always_on_display === false ? null : screenSettings(); if (displaySettings) popup.append(displaySettings);
    if (!popup.childElementCount) { root._sessionDropdownController.setPopup(null); return; }
    popup.style.maxHeight = 'min(80vh, 640px)'; popup.style.overflowY = 'auto';
    // Explicit intrinsic width prevents fixed-position shrink-to-fit width changing after placement.
    popup.style.width = 'max-content'; popup.style.maxWidth = 'calc(100vw - 8px)'; popup.style.boxSizing = 'border-box';
    popup.setAttribute('role','group'); popup.setAttribute('aria-label','Session actions');
    root._sessionDropdownController.setPopup(popup); document.body.append(popup);
    popup.addEventListener('keydown', e => { if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); root._sessionDropdownController.setPopup(null); anchor.focus?.(); } });
    // Expanding device preferences changes height; keep the complete scrollable popup inside the viewport.
    /** Reanchor the complete action popup after display preferences expand. */
    function positionPopup() {
      const r = anchor.getBoundingClientRect(); popup.style.position = 'fixed';
      const bounds = popup.getBoundingClientRect();
      popup.style.left = Math.max(4,Math.min(r.right-bounds.width,innerWidth-bounds.width-4)) + 'px';
      popup.style.top = Math.max(4,Math.min(r.bottom+4,innerHeight-bounds.height-4)) + 'px';
    }
    displaySettings?.addEventListener('toggle', positionPopup);
    positionPopup(); window.lucide?.createIcons();
    popup.querySelector('button:not(:disabled)')?.focus({preventScroll:true});
  }
  /** Cancel pending hold work without activating a row or changing the selected session. */
  function cancelHold() { if (hold) { clearTimeout(hold.timer); timers.delete(hold.timer); hold = null; } }
  /** Resolve the row or selected trigger without treating footer fields/buttons as rows. */
  function contextTarget(event) {
    if (event.target.closest('button,input,textarea,select')) return null;
    const row = event.target.closest('.session-row');
    if (row && menu.contains(row)) {
      if (checkboxMode || batchBusy) return null;
      return {item:[...items,...binItems].find(item => item.id === row.dataset.id),anchor:row};
    }
    if (event.target.closest('.session-dropdown-trigger') && root.contains(event.target)) return {item:items.find(item => item.id === selected),anchor:root.querySelector('.session-dropdown-trigger')};
    return null;
  }
  /** Open actions with no session navigation; visibility flags still apply inside the popup. */
  function openContext(target) { if (target?.item && !destroyed) { cancelHold(); actions(target.item,target.anchor); } }
  for (const host of [root,menu]) {
    host.addEventListener('contextmenu', event => {
      const target = contextTarget(event);
      if (!target?.item || config.gestures?.right_click === false) return;
      event.preventDefault(); event.stopPropagation(); cancelHold();
      if (Date.now() >= suppressClickUntil) openContext(target);
    }, {capture:true,signal:abort.signal});
    host.addEventListener('keydown', event => {
      if (event.key !== 'ContextMenu' && !(event.shiftKey && event.key === 'F10')) return;
      const target = contextTarget(event); if (!target?.item) return;
      event.preventDefault(); event.stopPropagation(); openContext(target);
    }, {capture:true,signal:abort.signal});
    host.addEventListener('pointerdown', event => {
      cancelHold();
      if (!['touch','pen'].includes(event.pointerType) || event.isPrimary === false || config.gestures?.long_press === false) return;
      const target = contextTarget(event); if (!target?.item) return;
      const ms = Number(config.gestures?.long_press_ms);
      const pending = {id:event.pointerId,x:event.clientX,y:event.clientY,timer:0};
      pending.timer = delay(() => { if (hold !== pending) return; hold = null; suppressClickUntil = Date.now()+1000; openContext(target); },Number.isFinite(ms) ? Math.max(250,Math.min(2000,ms)) : 550);
      hold = pending;
    }, {capture:true,signal:abort.signal});
    host.addEventListener('click', event => {
      if (Date.now() < suppressClickUntil && contextTarget(event)) { event.preventDefault(); event.stopImmediatePropagation(); }
    }, {capture:true,signal:abort.signal});
  }
  document.addEventListener('pointermove', event => {
    if (!hold || event.pointerId !== hold.id) return;
    const configured = Number(config.gestures?.move_tolerance_px);
    const tolerance = Number.isFinite(configured) ? Math.max(1,Math.min(50,configured)) : 10;
    if (Math.hypot(event.clientX-hold.x,event.clientY-hold.y) > tolerance) cancelHold();
  }, {signal:abort.signal});
  for (const type of ['pointerup','pointercancel','scroll']) document.addEventListener(type,cancelHold,{capture:true,signal:abort.signal});
  window.addEventListener('blur',cancelHold,{signal:abort.signal});
  /** Runs scoped paginated private search and discards stale/destroyed responses. */
  async function search(more = false) {
    const turn = ++epoch, q = query, scope = status, token = more ? continuation : null;
    searching = true; if (!more) { hits = []; continuation = null; } searchInfo = ''; render();
    try {
      const page = await client.search({q,status:scope,continuation:token});
      if (destroyed || turn !== epoch) return;
      hits = more ? [...hits,...page.results] : page.results; continuation = page.continuation;
      searchInfo = page.incomplete_due_to_size ? `${page.skipped_oversized_total} oversized records skipped; search is incomplete.` : (page.scan_complete ? 'Search complete' : 'More records remain; continue search.');
    } catch (error) { if (turn === epoch && !destroyed) searchInfo = 'Search failed: ' + (error.message || 'Unable to search private sessions'); }
    finally { if (turn === epoch && !destroyed) { searching = false; render(); } }
  }
  /** Loads active or recycled rows, then applies the current private-search query. */
  async function loadScope() {
    const turn = ++epoch, scope = status;
    const data = await client.sessions(scope);
    if (destroyed || turn !== epoch) return;
    if (scope === 'active') items = data.sessions;
    else binItems = data.sessions;
    if (query.trim()) await search(); else render();
  }
  let binItems = [];
  /** Rebuilds visible rows and controls while preserving search focus/selection. */
  function render() {
    if (menu.hidden || editing || destroyed) return;
    const focused = menu.querySelector('.session-manage-search-input');
    const focus = focused === document.activeElement, position = focused?.selectionStart;
    cancelHold();
    menu.replaceChildren();
    const rowList = document.createElement('div'); rowList.className = 'session-dropdown-rows';
    menu.append(rowList);
    const source = status === 'active' ? items : binItems;
    const rows = query.trim() ? hits.map(hit => ({item:source.find(s => s.id === hit.session_id),hit})).filter(r => r.item) : source.map(item => ({item}));
    // Rank Main Chats by activity; preserve supplied pinned/ordinary and search order below them.
    if (status === 'active') rows.sort((a,b) => {
      const am=a.item.main_chat===true,bm=b.item.main_chat===true;
      if(am!==bm)return am?-1:1;
      return am?(b.item.last_active_at||0)-(a.item.last_active_at||0)||String(a.item.id).localeCompare(String(b.item.id)):0;
    });
    for (const {item,hit} of rows) {
      const row = document.createElement('div'); row.className = 'session-row' + (item.id === selected ? ' selected' : '') + (item.pinned ? ' pinned' : '') + (status === 'recycled' ? ' recycled' : '');
      row.dataset.id = item.id; row.dataset.agent = item.agent_id; row.tabIndex = 0;
      if (checkboxMode && status === 'active') {
        const b = button('Select ' + (item.title || 'New Session'),checked.has(item.id) ? 'check-square' : 'square',() => toggleChecked(item.id),'session-row-checkbox');
        b.dataset.checked = checked.has(item.id) ? '1' : '0'; b.setAttribute('aria-pressed',String(checked.has(item.id))); b.disabled = batchBusy||protectedSession(item);
        row.append(b);
      }
      const title = document.createElement('span'); title.className = 'session-row-title';
      if (config.menu?.agent_icons !== false) { const avatar = document.createElement('span'); avatar.className = 'session-row-agent-icon' + (item.main_chat === true ? ' session-row-agent-main' : item.pinned ? ' session-row-agent-warning' : ''); avatar.innerHTML = icon('bot'); title.append(avatar); }
      const text = document.createElement('span'); text.className = 'session-row-title-label'; text.textContent = item.title || 'New Session'; text.hidden = config.menu?.session_name === false; title.append(text); row.append(title);
      if (hit) { const snippet = document.createElement('span'); snippet.className = 'session-row-msg-hit'; snippet.textContent = hit.snippet; snippet.title = hit.snippet; row.append(snippet); }
      if (config.menu?.status !== false) row.append(statusNode(item));
      if (status === 'recycled') {
        const b = button('Restore session','archive-restore',null,'session-row-bin'); b.textContent = 'bin';
        confirmButton(b,'Restore?',() => mutate(item,'restore')); if (config.menu?.restore !== false) row.append(b);
        const remove = button('Permanently delete session','trash-2',null,'session-row-action danger');
        confirmButton(remove,'Delete permanently?',() => mutate(item,'purge')); if (config.menu?.purge !== false) row.append(remove);
      } else if (!checkboxMode && config.menu?.row_actions === true) row.append(button('More…','more-vertical',() => actions(item,row.querySelector('.session-row-kebab')),'session-row-kebab'));
      rowList.append(row);
    }
    if (!rows.length) { const empty = document.createElement('div'); empty.className = 'session-dropdown-empty'; empty.textContent = searching ? 'Searching…' : searchInfo.startsWith('Search failed:') ? 'Search unavailable; no results verified' : query ? 'No matches in this page' : status === 'recycled' ? 'Recycle bin is empty' : 'No sessions yet'; menu.append(empty); }
    if (searchInfo) { const info = document.createElement('div'); info.className = 'session-dropdown-sync'; info.setAttribute('role','status'); info.textContent = searchInfo; menu.append(info); }
    if (continuation) { const more = button('Continue private search','search',() => search(true),'session-row-action'); more.textContent = 'Continue search'; more.disabled = searching; menu.append(more); }
    const bar = document.createElement('div'); bar.className = 'session-manage-row';
    if (config.footer?.enabled !== false && config.footer?.search !== false) {
      const wrap = document.createElement('span'); wrap.className = 'session-manage-search'; wrap.innerHTML = icon('search');
      const input = document.createElement('input'); input.className = 'session-manage-search-input'; input.placeholder = 'Search private titles and content…'; input.setAttribute('aria-label','Search private session titles and content'); input.value = query;
      input.disabled = batchBusy;
      input.oninput = () => { query = input.value; ++epoch; searching = false; hits = []; continuation = null; clearTimeout(timer); if (query.trim()) timer = delay(() => guard(() => search())(),250); else render(); };
      wrap.append(input); bar.append(wrap);
    }
    if (config.footer?.enabled !== false && config.footer?.recycle_bin !== false) { const b = button(status === 'active' ? 'Recycle bin' : 'Back to active sessions','recycle',async () => { status = status === 'active' ? 'recycled' : 'active'; hits = []; continuation = null; await loadScope(); }); b.setAttribute('aria-pressed',String(status === 'recycled')); b.disabled = batchBusy; bar.append(b); }
    if (config.footer?.enabled !== false && config.footer?.hidden_sessions !== false) { const b = button('Hidden sessions','eye-off',null); b.disabled = true; b.title = 'Hidden sessions — unavailable in v3'; bar.append(b); }
    if (config.footer?.enabled !== false && config.footer?.batch_delete !== false && status === 'active') {
      const b = button(checkboxMode ? 'Disable checkbox mode' : 'Enable checkbox mode','square',() => { checkboxMode = !checkboxMode; checked.clear(); render(); },'session-manage-checkbox-toggle');
      b.dataset.state = checkboxMode ? 'on' : 'off'; b.setAttribute('aria-pressed',String(checkboxMode)); b.disabled = batchBusy; bar.append(b);
      if (checkboxMode && checked.size) {
        const recycle = button(`Recycle selected (${checked.size})`,'trash-2',null,'session-manage-delete-selected');
        recycle.dataset.state = 'trash'; recycle.disabled = batchBusy;
        confirmButton(recycle,`Recycle ${checked.size} selected?`,recycleChecked); bar.append(recycle);
      }
    }
    if (config.footer?.enabled !== false && bar.childElementCount) menu.append(bar);
    const count = Number(config.menu?.default_rows);
    const visibleRows = Number.isInteger(count) ? Math.max(1,Math.min(100,count)) : 8;
    const height = [...rowList.children].slice(0,visibleRows).reduce((sum,row) => sum+row.getBoundingClientRect().height,0);
    rowList.style.maxHeight = height ? height+'px' : '';
    menu.style.setProperty('--session-menu-open-height',Math.min(menu._availableHeight || 480,menu.scrollHeight) + 'px');
    window.lucide?.createIcons();
    if (focus) { const input = menu.querySelector('input'); input?.focus(); if (position != null) input?.setSelectionRange(position,position); }
  }
  root.querySelector('#session-dropdown-kebab').onclick = e => { e.stopPropagation(); const item = items.find(s => s.id === selected); if (item) actions(item,e.currentTarget); };
  confirmButton(root.querySelector('#session-dropdown-delete'),'Recycle?',() => { const item = items.find(s => s.id === selected); return item ? mutate(item,'recycle') : undefined; });
  const tick = setInterval(() => { for (const node of [...menu.querySelectorAll('[data-ts]'),...root.querySelectorAll('[data-ts]')]) node.textContent = elapsed(Number(node.dataset.ts)); },1000);
  // Public lifecycle API keeps selected rows, configuration, search state and timers in sync.
  return {
    // Replace authoritative rows/selection and drop checkbox IDs no longer present.
    update(rows,id) { items = rows; selected = id; for (const id of checked) if (!items.some(item => item.id === id)) checked.delete(id); label(); render(); },
    // Reapply menu options; disabling batch delete also clears its transient selection.
    configure(cfg) { cancelHold(); config = cfg || {}; root._sessionDropdownController.setPopup(null); if (config.footer?.enabled === false || config.footer?.batch_delete === false) { checkboxMode = false; checked.clear(); } if (config.footer?.enabled === false || config.footer?.recycle_bin === false) status = 'active'; if (config.footer?.enabled === false || config.footer?.search === false) { ++epoch; clearTimeout(timer); query = ''; hits = []; continuation = null; searching = false; searchInfo = ''; } menu.style.setProperty('--session-dropdown-max-height',config.menu?.max_height || 'none'); label(); render(); },
    // Invalidate in-flight search and restore the default active, unfiltered list.
    reset() { cancelHold(); if (!batchBusy) { checkboxMode = false; checked.clear(); } ++epoch; clearTimeout(timer); query = ''; hits = []; continuation = null; searching = false; searchInfo = ''; status = 'active'; },
    // Cancel timers/listeners and detach editors before the dropdown root is removed.
    destroy() { cancelHold(); destroyed = true; ++epoch; clearTimeout(timer); clearInterval(tick); for (const t of timers) clearTimeout(t); abort.abort(); editing?.remove(); root.querySelector('#session-dropdown-kebab').onclick = null; root.querySelector('#session-dropdown-delete').onclick = null; }
  };
}
