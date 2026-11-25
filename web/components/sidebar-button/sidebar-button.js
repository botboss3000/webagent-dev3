// Sidebar button: the shared primitive for "open the sidebar". It never touches the
// sidebar itself — it emits shell:sidebar on the document, and the host that owns the
// sidebar decides what that means (shelf on a phone, expanded rail on a desktop).

import { icon } from '../icon/icon.js';

export function createSidebarButton({ label = 'Menu', iconName = 'menu', size = 18 } = {}) {
  const button = document.createElement('button');
  button.type = 'button';
  button.className = 'sidebar-button';
  button.setAttribute('aria-label', label);
  button.title = label;
  button.setAttribute('aria-controls', 'sidebar');
  button.setAttribute('aria-expanded', 'false');
  button.innerHTML = icon(iconName, size);

  button.addEventListener('click', () => {
    document.dispatchEvent(new CustomEvent('shell:sidebar', { bubbles: true }));
  });

  return button;
}
