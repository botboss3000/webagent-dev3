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
      updateComposerFade(h);
      root.style.setProperty('--app-height', h + 'px');
    }
    var keyboardWasOpen = false;
    var keyboardDismissed = false;
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
      if (keyboardWasOpen && !open && editable) keyboardDismissed = true;
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

    // @code-index {"version":1,"feature":"composer-keyboard-fade","aliases":["composer fade","keyboard fade"],"purpose":"Fades only composer/footer controls during mobile keyboard-sized viewport changes. Config: data/config/chat.json -> chat_common.composer.keyboard_fade, with desktop/mobile/widget profile overrides; saved settings are adopted on UI config reload.","span_lines":80,"configuration":{"status":"configured","references":[{"path":"data/config/chat.json","pointers":["/chat_common/composer/keyboard_fade/enabled","/chat_common/composer/keyboard_fade/fade_out_ms","/chat_common/composer/keyboard_fade/fade_in_ms","/chat_common/composer/keyboard_fade/settle_ms"],"profile_overrides":true}],"opportunities":[]}}
    var fadeSettings = {enabled: false, fade_out_ms: 60, fade_in_ms: 120, settle_ms: 80};
    var fadeSignature = '', fadeAnimations = [], fadeArea = null;
    var fadeActive = false, fadeCooling = false, fadeSettleTimer = null, fadeDeadline = null;
    var fadeHeight = vv ? vv.height : window.innerHeight, fadeStableHeight = fadeHeight;
    var fadeWidth = window.innerWidth, fadeScale = vv ? vv.scale : 1;
    var fadeStarted = 0, composerBlurredAt = -Infinity;
    var fadeReducedMotion = window.matchMedia('(prefers-reduced-motion: reduce)');

    /** Cancels owned opacity effects; never hides, disconnects, blurs or refocuses the editor. */
    function cancelComposerFade() {
      clearTimeout(fadeSettleTimer); clearTimeout(fadeDeadline);
      for (var animation of fadeAnimations) animation.cancel();
      fadeAnimations = []; fadeArea?.removeAttribute('data-keyboard-fading');
      fadeArea = null; fadeActive = false; fadeCooling = false;
      fadeStableHeight = fadeHeight;
    }
    /** Returns only pill/footer nodes; notices and the transcript never receive opacity effects. */
    function composerFadeControls(area) {
      return ['#chat-input-row', '#chat-footer-row', '#chat-footer-handle']
        .map(function (selector) { return area.querySelector(selector); }).filter(Boolean);
    }
    /** Ends a motion burst and restores each control's current CSS opacity with a short fade-in. */
    function revealComposerFade(watchdog) {
      clearTimeout(fadeDeadline); fadeActive = false;
      fadeCooling = watchdog === true; fadeStableHeight = fadeHeight;
      if (!fadeArea?.isConnected) { cancelComposerFade(); return; }
      var controls = composerFadeControls(fadeArea);
      var current = controls.map(function (node) { return getComputedStyle(node).opacity; });
      for (var animation of fadeAnimations) animation.cancel();
      fadeAnimations = []; fadeArea.removeAttribute('data-keyboard-fading');
      controls.forEach(function (node, index) {
        var target = getComputedStyle(node).opacity;
        if (!fadeSettings.fade_in_ms || current[index] === target) return;
        var effect = node.animate([{opacity: current[index]}, {opacity: target}], {duration: fadeSettings.fade_in_ms, easing: 'ease-out'});
        effect.onfinish = function () { effect.cancel(); };
        fadeAnimations.push(effect);
      });
    }
    /** Holds one fade across resize bursts, with a fixed watchdog so noisy events cannot leave controls faded. */
    function beginComposerFade(area) {
      var controls = composerFadeControls(area);
      if (!controls.length || controls.some(function (node) { return typeof node.animate !== 'function'; })) return;
      var current = controls.map(function (node) { return getComputedStyle(node).opacity; });
      cancelComposerFade(); fadeArea = area; fadeActive = true; fadeStarted = performance.now();
      area.setAttribute('data-keyboard-fading', '');
      controls.forEach(function (node, index) {
        fadeAnimations.push(node.animate([{opacity: current[index]}, {opacity: 0}], {duration: fadeSettings.fade_out_ms, easing: 'ease-in', fill: 'forwards'}));
      });
      // This recovery bound is an invariant, not a user-tunable delay.
      fadeDeadline = setTimeout(function () { revealComposerFade(true); }, 700);
    }
    /** Detects cumulative height changes; rotation, zoom, desktop and browser-chrome jitter bypass the fade. */
    function updateComposerFade(height) {
      var width = window.innerWidth, scale = vv ? vv.scale : 1;
      var input = document.getElementById('chat-input');
      var area = input?.closest('#chat-input-area');
      var eligible = fadeSettings.enabled && !fadeReducedMotion.matches && width <= 800
        && width === fadeWidth && scale === 1 && fadeScale === 1
        && area?.isConnected && !area.closest('.wa-chat-window');
      if (!eligible || fadeArea && fadeArea !== area) {
        cancelComposerFade(); fadeStableHeight = height;
      } else if (height !== fadeHeight) {
        var composerEngaged = document.activeElement === input || performance.now() - composerBlurredAt < 700;
        if (!fadeCooling && !fadeActive && composerEngaged && Math.abs(height - fadeStableHeight) > 120) beginComposerFade(area);
        clearTimeout(fadeSettleTimer);
        var remainingOut = fadeActive ? Math.max(0, fadeSettings.fade_out_ms - (performance.now() - fadeStarted)) : 0;
        fadeSettleTimer = setTimeout(function () {
          if (fadeActive) revealComposerFade(false);
          fadeStableHeight = fadeHeight; fadeCooling = false;
        }, Math.max(fadeSettings.settle_ms, remainingOut));
      }
      fadeHeight = height; fadeWidth = width; fadeScale = scale;
    }
    /** Receives the merged chat profile from chat-config; invalid durations fall back within bounded ranges. */
    window.__chatKeyboardFadeConfigure = function (raw) {
      raw = raw && typeof raw === 'object' ? raw : {};
      function duration(key, fallback, maximum) {
        return typeof raw[key] === 'number' && Number.isFinite(raw[key]) && raw[key] >= 0 && raw[key] <= maximum ? raw[key] : fallback;
      }
      var next = {enabled: raw.enabled === true, fade_out_ms: duration('fade_out_ms', 60, 300),
        fade_in_ms: duration('fade_in_ms', 120, 400), settle_ms: duration('settle_ms', 80, 250)};
      var signature = JSON.stringify(next);
      if (signature === fadeSignature) return;
      cancelComposerFade(); fadeSettings = next; fadeSignature = signature;
    };
    document.addEventListener('focusout', function (event) {
      if (event.target.id === 'chat-input') composerBlurredAt = performance.now();
    });
    fadeReducedMotion.addEventListener('change', cancelComposerFade);
    window.addEventListener('pagehide', cancelComposerFade);
    applyAll();
  })();
