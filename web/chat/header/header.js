// Builds the chat header: three rows of three cells filled from
// config/web/chat/chat-header.json. A control the config does not name is not rendered,
// and a row with no controls takes no space. Profiles are the same four blocks as the
// composer, so a control can appear in one view and not in another.

const CONFIG = '/config/web/chat/chat-header.json';

const CONTROLS = {
  sidebar: () => import('../controls/sidebar/sidebar.js'),
};

const ROWS = 3;
const CELLS = ['left', 'center', 'right'];

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

function headerProfile(config) {
  const common = config?.common || {};
  const narrow = window.matchMedia('(max-width: 800px)').matches;
  return merge(common, config?.[narrow ? 'mobile' : 'desktop'] || {});
}

async function control(id) {
  const load = CONTROLS[id];
  if (!load) return null;
  try {
    const module = await load();
    return module.create ? module.create() : null;
  } catch {
    return null;
  }
}

/* Applies the configured control size: one value for the header, and a control's own value
   for that button only. Custom properties inherit, so an override needs one line, no rule. */
function applyControlSizes(grid, profile) {
  const controls = profile?.controls || {};
  const shared = controls.default?.size_px;
  if (typeof shared === 'number' && shared > 0) {
    grid.style.setProperty('--control-size', `${shared}px`);
  }
  for (const [id, settings] of Object.entries(controls)) {
    if (id === 'default') continue;
    const size = settings?.size_px;
    if (typeof size !== 'number' || size <= 0) continue;
    const element = grid.querySelector(`[data-chat-control="${id}"]`);
    if (element) element.style.setProperty('--control-size', `${size}px`);
  }
}

/* Mounts the header into the region the chat partial provides (.chat-panel-head). */
export async function mountChatHeader(host) {
  if (!host || host.dataset.mounted === 'yes') return host;
  const profile = headerProfile(await loadConfig());
  const rows = profile.rows || [];

  const grid = document.createElement('div');
  grid.className = 'chat-head-grid';

  for (let index = 0; index < ROWS; index += 1) {
    const spec = rows[index] || {};
    const row = document.createElement('div');
    row.className = 'chat-head-row';
    row.dataset.chatHeadRow = String(index);
    for (const cellName of CELLS) {
      const cell = document.createElement('div');
      cell.className = `chat-head-cell chat-head-cell-${cellName}`;
      cell.dataset.chatHeadCell = cellName;
      for (const id of spec[cellName] || []) {
        const settings = profile?.controls?.[id] || {};
        const element = await control(id, settings);
        if (element) {
          element.dataset.chatControl = id;
          cell.append(element);
        }
      }
      row.append(cell);
    }
    if (row.querySelector('button')) grid.append(row);
  }

  applyControlSizes(grid, profile);
  host.replaceChildren(grid);
  host.dataset.mounted = 'yes';
  return host;
}
