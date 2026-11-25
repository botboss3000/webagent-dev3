// The content shell: its own header (sidebar button left, title centre, chat button right)
// and the body region the router renders pages into. It owns no routing: the host mounts
// the header, then sets the title on the elements it created and renders into #page-body.

import { icon } from '../components/icon/icon.js';
import { createSidebarButton } from '../components/sidebar-button/sidebar-button.js';

/* The chat button asks the host to show the chat; it never routes by itself. */
function chatButton() {
  const button = document.createElement('button');
  button.type = 'button';
  button.id = 'chat-switch';
  button.className = 'content-head-btn';
  button.setAttribute('aria-label', 'Open chat');
  button.title = 'Open chat';
  button.innerHTML = icon('message', 18);
  button.addEventListener('click', () => {
    document.dispatchEvent(new CustomEvent('shell:chat', { bubbles: true }));
  });
  return button;
}

function slot(className, ...children) {
  const element = document.createElement('div');
  element.className = className;
  element.append(...children);
  return element;
}

/* Renders the content header once and returns it; re-calls reuse the existing element. */
export function mountContentHeader(host) {
  if (!host) return null;
  const existing = host.querySelector('.content-head');
  if (existing) return existing;

  const title = document.createElement('h1');
  title.id = 'page-title';
  title.textContent = 'Sessions';

  const note = document.createElement('span');
  note.id = 'page-note';
  note.className = 'content-head-note';

  const sidebar = createSidebarButton();
  sidebar.id = 'menu-open';

  const header = document.createElement('header');
  header.className = 'content-head';
  header.append(
    slot('content-head-left', sidebar),
    slot('content-head-centre', title, note),
    slot('content-head-right', chatButton()),
  );

  host.prepend(header);
  return header;
}
