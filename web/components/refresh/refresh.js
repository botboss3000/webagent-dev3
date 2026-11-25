// The shell's refresh action, ported from the app: one gesture with two stages —
// pull to refresh, pull further for a hard refresh that clears browser data and
// reloads past the HTTP cache — plus the header button that runs the hard refresh
// directly. Replaces the plain browser reload as the app's refresh action.

const PULL_NORMAL_AT = 0.5; // share of the viewport height that arms a refresh
const PULL_HARD_AT = 0.85; // share that arms the hard refresh
const DASH = 98; // arc length of the indicator path

/* The browser data this shell owns; a hard refresh clears exactly these keys. */
const OWNED_KEYS = ['webagent.web.theme', 'webagent.web.sidebar'];

export function clearBrowserData() {
  for (const key of OWNED_KEYS) {
    try {
      localStorage.removeItem(key);
    } catch {
      /* private mode: nothing stored to clear */
    }
  }
}

/* Hard refresh: drop the shell's browser data, then reload past the cache. */
export function hardRefresh() {
  clearBrowserData();
  const url = new URL(location.href);
  url.searchParams.set('_', String(Date.now()));
  location.replace(url);
}

/* Plain refresh: reload the page and keep browser data. */
export function refresh() {
  location.reload();
}

/* The header button runs the hard refresh and cannot be pressed twice. */
export function wireRefreshButton(button) {
  if (!button) return;
  button.addEventListener('click', () => {
    if (button.disabled) return;
    button.disabled = true;
    button.classList.add('spinning');
    hardRefresh();
  });
}

// Two-stage pull from the top of the viewport: half a screen refreshes, most of
// one clears browser data. Returns a teardown so a page can stop listening.
export function installPullToRefresh({ startZone = 50 } = {}) {
  const indicator = document.getElementById('ptr-indicator');
  const soft = document.getElementById('ptr-soft');
  const hard = document.getElementById('ptr-hard');
  const hint = document.getElementById('ptr-hint');
  if (!indicator || !soft || !hard || !hint) return () => {};

  let dragging = false;
  let startY = 0;
  let stage = 0;

  const hide = () => {
    indicator.hidden = true;
    indicator.style.height = '0px';
    indicator.classList.remove('loading');
    hint.textContent = '';
    hint.classList.remove('show');
  };

  const onStart = (event) => {
    if (dragging || indicator.classList.contains('loading') || !event.touches?.length) return;
    const touch = event.touches[0];
    // Only the top band arms the gesture; a pull anywhere else stays scrolling.
    if (touch.clientY < 0 || touch.clientY > startZone || window.scrollY > 5) return;
    dragging = true;
    startY = touch.clientY;
    stage = 0;
    indicator.hidden = false;
    indicator.style.height = '0px';
    soft.setAttribute('stroke-dashoffset', String(DASH));
    hard.style.display = 'none';
    hard.setAttribute('stroke-dashoffset', String(DASH));
    hint.textContent = '';
    hint.classList.remove('show');
  };

  const onMove = (event) => {
    if (!dragging || !event.touches?.length) return;
    // A second finger is a pinch, never a refresh.
    if (event.touches.length > 1) {
      dragging = false;
      stage = 0;
      hide();
      return;
    }
    const raw = event.touches[0].clientY - startY;
    if (raw <= 0) {
      hide();
      dragging = false;
      return;
    }
    const height = window.innerHeight;
    const normal = height * PULL_NORMAL_AT;
    const hardAt = height * PULL_HARD_AT;
    indicator.style.height = `${Math.min(raw * 0.4, height * 0.85)}px`;
    if (raw >= hardAt) {
      stage = 2;
      soft.setAttribute('stroke-dashoffset', '0');
      hard.style.display = '';
      hard.setAttribute('stroke-dashoffset', String(DASH * (1 - Math.min((raw - normal) / (hardAt - normal), 1))));
      hint.textContent = 'HARD REFRESH';
    } else if (raw >= normal) {
      stage = 1;
      soft.setAttribute('stroke-dashoffset', '0');
      hard.style.display = 'none';
      hint.textContent = 'RELEASE TO REFRESH';
    } else {
      stage = 0;
      soft.setAttribute('stroke-dashoffset', String(DASH * (1 - raw / normal)));
      hard.style.display = 'none';
      hint.textContent = 'PULL TO REFRESH';
    }
    hint.classList.add('show');
    if (event.cancelable) event.preventDefault();
  };

  const onEnd = () => {
    if (!dragging) return;
    const action = stage;
    dragging = false;
    startY = 0;
    stage = 0;
    if (!action) {
      hide();
      return;
    }
    indicator.classList.add('loading');
    hint.textContent = action === 2 ? 'CLEARING BROWSER DATA' : 'REFRESHING';
    hint.classList.add('show');
    // One frame of feedback before the page goes away.
    if (action === 1) setTimeout(refresh, 120);
    if (action === 2) setTimeout(hardRefresh, 120);
  };

  const onCancel = () => {
    dragging = false;
    stage = 0;
    hide();
  };

  document.addEventListener('touchstart', onStart, { passive: true });
  document.addEventListener('touchmove', onMove, { passive: false });
  document.addEventListener('touchend', onEnd, { passive: true });
  document.addEventListener('touchcancel', onCancel, { passive: true });

  return () => {
    document.removeEventListener('touchstart', onStart);
    document.removeEventListener('touchmove', onMove);
    document.removeEventListener('touchend', onEnd);
    document.removeEventListener('touchcancel', onCancel);
  };
}
