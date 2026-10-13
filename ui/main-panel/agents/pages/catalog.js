'use strict';

// V3 mirrors V2's JSON directory and shared row styles, without V2 auth imports.
export async function loadAgentPages() {
  const response = await fetch(new URL('./catalog.json', import.meta.url), { cache: 'no-store' });
  if (!response.ok) throw Error('Agent page catalog unavailable');
  const catalog = await response.json();
  if (catalog.version !== 2) throw Error('Unsupported agent page catalog version');
  const ids = new Set();
  const types = new Set(['directory', 'agent_configuration', 'sessions']);
  if (!Array.isArray(catalog.pages) || !catalog.pages.length) throw Error('Agent page catalog is empty');
  for (const page of catalog.pages) {
    if (!/^[a-z][a-z0-9-]*$/.test(page.id) || ids.has(page.id) || !page.title || !page.panelId || !types.has(page.type)) throw Error('Invalid agent page catalog');
    ids.add(page.id);
  }
  for (const [id, type] of [['config','directory'],['sessions','sessions']]) {
    if (!catalog.pages.some(page => page.id === id && page.type === type) || catalog.pages.filter(page => page.type === type).length !== 1) throw Error('Invalid agent page scope');
  }
  return [...catalog.pages].sort((a, b) => (a.order || 0) - (b.order || 0));
}

// Preserve existing App imports during independently published UI transitions.
export { renderAgentDirectory } from './config/page.js';
