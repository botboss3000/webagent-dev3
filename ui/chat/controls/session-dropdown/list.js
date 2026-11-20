// V2 dropdown presentation/lifecycle, with independent Rust data authority.
// Session dropdown presentation and interactions.
import {bindNotes, createBlock, fillBlock, flagToggle, isLive, setFlagCatalog, tickTimes} from './row.js';
import {createExpandList} from '../../../components/expand-list/expand-list.js';
/** Renders active/archived sessions, private search results, and revision-fenced management actions. Controller callbacks own navigation; client writes retain retry receipts, and teardown fences pending work. */
export function createSessionList(root, {guard, refresh, select, client, archived = () => {}}) {
  const protectedSession=item=>item?.main_chat===true||item?.can_delete===false;
  let items = [], selected = '', config = {}, query = '', status = 'active';
  let hits = [], continuation = null, searchInfo = '', searching = false, epoch = 0, timer = 0, destroyed = false;
  let editing = null, checkboxMode = false, batchBusy = false;
  let hold = null, suppressClickUntil = 0;
  const checked = new Set(), expanded = new Set();
  const menu = root.querySelector('.session-dropdown-menu');
  // Header sub-agent tabs (chat.html #chat-sub-header); cached because header rows are rebuilt around it.
  const tabs = document.getElementById('chat-sub-header');
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
  /** Sub-agent sessions of each loaded parent, keyed by parent session ID. */
  function childrenOf() {
    const ids = new Set(items.map(item => item.id)), map = new Map();
    for (const item of items) if (item.parent_session_id && ids.has(item.parent_session_id)) {
      if (!map.has(item.parent_session_id)) map.set(item.parent_session_id, []);
      map.get(item.parent_session_id).push(item);
    }
    return map;
  }
  /** Loaded ancestors of `id`, nearest first; a cycle stops the walk. */
  function ancestors(id) {
    const out = []; let item = items.find(s => s.id === id);
    while (item?.parent_session_id && !out.includes(item.parent_session_id)) {
      item = items.find(s => s.id === item.parent_session_id); if (!item) break; out.push(item.id);
    }
    return out;
  }
  /** Shows the selected session's family as one Spawns (n) header chip; its menu lists Main, then every sub-agent. */
  function renderTabs() {
    if (!tabs) return;
    const kids = childrenOf(), rootId = ancestors(selected).at(-1) || selected;
    const members = [], walk = id => { for (const child of kids.get(id) || []) { members.push(child); walk(child.id); } };
    walk(rootId);
    const wrap = tabs.querySelector('#chat-sub-scroll-wrap');
    // The Broker (broker-chip.js), Artifacts, Tasks and Main Notes chips keep their place after Spawns.
    const kept = new Set(['chat-broker-chip', 'chat-artifacts-chip', 'chat-tasks-chip', 'chat-actions-chip', 'chat-main-notes-chip']);
    wrap.replaceChildren(...[...wrap.children].filter(node => kept.has(node.id)));
    tabs.toggleAttribute('data-empty', !members.length);
    if (!members.length) return;
    const chip = button(`${members.length} sub-agent session${members.length === 1 ? '' : 's'}`,'git-branch',() => familyMenu(chip,items.find(s => s.id === rootId),members),'sub-chip chip-spawn-menu');
    chip.firstChild.replaceWith(Object.assign(document.createElement('span'),{className:'chip-status',innerHTML:icon('git-branch')}));
    chip.append(`Spawns (${members.length})`);
    chip.setAttribute('aria-haspopup','menu'); chip.setAttribute('aria-expanded','false');
    wrap.prepend(chip); window.lucide?.createIcons();
  }
  /** Opens the family panel under its chip: Main and each sub-agent are expand-list rows.
   * Opening a row loads that session's first message (a sub-agent's starting prompt) once,
   * shown in two lines; pressing it opens the session. */
  function familyMenu(chip, main, members) {
    if (chip.getAttribute('aria-expanded') === 'true') { root._sessionDropdownController.setPopup(null); return; }
    const popup = document.createElement('div'); popup.className = 'family-spawn-menu sub-chip-menu spawn-panel wa-scroll-gutter';
    popup.setAttribute('aria-label','Sub-agent sessions');
    const people = [[main,'Main session',-1],...members.map((m,i) => [m,`Sub-agent ${i+1}`,i])];
    const go = id => { root._sessionDropdownController.setPopup(null); if (id !== selected) select(id); };
    const list = createExpandList({label: 'Sub-agent sessions', defaultOpen: () => false,
      onExpand: async (row, open, slot) => {
        if (!open) return;
        slot.replaceChildren(Object.assign(document.createElement('p'),{className:'spawn-prompt-status',textContent:'Loading…'}));
        let text = '';
        try { text = (await client.firstMessage(row.id))?.text || ''; }
        catch (error) { slot.replaceChildren(Object.assign(document.createElement('p'),{className:'spawn-prompt-status',textContent:error.message})); delete slot.dataset.loaded; return; }
        const link = document.createElement('button'); link.type = 'button'; link.className = 'spawn-prompt';
        link.textContent = text || 'No message yet'; link.title = 'Open ' + row.title;
        link.onclick = e => { e.stopPropagation(); go(row.id); };
        slot.replaceChildren(link);
      }});
    list.update(people.map(([item,labelText,i]) => ({id: item.id, title: labelText, lazy: true, status: 'pending', pill: item.id === selected ? 'Open' : false,
      icon: i < 0 ? 'LayoutDashboard' : 'GitBranch', badges: [item.title || (i < 0 ? 'Main chat' : 'New Session')]})));
    popup.append(list.element);
    root._sessionDropdownController.setPopup(popup); document.body.append(popup); chip.setAttribute('aria-expanded','true');
    const r = chip.getBoundingClientRect();
    popup.style.position = 'fixed'; popup.style.top = (r.bottom + 4) + 'px';
    popup.style.left = Math.max(4,Math.min(r.left,innerWidth - popup.offsetWidth - 8)) + 'px';
    // Any other press closes the menu; the chip itself toggles it.
    const close = e => { if (popup.isConnected && (popup.contains(e.target) || chip.contains(e.target))) return; document.removeEventListener('pointerdown',close,true); chip.setAttribute('aria-expanded','false'); if (popup.isConnected) root._sessionDropdownController.setPopup(null); };
    document.addEventListener('pointerdown',close,{capture:true,signal:abort.signal});
    new MutationObserver((_,observer) => { if (!popup.isConnected) { observer.disconnect(); chip.setAttribute('aria-expanded','false'); document.removeEventListener('pointerdown',close,true); } }).observe(document.body,{childList:true});
    popup.querySelector('button')?.focus({preventScroll:true});
  }
  /** Refreshes trigger label/status from the currently selected session. */
  function label() {
    const item = items.find(s => s.id === selected);
    // A selected session whose row has not loaded yet is not a new session; never claim it is.
    const block = root.querySelector('#session-dropdown-block');
    if (block && !editing) fillBlock(block, item, item?.title || (selected ? '\u2014' : 'New Session'));
    root.dataset.loaded = 'true';
    for (const id of ['icon','label','status','kebab','delete']) {
      const node = id === 'label' ? root.querySelector('#session-dropdown-block .session-row-title-label') : id === 'status' ? root.querySelector('#session-dropdown-block .srow-time') : root.querySelector('#session-dropdown-' + id);
      if (!node) continue;
      node.hidden = config.trigger?.[{icon:'agent_icon',label:'session_name',kebab:'row_actions',delete:'archive'}[id] || id] === false || (id === 'kebab' && config.trigger?.row_actions !== true) || (id === 'delete' && (config.trigger?.archive !== true || protectedSession(item)));
      node.style.display = node.hidden ? 'none' : '';
    }
    root.querySelector('#session-dropdown-icon').classList.toggle('session-row-agent-warning', !!item?.pinned && item?.main_chat !== true);
    root.querySelector('#session-dropdown-icon').classList.toggle('session-row-agent-main', item?.main_chat === true);
    root.querySelector('#session-dropdown-icon').classList.toggle('session-row-agent-running', isLive(item));
    root.querySelector('#session-dropdown-delete').disabled = !item || item.status === 'archived' || protectedSession(item);
  }
  /** Sends revision-fenced client actions with stable retry IDs for uncertain responses. */
  async function mutate(item, action, changes = {}, deferred = false) {
    if (['archive','delete'].includes(action)&&protectedSession(item)) throw Error('Main Chat cannot be archived or deleted');
    const body = {expected_revision:item.metadata_revision, ...changes};
    const receiptKey = JSON.stringify([item.id, action, body]);
    if (!receipts.has(receiptKey)) receipts.set(receiptKey, crypto.randomUUID());
    body.client_request_id = receipts.get(receiptKey);
    try {
      const result = await client[action](item.id, body);
      receipts.delete(receiptKey);
      if (destroyed) return;
      if (['archive','delete'].includes(action) && selected === item.id) archived(item.id);
      // The reply carries the updated metadata; live run fields stay from the list row.
      if (result.session) items = items.map(s => s.id === item.id ? {...s,...result.session} : s);
      if (!deferred && !destroyed) {
        root._sessionDropdownController.setPopup(null);
        // A rename, pin or flag change updates its row in place; archive and delete change the list.
        if (action === 'patch' && result.session) { label(); reconcile(); }
        else { await refresh(); if (status !== 'active') await loadScope(); }
      }
    } catch (error) {
      // Keep the exact receipt/body available for an uncertain-response retry.
      if (!deferred && !destroyed) { searchInfo = error.message || 'Session update failed'; render(); }
      throw error;
    }
  }
  // V2 checkbox management, backed by individually revision-fenced Rust writes.
  // A batch is not atomic: keep failed selections and their retry receipts.
  /** Archives selected sessions sequentially; failed rows stay selected for retry. */
  async function archiveChecked() {
    if (batchBusy || destroyed) return;
    const targets = items.filter(item => checked.has(item.id)&&!protectedSession(item));
    if (!targets.length) return;
    batchBusy = true; render();
    const failures = []; let completed = 0;
    try {
      for (const item of targets) {
        if (destroyed) break;
        try { await mutate(item, 'archive', {}, true); checked.delete(item.id); completed++; }
        catch (error) { failures.push({title:item.title || 'New Session', message:error.message || 'Archive failed'}); }
      }
      if (!destroyed) {
        try { await refresh(); await loadScope(); }
        catch (error) { failures.push({title:'Refresh',message:error.message || 'List refresh failed'}); }
        searchInfo = `Archived ${completed} of ${targets.length} selected sessions.` +
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
  /** Builds the row action popup at the pointer or under its anchor; unsupported V3 actions stay visibly disabled. */
  function actions(item, anchor, point) {
    const popup = document.createElement('div'); popup.className = 'session-row-actions'; popup.dataset.source = 'session-list'; popup.dataset.id = item.id;
    const archivedActions = [
      ['unarchive','Unarchive session','archive-restore',() => mutate(item,'unarchive')],
      ['delete','Delete session','trash-2',() => mutate(item,'delete')]
    ];
    for (const [key, title, glyph, fn] of (item.status === 'archived' ? archivedActions : [
      ['pinning',item.pinned ? 'Unpin session' : 'Pin session',item.pinned ? 'pin-off' : 'pin',() => mutate(item,'patch',{pinned:!item.pinned})],
      ['auto_rename','Auto rename','wand-2',null],
      ['rename','Rename session','pencil',() => { root._sessionDropdownController.setPopup(null); rename(item, anchor.closest('.session-row')?.querySelector('.session-row-title-label') || root.querySelector('#session-dropdown-block .session-row-title-label')); }],
      ['open_in_widget','Open in widget','messages-square',null],
      ['refresh','Refresh session','refresh-cw',async () => { await refresh(); await loadScope(); }],
      ['start_scratchpad','Start a scratchpad','notepad-text',null],
      ...(flagToggle(item) ? [['flag_status',flagToggle(item).title,flagToggle(item).glyph,() => mutate(item,'patch',flagToggle(item).changes)]] : []),
      ['archive','Archive session','archive',() => mutate(item,'archive')]
    ])) {
      if (config.menu?.[key] === false || (['archive','delete'].includes(key)&&protectedSession(item))) continue;
      const b = button(title,glyph,fn,'session-row-action' + (['archive','delete'].includes(key) ? ' danger' : ''));
      b.append(document.createTextNode(' ' + title)); b.disabled = !fn;
      if (!fn) b.title = title + ' — unavailable in v3';
      if (['archive','unarchive','delete'].includes(key)) confirmButton(b, key === 'archive' ? 'Archive?' : key === 'unarchive' ? 'Unarchive?' : 'Delete permanently?', fn);
      popup.append(b);
    }
    for (const [key,title,value] of [['agent_id','Agent',item.agent_id],['session_id','Session',item.id]]) {
      if (config.menu?.[key] === false) continue;
      const b = button(title,'copy',() => navigator.clipboard.writeText(value),'session-row-action session-row-id');
      b.append(document.createTextNode(` ${title}: ${value}`)); popup.append(b);
    }
    if (!popup.childElementCount) { root._sessionDropdownController.setPopup(null); return; }
    popup.style.maxHeight = 'min(80vh, 640px)'; popup.style.overflowY = 'auto';
    // Explicit intrinsic width prevents fixed-position shrink-to-fit width changing after placement.
    popup.style.width = 'max-content'; popup.style.maxWidth = 'calc(100vw - 8px)'; popup.style.boxSizing = 'border-box';
    popup.setAttribute('role','group'); popup.setAttribute('aria-label','Session actions');
    root._sessionDropdownController.setPopup(popup); document.body.append(popup);
    popup.addEventListener('keydown', e => { if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); root._sessionDropdownController.setPopup(null); anchor.focus?.(); } });
    /** Places the complete scrollable popup at the pointer, or under its anchor, inside the viewport. */
    function positionPopup() {
      popup.style.position = 'fixed';
      const bounds = popup.getBoundingClientRect();
      const r = point ? null : anchor.getBoundingClientRect();
      const left = point ? point.x : r.right - bounds.width;
      const top = point ? point.y : r.bottom + 4;
      popup.style.left = Math.max(4,Math.min(left,innerWidth-bounds.width-4)) + 'px';
      popup.style.top = Math.max(4,Math.min(top,innerHeight-bounds.height-4)) + 'px';
    }
    positionPopup(); window.lucide?.createIcons();
    popup.querySelector('button:not(:disabled)')?.focus({preventScroll:true});
    // Any press outside the panel dismisses it, including a press inside the dropdown.
    const dismissOnPress = event => {
      if (!popup.isConnected) { document.removeEventListener('pointerdown', dismissOnPress, true); return; }
      if (!popup.contains(event.target)) root._sessionDropdownController.setPopup(null);
    };
    document.addEventListener('pointerdown', dismissOnPress, {capture:true, signal:abort.signal});
  }
  /** Cancel pending hold work without activating a row or changing the selected session. */
  function cancelHold() { if (hold) { clearTimeout(hold.timer); timers.delete(hold.timer); hold = null; } }
  /** Resolve the row or selected trigger without treating footer fields/buttons as rows. */
  function contextTarget(event) {
    if (event.target.closest('button,input,textarea,select')) return null;
    const row = event.target.closest('.session-row');
    if (row && menu.contains(row)) {
      if (checkboxMode || batchBusy) return null;
      return {item:[...items,...archivedItems].find(item => item.id === row.dataset.id),anchor:row};
    }
    if (event.target.closest('.session-dropdown-trigger') && root.contains(event.target)) return {item:items.find(item => item.id === selected),anchor:root.querySelector('.session-dropdown-trigger')};
    return null;
  }
  /** Open actions at the pointer with no session navigation; visibility flags still apply inside the popup. */
  function openContext(target, point) { if (target?.item && !destroyed) { cancelHold(); actions(target.item,target.anchor,point); } }
  for (const host of [root,menu]) {
    host.addEventListener('contextmenu', event => {
      const target = contextTarget(event);
      if (!target?.item || config.gestures?.right_click === false) return;
      event.preventDefault(); event.stopPropagation(); cancelHold();
      if (Date.now() >= suppressClickUntil) openContext(target,{x:event.clientX,y:event.clientY});
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
      pending.timer = delay(() => { if (hold !== pending) return; hold = null; suppressClickUntil = Date.now()+1000; openContext(target,{x:pending.x,y:pending.y}); },Number.isFinite(ms) ? Math.max(250,Math.min(2000,ms)) : 550);
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
  /** Loads active or archived rows, then applies the current private-search query. */
  async function loadScope() {
    const turn = ++epoch, scope = status;
    const data = await client.sessions(scope);
    if (destroyed || turn !== epoch) return;
    setFlagCatalog(data.flags);
    if (scope === 'active') items = data.sessions;
    else archivedItems = data.sessions;
    if (query.trim()) await search(); else reconcile();
  }
  let archivedItems = [];
  /** The visible rows in order: Main Chats first, sub-agent sessions nested under expanded parents. */
  function visibleTree() {
    const source = status === 'active' ? items : archivedItems;
    const rows = query.trim() ? hits.map(hit => ({item:source.find(s => s.id === hit.session_id),hit})).filter(r => r.item) : source.map(item => ({item}));
    // Rank Main Chats by activity; preserve supplied pinned/ordinary and search order below them.
    if (status === 'active') rows.sort((a,b) => {
      const am=a.item.main_chat===true,bm=b.item.main_chat===true;
      if(am!==bm)return am?-1:1;
      return am?(b.item.last_active_at||0)-(a.item.last_active_at||0)||String(a.item.id).localeCompare(String(b.item.id)):0;
    });
    // Sub-agent sessions nest under their parent row in the unfiltered active list (v2 group tree).
    const nest = status === 'active' && !query.trim(), kids = nest ? childrenOf() : new Map();
    const nested = new Set([...kids.values()].flat().map(item => item.id));
    const tree = [], add = (entry,depth) => { tree.push({...entry,depth}); if (expanded.has(entry.item.id)) for (const item of kids.get(entry.item.id) || []) add({item},depth+1); };
    for (const entry of rows) if (!nested.has(entry.item.id)) add(entry,0);
    return {tree, kids};
  }
  /** What a row shows; a row is rebuilt only when this changes. */
  function rowSignature({item,hit,depth}, kids) {
    return JSON.stringify([item.title, item.pinned, item.main_chat, item.agent_id, item.status, item.flag, item.flag_dismissed,
      item.notification, item.flag_set_at, item.last_active_at, item.run_status, item.run_started_at, item.activity_counts,
      hit?.snippet, depth, item.id === selected, kids.get(item.id)?.length || 0, expanded.has(item.id),
      checkboxMode, checked.has(item.id), batchBusy, status]);
  }
  /** Build one session row. */
  function buildRow({item,hit,depth}, kids) {
    const row = document.createElement('div'); row.className = 'session-row' + (depth ? ' session-child-row' : '') + (item.id === selected ? ' selected' : '') + (item.pinned ? ' pinned' : '') + (status === 'archived' ? ' archived' : '');
    if (depth > 1) row.style.paddingLeft = (22 * depth) + 'px';
    row.dataset.id = item.id; row.dataset.agent = item.agent_id; row.tabIndex = 0;
    if (checkboxMode && status === 'active') {
      const b = button('Select ' + (item.title || 'New Session'),checked.has(item.id) ? 'check-square' : 'square',() => toggleChecked(item.id),'session-row-checkbox');
      b.dataset.checked = checked.has(item.id) ? '1' : '0'; b.setAttribute('aria-pressed',String(checked.has(item.id))); b.disabled = batchBusy||protectedSession(item);
      row.append(b);
    }
    const title = document.createElement('span'); title.className = 'session-row-title';
    if (depth) { const branch = document.createElement('span'); branch.className = 'session-row-child-icon'; branch.innerHTML = icon('git-branch'); title.append(branch); }
    const children = kids.get(item.id)?.length || 0;
    if (config.menu?.agent_icons !== false) {
      // A parent's avatar is its family disclosure control, as in v2.
      const open = expanded.has(item.id);
      const avatar = children ? button(`${open ? 'Hide' : 'Show'} ${children} sub-agent session${children === 1 ? '' : 's'}`,'bot',() => { if (open) expanded.delete(item.id); else expanded.add(item.id); render(); }) : document.createElement('span');
      avatar.className = 'session-row-agent-icon' + (children ? ' sess-title-agent-toggle' : '') + (item.main_chat === true ? ' session-row-agent-main' : item.pinned ? ' session-row-agent-warning' : '');
      if (children) avatar.setAttribute('aria-expanded',String(open)); else avatar.innerHTML = icon('bot');
      title.append(avatar);
    }
    title.querySelector('.session-row-agent-icon')?.classList.toggle('session-row-agent-running', isLive(item));
    const block = fillBlock(createBlock(), item, (item.title || 'New Session') + (children ? ` (${children})` : ''));
    block.querySelector('.session-row-title-label').hidden = config.menu?.session_name === false;
    block.querySelector('.srow-time').hidden = config.menu?.status === false;
    title.append(block); row.append(title);
    if (hit) { const snippet = document.createElement('span'); snippet.className = 'session-row-msg-hit'; snippet.textContent = hit.snippet; snippet.title = hit.snippet; row.append(snippet); }
    if (status === 'archived') {
      const b = button('Unarchive session','archive-restore',null,'session-row-archived'); b.textContent = 'archived';
      confirmButton(b,'Unarchive?',() => mutate(item,'unarchive')); if (config.menu?.unarchive !== false) row.append(b);
      const remove = button('Delete session','trash-2',null,'session-row-action danger');
      confirmButton(remove,'Delete permanently?',() => mutate(item,'delete')); if (config.menu?.delete !== false) row.append(remove);
    } else if (!checkboxMode && config.menu?.row_actions === true) row.append(button('More…','more-vertical',() => actions(item,row.querySelector('.session-row-kebab')),'session-row-kebab'));
    row.dataset.signature = rowSignature({item,hit,depth}, kids);
    return row;
  }
  /** Update rows in place: keep unchanged rows (and an open rename), rebuild changed ones,
   *  add new ones and drop removed ones, so scrolling and open editors survive a refresh. */
  function reconcile() {
    const rowList = menu.querySelector('.session-dropdown-rows');
    if (menu.hidden || destroyed || !rowList || query.trim()) return render();
    const {tree, kids} = visibleTree();
    if (!tree.length) return render();
    const existing = new Map([...rowList.children].map(node => [node.dataset.id, node]));
    const nodes = tree.map(entry => {
      const node = existing.get(entry.item.id);
      if (node && (node.contains(editing) || node.dataset.signature === rowSignature(entry, kids))) {
        fillBlock(node.querySelector('.srow-block'), entry.item, node.querySelector('.session-row-title-label')?.textContent);
        return node;
      }
      const row = buildRow(entry, kids);
      if (node) node.replaceWith(row);
      return row;
    });
    for (const [id, node] of existing) if (!tree.some(entry => entry.item.id === id)) node.remove();
    nodes.forEach((node, index) => { if (rowList.children[index] !== node) rowList.insertBefore(node, rowList.children[index] || null); });
    window.lucide?.createIcons();
  }
  /** Rebuilds visible rows and controls while preserving search focus/selection; `force`
   *  paints a menu that is about to open. */
  function render(force = false) {
    if ((menu.hidden && !force) || editing || destroyed) return;
    const focused = menu.querySelector('.session-manage-search-input');
    const focus = focused === document.activeElement, position = focused?.selectionStart;
    cancelHold();
    menu.replaceChildren();
    const rowList = document.createElement('div'); rowList.className = 'session-dropdown-rows';
    menu.append(rowList);
    const {tree, kids} = visibleTree();
    for (const entry of tree) rowList.append(buildRow(entry, kids));
    if (!tree.length) { const empty = document.createElement('div'); empty.className = 'session-dropdown-empty'; empty.textContent = searching ? 'Searching…' : searchInfo.startsWith('Search failed:') ? 'Search unavailable; no results verified' : query ? 'No matches in this page' : status === 'archived' ? 'No archived sessions' : 'No sessions yet'; menu.append(empty); }
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
    if (config.footer?.enabled !== false && config.footer?.archived !== false) { const b = button(status === 'active' ? 'Archived' : 'Back to active sessions','archive',async () => { status = status === 'active' ? 'archived' : 'active'; hits = []; continuation = null; await loadScope(); }); b.setAttribute('aria-pressed',String(status === 'archived')); b.disabled = batchBusy; bar.append(b); }
    if (config.footer?.enabled !== false && config.footer?.hidden_sessions !== false) { const b = button('Hidden sessions','eye-off',null); b.disabled = true; b.title = 'Hidden sessions — unavailable in v3'; bar.append(b); }
    if (config.footer?.enabled !== false && config.footer?.batch_archive !== false && status === 'active') {
      const b = button(checkboxMode ? 'Disable checkbox mode' : 'Enable checkbox mode','square',() => { checkboxMode = !checkboxMode; checked.clear(); render(); },'session-manage-checkbox-toggle');
      b.dataset.state = checkboxMode ? 'on' : 'off'; b.setAttribute('aria-pressed',String(checkboxMode)); b.disabled = batchBusy; bar.append(b);
      if (checkboxMode && checked.size) {
        const archive = button(`Archive selected (${checked.size})`,'archive',null,'session-manage-delete-selected');
        archive.dataset.state = 'archive'; archive.disabled = batchBusy;
        confirmButton(archive,`Archive ${checked.size} selected?`,archiveChecked); bar.append(archive);
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
  confirmButton(root.querySelector('#session-dropdown-delete'),'Archive?',() => { const item = items.find(s => s.id === selected); return item ? mutate(item,'archive') : undefined; });
  bindNotes(menu, abort.signal); bindNotes(root.querySelector('.session-dropdown-trigger'), abort.signal);
  // Refresh the shown times each second; row data arrives by push or on menu open.
  const tick = setInterval(() => { tickTimes(menu); tickTimes(root); },1000);
  // Public lifecycle API keeps selected rows, configuration, search state and timers in sync.
  return {
    /** Loaded sub-agent sessions under `id`, as [{id, label: 'Spawn n'}], for the Broker chip. */
    family(id) { const kids = childrenOf(), out = [], walk = parent => { for (const child of kids.get(parent) || []) { out.push({id:child.id,label:'Spawn ' + (out.length + 1)}); walk(child.id); } }; walk(id); return out; },
    // Replace authoritative rows/selection and drop checkbox IDs no longer present.
    update(rows,id,flags) { setFlagCatalog(flags); items = rows; if (id !== selected) for (const parent of ancestors(id)) expanded.add(parent); selected = id; for (const id of checked) if (!items.some(item => item.id === id)) checked.delete(id); label(); renderTabs(); reconcile(); },
    /** Merge pushed fields into one session and update only its trigger and row. */
    patch(id,fields) { const index = items.findIndex(item => item.id === id); if (index < 0 || destroyed) return; items[index] = {...items[index],...fields}; if (id === selected) label(); if (!menu.hidden) reconcile(); },
    /** Paint the cached rows into a menu that is opening; false when nothing is cached. */
    paintCached() { if (!items.length || status !== 'active' || query.trim() || destroyed) return false; render(true); return true; },
    // Reapply menu options; disabling batch delete also clears its transient selection.
    configure(cfg) { cancelHold(); config = cfg || {}; root._sessionDropdownController.setPopup(null); if (config.footer?.enabled === false || config.footer?.batch_archive === false) { checkboxMode = false; checked.clear(); } if (config.footer?.enabled === false || config.footer?.archived === false) status = 'active'; if (config.footer?.enabled === false || config.footer?.search === false) { ++epoch; clearTimeout(timer); query = ''; hits = []; continuation = null; searching = false; searchInfo = ''; } menu.style.setProperty('--session-dropdown-max-height',config.menu?.max_height || 'none'); label(); render(); },
    // Invalidate in-flight search and restore the default active, unfiltered list.
    reset() { cancelHold(); if (!batchBusy) { checkboxMode = false; checked.clear(); } ++epoch; clearTimeout(timer); query = ''; hits = []; continuation = null; searching = false; searchInfo = ''; status = 'active'; },
    // Cancel timers/listeners and detach editors before the dropdown root is removed.
    destroy() { cancelHold(); destroyed = true; ++epoch; clearTimeout(timer); clearInterval(tick); for (const t of timers) clearTimeout(t); abort.abort(); editing?.remove(); root.querySelector('#session-dropdown-kebab').onclick = null; root.querySelector('#session-dropdown-delete').onclick = null; }
  };
}
