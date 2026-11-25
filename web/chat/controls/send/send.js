// Send: the right-pointing triangle. The emphatic action; it composes the message
// and sends it once the transcript and backend are ported.

import { icon } from '../../../components/icon/icon.js';

export function create() {
  const button = document.createElement('button');
  button.type = 'button';
  button.className = 'composer-btn composer-btn-send';
  button.setAttribute('aria-label', 'Send');
  button.title = 'Send';
  button.innerHTML = icon('send', 16);
  return button;
}
