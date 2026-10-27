"use strict";
// Reuse the v2 renderer unchanged; the v3 scene owns its boot-only lifetime.
window.WA_BG?.whenReady('bullet-grid', function (renderer) {
  var canvas = document.querySelector('#boot-curtain canvas');
  if (canvas) renderer.start(canvas);
});
  (function () {
    var curtainDone = false;
    var appReadyDone = false;
    var curtain = document.getElementById('boot-curtain');
    function remove() {
      if (curtain && curtain.parentNode) curtain.parentNode.removeChild(curtain);
      curtain = null;
    }
    function liftCurtain() {
      if (curtainDone) return;
      curtainDone = true;
      if (!curtain) return;
      // Release the app under the curtain, then fade the black scene out over it.
      document.body.classList.remove('is-booting');
      var finished = false;
      function finish() {
        if (finished) return;
        finished = true;
        window.WA_BG?._get('bullet-grid')?.stop();
        remove();
        window.dispatchEvent(new CustomEvent('wa-boot-pointerend'));
      }
      if (window.matchMedia && window.matchMedia('(prefers-reduced-motion: reduce)').matches) { finish(); return; }
      var lifting = curtain;
      // The grid's own earlier fade bubbles here too; finish only on the curtain's.
      lifting.addEventListener('transitionend', function (e) { if (e.target === lifting) finish(); });
      // transitionend never fires in a hidden tab; cap the wait just past the 0.2s grid fade + 0.5s fade.
      setTimeout(finish, 900);
      curtain.classList.add('is-lifting');
    }
    function announceAppReady() {
      if (appReadyDone) return;
      appReadyDone = true;
      // Announce readiness once; late listeners can inspect the revealed flag.
      if (window.__boot) window.__boot.revealed = true;
      try { window.dispatchEvent(new Event('webagent-app-ready')); } catch (_) {}
    }
    function reveal() {
      announceAppReady();
      liftCurtain();
    }
    window.__boot = {
      _splashPending: false,
      revealed:       false,
      // The minimum shell is useful before catalogs, partials, or
      // ui-config finish. Lift only the curtain; appReady() owns readiness.
      shellReady:     liftCurtain,
      splashWillShow: function () { this._splashPending = true; },
      splashReady:    function () { reveal(); },
      appReady:       function () { if (!this._splashPending) reveal(); },
      reveal:         reveal,
    };
    // A timer cannot establish readiness. Keep the animation while the app is
    // still hidden; revealing it then would expose only an empty black page.
    setTimeout(() => {
      if (document.getElementById('app')?.hidden === false) liftCurtain();
    }, 10000);
  })();

window.addEventListener("error", () => window.__boot.reveal());
window.addEventListener("unhandledrejection", () => window.__boot.reveal());
try { document.body.classList.toggle("light-mode", localStorage.getItem("v3-theme") === "light"); } catch {}
// Keep the browser backing surface opaque and equal to the active body palette.
(function () {
  var root = document.documentElement;
  function syncCanvasBackground() {
    var color = getComputedStyle(document.body).backgroundColor;
    if (root.style.getPropertyValue('--app-canvas-bg') !== color) root.style.setProperty('--app-canvas-bg', color);
  }
  var paletteObserver = new MutationObserver(syncCanvasBackground);
  paletteObserver.observe(document.body, {attributes: true, attributeFilter: ['class', 'style']});
  paletteObserver.observe(root, {attributes: true, attributeFilter: ['class', 'style']});
  window.addEventListener('resize', syncCanvasBackground);
  window.visualViewport?.addEventListener('resize', syncCanvasBackground);
  document.addEventListener('load', function (event) { if (event.target.tagName === 'LINK') syncCanvasBackground(); }, true);
  syncCanvasBackground();
})();
  // ── Boot phantom cursor ─────────────────────────────────────────────
  // While the app boots (body.is-booting, set by the pre-paint script above
  // and cleared by the readiness coordinator), a boot input orbits
  // the page and drives the animated background through its own event channel.
  // Backgrounds therefore receive TWO stable inputs during load: the user's
  // real pointer and this orbit. The orbit no longer impersonates pointermove,
  // so moving the real mouse cannot make the two sources overwrite each other.
  // It stops only when boot finishes
  // (is-booting cleared AND the curtain removed from the DOM). Skipped under
  // prefers-reduced-motion — the backgrounds are display:none there anyway.
  (function () {
    if (window.matchMedia && window.matchMedia('(prefers-reduced-motion: reduce)').matches) return;
    var running = false, rafId = 0, lastEmit = 0, lastFrame = 0;
    var x = 0, y = 0;                 // current orbit point (viewport px)
    var angle = -Math.PI / 2;         // begin at the top of the circle
    var W = 0, H = 0;

    // Still booting? True while the pre-paint boot flag is set OR the boot
    // curtain is still in the DOM (both are removed synchronously at reveal).
    // This covers both orderings of "tab content mounted" vs "curtain lifts",
    // so the background is never static the moment the app becomes visible.
    function bootActive() {
      return !!document.body && (
        document.body.classList.contains('is-booting') ||
        !!document.getElementById('boot-curtain')
      );
    }

    function emit() {
      try {
        window.dispatchEvent(new CustomEvent('wa-boot-pointermove', {
          detail: { clientX: x, clientY: y }
        }));
      } catch (_) {}
    }

    function step(now) {
      if (!running) return;
      // Boot finished (is-booting cleared AND the curtain removed) → stop.
      if (!bootActive()) { stop(); return; }
      W = window.innerWidth; H = window.innerHeight;
      if (!lastFrame) lastFrame = now;

      // One broad, steady orbit every seven seconds. The radius is based on
      // the shorter viewport edge so the path stays circular (not elliptical)
      // at every aspect ratio, while running close enough to the perimeter to
      // repeatedly brush the dots/particles and trigger their normal response.
      var elapsed = Math.min(64, now - lastFrame);
      lastFrame = now;
      angle = (angle + elapsed * ((Math.PI * 2) / 7000)) % (Math.PI * 2);
      var radius = Math.max(48, Math.min(W, H) * 0.42);
      x = W * 0.5 + Math.cos(angle) * radius;
      y = H * 0.5 + Math.sin(angle) * radius;

      // ~30 synthetic moves/s is smooth enough to visibly "touch" the
      // background artifacts without making their particle emitters noisy.
      if (now - lastEmit > 33) { emit(); lastEmit = now; }
      rafId = requestAnimationFrame(step);
    }

    function start() {
      if (running) return;
      running = true;
      W = window.innerWidth; H = window.innerHeight;
      x = W * 0.5; y = H * 0.5 - Math.min(W, H) * 0.42;
      lastFrame = 0;
      rafId = requestAnimationFrame(step);
    }

    function stop() {
      running = false;
      if (rafId) { cancelAnimationFrame(rafId); rafId = 0; }
      try { window.dispatchEvent(new CustomEvent('wa-boot-pointerend')); } catch (_) {}
    }

    if (bootActive()) start();
  })();
