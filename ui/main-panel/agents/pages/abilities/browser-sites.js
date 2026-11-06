// Saved site sign-ins belong to the signed-in user; cookies and vault values never come back from the API.
import { requestTouchVibration } from '../../../../shared/vibrate.js';
const node = (tag, cls = '', text = '') => {
  const el = document.createElement(tag); el.className = cls; el.textContent = text; return el;
};
const when = seconds => seconds ? new Date(seconds * 1000).toLocaleString() : 'never';
const states = { signed_in: 'Signed in', expired: 'Needs sign-in again', failed: 'Sign-in failed' };

export function buildBrowserSites({ request }) {
  const content = node('div');
  content.style.overflowWrap = 'anywhere';
  content.append(
    node('p', 'ac-hint', 'Sites the browser stays signed in to for you. The WebAgent app always signs in by itself (Sign in to WebAgent above).'),
    node('p', 'ac-hint', 'To add one, ask the agent in a chat to set up your sign-in for the site. It asks you to save your user name, password and any authenticator secret in the private credential panel, signs in with them, then asks you to approve saving the sign-in.'),
    node('p', 'ac-hint', 'The agent only names vault entries and never sees a password, code or cookie. Each value is typed only into sign-in fields on its own site, and each saved sign-in opens only its own site. Removing a site deletes its saved sign-in; your vault entries stay.'));
  const list = node('div');
  const notice = node('p', 'ac-hint', 'Loading signed-in sites…');
  notice.setAttribute('role', 'status');
  content.append(list, notice);
  const load = async () => {
    try {
      const { sites = [] } = await request('/browser/sites');
      list.replaceChildren();
      for (const site of sites) {
        const card = node('div', 'conn-card');
        const steps = site.steps ? site.steps + ' sign-in steps' : 'no sign-in steps';
        card.append(node('strong', '', site.origin),
          node('p', 'ac-hint', (states[site.status] || 'Saved') + ' · ' + steps),
          node('p', 'ac-hint', 'Saved ' + when(site.saved_at) + ' · last used ' + when(site.last_used)));
        if (site.vault?.length) card.append(node('p', 'ac-hint', 'Vault entries: ' + site.vault.join(', ')));
        const remove = node('button', 'ac-btn ac-btn-sm', 'Remove sign-in'); remove.type = 'button';
        remove.onclick = async event => {
          requestTouchVibration(event);
          if (!window.confirm('Remove the saved sign-in for ' + site.origin + '?')) return;
          remove.disabled = true;
          try { await request('/browser/sites/' + encodeURIComponent(site.id), 'DELETE'); await load(); }
          catch { notice.textContent = 'Could not remove the sign-in. Reload and try again.'; remove.disabled = false; }
        };
        card.append(remove);
        list.append(card);
      }
      notice.textContent = sites.length ? '' : 'No saved sign-ins yet.';
    } catch {
      notice.textContent = 'Signed-in sites are unavailable. Reload to retry.';
    }
  };
  load();
  // Name above its content, like the text settings, so the explanation wraps on phones.
  const stacked = node('div', 'ac-gate-param'), slot = node('div', 'ac-gate-param-control');
  slot.append(content);
  stacked.append(node('div', 'ac-gate-param-name', 'Signed-in sites'), slot);
  return stacked;
}
