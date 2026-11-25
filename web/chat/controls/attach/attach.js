// Attach: the + button. Opens the file picker; nothing is uploaded yet (no chat
// backend in web/), so picking files has no further effect.

import { icon } from '../../../components/icon/icon.js';

export function create() {
  const button = document.createElement('button');
  button.type = 'button';
  button.className = 'composer-btn composer-btn-attach';
  button.setAttribute('aria-label', 'Attach files');
  button.title = 'Attach files';
  button.innerHTML = icon('plus', 18);

  button.addEventListener('click', () => {
    const input = document.createElement('input');
    input.type = 'file';
    input.multiple = true;
    input.hidden = true;
    input.addEventListener('change', () => input.remove());
    document.body.append(input);
    input.click();
  });

  return button;
}
