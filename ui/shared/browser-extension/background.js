// WebAgent Browser Extension service worker.
//
// Linking: the content script on WebAgent's pages passes a one-time code; it is traded for
// this device's secret. Linking to another account than the last one asks the user first.
// The extension sends its version with every request; the server refuses outdated ones. Work: long-poll the server for the linked account's actions, run
// each one with the debugger in the agent's own window, and post the result. The agent
// never touches tabs outside its own tab group, never acts on a site outside the agent's
// list, and never on this app's own address or a private one (guard.js, checked at every
// action and again before each input).
import {COMMANDS, consentRefusal, describeTab, frameRefusal, hostOf, siteAllowed, tabRefusal} from './guard.js';

const API = '/api/v3/browser';
const VERSION = chrome.runtime.getManifest().version;
const HEADERS = {'content-type': 'application/json', 'x-webagent-extension': VERSION};
// How long a declined link to another account is not asked about again.
const DECLINE_MS = 10 * 60 * 1000;
// The overlay and its take-over signal live in a separate script world, out of the page's reach.
const WORLD = 'webagent-overlay';
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
    headers: {...HEADERS, authorization: 'Bearer ' + link.secret},
    body: JSON.stringify(body || {}),
  });
}

async function linkWith(code, origin) {
  // Look at the code first: which account would this browser be linked to?
  const look = await fetch(origin + API + '/link', {method: 'POST', headers: HEADERS,
    body: JSON.stringify({code, preview: true})});
  if (!look.ok) throw new Error('Link failed: ' + look.status);
  const target = await look.json();
  const previous = await local.get('link');
  // The account linked last stays remembered after an unlink, so a later link to another
  // account is still noticed. Installs from before 0.3.0 only kept `link`.
  const last = await local.get('last_account') || (previous && {user_id: previous.user_id, origin: previous.origin});
  if (last && (last.user_id !== target.user_id || last.origin !== origin)) {
    const key = origin + ' ' + target.user_id;
    const declined = await session.get('declined') || {};
    if (declined[key] > Date.now()) return {linked: false};
    const yes = await confirmLink({account: target.username || target.user_id, host: hostOf(origin),
      previous: last.username || 'another account', previous_host: hostOf(last.origin)});
    if (!yes) {
      await session.set('declined', {...declined, [key]: Date.now() + DECLINE_MS});
      return {linked: false};
    }
  }
  if (previous) await api(previous, '/unlink').catch(() => {});
  const response = await fetch(origin + API + '/link', {
    method: 'POST', headers: HEADERS,
    body: JSON.stringify({code, name: 'Chrome on ' + (navigator.userAgentData?.platform || 'this computer')}),
  });
  if (!response.ok) throw new Error('Link failed: ' + response.status);
  const linked = await response.json();
  await local.set('link', {origin, secret: linked.secret, user_id: linked.user_id, username: target.username,
    device_id: linked.device_id});
  await local.set('last_account', {user_id: linked.user_id, username: target.username, origin});
  poll();
  return {linked: true};
}

// Ask in a small extension window whether to link to `details.account`. Closing the window,
// or no answer before the link code runs out, means no.
let asking = null;
function confirmLink(details) {
  if (asking) return Promise.resolve(false);
  const id = crypto.randomUUID();
  return new Promise(resolve => {
    let windowId = null;
    // Keep the worker running while the user decides.
    const keepAlive = setInterval(() => chrome.runtime.getPlatformInfo(), 20000);
    const closed = removed => { if (removed === windowId) done(false); };
    const timer = setTimeout(() => done(false), 100000);
    function done(answer) {
      if (asking?.id !== id) return;
      asking = null;
      clearTimeout(timer);
      clearInterval(keepAlive);
      chrome.windows.onRemoved.removeListener(closed);
      session.set('confirm', null);
      if (windowId != null) chrome.windows.remove(windowId).catch(() => {});
      resolve(answer);
    }
    asking = {id, done};
    chrome.windows.onRemoved.addListener(closed);
    session.set('confirm', {id, ...details})
      .then(() => chrome.windows.create({url: chrome.runtime.getURL('confirm.html'), type: 'popup',
        width: 440, height: 300, focused: true}))
      .then(window => { windowId = window.id; }, () => done(false));
  });
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
    const fromConfirm = sender.url === chrome.runtime.getURL('confirm.html');
    if (message.type === 'link-state') {
      const link = await local.get('link');
      return {user_id: link?.user_id || null, username: link?.username || null, origin: link?.origin || null,
        online: polling, paused: await session.get('paused') || false, outdated: await local.get('outdated') || null};
    }
    if (message.type === 'link' && !fromPopup && allowedOrigin(sender) && sender.origin === message.origin) {
      return linkWith(message.code, message.origin);
    }
    if (message.type === 'confirm-state' && fromConfirm) return session.get('confirm');
    if (message.type === 'confirm-answer' && fromConfirm && asking?.id === message.id) {
      asking.done(message.link === true);
      return {answered: true};
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
        if (response.status === 426) {
          // Too old for this server: say so in the popup and ask again now and then.
          const {error} = await response.json().catch(() => ({}));
          await local.set('outdated', error || 'This extension is out of date; download it again from WebAgent.');
          await sleep(60000);
          continue;
        }
        if (!response.ok) throw new Error('poll ' + response.status);
        if (await local.get('outdated')) await chrome.storage.local.remove('outdated');
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
    // The app's own hosts: those the server names, plus the one this browser is linked to.
    const blocked = [...new Set([...(call.blocked_hosts || []), hostOf(link.origin)].filter(Boolean))];
    body = {id: call.id, output: await act(call.args, {sites: call.sites || [], blocked, consent: call.consent ?? null})};
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

const failure = (message, started = false) => Object.assign(new Error(message), {started});

// The agent's tabs: those it opened that are still in its tab group. A tab the user drags out
// of the group is theirs from then on.
async function agentTabs() {
  const ids = await session.get('tabs') || [];
  const group = await session.get('group');
  const tabs = [];
  for (const id of ids) {
    try {
      const tab = await chrome.tabs.get(id);
      if (group != null && tab.groupId === group) tabs.push(tab);
    } catch { /* closed by the user */ }
  }
  await session.set('tabs', tabs.map(tab => tab.id));
  return tabs;
}

async function currentTab(wanted) {
  const tabs = await agentTabs();
  const id = wanted ?? await session.get('current');
  const tab = tabs.find(t => t.id === id) || (wanted == null ? tabs.at(-1) : null);
  if (!tab) throw failure(wanted == null ? 'No agent tab is open; use open first' : `Tab ${wanted} is not an agent tab; call tabs`);
  await session.set('current', tab.id);
  return tab;
}

async function context(rules) {
  return {...rules, group: await session.get('group')};
}

// The tab as it is now, refused when it has left the site list or reached a forbidden host
// since the last check. On a forbidden host the debugger lets go of the tab.
async function guard(tabId, action, rules) {
  let tab;
  try { tab = await chrome.tabs.get(tabId); } catch { throw failure('The agent tab was closed'); }
  const refusal = tabRefusal(action, tab, await context(rules));
  if (refusal) {
    if (refusal.includes('never acts there')) await detach(tabId);
    throw failure(refusal);
  }
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
    // Chrome refuses bounds that are mostly off-screen (a second monitor, a narrow screen);
    // the window still opens at Chrome's own position then.
    const window = await chrome.windows.create({url, focused: true, ...placement})
      .catch(() => chrome.windows.create({url, focused: true}));
    tab = window.tabs[0];
  }
  const groupId = await chrome.tabs.group({tabIds: [tab.id], ...(tabs[0]?.groupId > -1
    ? {groupId: tabs[0].groupId} : {createProperties: {windowId: tab.windowId}})});
  await chrome.tabGroups.update(groupId, {title: 'WebAgent', color: 'purple'});
  await session.set('group', groupId);
  await session.set('tabs', [...tabs.map(t => t.id), tab.id]);
  await session.set('current', tab.id);
  return tab;
}

// ---- Debugger ---------------------------------------------------------------------------

const attached = new Set();
let overlaySource;

async function cdp(tabId, method, params = {}) {
  if (!COMMANDS.has(method)) throw failure(`DevTools command ${method} is not allowed`);
  return chrome.debugger.sendCommand({tabId}, method, params);
}

async function detach(tabId) {
  attached.delete(tabId);
  await chrome.debugger.detach({tabId}).catch(() => {});
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
  // Only the overlay's own world sees the take-over signal and the overlay controls, so page
  // scripts can neither hand control back nor silence the user's take-over.
  await cdp(tabId, 'Runtime.addBinding', {name: '__webagentUser', executionContextName: WORLD});
  await cdp(tabId, 'Page.addScriptToEvaluateOnNewDocument', {source: overlaySource, worldName: WORLD});
  await inWorld(tabId, overlaySource).catch(() => {});
}

// The overlay world of the tab's current document (created once per document, then reused).
async function world(tabId) {
  const {frameTree} = await cdp(tabId, 'Page.getFrameTree');
  const {executionContextId} = await cdp(tabId, 'Page.createIsolatedWorld',
    {frameId: frameTree.frame.id, worldName: WORLD});
  return executionContextId;
}

// A document that is still loading may replace the world between the two calls: try again.
async function inWorld(tabId, expression) {
  for (let attempt = 1; ; attempt += 1) {
    try {
      return await cdp(tabId, 'Runtime.evaluate', {expression, contextId: await world(tabId), returnByValue: true});
    } catch (error) {
      if (attempt >= 3 || !/context/i.test(String(error.message))) throw error;
      await sleep(150);
    }
  }
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

// The overlay script returns at once where it already runs, so a new world gets it here too.
async function overlay(tabId, call) {
  await inWorld(tabId, `${overlaySource}\nwindow.__webagentOverlay?.${call}`);
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

// The tab after an action; one that has left the list shows only its host.
async function page(tabId, rules) {
  return describeTab(await chrome.tabs.get(tabId), await context(rules));
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

// The web address of the frame a click at (x, y) would land in: the hit node's own frame, or the
// frame a frame element (iframe, frame, object, embed) shows there. A child frame in another
// process is not in this tab's frame tree; its element's address stands in, and a frame whose
// address cannot be told is refused (cross-site frames run in another process).
const attribute = (node, name) => {
  const list = node.attributes || [];
  for (let i = 0; i + 1 < list.length; i += 2) if (list[i] === name) return list[i + 1];
  return null;
};
async function frameAt(tabId, x, y) {
  const {frameTree} = await cdp(tabId, 'Page.getFrameTree');
  const urls = new Map();
  const walk = tree => { urls.set(tree.frame.id, tree.frame.url); (tree.childFrames || []).forEach(walk); };
  walk(frameTree);
  const top = frameTree.frame;
  const unknown = 'The click would land in a frame whose address cannot be read; the agent does not click there';
  const hit = await cdp(tabId, 'DOM.getNodeForLocation', {x: Math.round(x), y: Math.round(y), includeUserAgentShadowDOM: true});
  let url = top.url;
  if (hit.frameId && hit.frameId !== top.id) {
    url = urls.get(hit.frameId);
    if (!url) throw failure(unknown);
  }
  const {node} = await cdp(tabId, 'DOM.describeNode', {backendNodeId: hit.backendNodeId});
  if (/^(IFRAME|FRAME|OBJECT|EMBED|FENCEDFRAME|PORTAL)$/.test(node.nodeName || '')) {
    const source = attribute(node, 'src') ?? attribute(node, 'data');
    let shown = node.frameId && urls.get(node.frameId);
    if (!shown && source) { try { shown = new URL(source, url).href; } catch { /* refused below */ } }
    if (!shown) throw failure(unknown);
    url = shown;
  }
  return url;
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
      // A heading without a computed name keeps its text in its children.
      if (INTERACTIVE.has(role) || (role === 'heading' && name)) return;
    }
    for (const child of node.childIds || []) walk(byId.get(child), depth + 1);
  };
  walk(nodes[0], 0);
  labels.set(tabId, names);
  const text = lines.join('\n');
  const part = text.slice(offset, offset + READ_LIMIT);
  const next = offset + READ_LIMIT < text.length ? offset + READ_LIMIT : null;
  const tab = await chrome.tabs.get(tabId);
  return {tab: tabId, url: tab.url, title: tab.title, text: part, next_offset: next};
}

// ---- Actions ----------------------------------------------------------------------------

async function act(args, rules) {
  if (await session.get('paused')) {
    throw failure('The user took over this browser window. Wait until they hand control back, then read the page again.');
  }
  const {action} = args;
  if (action === 'tabs') {
    const current = await session.get('current');
    const ctx = await context(rules);
    return {tabs: (await agentTabs()).map(t => ({...describeTab(t, ctx), current: t.id === current}))};
  }
  if ((action === 'open' || action === 'navigate') && !siteAllowed(args.url, rules.sites, rules.blocked)) {
    throw failure(`${args.url} is not on this agent's site list, or is an address the agent never opens`);
  }
  const unagreed = (action === 'open' || action === 'navigate') && consentRefusal(args.url, rules.sites, rules.consent);
  if (unagreed) throw failure(unagreed);
  if (action === 'open') {
    const tab = await openTab(args.url);
    await attach(tab.id);
    await settle(tab.id);
    // A redirect to a forbidden host lets go of the tab and says so.
    await guard(tab.id, 'navigate', rules);
    return page(tab.id, rules);
  }
  // Checked at every action against where the tab is now; navigate's target was checked
  // above, so it may also lead off a page that left the list.
  const tab = await guard((await currentTab(args.tab)).id, action, rules);
  if (action === 'close') {
    await detach(tab.id);
    await chrome.tabs.remove(tab.id);
    return {closed: tab.id};
  }
  // Before each input: the page may have moved since the check above.
  const still = () => guard(tab.id, action, rules);
  await attach(tab.id);
  await overlay(tab.id, 'user(false)');
  if (action === 'navigate') {
    await overlay(tab.id, `caption(${JSON.stringify('Opening ' + args.url)})`);
    await still();
    await cdp(tab.id, 'Page.navigate', {url: args.url});
    await settle(tab.id);
    return page(tab.id, rules);
  }
  if (action === 'read') {
    await overlay(tab.id, 'caption("Reading the page")');
    const before = await still();
    const text = await read(tab.id, args.offset || 0);
    // A page that moved during the read is not returned: it may be somewhere else now.
    const after = await still();
    if (after.url !== before.url) throw failure('The page changed while it was read; read again');
    return text;
  }
  const label = labels.get(tab.id)?.get(args.ref) || 'the page';
  if (action === 'click') {
    const {x, y} = await pointAt(tab.id, args.ref, 'Clicking ' + label);
    await overlay(tab.id, 'expect(800)');
    const here = await still();
    const intoFrame = frameRefusal(await frameAt(tab.id, x, y), here.url, rules.sites);
    if (intoFrame) { await overlay(tab.id, 'box(null)').catch(() => {}); throw failure(intoFrame); }
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
    return {...await page(tab.id, rules), clicked: label};
  }
  if (action === 'type') {
    await pointAt(tab.id, args.ref, 'Typing into ' + label);
    await cdp(tab.id, 'DOM.focus', {backendNodeId: args.ref});
    // Resolved in the overlay's world, so page scripts cannot fake the password check.
    const {object} = await cdp(tab.id, 'DOM.resolveNode',
      {backendNodeId: args.ref, executionContextId: await world(tab.id)});
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
        await still();
        await cdp(tab.id, 'Input.insertText', {text: chunk});
        typed += chunk;
        await overlay(tab.id, `caption(${JSON.stringify(`Typing into ${label}: ` + (secret ? '•'.repeat(typed.length) : typed).slice(-60))})`);
        await sleep(25);
      }
      if (args.submit) { await still(); await key(tab.id, 'Enter'); }
    } catch (error) {
      throw failure(String(error.message || error), true);
    }
    await overlay(tab.id, 'box(null)');
    await settle(tab.id);
    return {...await page(tab.id, rules), typed_into: label, submitted: args.submit === true};
  }
  if (action === 'press') {
    if (args.ref != null) {
      await pointAt(tab.id, args.ref, `Pressing ${args.key} on ${label}`);
      await cdp(tab.id, 'DOM.focus', {backendNodeId: args.ref});
    } else {
      await overlay(tab.id, `caption(${JSON.stringify('Pressing ' + args.key)})`);
    }
    await still();
    try { await key(tab.id, args.key); } catch (error) { throw failure(String(error.message || error), true); }
    await settle(tab.id);
    return {...await page(tab.id, rules), pressed: args.key};
  }
  if (action === 'scroll') {
    const {width, height} = await evaluate(tab.id, '({width: innerWidth, height: innerHeight})');
    await overlay(tab.id, 'caption("Scrolling")');
    await overlay(tab.id, 'expect(600)');
    await still();
    await cdp(tab.id, 'Input.dispatchMouseEvent', {type: 'mouseWheel', x: width / 2, y: height / 2,
      deltaX: 0, deltaY: args.dy || 600});
    await sleep(300);
    return {...await page(tab.id, rules), scroll_y: await evaluate(tab.id, 'scrollY')};
  }
  throw failure('Unknown action ' + action);
}
