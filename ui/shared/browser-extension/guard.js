// The extension's own safety rules, kept free of Chrome APIs so node --test can check them.
// The server checks open and navigate addresses before queuing; these rules hold here anyway,
// at every action, against the tab's address at that moment (BROWSER-5).

// The only DevTools Protocol commands the extension sends. Nothing here reads cookies or
// storage, attaches to other targets, or reaches browser-wide state.
export const COMMANDS = new Set([
  'Page.enable', 'Page.addScriptToEvaluateOnNewDocument', 'Page.navigate', 'Page.getFrameTree',
  'Page.createIsolatedWorld',
  'Runtime.enable', 'Runtime.addBinding', 'Runtime.evaluate', 'Runtime.callFunctionOn',
  'Accessibility.enable', 'Accessibility.getFullAXTree',
  'DOM.getDocument', 'DOM.scrollIntoViewIfNeeded', 'DOM.getBoxModel', 'DOM.focus', 'DOM.resolveNode',
  'DOM.getNodeForLocation', 'DOM.describeNode',
  'Input.dispatchMouseEvent', 'Input.dispatchKeyEvent', 'Input.insertText',
]);

// Name suffixes that only resolve inside a private network (as the server's list).
const PRIVATE_SUFFIXES = ['localhost', 'local', 'localdomain', 'internal', 'intranet', 'lan',
  'home.arpa', 'ts.net', 'tailscale.net'];

const related = (a, b) => a === b || a.endsWith('.' + b);

export function hostOf(url) {
  try { return new URL(url).hostname.toLowerCase().replace(/\.$/, ''); } catch { return ''; }
}

// Why the agent may never act on `host`, or null when only the site list decides. `blocked`
// holds the app's own hosts: from the queued call and from the address this extension is
// linked to.
export function forbiddenHost(host, blocked) {
  host = String(host || '').toLowerCase().replace(/\.$/, '');
  if (blocked.some(app => app && (related(host, app) || related(app, host)))) return "this app's own address";
  const last = host.split('.').at(-1);
  // The URL parser has already turned 0x7f.1 or 2130706433 into a dotted address.
  if (host.includes(':') || host.includes('[') || /^(\d|0x)/.test(last)) return 'an IP address';
  if (PRIVATE_SUFFIXES.some(suffix => related(host, suffix))) return 'a private-network address';
  return null;
}

// Whether the agent may load `url`: http(s), on the list, and not forbidden.
export function siteAllowed(url, sites, blocked) {
  let parsed;
  try { parsed = new URL(url); } catch { return false; }
  if (!['http:', 'https:'].includes(parsed.protocol)) return false;
  const host = hostOf(url);
  if (forbiddenHost(host, blocked)) return false;
  return sites.some(site => related(host, site));
}

// D3: a member of someone else's agent agrees to each listed site first. The server then sends
// `consent`, the entries they agreed to; null means the person owns or operates the agent.
export function consentRefusal(url, sites, consent) {
  if (!Array.isArray(consent)) return null;
  const host = hostOf(url);
  const entry = sites.find(site => related(host, site));
  if (entry && consent.includes(entry)) return null;
  return `The user has not agreed to let this agent use ${entry || host} in their browser; they allow it under Manage account → Linked browsers`;
}

// D6: a click lands where it is aimed, so one aimed into a frame from another site than the tab's
// own would act on that site. Same site: the same host, or both under one entry of the site
// list. A frame without a web address (about:blank, srcdoc, data:) belongs to its page; a blob:
// address is judged by the origin inside it.
export function frameRefusal(frameUrl, tabUrl, sites) {
  const url = String(frameUrl || '').replace(/^blob:/i, '');
  if (!/^https?:/i.test(url)) return null;
  const frame = hostOf(url), tab = hostOf(tabUrl);
  if (frame === tab) return null;
  const entry = host => sites.find(site => related(host, site));
  if (entry(tab) && entry(frame) === entry(tab)) return null;
  return `The click would land in a frame from ${frame}, another site than this tab's ${tab}; the agent does not click inside other sites' frames`;
}

// The refusal for `action` on `tab` as it is now, or null. A tab on a forbidden host may only
// be closed; a tab off the list may also be navigated (to a checked address); a tab outside
// the agent's own group is not the agent's; a site the user has not agreed to (D3) is refused like
// one off the list. `pendingUrl` counts too: a page that is leaving is judged by where it goes.
export function tabRefusal(action, tab, {sites, blocked, group, consent = null}) {
  if (group == null || tab.groupId !== group) {
    return 'This tab is not in the WebAgent tab group, so the agent does not act on it; open a new tab';
  }
  if (action === 'close') return null;
  const urls = [tab.url, tab.pendingUrl].filter(Boolean);
  if (!urls.length) return 'The agent tab has no address yet; read it again in a moment';
  for (const url of urls) {
    const host = hostOf(url);
    const reason = /^https?:/.test(url) ? forbiddenHost(host, blocked) : null;
    if (reason) return `The agent tab is on ${host}, ${reason}; the agent never acts there and may only close the tab`;
    if (action !== 'navigate' && !siteAllowed(url, sites, blocked)) {
      return `The agent tab is on ${host || url}, which is not on this agent's site list`;
    }
    const unagreed = action !== 'navigate' && consentRefusal(url, sites, consent);
    if (unagreed) return unagreed;
  }
  return null;
}

// What `tabs` and action results show of a tab: a tab the agent may not act on shows only its
// host, not its title or full address.
export function describeTab(tab, context) {
  const shown = tabRefusal('read', tab, context) === null;
  return shown ? {tab: tab.id, url: tab.url, title: tab.title}
    : {tab: tab.id, host: hostOf(tab.url), not_allowed: true};
}
