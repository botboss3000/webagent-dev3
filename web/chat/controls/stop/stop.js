// Stop: the red square. Rendered in its resting state; there is no run to stop yet.

import { icon } from '../../../components/icon/icon.js';

export function create() {
  const button = document.createElement('button');
  button.type = 'button';
  button.className = 'composer-btn composer-btn-stop';
  button.setAttribute('aria-label', 'Stop');
  button.title = 'Stop';
  button.innerHTML = icon('square', 16);
  return button;
}
