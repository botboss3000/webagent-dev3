import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import test from 'node:test';
import {COMMANDS, consentRefusal, describeTab, forbiddenHost, frameRefusal, siteAllowed, tabRefusal} from './guard.js';

const APP = ['dev.webagent.live'];
const rules = {sites: ['example.com'], blocked: APP, group: 7};
const tab = (url, extra = {}) => ({id: 3, groupId: 7, url, title: 'Page', ...extra});

test('a click queued for a tab that has navigated to the app host is refused', () => {
  const refusal = tabRefusal('click', tab('https://dev.webagent.live/app?tab=admin'), rules);
  assert.match(refusal, /own address/);
  // Listed by the server or not: the app host is blocked even with it on the site list.
  const listed = {...rules, sites: ['example.com', 'dev.webagent.live', 'webagent.live']};
  for (const action of ['read', 'click', 'type', 'press', 'scroll', 'navigate']) {
    assert.match(tabRefusal(action, tab('https://dev.webagent.live/app'), listed), /never acts there/, action);
  }
  // The same click on a listed site goes ahead, and a tab on the app may still be closed.
  assert.equal(tabRefusal('click', tab('https://www.example.com/a'), rules), null);
  assert.equal(tabRefusal('close', tab('https://dev.webagent.live/app'), rules), null);
});

test('a page leaving for the app host is judged by where it goes', () => {
  const leaving = tab('https://example.com/a', {pendingUrl: 'https://dev.webagent.live/app'});
  assert.match(tabRefusal('type', leaving, rules), /own address/);
});

test('every action is checked against the tab address, not only open and navigate', () => {
  for (const action of ['read', 'click', 'type', 'press', 'scroll']) {
    assert.match(tabRefusal(action, tab('https://other.test/'), rules), /not on this agent's site list/, action);
  }
  // Off the list but not forbidden: navigating back to a listed address is still possible.
  assert.equal(tabRefusal('navigate', tab('https://other.test/'), rules), null);
  assert.match(tabRefusal('read', tab('', {pendingUrl: ''}), rules), /no address yet/);
});

test('loopback, private, link-local, Tailscale hosts and IP literals are refused even when listed', () => {
  const hosts = ['localhost', 'app.localhost', '127.0.0.1', '10.1.2.3', '192.168.1.10', '172.16.0.5',
    '169.254.169.254', '100.87.9.65', '[::1]', 'nas.local', 'db.corp.internal', 'printer.lan',
    'router.home.arpa', 'ar-server.tail1234.ts.net'];
  for (const host of hosts) {
    assert.ok(forbiddenHost(host, APP), host);
    const listed = {...rules, sites: [host.replace(/[[\]]/g, '')]};
    assert.equal(siteAllowed(`http://${host}/`, listed.sites, APP), false, host);
    assert.match(tabRefusal('read', tab(`http://${host}/x`), listed), /never acts there/, host);
  }
  // The URL parser turns other spellings of an address into the dotted form first.
  assert.ok(forbiddenHost(new URL('http://0x7f.1/').hostname, APP));
  assert.ok(forbiddenHost(new URL('http://2130706433/').hostname, APP));
  assert.equal(forbiddenHost('crm.example.com', APP), null);
  assert.equal(siteAllowed('https://crm.example.com/x', ['example.com'], APP), true);
  assert.equal(siteAllowed('file:///etc/passwd', ['example.com'], APP), false);
});

test('the agent acts only on tabs in its own tab group', () => {
  assert.match(tabRefusal('click', tab('https://example.com/', {groupId: 9}), rules), /tab group/);
  assert.match(tabRefusal('close', tab('https://example.com/', {groupId: -1}), rules), /tab group/);
  // With no group recorded (state lost), nothing is the agent's.
  assert.match(tabRefusal('read', tab('https://example.com/'), {...rules, group: null}), /tab group/);
});

test('only a fixed set of DevTools commands is sent, none reading cookies, storage or other targets', () => {
  for (const method of ['Network.getCookies', 'Network.getAllCookies', 'Storage.getCookies',
    'DOMStorage.getDOMStorageItems', 'IndexedDB.requestData', 'Target.attachToTarget',
    'Target.setAutoAttach', 'Target.createTarget', 'Browser.getVersion', 'Browser.setDownloadBehavior',
    'Network.enable', 'Fetch.enable', 'Page.captureScreenshot']) {
    assert.equal(COMMANDS.has(method), false, method);
  }
  assert.ok([...COMMANDS].every(m => /^(Page|Runtime|Accessibility|DOM|Input)\./.test(m)));
  // Every command the service worker sends is on the list (and nothing calls the debugger
  // directly except through cdp()).
  const source = readFileSync(new URL('./background.js', import.meta.url), 'utf8');
  const used = [...source.matchAll(/cdp\([^,]+, '([A-Za-z]+\.[A-Za-z]+)'/g)].map(m => m[1]);
  assert.ok(used.length > 15);
  for (const method of used) assert.ok(COMMANDS.has(method), method);
  assert.equal((source.match(/chrome\.debugger\.sendCommand/g) || []).length, 1);
});

test('tabs the agent may not act on show only their host', () => {
  assert.deepEqual(describeTab(tab('https://dev.webagent.live/app'), rules),
    {tab: 3, host: 'dev.webagent.live', not_allowed: true});
  assert.deepEqual(describeTab(tab('https://example.com/a'), rules),
    {tab: 3, url: 'https://example.com/a', title: 'Page'});
});

test('the overlay runs in its own script world, out of the page scripts reach', () => {
  const source = readFileSync(new URL('./background.js', import.meta.url), 'utf8');
  assert.match(source, /'Runtime\.addBinding', \{name: '__webagentUser', executionContextName: WORLD\}/);
  assert.match(source, /'Page\.addScriptToEvaluateOnNewDocument', \{source: overlaySource, worldName: WORLD\}/);
  assert.doesNotMatch(source, /expression: `window\.__webagentOverlay/);
});

test('a member of a shared agent acts only on sites they agreed to', () => {
  const member = {...rules, sites: ['example.com', 'crm.test'], consent: ['example.com']};
  // Agreed: example.com and its subdomains; not agreed: crm.test, refused for every page action.
  assert.equal(tabRefusal('click', tab('https://www.example.com/a'), member), null);
  for (const action of ['read', 'click', 'type', 'press', 'scroll']) {
    assert.match(tabRefusal(action, tab('https://crm.test/x'), member), /not agreed/, action);
  }
  assert.match(consentRefusal('https://crm.test/', member.sites, member.consent), /crm\.test.*Linked browsers/);
  assert.equal(consentRefusal('https://app.example.com/', member.sites, member.consent), null);
  // Nothing agreed yet refuses everything; owners and operators get no consent list at all.
  assert.match(tabRefusal('read', tab('https://www.example.com/'), {...member, consent: []}), /not agreed/);
  assert.equal(consentRefusal('https://crm.test/', member.sites, null), null);
  assert.equal(tabRefusal('read', tab('https://crm.test/'), {...rules, sites: ['crm.test']}), null);
  // Closing stays possible, and navigating away is judged by its target in act().
  assert.equal(tabRefusal('close', tab('https://crm.test/x'), member), null);
  assert.equal(tabRefusal('navigate', tab('https://crm.test/x'), member), null);
  const source = readFileSync(new URL('./background.js', import.meta.url), 'utf8');
  assert.match(source, /consentRefusal\(args\.url, rules\.sites, rules\.consent\)/);
  assert.match(source, /consent: call\.consent \?\? null/);
});

test('a click into a frame from another site is refused, the page and its own frames are not', () => {
  const sites = ['site-a.test', 'example.com'];
  const page = 'https://www.site-a.test/form';
  assert.match(frameRefusal('https://ads.other.test/frame', page, sites), /frame from ads\.other\.test/);
  // A listed site in a frame of another listed site is still another site.
  assert.match(frameRefusal('https://example.com/embed', page, sites), /another site/);
  assert.match(frameRefusal('blob:https://other.test/1f0e', page, sites), /other\.test/);
  for (const same of [page, 'https://www.site-a.test/other', 'https://cdn.site-a.test/widget',
    'about:blank', 'about:srcdoc', 'data:text/html,hi', 'blob:https://www.site-a.test/1f0e']) {
    assert.equal(frameRefusal(same, page, sites), null, same);
  }
  const source = readFileSync(new URL('./background.js', import.meta.url), 'utf8');
  assert.match(source, /frameRefusal\(await frameAt\(tab\.id, x, y\), here\.url, rules\.sites\)/);
  // The check runs before the mouse is pressed.
  assert.ok(source.indexOf('frameAt(tab.id, x, y)') < source.indexOf("type: 'mousePressed'"));
});

test('the extension names its version and asks before linking to another account', () => {
  const source = readFileSync(new URL('./background.js', import.meta.url), 'utf8');
  const manifest = JSON.parse(readFileSync(new URL('./manifest.json', import.meta.url), 'utf8'));
  assert.equal(manifest.version, '0.3.0');
  assert.match(source, /'x-webagent-extension': VERSION/);
  assert.match(source, /response\.status === 426/);
  // Preview first; only a different account (or app) than the last asks; the same one stays silent.
  assert.ok(source.indexOf('preview: true') < source.indexOf("api(previous, '/unlink')"));
  assert.match(source, /last\.user_id !== target\.user_id \|\| last\.origin !== origin/);
  assert.match(source, /await confirmLink\(/);
  // Only the extension's own confirmation page can answer.
  assert.match(source, /sender\.url === chrome\.runtime\.getURL\('confirm\.html'\)/);
  const install = readFileSync(new URL('./install.js', import.meta.url), 'utf8');
  for (const file of ['confirm.html', 'confirm.js']) assert.ok(install.includes(`'${file}'`), file);
});
