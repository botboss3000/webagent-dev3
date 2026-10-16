// @code-index {"version":1,"feature":"chat-session-selector","aliases":["session dropdown","conversation selector"],"purpose":"Session dropdown presentation and interactions. Config: ui/chat/controls/session-dropdown/session-dropdown.json -> common.trigger, common.menu, common.footer, common.gestures with desktop/mobile/widget overlays; adopted on page reload.","span_lines":40,"configuration":{"status":"configured","references":[{"path":"ui/chat/controls/session-dropdown/session-dropdown.json","pointers":["/common/trigger","/common/menu","/common/footer","/common/gestures","/desktop","/mobile","/widget"]}],"opportunities":[]}}
// Adapted from running v2 session-dropdown/controller.js. V3 owns data/actions.
/** Mounts menu positioning, keyboard/focus behavior, animation, and teardown around V3-owned data callbacks. */
export function mountSessionDropdown(root, {refresh, select, guard, resetSearch = () => {}}) {
 const dropdown=root, menu=root.querySelector('.session-dropdown-menu');
 const app={currentUserId:true};
 let _openRequestId=0,_keyboardDismissRequested=false,_closeAnimationTimer=0,_openAnimationTimer=0;
 const abort=new AbortController();
 // V2-only reset hooks are inert because V3 list.js owns these controls/states.
 const _resetAllDeleteButtons=()=>{}, _cancelPendingSearchCommit=()=>{}, _clearMsgSearch=()=>{};
 const _setShowHidden=()=>{}, _setSearchQuery=()=>{resetSearch();const search=menu.querySelector('input');if(search)search.value='';};
 // Keep the legacy load-state interface callable; this V3 controller has no shared store.
 const sessionDropdownStore={setLoadState:()=>{}};
 // V3 is browser-backed here; the legacy non-browser cache branch stays disabled.
 const storageAdapter={isBrowser:true};
 // Data refresh is supplied by panel.js; the sentinel is false on success and failure.
 const populateSessionSelect=async()=>{try{await refresh();return false;}catch(error){menu.textContent=error.message;return false;}};
 let popup=null;
 // Replacing/closing a row popup always removes its current DOM node.
 const _closeRowActions=()=>{popup?.remove();popup=null;};
  /** Mirrors the menu syncing flag into a row above list.js managed content. */
  function _syncStatusRow() {
    if (!menu) return;
    const syncing = menu.dataset.syncing === 'true';
    const existing = menu.querySelector(':scope > .session-dropdown-sync[data-controller-sync]');
    if (!syncing) {
      if (existing) existing.remove();
      return;
    }
    let sync = existing;
    if (!sync) {
      sync = document.createElement('div');
      sync.className = 'session-dropdown-sync';
      sync.dataset.controllerSync = 'true';
      sync.textContent = 'Syncing…';
    }
    const footer = menu.querySelector(':scope > .session-manage-row');
    if (footer) {
      if (footer.previousElementSibling !== sync) footer.before(sync);
    } else if (menu.lastElementChild !== sync) {
      menu.appendChild(sync);
    }
  }
  // React to both data-syncing toggles and menu re-renders (rows/footer are
  // rebuilt by list.js) so the sync row is always positioned above the footer.
  const _syncRowObserver = new MutationObserver(_syncStatusRow);
  _syncRowObserver.observe(menu, { attributes: true, attributeFilter: ['data-syncing'], childList: true });
  // Ensure it is correct if the menu already carries data-syncing at mount time.
  _syncStatusRow();

  /** Identifies editable focus that may have opened the mobile keyboard. */
  function _isEditableElement(el) {
    if (!el || el === document.body) return false;
    return el.matches?.('input, textarea, select, [contenteditable="true"]') || false;
  }

  /** Uses the visual viewport when available for keyboard-settle measurements. */
  function _viewportHeight() {
    return window.visualViewport?.height || window.innerHeight;
  }

  // Focus is the reliable keyboard signal. Some Android WebViews shrink both
  // visualViewport and innerHeight, so comparing those two values can falsely
  // report that the keyboard is already closed.
  /** Blurs editable focus before opening, preserving the composer draft caret. */
  function _dismissKeyboardBeforeOpen() {
    const active = document.activeElement;
    // The conversation trigger is part of the composer controls. Opening its
    // menu must preserve the draft caret rather than collapse the empty pill.
    if (active?.id === 'chat-input') return false;
    const hadEditableFocus = _isEditableElement(active)
      || document.body.classList.contains('chat-pill-focused');
    if (!hadEditableFocus) return false;

    _keyboardDismissRequested = true;
    active?.blur?.();
    document.body.classList.remove('chat-pill-focused');
    return true;
  }

  /** Waits for viewport growth/settling with a bounded timeout before menu reveal. */
  function _waitForKeyboardDismiss() {
    const vv = window.visualViewport;
    const startHeight = _viewportHeight();
    let lastHeight = startHeight;
    let lastChangeAt = performance.now();
    // Whether the viewport has actually moved since the blur. False on desktop
    // (blurring an input never resizes the viewport) — lets the wait finish in
    // ~120ms instead of the full 450ms settle window when there is no keyboard.
    let sawResize = false;
    let finished = false;
    let frame = 0;
    let timeout = 0;

    return new Promise(resolve => {
      // Purpose: Stop polling and remove temporary listeners after keyboard dismissal or timeout.
      const finish = () => {
        if (finished) return;
        finished = true;
        cancelAnimationFrame(frame);
        clearTimeout(timeout);
        vv?.removeEventListener('resize', noteResize);
        window.removeEventListener('resize', noteResize);
        if (menu._deferredOpenCleanup === finish) menu._deferredOpenCleanup = null;
        requestAnimationFrame(() => requestAnimationFrame(resolve));
      };
      // Purpose: Record keyboard-induced viewport motion and its latest settling timestamp.
      const noteResize = () => {
        sawResize = true;
        lastHeight = _viewportHeight();
        lastChangeAt = performance.now();
      };
      // Purpose: Recheck viewport settling until keyboard dismissal is confirmed or the bounded wait expires.
      const check = now => {
        const height = _viewportHeight();
        if (Math.abs(height - lastHeight) > 1) {
          lastHeight = height;
          lastChangeAt = now;
          // A measured height change also counts as motion (covers browsers
          // that resize without firing a resize event).
          sawResize = true;
        }
        const viewportGrew = height > startHeight + 40;
        const quietFor = now - lastChangeAt;
        // Fast path (desktop / no keyboard): nothing moved within a short
        // grace window — there is no keyboard to wait for, open immediately.
        if (!sawResize && quietFor >= 120) { finish(); return; }
        // Keyboard collapse confirmed: wait until the viewport has grown AND
        // settled, or (motion without growth) the full settle window.
        if ((viewportGrew && quietFor >= 120) || (sawResize && quietFor >= 450)) {
          finish();
          return;
        }
        frame = requestAnimationFrame(check);
      };

      vv?.addEventListener('resize', noteResize);
      window.addEventListener('resize', noteResize);
      frame = requestAnimationFrame(check);
      timeout = setTimeout(finish, 2500);
      menu._deferredOpenCleanup = finish;
    });
  }

  // The menu must have lightweight, stable geometry before its opening
  // animation starts. Render only phantom rows here: cache reads and row
  // rendering wait until the opening transition has fully settled.
  // SESSIONS-SKELETON (breadcrumb) — three places paint session-list skeletons
  // and must stay in sync on row count + line-width ranges. THIS function is
  // the reference: 6–15 rows, each line 30–100% wide. The sessions table
  // mirrors it in ui/main-panel/agents/sessions/js/sessions-page.js
  // (_renderSkeletonRows — first content column fixed 500px) and the static
  // phantom rows in ui/main-panel/agents/agents.html (same classes + ranges).
  /** Paints stable phantom rows so opening geometry is ready before list loading. */
  function _renderSessionMenuSkeleton() {
    if (!menu) return;
    menu.replaceChildren();
    menu.dataset.loading = 'true';
    menu.setAttribute('aria-busy', 'true');
    // Randomized phantom rows — 6–15 rows, each line 30–100% wide — so the
    // skeleton reads as a real, ragged list of session titles instead of a
    // uniform, predictable stack. (Breadcrumb: see SESSIONS-SKELETON above.)
    const rowCount = 6 + Math.floor(Math.random() * 10); // 6–15 rows
    for (let i = 0; i < rowCount; i++) {
      const row = document.createElement('div');
      row.className = 'session-dropdown-loading-row';
      row.setAttribute('aria-hidden', 'true');
      const lineWidth = 30 + Math.floor(Math.random() * 71); // 30–100%
      row.innerHTML = `
        <span class="session-dropdown-loading-icon"></span>
        <span class="session-dropdown-loading-line" style="max-width: ${lineWidth}%"></span>
      `;
      menu.appendChild(row);
    }
  }

  /** Chooses the loading skeleton or anonymous empty state before revealing the menu. */
  function _prepareSessionMenuContent() {
    if (app.currentUserId) {
      _renderSessionMenuSkeleton();
    } else {
      menu.replaceChildren();
      delete menu.dataset.loading;
      menu.removeAttribute('aria-busy');
      const empty = document.createElement('div');
      empty.className = 'session-dropdown-empty';
      empty.textContent = 'No sessions yet';
      menu.appendChild(empty);
    }
  }

  // The menu is moved to document.body while open to escape clipped header
  // ancestors, so it no longer follows its trigger automatically. Keep this
  // geometry update separate from the open animation so a window or chat-panel
  // resize can re-anchor the already-open menu without restarting that animation.
  /** Reanchors the detached menu to its trigger and computes available viewport height. */
  function _positionOpenMenu() {
    if (!menu) return;

    const rect = dropdown.getBoundingClientRect();
    const viewport = window.visualViewport;
    const viewportTop = viewport?.offsetTop || 0;
    const viewportHeight = viewport?.height || window.innerHeight;
    const viewportBottom = viewportTop + viewportHeight;
    const spaceBelow = viewportBottom - rect.bottom;
    const spaceAbove = rect.top - viewportTop;
    const flipUp = spaceBelow < 180 && spaceAbove > spaceBelow;

    menu.style.top = flipUp ? 'auto' : rect.bottom + 'px';
    menu.style.bottom = flipUp ? (window.innerHeight - rect.top) + 'px' : 'auto';
    const viewportCap = Math.max(100, Math.floor(viewportHeight * 0.66));
    menu._availableHeight = Math.min(viewportCap, Math.max(100, Math.floor((flipUp ? spaceAbove : spaceBelow) - 8)));

    // Keep the detached menu flush with the session chip on every layout,
    // including mobile and while the chat panel is being dragged.
    menu.style.left = rect.left + 'px';
    menu.style.right = 'auto';
    menu.style.width = rect.width + 'px';
    menu.style.minWidth = '0';
    const ddStyle = getComputedStyle(dropdown);
    menu.style.maxWidth = ddStyle.maxWidth !== 'none' ? ddStyle.maxWidth : 'none';
    menu.style.transform = '';
    menu.classList.toggle('flip-up', flipUp);
    dropdown.classList.toggle('flip-up', flipUp);
  }

  /** Resets stale animation state, detaches the menu, positions it, and begins opening. */
  function _positionAndShowMenu() {
    if (!menu) return;
    clearTimeout(_closeAnimationTimer);
    clearTimeout(_openAnimationTimer);
    _closeAnimationTimer = 0;
    _openAnimationTimer = 0;
    menu._closeAnimation?.cancel();
    menu._closeAnimation = null;
    menu.style.maxHeight = '';
    menu.style.overflow = '';
    // Clear a stale transition lock (an interrupted close sets transition:none)
    // and any stale measured height so every open starts from a clean slate.
    menu.style.transition = '';
    menu.style.removeProperty('--session-menu-open-height');
    // Move to body so it escapes overflow-clipped ancestors
    for (const staleMenu of document.querySelectorAll('#session-dropdown-menu')) {
      if (staleMenu !== menu && staleMenu.parentNode === document.body && staleMenu.dataset.reactManaged !== 'true') staleMenu.remove();
    }
    if (menu.dataset.reactManaged !== 'true' && menu.parentNode !== document.body) document.body.appendChild(menu);

    menu.style.position = 'fixed';
    // Flush against the chip (no gap): when open the chip is the panel's
    // title row, so the menu attaches directly to its edge.
    _positionOpenMenu();

    // Now reveal. _positionOpenMenu() has already applied the correct
    // flip-up state to both the detached menu and its trigger.
    menu.hidden = false;
    menu.dataset.state = 'opening';
    menu.classList.remove('closing');
    dropdown.classList.add('open');
    _resetAllDeleteButtons();
  }

  /** Opens after any keyboard settle, then loads sessions only after animation completes. */
  async function openMenu() {
    if (!menu) return;
    const requestId = ++_openRequestId;
    const shouldWait = _keyboardDismissRequested || _dismissKeyboardBeforeOpen();
    _keyboardDismissRequested = false;
    if (shouldWait) {
      await _waitForKeyboardDismiss();
      if (requestId !== _openRequestId) return;
    }
    // Do this before revealing the menu. The following measurement can now
    // never observe an empty list while the async session fetch is pending.
    _prepareSessionMenuContent();
    _positionAndShowMenu();
    requestAnimationFrame(() => {
      if (requestId !== _openRequestId || menu.dataset.state !== 'opening') return;
      const availableHeight = Math.min(menu._availableHeight || menu.scrollHeight, parseFloat(getComputedStyle(menu).maxHeight) || Infinity);
      // The loading shell grows to the same final cap as the populated list.
      // It must not derive its opening height from the randomized skeleton.
      const targetHeight = availableHeight;
      menu.style.setProperty('--session-menu-open-height', `${targetHeight}px`);
      const rows = Array.from(menu.children);
      const stagger = rows.length ? Math.min(0.045, 0.3 / rows.length) : 0.035;
      rows.forEach((row, index) => {
        row.style.animationDelay = `${(index * stagger).toFixed(3)}s`;
      });
      // Match the tool-call disclosure: establish the collapsed frame, then
      // transition to the measured list height on the following frame.
      menu.offsetHeight;
      requestAnimationFrame(() => {
        if (requestId !== _openRequestId || menu.dataset.state !== 'opening') return;
        menu.classList.add('opening');
        menu.dataset.state = 'open';
        // DEBUG-TAG: drop-down has actually opened
        
        const totalMs = rows.length * stagger * 1000 + 180;
        _openAnimationTimer = setTimeout(() => {
          menu.classList.remove('opening');
          rows.forEach(row => { row.style.animationDelay = ''; });
          _openAnimationTimer = 0;
          // Defer the V3 refresh callback until the opening animation has settled.
          if (requestId === _openRequestId && menu.dataset.state === 'open') _loadSessionList();
        }, totalMs);
      });
    });
  }

  /** Brackets the supplied V3 refresh callback with sync indicators; the legacy cache result is dormant. */
  async function _loadSessionList() {
    if (!app.currentUserId) return;
    const userId = app.currentUserId;
    menu.dataset.syncing = 'true';
    menu.setAttribute('aria-busy', 'true');
    sessionDropdownStore.setLoadState('syncing');
    try {
      // V3's helper ignores the inherited cache arguments and delegates to panel.js refresh.
      // It returns false on success, so the preserved non-browser follow-up is unreachable here.
      const paintedFromCache = await populateSessionSelect(userId, { preferCache: true });
      if (paintedFromCache && !storageAdapter.isBrowser) {
        await populateSessionSelect(userId);
      }
    } finally {
      delete menu.dataset.syncing;
      menu.removeAttribute('aria-busy');
      sessionDropdownStore.setLoadState('idle');
    }
  }

  /** Finalizes closing state, reparents the menu, and clears temporary geometry. */
  function _finishMenuClose() {
    clearTimeout(_closeAnimationTimer);
    _closeAnimationTimer = 0;
    if (menu.dataset.state !== 'closing') return;
    menu.hidden = true;
    delete menu.dataset.state;
    menu.classList.remove('flip-up');
    // Return the menu to its original parent so next open's body-append works
    if (menu.dataset.reactManaged !== 'true' && menu.parentNode !== dropdown) {
      dropdown.appendChild(menu);
    }
    // Clear inline position styles so the next open recalculates fresh
    menu.style.position = '';
    menu.style.top = '';
    menu.style.left = '';
    menu.style.bottom = '';
    menu.style.transform = '';
    menu.style.width = '';
    menu.style.minWidth = '';
    menu.style.maxWidth = '';
    menu.style.maxHeight = '';
    menu.style.overflow = '';
    menu.style.transition = '';
    menu.style.removeProperty('--session-menu-open-height');
    menu._availableHeight = 0;
    menu.classList.remove('opening', 'closing');
    menu._closeAnimation?.cancel();
    menu._closeAnimation = null;
  }

  /** Cancels deferred opening/search work, animates shut, and resets transient controls. */
  function closeMenu({ immediate = false } = {}) {
    if (!menu) return;
    // A pending search-input debounce must not fire after the menu closes —
    // it would commit a half-typed query the user thought they abandoned.
    _cancelPendingSearchCommit();
    _openRequestId++;
    dropdown.classList.remove('open');
    dropdown.classList.remove('flip-up');
    // Clean up the keyboard-deferred open handler if it's pending
    menu._deferredOpenCleanup?.();
    if (!menu.hidden) {
      clearTimeout(_openAnimationTimer);
      _openAnimationTimer = 0;
      menu.classList.remove('opening');
      const rows = Array.from(menu.children);
      const stagger = rows.length ? Math.min(0.035, 0.18 / rows.length) : 0.025;
      // Lock the current rendered height before transitioning it to zero.
      menu.style.maxHeight = `${menu.getBoundingClientRect().height}px`;
      menu.style.overflow = 'hidden';
      menu.offsetHeight;
      rows.reverse().forEach((row, index) => {
        row.style.animationDelay = `${(index * stagger).toFixed(3)}s`;
      });
      menu.classList.add('closing');
      menu.dataset.state = 'closing';
      clearTimeout(_closeAnimationTimer);
      if (immediate) {
        _finishMenuClose();
      } else if (window.matchMedia?.('(prefers-reduced-motion: reduce)').matches) {
        _finishMenuClose();
      } else if (typeof menu.animate !== 'function') {
        requestAnimationFrame(() => {
          if (menu.dataset.state === 'closing') menu.style.maxHeight = '0px';
        });
        _closeAnimationTimer = setTimeout(_finishMenuClose, 380);
      } else {
        const startHeight = menu.getBoundingClientRect().height;
        const endTranslate = menu.classList.contains('flip-up') ? '4px' : '-4px';
        // Suppress CSS transitions so they don't fight the WAAPI animation.
        menu.style.transition = 'none';
        const cornerRadius = getComputedStyle(menu).borderTopLeftRadius || '14px';
        menu._closeAnimation = menu.animate([
          {
            maxHeight: `${startHeight}px`,
            clipPath: `inset(0 0 0 0 round ${cornerRadius})`,
            opacity: 1,
            transform: 'translateY(0)',
          },
          {
            maxHeight: '0px',
            clipPath: menu.classList.contains('flip-up')
              ? `inset(100% 0 0 0 round ${cornerRadius})`
              : `inset(0 0 100% 0 round ${cornerRadius})`,
            opacity: 0,
            transform: `translateY(${endTranslate})`,
          },
        ], {
          duration: 340,
          easing: 'cubic-bezier(.4, 0, .2, 1)',
          fill: 'forwards',
        });
        menu._closeAnimation.onfinish = _finishMenuClose;
        menu._closeAnimation.oncancel = () => {};
        // Fallback for embedded browsers that expose animate() but fail to
        // deliver the finish callback when the tab loses visibility.
        _closeAnimationTimer = setTimeout(_finishMenuClose, 450);
      }
    }
    _resetAllDeleteButtons();
    _closeRowActions();
    // Reset manage mode so the dropdown reopens in the normal (non-hidden) view.
    _setShowHidden(false);
    // Reset the footer search too — the dropdown must reopen with an empty
    // search field showing the full session list, never the previous results.
    _setSearchQuery('');
    _clearMsgSearch();
  }


 // Panel-owned document capture supplies feedback for this trigger, detached menu and row-action popup.

 /** Toggles open/closed unless a closing animation is still in progress. */
 function toggle(){if(!menu.hidden && menu.dataset.state!=='closing')closeMenu();else if(menu.dataset.state!=='closing')openMenu();}
 root.addEventListener('click',e=>{if(e.target.closest('button,input'))return;e.stopPropagation();toggle();},{signal:abort.signal});
 root.addEventListener('keydown',e=>{if(e.target.closest('button,input'))return;if(['Enter',' '].includes(e.key)){e.preventDefault();toggle();}},{signal:abort.signal});
 menu.addEventListener('click',e=>{e.stopPropagation();if(e.target.closest('button,input'))return;const row=e.target.closest('.session-row');if(row&&!row.classList.contains('recycled')){closeMenu();guard(()=>select(row.dataset.id))();}},{signal:abort.signal});
 menu.addEventListener('keydown',e=>{const row=e.target.closest('.session-row');if(row&&!row.classList.contains('recycled')&&!e.target.closest('button,input')&&['Enter',' '].includes(e.key)){e.preventDefault();closeMenu();guard(()=>select(row.dataset.id))();}},{signal:abort.signal});
 // Shared outside click/focus callback accounts for the menu being detached to body.
 const outside=e=>{if(!root.contains(e.target)&&!menu.contains(e.target)&&!popup?.contains(e.target))closeMenu();};
 document.addEventListener('click',outside,{signal:abort.signal});document.addEventListener('focusin',outside,{signal:abort.signal});
 document.addEventListener('keydown',e=>{if(e.key==='Escape'&&popup){_closeRowActions();return;}if(e.key==='Escape'&&!menu.hidden){closeMenu();root.querySelector('.session-dropdown-trigger').focus();}},{signal:abort.signal});
 // Resize callback reanchors an open detached menu without restarting its animation.
 const reposition=()=>{if(!menu.hidden)_positionOpenMenu();};
 window.addEventListener('resize',reposition,{signal:abort.signal});window.visualViewport?.addEventListener('resize',reposition,{signal:abort.signal});
 // Both end controls share the cap centers, half the border-box height from each edge.
 const avatar=root.querySelector('#session-dropdown-icon');
 const capActions=[...root.querySelectorAll('#session-dropdown-kebab, #session-dropdown-delete')];
 const insetProperties=['padding-left','padding-right'];
 const originalInsets=new Map(insetProperties.map(property=>[property,[root.style.getPropertyValue(property),root.style.getPropertyPriority(property)]]));
 const restoreCapInset=property=>{const original=originalInsets.get(property);if(original[0])root.style.setProperty(property,...original);else root.style.removeProperty(property);};
 function updateCapInsets(){
   if(!root.getClientRects().length){insetProperties.forEach(restoreCapInset);return;}
   const style=getComputedStyle(root);
   const height=parseFloat(style.height);
   if(!Number.isFinite(height))return;
   // Expanded corners end at the side tangents; straight borders continue to the menu.
   root.style.setProperty('--session-selector-cap-radius', `${height / 2}px`);
   // More owns the right cap unless an explicitly enabled Delete button follows it.
   const endAction=capActions.filter(node=>node.getClientRects().length).pop();
   for(const [property,node,borderProperty] of [['padding-left',avatar,'borderLeftWidth'],['padding-right',endAction,'borderRightWidth']]){
     if(!node?.getClientRects().length){restoreCapInset(property);continue;}
     const nodeStyle=getComputedStyle(node);
     let width=parseFloat(nodeStyle.width);
     if(nodeStyle.boxSizing!=='border-box')for(const key of ['paddingLeft','paddingRight','borderLeftWidth','borderRightWidth'])width+=parseFloat(nodeStyle[key])||0;
     if(!Number.isFinite(width))continue;
     const border=parseFloat(style[borderProperty])||0;
     const inset=`max(0px, calc((${height}px - ${width}px) / 2 - ${border}px))`;
     if(root.style.getPropertyValue(property)!==inset)root.style.setProperty(property,inset,'important');
   }
 }
 const observer=new ResizeObserver(()=>{updateCapInsets();reposition();});
 observer.observe(root);if(avatar)observer.observe(avatar);
 for(const node of capActions)observer.observe(node,{box:'border-box'});
 updateCapInsets();
 // Returned controller is the list renderer’s lifecycle/configuration bridge.
 // Expose menu lifecycle plus popup ownership to the sibling list renderer.
 /** Methods: open/close control the animated menu, isOpen reports visibility, setPopup transfers popup ownership to the list, and destroy invalidates pending opens and removes listeners. */
 const handle={open:openMenu,close:closeMenu,isOpen:()=>!menu.hidden,destroy(){_openRequestId++;menu._deferredOpenCleanup?.();closeMenu({immediate:true});abort.abort();_syncRowObserver.disconnect();observer.disconnect();insetProperties.forEach(restoreCapInset);root.style.removeProperty('--session-selector-cap-radius');clearTimeout(_openAnimationTimer);clearTimeout(_closeAnimationTimer);menu.remove();_closeRowActions();},setPopup(node){_closeRowActions();popup=node;}};
 root._sessionDropdownController=handle;return handle;
}
