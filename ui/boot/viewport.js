// @code-index {"version":1,"feature":"mobile-keyboard-viewport","aliases":["keyboard dismissal","mobile keyboard","visible viewport"],"purpose":"Tracks viewport geometry and OS keyboard dismissal; only trusted editor interaction clears dismissal, and programmatic focus never clears it.","span_lines":80,"configuration":{"status":"not_applicable","rationale":"Focus preservation and honoring OS keyboard dismissal are interaction invariants rather than user preferences."}}
"use strict";
  // Track the visible viewport height so the app shrinks when the mobile
  // on-screen keyboard appears. dvh handles browser chrome but on Android
  // Chrome does not shrink for keyboards, so we drive --app-height from
  // window.visualViewport.height (falls back to innerHeight).
  //
  // We ALSO flag the mobile soft keyboard being open by toggling `body.kb-open`:
  // the keyboard is open when the visual viewport is meaningfully shorter than
  // the (constant) layout viewport AND an editable field holds focus. Some pages
  // hide bottom-docked clutter while it's up so the focused field isn't covered —
  // e.g. the App Config chat pill + sticky section headings (see app3.css
  // `body.kb-open` rules). Class is mobile-gated here, so desktop never sets it.
  (function () {
    var root = document.documentElement;
    var vv = window.visualViewport;
    function apply() {
      var h = (vv ? vv.height : window.innerHeight);
      applyComposerMotion(h);
    }
    var keyboardWasOpen = false;
    var keyboardDismissed = false, keyboardWidth = window.innerWidth;
    function applyKb() {
      if (!document.body) return;
      var isMobile = window.innerWidth <= 800;       // mirrors __isMobileChatLayout / the CSS breakpoint
      var vvh = (vv ? vv.height : window.innerHeight);
      var ae = document.activeElement;
      var editable = !!ae && (ae.tagName === 'INPUT' || ae.tagName === 'TEXTAREA' || ae.isContentEditable);
      // Keyboards are ~250px+; a 120px floor clears browser-chrome jitter.
      var open = isMobile && editable && (window.innerHeight - vvh) > 120;
      // The OS keyboard-dismiss control can leave the composer focused. Some
      // mobile browsers briefly report a shrunken viewport again during the
      // hide animation; latch that dismissal until the user re-engages an editor.
      if (keyboardWasOpen && !open && editable) {
        keyboardDismissed = true;
        // Match the v2 OS-dismiss flow: defer empty-editor blur until resize settles.
        // Do not collapse on rotation/zoom, blur a replacement editor, or discard a draft.
        if (isMobile && window.innerWidth === keyboardWidth && (!vv || vv.scale === 1)
            && ae.id === 'chat-input' && ae.value === '') requestAnimationFrame(function () {
          if (keyboardDismissed && document.activeElement === ae && ae.isConnected && ae.value === ''
              && window.innerWidth === keyboardWidth && (!vv || vv.scale === 1)
              && window.innerHeight - (vv ? vv.height : window.innerHeight) <= 120) ae.blur();
        });
      }
      keyboardWidth = window.innerWidth;
      keyboardWasOpen = open;
      open = open && !keyboardDismissed;
      document.body.classList.toggle('kb-open', open);
      // hide-header-on-keyboard threshold check. When the toggle is ON and
      // threshold=0 we always show the hidden state; with a positive threshold
      // we only hide when the remaining viewport height is below it.
      if (document.body.classList.contains('hide-header-kb')) {
        var raw = document.body.dataset.hhkThreshold;
        var thresh = parseInt(raw, 10);
        if (isNaN(thresh) || thresh < 0) thresh = 200;
        var below = thresh === 0 ? open : (open && vvh < thresh);
        document.body.classList.toggle('hhk-active', below);
      } else {
        document.body.classList.remove('hhk-active');
      }
    }
    function applyAll() { apply(); applyKb(); }
    window.__appHeightApply = apply;
    if (vv) {
      vv.addEventListener('resize', applyAll);
      vv.addEventListener('scroll', applyAll);
    }
    window.addEventListener('resize', applyAll);
    window.addEventListener('orientationchange', function () {
      setTimeout(applyAll, 120);
    });
    // rAF scroll-lock: fights iOS Safari auto-scroll during keyboard open.
    // On focus, forces scrollTo(0,0) every frame for 400ms — the keyboard
    // animation duration — so iOS can't overshoot the pill.
    var scrollLockRaf = null;
    function scrollLock() { window.scrollTo(0, 0); }
    function rAFScrollLock() {
      var start = Date.now();
      scrollLock();
      if (scrollLockRaf) cancelAnimationFrame(scrollLockRaf);
      function loop() {
        scrollLock();
        if (Date.now() - start < 400) {
          scrollLockRaf = requestAnimationFrame(loop);
        }
      }
      scrollLockRaf = requestAnimationFrame(loop);
    }
    function needsMobileKeyboardLock(target) {
      if (window.innerWidth > 800 || !target) return false;
      return target.tagName === 'INPUT'
        || target.tagName === 'TEXTAREA'
        || target.isContentEditable;
    }
    document.addEventListener('focusin', function (event) {
      apply();       // update --app-height NOW so iOS Safari's auto-scroll
      applyKb();     // doesn't overshoot before the rAF loop kicks in
      // This lock exists only for a mobile soft keyboard. Running it for every
      // desktop button focus starts a 400 ms scroll loop between mousedown and
      // click, which makes controls across the app appear to need two clicks.
      if (needsMobileKeyboardLock(event.target)) rAFScrollLock();
    });
    // Only deliberate editor engagement clears OS dismissal; programmatic focus does not.
    function engageEditor(event) {
      if (!event.isTrusted || !needsMobileKeyboardLock(event.target)) return;
      keyboardDismissed = false;
      applyKb();
    }
    document.addEventListener('pointerdown', engageEditor, true);
    document.addEventListener('keydown', engageEditor, true);
    document.addEventListener('beforeinput', engageEditor, true);
    document.addEventListener('input', engageEditor, true);
    document.addEventListener('focusout', function () { setTimeout(applyKb, 0); });

    // @code-index {"version":1,"feature":"composer-keyboard-motion","aliases":["composer movement","keyboard smoothing"],"purpose":"Keeps app viewport sizing immediate while composer/footer controls ease from their current rendered position to the new layout. Config: data/config/chat.json -> composer.keyboard_motion in merged profiles; no focus or opacity changes.","span_lines":70,"configuration":{"status":"configured","references":[{"path":"data/config/chat.json","pointers":["/chat_common/composer/keyboard_motion/enabled","/chat_common/composer/keyboard_motion/duration_ms"],"profile_overrides":true}],"opportunities":[]}}
    var motionSettings = {enabled: false, duration_ms: 140}, motionSignature = '';
    var motionEffects = [], motionHeight = vv ? vv.height : window.innerHeight;
    var motionWidth = window.innerWidth, motionScale = vv ? vv.scale : 1;
    var motionBlurredAt = -Infinity;
    var motionReduced = window.matchMedia('(prefers-reduced-motion: reduce)');
    /** Cancels only our translations, exposing the already-current layout without touching focus. */
    function cancelComposerMotion() {
      for (var effect of motionEffects) effect.cancel();
      motionEffects = [];
    }
    /** FLIP the visible controls: measure before cancellation, resize immediately, then ease to their new positions. */
    function applyComposerMotion(height) {
      var width = window.innerWidth, scale = vv ? vv.scale : 1;
      var input = document.getElementById('chat-input'), area = input?.closest('#chat-input-area');
      var eligible = motionSettings.enabled && motionSettings.duration_ms > 0 && !motionReduced.matches
        && width <= 800 && width === motionWidth && scale === 1 && motionScale === 1
        && area?.isConnected && !area.closest('.wa-chat-window') && !window.__CHAT_PORTAL__
        && (document.activeElement === input || performance.now() - motionBlurredAt < 700)
        && (Math.max(window.innerHeight - motionHeight, window.innerHeight - height) > 120 || Math.abs(height - motionHeight) > 120)
        && CSS.supports('translate', '0px 1px');
      if (height === motionHeight) {
        if (!eligible) cancelComposerMotion();
        root.style.setProperty('--app-height', height + 'px');
      } else {
        var controls = eligible ? ['#chat-input-row', '#chat-footer-row', '#chat-footer-handle']
          .map(function (selector) { return area.querySelector(selector); }).filter(Boolean) : [];
        // Read the current animated position before cancelling, so interruptions cannot jump backwards.
        var before = controls.map(function (node) { return node.getBoundingClientRect().top; });
        cancelComposerMotion(); root.style.setProperty('--app-height', height + 'px');
        controls.forEach(function (node, index) {
          var offset = before[index] - node.getBoundingClientRect().top;
          if (Math.abs(offset) < 0.5 || typeof node.animate !== 'function') return;
          // Add individual translation to existing CSS transforms; never overwrite drag/rotation styles.
          var effect = node.animate([{translate: '0px ' + offset + 'px'}, {translate: '0px 0px'}],
            {duration: motionSettings.duration_ms, easing: 'cubic-bezier(.2,.8,.2,1)', composite: 'add'});
          motionEffects.push(effect);
          effect.onfinish = function () { effect.cancel(); motionEffects = motionEffects.filter(function (current) { return current !== effect; }); };
        });
      }
      motionHeight = height; motionWidth = width; motionScale = scale;
    }
    /** Adopt bounded motion preferences from the merged chat profile on UI configuration reload. */
    window.__chatKeyboardMotionConfigure = function (raw) {
      raw = raw && typeof raw === 'object' ? raw : {};
      var duration = typeof raw.duration_ms === 'number' && Number.isFinite(raw.duration_ms) && raw.duration_ms >= 0 && raw.duration_ms <= 300 ? raw.duration_ms : 140;
      var next = {enabled: raw.enabled === true, duration_ms: duration}, signature = JSON.stringify(next);
      if (signature === motionSignature) return;
      cancelComposerMotion(); motionSettings = next; motionSignature = signature;
    };
    document.addEventListener('focusout', function (event) { if (event.target.id === 'chat-input') motionBlurredAt = performance.now(); });
    motionReduced.addEventListener('change', cancelComposerMotion);
    window.addEventListener('pagehide', cancelComposerMotion);
    applyAll();
  })();
