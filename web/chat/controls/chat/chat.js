// Chat: the chat shell's own way back. It emits the same shell:chat event the content
// header's chat button does, so the host routes back to the panel.

import { icon } from '../../../components/icon/icon.js';

export function create() {
  const button = document.createElement('button');
  button.type = 'button';
  button.id = 'chat-toggle';
  button.className = 'chat-head-btn';
  button.setAttribute('aria-label', 'Back to the panel');
  button.title = 'Back to the panel';
  button.innerHTML = icon('chevron', 18);
  button.style.transform = 'rotate(180deg)';
  button.addEventListener('click', () => {
    document.dispatchEvent(new CustomEvent('shell:chat', { bubbles: true }));
  });
  return button;
}
