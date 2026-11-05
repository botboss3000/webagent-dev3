// WebAgent Browser Extension service worker.
//
// Linking: the content script on WebAgent's pages passes a one-time code; it is traded for
// this device's secret. Work: long-poll the server for the linked account's actions, run
// each one with the debugger in the agent's own window, and post the result. The agent
// never touches tabs it did not open, and never acts on a site outside the agent's list.
const API = '/api/v3/browser';
const READ_LIMIT = 12000;
const KEYS = {Enter: 13, Tab: 9, Escape: 27, Space: 32, ArrowLeft: 37, ArrowUp: 38, ArrowRight: 39,
  ArrowDown: 40, PageUp: 33, PageDown: 34, Home: 36, End: 35, Backspace: 8};
const INTERACTIVE = new Set(['button', 'link', 'textbox', 'searchbox', 'combobox', 'checkbox', 'radio',
  'menuitem', 'menuitemcheckbox', 'menuitemradio', 'tab', 'option', 'switch', 'slider', 'spinbutton',
  'listbox', 'treeitem']);

const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
const local = {
  get: async key => (await chrome.storage.local.get(key))[key],
  set: (key, value) => chrome.storage.local.set({[key]: value}),
};
const session = {
  get: async key => (await chrome.storage.session.get(key))[key],
  set: (key, value) => chrome.storage.session.set({[key]: value}),
};

// ---- Linking ----------------------------------------------------------------------------

async function api(link, path, body) {
  return fetch(link.origin + API + path, {
    method: 'POST',
    headers: {'content-type': 'application/json', authorization: 'Bearer ' + link.secret},
    body: JSON.stringify(body || {}),
  });
}

async function linkWith(code, origin) {
  const previous = await local.get('link');
  if (previous) await api(previous, '/unlink').catch(() => {});
  const response = await fetch(origin + API + '/link', {
    method: 'POST', headers: {'content-type': 'application/json'},
    body: JSON.stringify({code, name: 'Chrome on ' + (navigator.userAgentData?.platform || 'this computer')}),
  });
  if (!response.ok) throw new Error('Link failed: ' + response.status);
  const linked = await response.json();
  await local.set('link', {origin, secret: linked.secret, user_id: linked.user_id, device_id: linked.device_id});
  poll();
}

async function unlink() {
  const link = await local.get('link');
  if (link) await api(link, '/unlink').catch(() => {});
  await chrome.storage.local.remove('link');
}

const allowedOrigin = sender => {
  const origin = sender.origin || (sender.url && new URL(sender.url).origin);
  return chrome.runtime.getManifest().host_permissions.some(p => p.startsWith(origin + '/'));
};

chrome.runtime.onMessage.addListener((message, sender, respond) => {
  (async () => {
    if (sender.id !== chrome.runtime.id) return null;
    const fromPopup = !sender.tab;
    if (message.type === 'link-state') {
      const link = await local.get('link');
      return {user_id: link?.user_id || null, origin: link?.origin || null, online: polling, paused: await session.get('paused') || false};
    }
    if (message.type === 'link' && !fromPopup && allowedOrigin(sender) && sender.origin === message.origin) {
      await linkWith(message.code, message.origin);
      return {linked: true};
    }
    if (message.type === 'unlink' && fromPopup) { await unlink(); return {unlinked: true}; }
    if (message.type === 'hand-back' && fromPopup) { await setPaused(false); return {paused: false}; }
    return null;
  })().then(respond, error => respond({error: String(error.message || error)}));
  return true;
});

// ---- Polling ----------------------------------------------------------------------------

let polling = false;
async function poll() {
  if (polling) return;
  polling = true;
  // Chrome stops an idle worker after 30 s; an extension call every 20 s keeps it running
  // while it waits on the server, and the alarm below restarts it if it was stopped anyway.
  const keepAlive = setInterval(() => chrome.runtime.getPlatformInfo(), 20000);
  try {
    for (;;) {
      const link = await local.get('link');
      if (!link) return;
      let call = null;
      try {
        const response = await api(link, '/next');
        if (response.status === 401) { await chrome.storage.local.remove('link'); return; }
        if (!response.ok) throw new Error('poll ' + response.status);
        ({call} = await response.json());
      } catch {
        await sleep(5000);
        continue;
      }
      if (call) await run(link, call);
    }
  } finally {
    clearInterval(keepAlive);
    polling = false;
  }
}

async function run(link, call) {
  const ack = await api(link, '/ack', {id: call.id});
  if (!ack.ok) return;
  let body;
  try {
    body = {id: call.id, output: await act(call.args, call.sites || [])};
  } catch (error) {
    body = {id: call.id, error: String(error.message || error), started: error.started === true};
  }
  await api(link, '/result', body).catch(() => {});
}

chrome.alarms.create('poll', {periodInMinutes: 0.5});
chrome.alarms.onAlarm.addListener(() => poll());
chrome.runtime.onStartup.addListener(() => poll());
chrome.runtime.onInstalled.addListener(() => poll());
poll();

// ---- The agent window -------------------------------------------------------------------

function siteAllowed(url, sites) {
  try {
    const parsed = new URL(url);
    if (!['http:', 'https:'].includes(parsed.protocol)) return false;
    const host = parsed.hostname.toLowerCase();
    return sites.some(site => host === site || host.endsWith('.' + site));
  } catch { return false; }
}

const failure = (message, started = false) => Object.assign(new Error(message), {started});

async function agentTabs() {
  const ids = await session.get('tabs') || [];
  const tabs = [];
  for (const id of ids) {
    try { tabs.push(await chrome.tabs.get(id)); } catch { /* closed by the user */ }
  }
  await session.set('tabs', tabs.map(tab => tab.id));
  return tabs;
}

async function currentTab(wanted) {
  const tabs = await agentTabs();
  const id = wanted ?? await session.get('current');
  const tab = tabs.find(t => t.id === id) || tabs.at(-1);
  if (!tab) throw failure('No agent tab is open; use open first');
  await session.set('current', tab.id);
  return tab;
}

async function openTab(url) {
  const tabs = await agentTabs();
  let tab;
  if (tabs.length) {
    tab = await chrome.tabs.create({windowId: tabs[0].windowId, url, active: true});
  } else {
    // The first agent tab gets its own window beside the user's, so they can watch it work.
    const beside = await chrome.windows.getLastFocused().catch(() => null);
    const placement = beside?.width >= 1200 && beside.state === 'normal'
      ? {left: beside.left + Math.round(beside.width / 2), top: beside.top,
        width: Math.round(beside.width / 2), height: beside.height}
      : {};
    const window = await chrome.windows.create({url, focused: true, ...placement});
    tab = window.tabs[0];
  }
  await session.set('tabs', [...tabs.map(t => t.id), tab.id]);
  await session.set('current', tab.id);
  const groupId = await chrome.tabs.group({tabIds: [tab.id], ...(tabs[0]?.groupId > -1
    ? {groupId: tabs[0].groupId} : {createProperties: {windowId: tab.windowId}})});
  await chrome.tabGroups.update(groupId, {title: 'WebAgent', color: 'purple'});
  return tab;
}

// ---- Debugger ---------------------------------------------------------------------------

const attached = new Set();
let overlaySource;

async function cdp(tabId, method, params = {}) {
  return chrome.debugger.sendCommand({tabId}, method, params);
}

async function attach(tabId) {
  if (attached.has(tabId)) return;
  try {
    await chrome.debugger.attach({tabId}, '1.3');
  } catch (error) {
    if (!String(error.message).includes('Already attached')) throw error;
  }
  attached.add(tabId);
  overlaySource ??= await (await fetch(chrome.runtime.getURL('overlay.js'))).text();
  await cdp(tabId, 'Page.enable');
  await cdp(tabId, 'Runtime.enable');
  await cdp(tabId, 'Runtime.addBinding', {name: '__webagentUser'});
  await cdp(tabId, 'Page.addScriptToEvaluateOnNewDocument', {source: overlaySource});
  await cdp(tabId, 'Runtime.evaluate', {expression: overlaySource});
}

chrome.debugger.onDetach.addListener(source => attached.delete(source.tabId));
chrome.tabs.onRemoved.addListener(tabId => attached.delete(tabId));
chrome.debugger.onEvent.addListener(async (source, method, params) => {
  if (method !== 'Runtime.bindingCalled' || params.name !== '__webagentUser') return;
  if (!(await session.get('tabs') || []).includes(source.tabId)) return;
  if (params.payload === 'took-over') await setPaused(true);
  if (params.payload === 'resume') await setPaused(false);
});

async function setPaused(paused) {
  await session.set('paused', paused);
  for (const tab of await agentTabs()) {
    if (attached.has(tab.id)) await overlay(tab.id, `user(${paused})`).catch(() => {});
  }
}

async function overlay(tabId, call) {
  await cdp(tabId, 'Runtime.evaluate', {expression: `window.__webagentOverlay?.${call}`});
}

async function evaluate(tabId, expression) {
  const {result} = await cdp(tabId, 'Runtime.evaluate', {expression, returnByValue: true});
  return result.value;
}

async function settle(tabId) {
  for (let i = 0; i < 60; i += 1) {
    if (await evaluate(tabId, 'document.readyState').catch(() => '') === 'complete') return;
    await sleep(250);
  }
}

async function page(tabId) {
  const tab = await chrome.tabs.get(tabId);
  return {tab: tabId, url: tab.url, title: tab.title};
}

async function center(tabId, ref) {
  await cdp(tabId, 'DOM.getDocument', {depth: 0});
  await cdp(tabId, 'DOM.scrollIntoViewIfNeeded', {backendNodeId: ref}).catch(() => {});
  let model;
  try {
    ({model} = await cdp(tabId, 'DOM.getBoxModel', {backendNodeId: ref}));
  } catch {
    throw failure(`ref ${ref} is no longer on the page; read again`);
  }
  const q = model.border;
  const xs = [q[0], q[2], q[4], q[6]], ys = [q[1], q[3], q[5], q[7]];
  const rect = {x: Math.min(...xs), y: Math.min(...ys), width: Math.max(...xs) - Math.min(...xs),
    height: Math.max(...ys) - Math.min(...ys)};
  return {x: rect.x + rect.width / 2, y: rect.y + rect.height / 2, rect};
}

async function pointAt(tabId, ref, caption) {
  const target = await center(tabId, ref);
  await overlay(tabId, `box(${JSON.stringify(target.rect)})`);
  await overlay(tabId, `caption(${JSON.stringify(caption)})`);
  await overlay(tabId, `move(${target.x},${target.y})`);
  await sleep(400);
  return target;
}

async function key(tabId, name) {
  const code = KEYS[name];
  const text = name === 'Enter' ? '\r' : name === 'Space' ? ' ' : undefined;
  const base = {key: name === 'Space' ? ' ' : name, code: name === 'Space' ? 'Space' : name,
    windowsVirtualKeyCode: code, nativeVirtualKeyCode: code};
  await overlay(tabId, 'expect(600)');
  await cdp(tabId, 'Input.dispatchKeyEvent', {type: text ? 'keyDown' : 'rawKeyDown', text, ...base});
  await cdp(tabId, 'Input.dispatchKeyEvent', {type: 'keyUp', ...base});
}

// ---- Reading ----------------------------------------------------------------------------

const labels = new Map();

async function read(tabId, offset = 0) {
  await cdp(tabId, 'Accessibility.enable');
  const {nodes} = await cdp(tabId, 'Accessibility.getFullAXTree');
  const byId = new Map(nodes.map(node => [node.nodeId, node]));
  const lines = [];
  const names = new Map();
  const walk = (node, depth) => {
    if (!node || depth > 200) return;
    const role = node.role?.value || '';
    const name = (node.name?.value || '').replace(/\s+/g, ' ').trim();
    if (!node.ignored) {
      const value = node.value?.value;
      const state = (node.properties || [])
        .filter(p => ['checked', 'selected', 'expanded', 'disabled', 'focused'].includes(p.name) && p.value?.value)
        .map(p => p.name + (p.value.value === true ? '' : '=' + p.value.value));
      if (INTERACTIVE.has(role) && node.backendDOMNodeId) {
        names.set(node.backendDOMNodeId, `${role} "${name}"`);
        lines.push(`[${node.backendDOMNodeId}] ${role} "${name}"` + (value != null && value !== '' ? ` value="${value}"` : '')
          + (state.length ? ` (${state.join(', ')})` : ''));
      } else if (role === 'heading' && name) {
        lines.push('# ' + name);
      } else if ((role === 'StaticText' || role === 'image') && name) {
        lines.push(role === 'image' ? `[image] ${name}` : name);
      }
      if (INTERACTIVE.has(role) || role === 'heading') return;
    }
    for (const child of node.childIds || []) walk(byId.get(child), depth + 1);
  };
  walk(nodes[0], 0);
  labels.set(tabId, names);
  const text = lines.join('\n');
  const part = text.slice(offset, offset + READ_LIMIT);
  const next = offset + READ_LIMIT < text.length ? offset + READ_LIMIT : null;
  return {...await page(tabId), text: part, next_offset: next};
}

// ---- Actions ----------------------------------------------------------------------------

async function act(args, sites) {
  if (await session.get('paused')) {
    throw failure('The user took over this browser window. Wait until they hand control back, then read the page again.');
  }
  const {action} = args;
  if (action === 'tabs') {
    const current = await session.get('current');
    return {tabs: (await agentTabs()).map(t => ({tab: t.id, url: t.url, title: t.title, current: t.id === current}))};
  }
  if ((action === 'open' || action === 'navigate') && !siteAllowed(args.url, sites)) {
    throw failure(`${args.url} is not on this agent's site list`);
  }
  if (action === 'open') {
    const tab = await openTab(args.url);
    await attach(tab.id);
    await settle(tab.id);
    return page(tab.id);
  }
  const tab = await currentTab(args.tab);
  if (action === 'close') {
    await chrome.tabs.remove(tab.id);
    return {closed: tab.id};
  }
  if (!siteAllowed(tab.url, sites)) {
    throw failure(`The agent tab is on ${new URL(tab.url).hostname}, which is not on this agent's site list`);
  }
  await attach(tab.id);
  await overlay(tab.id, 'user(false)');
  if (action === 'navigate') {
    await overlay(tab.id, `caption(${JSON.stringify('Opening ' + args.url)})`);
    await cdp(tab.id, 'Page.navigate', {url: args.url});
    await settle(tab.id);
    return page(tab.id);
  }
  if (action === 'read') {
    await overlay(tab.id, 'caption("Reading the page")');
    return read(tab.id, args.offset || 0);
  }
  const label = labels.get(tab.id)?.get(args.ref) || 'the page';
  if (action === 'click') {
    const {x, y} = await pointAt(tab.id, args.ref, 'Clicking ' + label);
    await overlay(tab.id, 'expect(800)');
    await cdp(tab.id, 'Input.dispatchMouseEvent', {type: 'mouseMoved', x, y});
    try {
      await cdp(tab.id, 'Input.dispatchMouseEvent', {type: 'mousePressed', x, y, button: 'left', clickCount: 1});
      await cdp(tab.id, 'Input.dispatchMouseEvent', {type: 'mouseReleased', x, y, button: 'left', clickCount: 1});
    } catch (error) {
      throw failure(String(error.message || error), true);
    }
    await overlay(tab.id, `ripple(${x},${y})`).catch(() => {});
    await sleep(300);
    await settle(tab.id);
    return {...await page(tab.id), clicked: label};
  }
  if (action === 'type') {
    await pointAt(tab.id, args.ref, 'Typing into ' + label);
    await cdp(tab.id, 'DOM.focus', {backendNodeId: args.ref});
    const {object} = await cdp(tab.id, 'DOM.resolveNode', {backendNodeId: args.ref});
    const {result} = await cdp(tab.id, 'Runtime.callFunctionOn', {objectId: object.objectId, returnByValue: true,
      functionDeclaration: `function(clear){
        if (clear && typeof this.select === 'function') this.select();
        else if (clear && this.isContentEditable) { const r = document.createRange(); r.selectNodeContents(this);
          const s = getSelection(); s.removeAllRanges(); s.addRange(r); }
        return this.type === 'password'; }`,
      arguments: [{value: args.clear !== false}]});
    const secret = result.value === true;
    let typed = '';
    try {
      // A few characters at a time, so the user can follow the typing.
      for (const chunk of args.text.match(/[\s\S]{1,4}/g) || []) {
        await overlay(tab.id, 'expect(400)');
        await cdp(tab.id, 'Input.insertText', {text: chunk});
        typed += chunk;
        await overlay(tab.id, `caption(${JSON.stringify(`Typing into ${label}: ` + (secret ? '•'.repeat(typed.length) : typed).slice(-60))})`);
        await sleep(25);
      }
      if (args.submit) await key(tab.id, 'Enter');
    } catch (error) {
      throw failure(String(error.message || error), true);
    }
    await overlay(tab.id, 'box(null)');
    await settle(tab.id);
    return {...await page(tab.id), typed_into: label, submitted: args.submit === true};
  }
  if (action === 'press') {
    if (args.ref != null) {
      await pointAt(tab.id, args.ref, `Pressing ${args.key} on ${label}`);
      await cdp(tab.id, 'DOM.focus', {backendNodeId: args.ref});
    } else {
      await overlay(tab.id, `caption(${JSON.stringify('Pressing ' + args.key)})`);
    }
    try { await key(tab.id, args.key); } catch (error) { throw failure(String(error.message || error), true); }
    await settle(tab.id);
    return {...await page(tab.id), pressed: args.key};
  }
  if (action === 'scroll') {
    const {width, height} = await evaluate(tab.id, '({width: innerWidth, height: innerHeight})');
    await overlay(tab.id, 'caption("Scrolling")');
    await overlay(tab.id, 'expect(600)');
    await cdp(tab.id, 'Input.dispatchMouseEvent', {type: 'mouseWheel', x: width / 2, y: height / 2,
      deltaX: 0, deltaY: args.dy || 600});
    await sleep(300);
    return {...await page(tab.id), scroll_y: await evaluate(tab.id, 'scrollY')};
  }
  throw failure('Unknown action ' + action);
}
