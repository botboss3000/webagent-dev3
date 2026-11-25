// Component gallery: lists every component in components.js and shows it live.
// Administrators only — this page lives under web/admin/, which the server gates.
//
// The chat components were removed with the chat panel, so the specimens here are
// the frame primitives; a ported component adds its specimen when it lands.

import { components } from '../../components/components.js';
import { icon } from '../../components/icon/icon.js';

// One specimen per component id. A component without a specimen is still listed,
// with a note saying so, so the gallery can never quietly skip a component.
const SPECIMENS = {
  badge: `
    <div class="g-row">
      <span class="badge">Default</span>
      <span class="badge badge-live">Live</span>
      <span class="badge badge-count">3 sessions</span>
      <span class="avatar avatar-sm">W</span>
      <span class="avatar avatar-accent">A</span>
    </div>`,
  button: `
    <div class="g-row">
      <button type="button" class="btn">Default</button>
      <button type="button" class="btn btn-primary">Primary</button>
      <button type="button" class="btn btn-quiet">Quiet</button>
      <button type="button" class="btn btn-danger">Danger</button>
      <button type="button" class="btn btn-sm">Small</button>
      <button type="button" class="btn" disabled>Disabled</button>
    </div>`,
  card: `
    <div class="card">
      <div class="card-head"><h3 class="card-title">Card title</h3></div>
      <p class="card-hint">One idea, one bordered surface.</p>
    </div>`,
  elevation: `
    <div class="g-row">
      <div class="card elevation-1">1</div>
      <div class="card elevation-2">2</div>
      <div class="card elevation-3">3</div>
      <div class="card elevation-4">4</div>
    </div>`,
  grid: `
    <div class="grid grid-narrow">
      <div class="card">One</div>
      <div class="card">Two</div>
      <div class="card">Three</div>
    </div>`,
  stack: `
    <div class="stack stack-tight">
      <div class="card">First block</div>
      <div class="card">Second block</div>
    </div>`,
  'sidebar-button': `
    <button type="button" class="sidebar-button" aria-label="Menu" title="Menu">
      <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M4 7h16M4 12h16M4 17h16"/></svg>
    </button>`,
  list: `
    <div class="list">
      <button type="button" class="list-row">
        <span class="list-text"><span class="list-title">First row</span><span class="list-hint">With a hint line</span></span>
        <span class="list-trailing">2m</span>
      </button>
      <button type="button" class="list-row">
        <span class="list-text"><span class="list-title">Second row</span></span>
        <span class="list-trailing">1h</span>
      </button>
    </div>`,
  stat: `
    <div class="stat-row">
      <div class="stat"><span class="stat-label">Sessions</span><span class="stat-value">128</span><span class="stat-note">last 7 days</span></div>
      <div class="stat"><span class="stat-label">Tokens</span><span class="stat-value">1.2M</span><span class="stat-note">in and out</span></div>
    </div>`,
  tile: `
    <div class="tiles">
      <button type="button" class="tile"><span class="tile-head"><span class="tile-title">Identity</span></span><span class="tile-hint">Name, avatar and chat modes</span></button>
      <button type="button" class="tile"><span class="tile-head"><span class="tile-title">Models</span></span><span class="tile-hint">Assignments and connections</span></button>
    </div>`,
  icon: `
    <div class="g-row g-icons">${['grid', 'users', 'cpu', 'settings', 'hammer', 'layers', 'message', 'bot', 'plus', 'book', 'menu', 'sun', 'moon']
      .map((name) => icon(name, 20))
      .join('')}</div>`,
  refresh: `
    <div class="g-row">
      <svg viewBox="0 0 48 48" width="44" height="44" aria-hidden="true">
        <path class="ptr-arc ptr-arc-soft" d="M24,7 A17,17 0 1,1 10.3,15.3 L15.5,10.5" stroke-dasharray="98" stroke-dashoffset="40"/>
        <path class="ptr-arc ptr-arc-hard" d="M24,7 A17,17 0 1,1 10.3,15.3 L15.5,10.5" stroke-dasharray="98" stroke-dashoffset="98"/>
      </svg>
      <span class="ptr-hint show">Hard refresh</span>
    </div>`,
};

function card(component) {
  const article = document.createElement('article');
  article.className = 'gallery-card';
  article.dataset.component = component.id;

  const head = document.createElement('div');
  head.className = 'gallery-head';

  const title = document.createElement('h2');
  title.className = 'gallery-title';
  title.textContent = component.title;
  head.append(title);

  const file = document.createElement('span');
  file.className = 'badge badge-count';
  file.textContent = component.css || component.js;
  head.append(file);
  article.append(head);

  const summary = document.createElement('p');
  summary.className = 'card-hint';
  summary.textContent = component.summary;
  article.append(summary);

  const body = document.createElement('div');
  body.className = 'gallery-body';
  const specimen = SPECIMENS[component.id];
  if (specimen) body.innerHTML = specimen;
  else {
    const missing = document.createElement('p');
    missing.className = 'card-note';
    missing.textContent = 'No specimen yet.';
    body.append(missing);
  }
  article.append(body);
  return article;
}

function boot() {
  const host = document.getElementById('gallery-grid');
  if (!host) return;
  document.getElementById('gallery-count').textContent = String(components.length);
  for (const component of components) host.append(card(component));
}

if (document.readyState === 'loading') {
  document.addEventListener('DOMContentLoaded', boot, { once: true });
} else {
  boot();
}
