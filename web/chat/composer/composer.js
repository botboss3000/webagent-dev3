// Builds the composer grid: three rows of three cells, each cell listing control ids
// that come from config/web/chat/chat-footer.json. A control the config does not name
// is not rendered, and a row with no controls takes no space.

const CONFIG = '/config/web/chat/chat-footer.json';

const CONTROLS = {
  input: () => import('../controls/input/input.js'),
  attach: () => import('../controls/attach/attach.js'),
  stop: () => import('../controls/stop/stop.js'),
  mic: () => import('../controls/mic/mic.js'),
  send: () => import('../controls/send/send.js'),
};

const ROWS = 3;
const CELLS = ['left', 'center', 'right'];

// The profile the composer mounted with, reused for state changes: a focus switch must not
// wait on another config fetch, or the bar lags behind the keyboard.
let mountedProfile = null;

/* Objects merge, arrays replace, null inherits — the rule the config documents. */
function merge(base, override) {
  if (override === null || override === undefined) return base;
  if (Array.isArray(override)) return override;
  if (typeof override !== 'object') return override;
  const merged = { ...(base || {}) };
  for (const [key, value] of Object.entries(override)) {
    merged[key] = merge(base?.[key], value);
  }
  return merged;
}

async function loadConfig() {
  try {
    const response = await fetch(CONFIG, { headers: { accept: 'application/json' } });
    if (!response.ok) return null;
    return await response.json();
  } catch {
    return null;
  }
}

function composerProfile(config) {
  const common = config?.common || {};
  const narrow = window.matchMedia('(max-width: 800px)').matches;
  return merge(common, config?.[narrow ? 'mobile' : 'desktop'] || {});
}

/* Mounts one control with the settings config gives it. A control switched off in config
   is not rendered, so a slot can be retired without editing the rows. */
async function control(id, settings = {}) {
  if (settings.enabled === false) return null;
  const load = CONTROLS[id];
  if (!load) return null;
  try {
    const module = await load();
    return module.create ? module.create(settings) : null;
  } catch {
    return null;
  }
}

/* The value for this device: the coarse key on a finger, the plain key otherwise. A missing
   coarse value falls back to the token, never to the configured fine number, so a phone can
   not silently drop under the 44px touch minimum. */
function pointerValue(settings, key) {
  if (!settings) return null;
  const coarse = window.matchMedia('(pointer: coarse)').matches;
  const value = coarse ? settings[`${key}_coarse`] : settings[key];
  return typeof value === 'number' && value > 0 ? value : null;
}

/* Applies the configured control size: the surface scale first, then a control's own size and
   hit target for that button only. Custom properties inherit, so an override is one line. */
function applyControlSizes(surface, profile) {
  const controls = profile?.controls || {};
  const shared = pointerValue(profile?.scale, 'control_px') ?? pointerValue(controls.default, 'size_px');
  if (shared) surface.style.setProperty('--control-size', `${shared}px`);
  for (const [id, settings] of Object.entries(controls)) {
    if (id === 'default') continue;
    const element = surface.querySelector(`.composer-btn-${id}`);
    if (!element) continue;
    const size = pointerValue(settings, 'size_px');
    if (size) element.style.setProperty('--control-size', `${size}px`);
    const hit = pointerValue(settings, 'hit_px');
    if (hit) element.style.setProperty('--hit-target-min', `${hit}px`);
  }
}

/* Sizes the bar from its own controls and applies the surface settings the config names.
   There are no fallbacks: an absent height is the tallest control (0 when there is none),
   an absent radius is half the height, and absent padding means no padding at all. */
function applySurface(surface, profile) {
  const spec = profile?.surface || {};
  const controls = [...surface.querySelectorAll('.composer-btn, .composer-input')];
  const tallest = controls.reduce((max, el) => Math.max(max, el.getBoundingClientRect().height), 0);
  // Rows stack, so the bar must also cover the grid it holds: the tallest single control
  // is only the floor for a one-row bar, not for a bar with a field above its buttons.
  const grid = surface.querySelector('.composer-grid');
  const stacked = grid ? grid.getBoundingClientRect().height : 0;
  const height = spec.height_px ?? Math.max(tallest, stacked);
  surface.style.setProperty('--composer-height', `${height}px`);
  surface.style.setProperty('--composer-radius', `${spec.radius_px ?? height / 2}px`);
  const pad = spec.padding_px;
  surface.style.padding = Array.isArray(pad)
    ? pad.map((value) => `${value}px`).join(' ')
    : pad == null
      ? ''
      : `${pad}px`;
  surface.classList.toggle('is-transparent', spec.background === 'transparent');
}

/* Rows are keyed by number, so a profile can override one row and inherit the rest. */
function rowSpecs(rows) {
  if (!rows || typeof rows !== 'object') return [];
  return Object.keys(rows)
    .sort((a, b) => Number(a) - Number(b))
    .map((key) => rows[key] || {});
}

/* Renders one state (active or idle) into a fresh bar: the surface plus its grid. */
async function renderComposer(host, profile, state) {
  const rows = rowSpecs(profile?.[state]?.rows);
  const grid = document.createElement('div');
  grid.className = 'composer-grid';
  grid.dataset.composerState = state;

  for (let index = 0; index < Math.max(ROWS, rows.length); index += 1) {
    const spec = rows[index] || {};
    const row = document.createElement('div');
    row.className = 'composer-row';
    row.dataset.composerRow = String(index + 1);
    let filled = 0;
    for (const cellName of CELLS) {
      const cell = document.createElement('div');
      cell.className = `composer-cell composer-cell-${cellName}`;
      cell.dataset.composerCell = cellName;
      for (const id of spec[cellName] || []) {
        const element = await control(id, profile?.controls?.[id] || {});
        if (element) {
          cell.append(element);
          filled += 1;
        }
      }
      row.append(cell);
    }
    if (filled > 0) grid.append(row);
  }

  const surface = document.createElement('div');
  surface.className = 'composer-surface';
  surface.append(grid);
  applyControlSizes(surface, profile);
  host.replaceChildren(surface);
  applySurface(surface, profile);
  return { surface, grid };
}

/* Keeps what the field holds, where its caret sits and whether it owns focus, so a state
   change can rebuild the grid without interrupting typing. */
function snapshotField(host) {
  const field = host.querySelector('.composer-input');
  if (!field) return null;
  return {
    value: field.value,
    start: field.selectionStart,
    end: field.selectionEnd,
    focused: document.activeElement === field,
  };
}

function restoreField(host, saved) {
  if (!saved) return;
  const field = host.querySelector('.composer-input');
  if (!field) return;
  field.value = saved.value;
  if (typeof saved.start === 'number' && typeof field.setSelectionRange === 'function') {
    try {
      field.setSelectionRange(saved.start, saved.end);
    } catch {
      // A field that cannot take a selection keeps its value; the caret lands at the end.
    }
  }
  if (saved.focused) field.focus({ preventScroll: true });
}

/* Switches the composer between idle and active without losing the draft, caret or focus. */
export async function applyComposerState(host, state) {
  if (!host) return null;
  const next = state === 'idle' ? 'idle' : 'active';
  if (host.dataset.composerState === next) return null;
  const saved = snapshotField(host);
  const mounted = await setComposerState(host, next);
  restoreField(host, saved);
  return mounted;
}

/* Idle while the field is empty and unfocused; active once it holds text or owns focus. */
function wireComposerState(host) {
  const sync = () => {
    const field = host.querySelector('.composer-input');
    if (!field) return;
    const engaged = document.activeElement === field || field.value.trim() !== '';
    void applyComposerState(host, engaged ? 'active' : 'idle');
  };
  host.addEventListener('focusin', sync);
  host.addEventListener('focusout', () => setTimeout(sync, 0));
  host.addEventListener('input', sync);
}

/* Mounts the composer into the region the chat partial provides (#composer). */
export async function mountComposer(host) {
  if (!host) return null;
  const config = await loadConfig();
  const profile = composerProfile(config);
  // An untouched composer starts idle: one row, the field beside its buttons. A mount whose
  // config carries no idle shape stays active, because idle would render nothing at all.
  const requested = host.dataset.composerState;
  const state =
    requested === 'active' || requested === 'idle'
      ? requested
      : profile?.idle?.rows
        ? 'idle'
        : 'active';
  const mounted = await renderComposer(host, profile, state);
  mountedProfile = profile;
  host.dataset.composerState = state;
  host.dataset.mounted = 'yes';
  wireComposerState(host);
  return { ...mounted, profile };
}

/* Switches between the active and idle geometry from the same config. The mount's profile is
   reused so the change lands on the next frame instead of after a config round trip. */
export async function setComposerState(host, state) {
  if (!host) return null;
  const profile = mountedProfile || composerProfile(await loadConfig());
  const next = state === 'idle' ? 'idle' : 'active';
  const mounted = await renderComposer(host, profile, next);
  // Written after the grid exists, so the dataset never claims a state the DOM has not reached.
  host.dataset.composerState = next;
  return mounted;
}
