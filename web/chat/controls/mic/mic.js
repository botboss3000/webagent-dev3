// Dictate: the lucide microphone. Rendered in its resting state; voice capture is
// not part of this release.

import { icon } from '../../../components/icon/icon.js';

export function create() {
  const button = document.createElement('button');
  button.type = 'button';
  button.className = 'composer-btn composer-btn-mic';
  button.setAttribute('aria-label', 'Dictate');
  button.title = 'Dictate';
  button.innerHTML = icon('mic', 18);
  return button;
}
