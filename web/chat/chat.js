// Mounts the chat shell partial into a host element, then the composer into the
// composer region it provides. Behaviour only: the markup is chat.html and the frame
// styles are chat.css.

import { WEB_ASSET_VERSION } from '../shell/version.js';
import { mountComposer } from './composer/composer.js';

const PARTIAL = new URL(`./chat.html?v=${WEB_ASSET_VERSION}`, import.meta.url);

export async function mountChat(host) {
  if (!host || host.dataset.mounted === 'yes') return host;
  const response = await fetch(PARTIAL, { headers: { accept: 'text/html' } });
  if (!response.ok) throw new Error(`chat shell ${response.status}`);
  host.innerHTML = await response.text();
  host.dataset.mounted = 'yes';
  await mountComposer(host.querySelector('#composer'));
  return host;
}
