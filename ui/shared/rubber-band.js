"use strict";
// Rubber-band overscroll for a horizontal tab carousel (V2 rubber-band.js feel).
// Matches the footer handle's spring curve. Intercepts wheel + touch at
// scroll boundaries and applies a diminishing translateX that snaps back
// on release. CSS overscroll-behavior-x: none prevents native bounce from
// competing on macOS trackpad.
export function applyRubberBand(tabBar) {
  var _rbX = 0, _rbTimer = null;
  var MAX_PULL = 60, RESIST = 0.35;
  var SPRING = 'cubic-bezier(0.25, 0.46, 0.45, 0.94)';
  var _touchLastX = 0, _touchActive = false;
  var _snapping = false;
  var _snapTimeout = null;

  function _snap() {
    if (Math.abs(_rbX) < 0.5) return;
    _snapping = true;
    tabBar.style.transition = 'transform 0.45s ' + SPRING;
    _rbX = 0;
    tabBar.style.transform = '';
    clearTimeout(_rbTimer);
    _rbTimer = setTimeout(function () {
      tabBar.style.transition = '';
    }, 500);
    // Windows precision trackpad momentum can last 2+ s — keep ALL
    // wheel events suppressed for long enough that the tail is gone
    // before the rubber band re-arms. Otherwise residual momentum
    // triggers a second, weaker band after the first snap ends.
    clearTimeout(_snapTimeout);
    _snapTimeout = setTimeout(function () { _snapping = false; }, 2500);
  }

  function _pull(dx) {
    if (_snapping) return false;
    clearTimeout(_rbTimer);
    tabBar.style.transition = 'none';
    var maxS = tabBar.scrollWidth - tabBar.clientWidth;
    var atL = tabBar.scrollLeft <= 0 && dx < 0;
    var atR = tabBar.scrollLeft >= maxS - 0.5 && dx > 0;
    if (!atL && !atR) {
      if (Math.abs(_rbX) > 0.5) _snap();
      return false;
    }
    _rbX -= dx * RESIST;
    _rbX = Math.max(-MAX_PULL, Math.min(MAX_PULL, _rbX));
    tabBar.style.transform = 'translateX(' + _rbX + 'px)';
    _rbTimer = setTimeout(_snap, 120);
    return true;
  }

  // Wheel / trackpad — uses the dominant axis so a mouse wheel (which
  // produces deltaY, not deltaX) also scrolls the tab strip horizontally.
  // Boundary rubber band still fires; mid-strip, manual scrollLeft keeps
  // the page from scrolling vertically.
  tabBar.addEventListener('wheel', function (e) {
    // Pick the axis with the larger magnitude — trackpad → deltaX,
    // mouse wheel → deltaY.
    var dx = Math.abs(e.deltaX) > Math.abs(e.deltaY) ? e.deltaX : e.deltaY;
    if (!dx) return;
    var maxS = tabBar.scrollWidth - tabBar.clientWidth;
    if (_snapping) {
      if ((tabBar.scrollLeft <= 0 && dx < 0) || (tabBar.scrollLeft >= maxS - 0.5 && dx > 0)) {
        e.preventDefault();
      }
      return;
    }
    if (_pull(dx)) { e.preventDefault(); return; }
    // Not at a boundary — scroll the strip ourselves so deltaY from a
    // physical wheel doesn't scroll the page instead.
    tabBar.scrollBy({ left: dx, behavior: 'instant' });  // instant: strips may set scroll-behavior:smooth
    e.preventDefault();
  }, { passive: false });

  // Touch — track inter-frame delta past the boundary
  tabBar.addEventListener('touchstart', function (e) {
    var t = e.touches[0];
    if (!t) return;
    _touchLastX = t.clientX;
    _touchActive = true;
  }, { passive: true });

  tabBar.addEventListener('touchmove', function (e) {
    if (!_touchActive || tabBar.querySelector('.dragging')) return;  // a tab reorder owns the touch
    var t = e.touches[0];
    if (!t) return;
    var dx = _touchLastX - t.clientX;  // positive = swipe left
    _touchLastX = t.clientX;
    var maxS = tabBar.scrollWidth - tabBar.clientWidth;
    var atL = tabBar.scrollLeft <= 0 && dx < 0;
    var atR = tabBar.scrollLeft >= maxS - 0.5 && dx > 0;
    if (atL || atR) {
      clearTimeout(_rbTimer);
      tabBar.style.transition = 'none';
      _rbX -= dx * RESIST;
      _rbX = Math.max(-MAX_PULL, Math.min(MAX_PULL, _rbX));
      tabBar.style.transform = 'translateX(' + _rbX + 'px)';
      _rbTimer = setTimeout(_snap, 120);
      e.preventDefault();  // suppress native overscroll
    } else if (Math.abs(_rbX) > 0.5) {
      _snap();
    }
  }, { passive: false });

  function _touchEnd() {
    _touchActive = false;
    if (Math.abs(_rbX) > 0.5) _snap();
  }
  tabBar.addEventListener('touchend', _touchEnd, { passive: true });
  tabBar.addEventListener('touchcancel', _touchEnd, { passive: true });
}
