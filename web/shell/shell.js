// Shell behaviour: theme, sidebar, phone drawer, hash router, chat panel mount.
// Plain modules, no build step. Admin pages are imported only when an
// administrator opens them, so a signed-out visitor never fetches that folder.

import { icon } from '../components/icon/icon.js';
import { mountChat } from '../chat/chat.js';

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

async function renderRoute() {
  state.route = currentRoute();
  markActive();
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

/* ── Chat resize grip: drag the divider to set the chat column width ── */
function wireGrip() {
  const grip = document.getElementById('chat-resize-handle');
  const stage = document.querySelector('.stage');
  if (!grip || !stage) return;

  const limits = () => {
    const width = stage.getBoundingClientRect().width;
    return { min: Math.min(240, width / 2), max: Math.max(width / 2, width - 320) };
  };

  const apply = (px) => {
    const { min, max } = limits();
    const width = Math.round(Math.min(max, Math.max(min, px)));
    document.documentElement.style.setProperty('--chat-panel-width', `${width}px`);
    grip.setAttribute('aria-valuenow', String(width));
    return width;
  };

  const { min, max } = limits();
  grip.setAttribute('aria-valuemin', String(Math.round(min)));
  grip.setAttribute('aria-valuemax', String(Math.round(max)));
  grip.setAttribute('aria-valuenow', String(Math.round(grip.parentElement.querySelector('#chat-column')?.getBoundingClientRect().width || 380)));

  let dragging = false;
  grip.addEventListener('pointerdown', (event) => {
    dragging = true;
    grip.classList.add('resizing');
    document.body.classList.add('chat-resizing');
    grip.setPointerCapture?.(event.pointerId);
    event.preventDefault();
  });
  grip.addEventListener('pointermove', (event) => {
    if (!dragging) return;
    const stageBox = stage.getBoundingClientRect();
    apply(stageBox.right - event.clientX);
  });
  const end = (event) => {
    if (!dragging) return;
    dragging = false;
    grip.classList.remove('resizing');
    document.body.classList.remove('chat-resizing');
    if (event?.pointerId !== undefined) grip.releasePointerCapture?.(event.pointerId);
  };
  grip.addEventListener('pointerup', end);
  grip.addEventListener('pointercancel', end);
  grip.addEventListener('lostpointercapture', end);

  // Keyboard: arrows step, Home/End jump to the clamp limits.
  grip.addEventListener('keydown', (event) => {
    const current = Number(grip.getAttribute('aria-valuenow')) || 380;
    const step = event.shiftKey ? 50 : 10;
    if (event.key === 'ArrowLeft') apply(current + step);
    else if (event.key === 'ArrowRight') apply(current - step);
    else if (event.key === 'Home') apply(limits().max);
    else if (event.key === 'End') apply(limits().min);
    else return;
    event.preventDefault();
  });

  window.addEventListener('resize', () => {
    const current = Number(grip.getAttribute('aria-valuenow')) || 380;
    const { min: lo, max: hi } = limits();
    grip.setAttribute('aria-valuemin', String(Math.round(lo)));
    grip.setAttribute('aria-valuemax', String(Math.round(hi)));
    if (current < lo || current > hi) apply(current);
  });
}

/* ── Phone view switch ── */
function wirePhoneSwitch() {
  const buttons = document.querySelectorAll('.phone-switch button');
  const set = (view) => {
    document.body.classList.toggle('view-chat', view === 'chat');
    document.body.classList.toggle('view-panel', view !== 'chat');
    for (const button of buttons) {
      button.classList.toggle('active', button.dataset.view === view);
      button.setAttribute('aria-pressed', String(button.dataset.view === view));
    }
  };
  for (const button of buttons) {
    button.addEventListener('click', () => set(button.dataset.view));
  }
  set('panel');
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

  state.me = await loadMe();
  state.agents = await loadAgents();
  // The account footer shows who is signed in; the mock keeps it honest when signed out.
  const nameHost = document.getElementById('account-name');
  if (nameHost) nameHost.textContent = state.me?.is_admin ? 'Administrator' : 'Signed out';
  const roleHost = document.getElementById('account-role');
  if (roleHost) roleHost.textContent = state.me?.is_admin ? 'Manage account' : 'Sign in';

  renderNav();
  wireGrip();
  wirePhoneSwitch();
  await renderRoute();
  await mountChat(document.getElementById('chat-column'));
}

if (document.readyState === 'loading') {
  document.addEventListener('DOMContentLoaded', boot, { once: true });
} else {
  boot();
}
