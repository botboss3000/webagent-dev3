// Component gallery: lists every component in components.js and shows it live.
// Administrators only — this page lives under web/admin/, which the server gates.

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
  'chat-header': `
    <div class="g-frame">
      <header class="chat-head">
        <div class="chat-head-row">
          <span class="chat-agent-name">WebAgent</span>
          <span class="chat-head-spacer"></span>
          <button type="button" class="chat-new-button" aria-label="New conversation">+</button>
        </div>
        <div class="chat-head-row">
          <button type="button" class="chat-session-chip"><span class="chat-session-title">Shell review</span><span class="chat-session-age">2m</span></button>
        </div>
      </header>
    </div>`,
  message: `
    <div class="messages">
      <article class="msg msg-user"><span class="msg-label">You</span><div class="msg-body"><p>A user turn.</p></div></article>
      <article class="msg msg-agent"><span class="msg-label">Agent</span><div class="msg-body"><p>An agent turn with <code>inline code</code> and a <a href="#/wiki">link</a>.</p></div></article>
    </div>`,
  activity: `
    <div class="activity activity-open">
      <button type="button" class="activity-head" aria-expanded="true">
        <span class="activity-chev">›</span>
        <span class="activity-label">2 tool calls, 1 message</span>
        <span class="activity-status">Completed</span>
      </button>
      <div class="activity-body">
        <div class="step">
          <div class="step-head"><span class="step-num">Step 1</span><span class="step-count">2 tool calls</span></div>
          <div class="step-calls">
            <details class="call"><summary><span class="call-glyph">R</span><span class="call-label">Read</span><span class="call-chip">shell.css</span><span class="call-note">182 lines</span></summary>
              <div class="call-detail"><h4>Arguments</h4><pre>{ "path": "web/shell/shell.css" }</pre></div>
            </details>
          </div>
        </div>
      </div>
    </div>`,
  composer: `
    <div class="composer">
      <div class="composer-pill">
        <div class="composer-row">
          <textarea class="composer-input" rows="1" placeholder="Chat" aria-label="Message"></textarea>
          <button type="button" class="composer-button" aria-label="Attach a file">+</button>
          <button type="button" class="composer-button composer-send" aria-label="Send" disabled>↑</button>
        </div>
        <div class="composer-stats"><span>in <b>3,184</b></span><span>out <b>612</b></span></div>
      </div>
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
