// Shell behaviour: theme, sidebar, phone drawer and the hash router.
// Plain modules, no build step. Admin pages are imported only when an
// administrator opens them, so a signed-out visitor never fetches that folder.
//
// The chat column is gone: this release is the frame only. Components are
// ported back one at a time, so nothing mounts into #page-body by itself.

import { icon } from '../components/icon/icon.js';
import { installPullToRefresh, wireRefreshButton } from '../components/refresh/refresh.js';

// The phone header switch shows a chat bubble on the panel and a back chevron on chat.
const CHAT_ICON =
  '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M20 11.5a8 8 0 0 1-8.2 8 8.6 8.6 0 0 1-3.5-.7L4 20l1.3-4a8 8 0 0 1-1.3-4.4A8 8 0 0 1 12 3.5a8 8 0 0 1 8 8z"/></svg>';
const BACK_ICON =
  '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" aria-hidden="true"><path d="M15 6l-6 6 6 6"/></svg>';

const THEME_KEY = 'webagent.web.theme';
const COLLAPSE_KEY = 'webagent.web.sidebar';

const state = {
  me: null,
  agents: [],
  route: '',
  pages: new Map(),
};

/* ── Theme: OS default, with a remembered per-browser override ── */
function applyTheme() {
  const saved = localStorage.getItem(THEME_KEY);
  const prefersLight = window.matchMedia('(prefers-color-scheme: light)').matches;
  const light = saved ? saved === 'light' : prefersLight;
  document.body.classList.toggle('light-mode', light);
  const button = document.getElementById('theme-toggle');
  if (button) button.setAttribute('aria-pressed', String(light));
}

function toggleTheme() {
  const light = !document.body.classList.contains('light-mode');
  localStorage.setItem(THEME_KEY, light ? 'light' : 'dark');
  applyTheme();
}

/* ── Sidebar ── */
function applyCollapse() {
  const collapsed = localStorage.getItem(COLLAPSE_KEY) === 'collapsed';
  document.body.classList.toggle('sidebar-collapsed', collapsed);
  const button = document.getElementById('sidebar-toggle');
  if (button) button.setAttribute('aria-expanded', String(!collapsed));
}

function toggleCollapse() {
  const collapsed = !document.body.classList.contains('sidebar-collapsed');
  localStorage.setItem(COLLAPSE_KEY, collapsed ? 'collapsed' : 'expanded');
  applyCollapse();
}

function closeMenu() {
  document.body.classList.remove('menu-open');
  const open = document.getElementById('menu-open');
  if (open) open.setAttribute('aria-expanded', 'false');
}

/* ── Data ── */
async function loadMe() {
  try {
    const response = await fetch('/api/v3/auth/me', { headers: { accept: 'application/json' } });
    if (!response.ok) return { is_admin: false };
    return await response.json();
  } catch {
    return { is_admin: false };
  }
}

// Real agents when the API answers; three mocks keep the shell usable without it.
async function loadAgents() {
  try {
    const response = await fetch('/api/v3/agents', { headers: { accept: 'application/json' } });
    if (response.ok) {
      const body = await response.json();
      const list = Array.isArray(body) ? body : body.agents || [];
      if (list.length) {
        return list.slice(0, 8).map((agent) => ({
          id: String(agent.id ?? agent.agent_id ?? agent.name),
          name: String(agent.name ?? 'Agent'),
          owned: Boolean(agent.owned ?? agent.is_owner),
        }));
      }
    }
  } catch {
    /* fall through to the mocks */
  }
  return [
    { id: 'mock-webagent', name: 'WebAgent', owned: true },
    { id: 'mock-reviewer', name: 'Code Reviewer', owned: true },
    { id: 'mock-writer', name: 'Draft Writer', owned: false },
  ];
}

/* ── Sidebar markup ── */
function navItem({ iconName, label, href, count, avatar, active }) {
  const button = document.createElement('button');
  button.type = 'button';
  button.className = 'nav-item' + (active ? ' active' : '');
  button.dataset.href = href;
  if (avatar) {
    const badge = document.createElement('span');
    badge.className = 'nav-avatar';
    badge.setAttribute('aria-hidden', 'true');
    badge.textContent = avatar;
    button.append(badge);
  } else {
    button.insertAdjacentHTML('beforeend', icon(iconName, 16));
  }
  const name = document.createElement('span');
  name.className = 'nav-label';
  name.textContent = label;
  button.append(name);
  if (count) {
    const badge = document.createElement('span');
    badge.className = 'nav-count';
    badge.textContent = count;
    button.append(badge);
  }
  button.title = label;
  return button;
}

function navGroup(title, items) {
  const group = document.createElement('div');
  group.className = 'nav-group';
  const heading = document.createElement('div');
  heading.className = 'nav-group-title';
  heading.textContent = title;
  group.append(heading);
  for (const item of items) group.append(navItem(item));
  return group;
}

function renderNav() {
  const host = document.getElementById('sidebar-nav');
  if (!host) return;
  host.replaceChildren();

  if (state.me?.is_admin) {
    host.append(
      navGroup('Administrator', [
        { iconName: 'grid', label: 'Dashboard', href: '#/admin/dashboard' },
        { iconName: 'users', label: 'Users', href: '#/admin/users' },
        { iconName: 'cpu', label: 'Models', href: '#/admin/models' },
        { iconName: 'settings', label: 'Settings', href: '#/admin/settings' },
        { iconName: 'hammer', label: 'Builds', href: '#/admin/builds' },
        { iconName: 'layers', label: 'Components', href: '/web/admin/gallery/' },
      ]),
    );
  }

  host.append(
    navGroup('Session Management', [
      { iconName: 'message', label: 'All sessions', href: '#/sessions' },
    ]),
  );

  const agentItems = state.agents.map((agent) => ({
    iconName: 'bot',
    avatar: agent.name.slice(0, 1).toUpperCase(),
    label: agent.name,
    href: `#/agents/${encodeURIComponent(agent.id)}`,
  }));
  agentItems.push({ iconName: 'plus', label: 'New agent', href: '#/agents/new' });
  host.append(navGroup('Agent Management', agentItems));

  host.append(navGroup('Wiki', [{ iconName: 'book', label: 'Wiki', href: '#/wiki' }]));

  markActive();
}

function markActive() {
  for (const item of document.querySelectorAll('.nav-item')) {
    const href = item.dataset.href || '';
    const active = href.startsWith('#') ? href === state.route : false;
    item.classList.toggle('active', active);
    if (active) item.setAttribute('aria-current', 'page');
    else item.removeAttribute('aria-current');
  }
}

/* ── Router ── */
function currentRoute() {
  const hash = window.location.hash || '#/sessions';
  return hash.startsWith('#') ? hash.slice(1) : hash;
}

async function resolvePage(route) {
  if (route.startsWith('/agents/new')) return loadPage('../pages/new-agent/page.js', 'new-agent');
  if (route.startsWith('/agents/')) return loadPage('../pages/agent/page.js', 'agent');
  if (route.startsWith('/sessions')) return loadPage('../pages/sessions/page.js', 'sessions');
  if (route.startsWith('/chat')) return loadPage('../pages/chat/page.js', 'chat');
  if (route.startsWith('/wiki')) return loadPage('../pages/wiki/page.js', 'wiki');
  if (route.startsWith('/account')) return loadPage('../pages/account/page.js', 'account');
  const admin = route.match(/^\/admin\/([a-z-]+)$/);
  if (admin) {
    const name = admin[1];
    if (!state.me?.is_admin) return missingPage('Administrator pages');
    // Dynamic so the administrator folder is never fetched by other visitors.
    return loadPage(`../admin/pages/${name}/page.js`, name);
  }
  return loadPage('../pages/sessions/page.js', 'sessions');
}

async function loadPage(specifier, key) {
  if (!state.pages.has(key)) {
    const module = await import(specifier);
    state.pages.set(key, module);
  }
  return state.pages.get(key);
}

function missingPage(title) {
  return {
    meta: { title, note: 'Not available' },
    render(host) {
      host.append(noteLine('Administrator only.'));
    },
  };
}

function noteLine(text) {
  const note = document.createElement('p');
  note.className = 'card-note';
  note.textContent = text;
  return note;
}

// The phone switch points at the chat page, and back at the panel once there.
function syncChatSwitch() {
  const button = document.getElementById('chat-switch');
  if (!button) return;
  const onChat = state.route.startsWith('/chat');
  button.setAttribute('aria-pressed', String(onChat));
  button.setAttribute('aria-label', onChat ? 'Back to the panel' : 'Open chat');
  button.innerHTML = onChat ? BACK_ICON : CHAT_ICON;
}

async function renderRoute() {
  state.route = currentRoute();
  markActive();
  syncChatSwitch();
  // The chat page fills the panel edge to edge, so the shell drops its body padding there.
  document.body.classList.toggle('chat-route', state.route.startsWith('/chat'));
  const titleHost = document.getElementById('page-title');
  const noteHost = document.getElementById('page-note');
  const body = document.getElementById('page-body');
  if (!body) return;
  body.replaceChildren();
  try {
    const page = await resolvePage(state.route);
    const meta = page.meta || {};
    if (titleHost) titleHost.textContent = meta.title || 'Page';
    if (noteHost) noteHost.textContent = meta.note || '';
    page.render(body, { route: state.route, me: state.me, agents: state.agents });
  } catch (error) {
    if (titleHost) titleHost.textContent = 'Page';
    if (noteHost) noteHost.textContent = '';
    body.append(noteLine('This page could not be loaded.'));
    console.error(error);
  }
  closeMenu();
}

/* ── Boot ── */
async function boot() {
  applyTheme();
  applyCollapse();

  const media = window.matchMedia('(prefers-color-scheme: light)');
  media.addEventListener?.('change', () => {
    if (!localStorage.getItem(THEME_KEY)) applyTheme();
  });

  document.getElementById('theme-toggle')?.addEventListener('click', toggleTheme);
  document.getElementById('sidebar-toggle')?.addEventListener('click', toggleCollapse);
  document.getElementById('menu-open')?.addEventListener('click', () => {
    document.body.classList.add('menu-open');
    document.getElementById('menu-open')?.setAttribute('aria-expanded', 'true');
  });
  document.getElementById('sidebar-backdrop')?.addEventListener('click', closeMenu);
  document.addEventListener('keydown', (event) => {
    if (event.key === 'Escape') closeMenu();
  });

  document.getElementById('sidebar-nav')?.addEventListener('click', (event) => {
    const item = event.target.closest('.nav-item');
    if (!item) return;
    const href = item.dataset.href || '';
    if (href.startsWith('/web/')) {
      window.location.assign(href);
      return;
    }
    window.location.hash = href;
  });
  window.addEventListener('hashchange', renderRoute);

  const account = document.getElementById('account-button');
  account?.addEventListener('click', () => {
    window.location.hash = '#/account';
  });

  document.getElementById('chat-switch')?.addEventListener('click', () => {
    window.location.hash = currentRoute().startsWith('/chat') ? '#/sessions' : '#/chat';
  });

  state.me = await loadMe();
  state.agents = await loadAgents();
  // The account footer shows who is signed in; the shell footer echoes the state.
  const nameHost = document.getElementById('account-name');
  if (nameHost) nameHost.textContent = state.me?.is_admin ? 'Administrator' : 'Signed out';
  const roleHost = document.getElementById('account-role');
  if (roleHost) roleHost.textContent = state.me?.is_admin ? 'Manage account' : 'Sign in';
  const statusHost = document.getElementById('footer-status');
  if (statusHost) {
    statusHost.textContent = state.me?.is_admin ? 'Signed in as administrator' : 'Signed out';
  }

  renderNav();

  // The refresh component is the shell's refresh action: the header button runs the
  // hard refresh (administrators only, as in the app) and the pull gesture offers both
  // stages — refresh, then hard refresh.
  const refreshButton = document.getElementById('refresh-button');
  if (refreshButton) refreshButton.hidden = !state.me?.is_admin;
  wireRefreshButton(refreshButton);
  installPullToRefresh();

  await renderRoute();
}

if (document.readyState === 'loading') {
  document.addEventListener('DOMContentLoaded', boot, { once: true });
} else {
  boot();
}

/* ── Chat shell: configuration, the always-visible desktop column and its divider ──
   The view type is declared by the mount, never inferred from width: this column
   mounts the desktop profile, the chat page mounts the mobile one, a widget would
   mount the widget profile. narrow_follows_mobile lets a profile hand a narrow
   window to the mobile profile. */

const CHAT_SHELL_CONFIG = '/config/web/chat/chat-shell.json';

/* Objects merge, arrays replace, null inherits — the rule the config documents. */
function mergeProfile(base, override) {
  if (override === null || override === undefined) return base;
  if (Array.isArray(override)) return override;
  if (typeof override !== 'object') return override;
  const merged = { ...(base || {}) };
  for (const [key, value] of Object.entries(override)) {
    merged[key] = mergeProfile(base?.[key], value);
  }
  return merged;
}

async function loadChatShellConfig() {
  try {
    const response = await fetch(CHAT_SHELL_CONFIG, { headers: { accept: 'application/json' } });
    if (!response.ok) return null;
    return await response.json();
  } catch {
    return null;
  }
}

function chatShellProfile(config, mount) {
  const common = config?.common || {};
  const desktop = mergeProfile(common, config?.desktop || {});
  const mobile = mergeProfile(common, config?.mobile || {});
  // The mount decides the shape, so a missing or unreachable config is never a reason
  // to drop the chat: an absent chat_is falls back to what this mount is.
  if (!desktop.chat_is) desktop.chat_is = 'column';
  if (!mobile.chat_is) mobile.chat_is = 'page';
  if (mount === 'page') return mobile;
  const mobileAt = mobile.breakpoints?.mobile_px ?? 800;
  const narrow = window.matchMedia(`(max-width: ${mobileAt}px)`).matches;
  return narrow && desktop.narrow_follows_mobile === true ? mobile : desktop;
}

/* Writes the active profile's geometry onto the document as CSS variables. */
function applyChatShellProfile(profile) {
  const style = document.documentElement.style;
  const column = profile.column || {};
  const divider = profile.divider || {};
  style.setProperty('--chat-panel-width', `${column.default_width_px ?? 380}px`);
  style.setProperty('--chat-panel-min-width', `${column.min_width_px ?? 250}px`);
  style.setProperty('--chat-divider-hit-width', `${divider.hit_width_px ?? 14}px`);
  style.setProperty('--chat-divider-grip-width', `${divider.grip_width_px ?? 2}px`);
  style.setProperty('--chat-divider-grip-height', `${divider.grip_height_px ?? 36}px`);
  style.setProperty('--chat-divider-grip-height-active', `${divider.grip_height_active_px ?? 56}px`);
  document.body.dataset.chatProfile = profile.chat_is || 'column';
  return profile;
}

/* The column is always visible for now; showing and hiding it is a later decision. */
async function mountChatColumn(config) {
  const host = document.getElementById('chat-column');
  if (!host) return;
  const profile = applyChatShellProfile(chatShellProfile(config, 'column'));
  if (profile.chat_is !== 'column') return;
  const { mountChat } = await import('../chat/chat.js');
  await mountChat(host);
  wireChatDivider(profile);
}

/* Drag or arrow-key the divider to set the column width, clamped to the config. */
function wireChatDivider(profile) {
  const divider = document.getElementById('chat-divider');
  const stage = document.querySelector('.stage');
  if (!divider || !stage) return;
  const column = profile.column || {};
  const min = column.min_width_px ?? 250;
  const current = () => Math.round(document.getElementById('chat-column')?.getBoundingClientRect().width || 0);
  const clamp = (px) => {
    const stageWidth = stage.getBoundingClientRect().width;
    return Math.round(Math.min(Math.max(min, stageWidth - Math.min(320, stageWidth / 2)), Math.max(min, px)));
  };
  const apply = (px) => {
    const width = clamp(px);
    document.documentElement.style.setProperty('--chat-panel-width', `${width}px`);
    divider.setAttribute('aria-valuenow', String(width));
    return width;
  };

  divider.setAttribute('aria-valuemin', String(min));
  divider.setAttribute('aria-valuenow', String(current()));

  let dragging = false;
  divider.addEventListener('pointerdown', (event) => {
    dragging = true;
    divider.classList.add('resizing');
    document.body.classList.add('chat-resizing');
    divider.setPointerCapture?.(event.pointerId);
    event.preventDefault();
  });
  divider.addEventListener('pointermove', (event) => {
    if (!dragging) return;
    const box = stage.getBoundingClientRect();
    document.documentElement.style.setProperty('--drag-y', `${Math.round(event.clientY - box.top)}px`);
    apply(box.right - event.clientX);
  });
  const end = (event) => {
    if (!dragging) return;
    dragging = false;
    divider.classList.remove('resizing');
    document.body.classList.remove('chat-resizing');
    if (event?.pointerId !== undefined) divider.releasePointerCapture?.(event.pointerId);
  };
  divider.addEventListener('pointerup', end);
  divider.addEventListener('pointercancel', end);
  divider.addEventListener('lostpointercapture', end);
  divider.addEventListener('keydown', (event) => {
    const step = event.shiftKey ? 50 : 10;
    if (event.key === 'ArrowLeft') apply(current() + step);
    else if (event.key === 'ArrowRight') apply(current() - step);
    else if (event.key === 'Home') apply(min);
    else if (event.key === 'End') apply(stage.getBoundingClientRect().width);
    else return;
    event.preventDefault();
  });
}

void (async () => {
  const config = await loadChatShellConfig();
  await mountChatColumn(config);
})();
