// Mounts the chat shell partial into a host element, then its own header and composer.
// Behaviour only: the markup is chat.html, the frame styles are chat.css.

import { WEB_ASSET_VERSION } from '../shell/version.js';
import { mountChatHeader } from './header/header.js';
import { mountComposer } from './composer/composer.js';

const PARTIAL = new URL(`./chat.html?v=${WEB_ASSET_VERSION}`, import.meta.url);

export async function mountChat(host) {
  if (!host || host.dataset.mounted === 'yes') return host;
  const response = await fetch(PARTIAL, { headers: { accept: 'text/html' } });
  if (!response.ok) throw new Error(`chat shell ${response.status}`);
  host.innerHTML = await response.text();
  host.dataset.mounted = 'yes';
  await mountChatHeader(host.querySelector('.chat-panel-head'));
  await mountComposer(host.querySelector('#composer'));
  return host;
}
