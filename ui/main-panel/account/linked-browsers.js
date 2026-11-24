'use strict';
// Linked browsers (Manage account): the WebAgent browser extensions linked to this account,
// each revocable, and the sites shared agents asked to use in this person's browser. A
// member of someone else's agent agrees to each site once; the server and the extension
// both refuse a site without that agreement.
import { requestTouchVibration } from '../../shared/vibrate/vibrate.js';

export function mountLinkedBrowsers(host, request) {
  let disposed = false, generation = 0;
  const node = (tag, text, cls) => { const n = document.createElement(tag); if (text) n.textContent = text; if (cls) n.className = cls; return n; };
  const when = seconds => seconds ? new Date(seconds * 1000).toLocaleString() : 'never';
  const status = node('p', '', 'account-status'); status.setAttribute('role', 'status');
  const row = (title, detail, buttons) => {
    const line = node('div', null, 'account-signin-row');
    const details = node('div', null, 'account-signin-details');
    details.append(node('strong', title), node('span', detail));
    const actions = node('div', null, 'account-linked-actions');
    actions.append(...buttons);
    line.append(details, actions);
    return line;
  };
  const button = (label, primary, action, failure) => {
    const b = node('button', label, 'account-btn' + (primary ? ' account-btn-primary' : '')); b.type = 'button';
    b.onclick = async event => {
      requestTouchVibration(event);
      b.disabled = true;
      try { await action(); if (!disposed) await load(); }
      catch (error) { if (!disposed) { status.textContent = failure + ' ' + error.message; b.disabled = false; } }
    };
    return b;
  };
  async function load() {
    const current = ++generation;
    try {
      const [{ devices = [] }, { consents = [] }, roster] = await Promise.all([
        request('/browser/devices'), request('/browser/consents'),
        request('/models/agents').catch(() => ({ agents: [] }))]);
      if (disposed || current !== generation) return;
      const names = new Map((roster.agents || []).map(agent => [agent.id, agent.name || agent.display_name || agent.id]));
      const agent = id => names.get(id) || id;
      const browsers = node('div');
      for (const device of devices) {
        const state = device.outdated ? ' · out of date, update the extension' : device.online ? ' · online' : '';
        browsers.append(row(device.name || 'Browser', 'Linked ' + when(device.created) + ' · last used ' + when(device.last_seen) + state, [
          button('Revoke', false, () => request('/browser/devices/' + encodeURIComponent(device.id), 'DELETE'), 'Could not revoke the browser.')]));
      }
      if (!devices.length) browsers.append(node('p', 'No browser is linked. The WebAgent browser extension links itself while you are signed in here.', 'account-session-help'));
      const asked = consents.filter(c => c.state === 'asked'), allowed = consents.filter(c => c.state === 'allowed');
      const sites = node('div');
      for (const consent of asked) {
        sites.append(row(consent.host, agent(consent.agent_id) + ' asks to use this site in your browser', [
          button('Allow', true, () => request('/browser/consents/' + consent.id, 'POST'), 'Could not allow the site.'),
          button('Decline', false, () => request('/browser/consents/' + consent.id, 'DELETE'), 'Could not decline the site.')]));
      }
      for (const consent of allowed) {
        sites.append(row(consent.host, 'Allowed for ' + agent(consent.agent_id) + ' on ' + when(consent.at), [
          button('Remove', false, () => request('/browser/consents/' + consent.id, 'DELETE'), 'Could not remove the site.')]));
      }
      if (!consents.length) sites.append(node('p', 'No shared agent has asked to use a site in your browser.', 'account-session-help'));
      host.replaceChildren(
        node('h2', 'Linked browsers', 'account-section-title'),
        node('p', 'Browsers where the WebAgent extension lets your agents work. Revoke one to end its link at once; the extension stops at its next request.', 'account-session-help'),
        browsers,
        node('h3', 'Sites for shared agents', 'account-linked-subtitle'),
        node('p', 'An agent someone else runs may use a site in your browser only after you allow it here. Removing a site stops it at the agent’s next step.', 'account-session-help'),
        sites,
        status);
      status.textContent = '';
    } catch (error) {
      if (!disposed && current === generation) host.replaceChildren(node('h2', 'Linked browsers', 'account-section-title'), node('p', error.message, 'account-status'));
    }
  }
  load();
  return () => { disposed = true; generation++; host.replaceChildren(); };
}
