// Remote sites belong to the signed-in user; credentials never come back from the API.
import { buildConfigRow } from '../../../../shared/ability-row-controls.js';
import { requestTouchVibration } from '../../../../shared/vibrate.js';
const node = (tag, text = '') => { const el = document.createElement(tag); el.textContent = text; return el; };
const fields = [
  ['github_owner', 'GitHub owner', ''], ['github_repo', 'GitHub repository', ''],
  ['production_branch', 'Production branch', 'main'], ['cloudflare_account_id', 'Cloudflare account ID', ''],
  ['worker_name', 'Worker name', ''], ['build_token_uuid', 'Cloudflare build-token UUID', ''],
  ['root_directory', 'Repository root', '/'],
  ['build_command', 'Remote build command', 'npm install --prefix /tmp/webagent-wrangler --ignore-scripts --no-audit --no-fund --package-lock=false --registry=https://registry.npmjs.org wrangler@4.145.0'],
];
export function buildSourceControlControls({ data, request, scope, enabled, status, rerender }) {
  const content = node('div'); content.className = 'ac-list';
  content.append(node('p', 'Remote sites use GitHub for source and Cloudflare for builds and previews. Nothing is cloned or built on WebAgent. Connections belong to your user; credentials use the private vault panel. Live provider testing remains pending until configured.'));
  if (!data || data.load_failed) { content.append(node('p', 'Remote site connections are unavailable. Reload to retry.')); return buildConfigRow({ name: 'Remote sites', control: content }); }
  const sites = data.sites || [];
  const add = site => {
    const form = node('form'); form.className = 'ac-list';
    const controls = {};
    form.append(node('strong', site ? site.id : 'New remote site'));
    const input = (key, title, value, type = 'text') => {
      const el = node('input'); el.className = 'ac-input'; el.type = type; el.value = value;
      el.maxLength = type === 'password' ? 4096 : 512; el.autocomplete = type === 'password' ? 'new-password' : 'off';
      el.setAttribute('aria-label', title); el.disabled = !enabled; controls[key] = el;
      form.append(buildConfigRow({ name: title, control: el })); return el;
    };
    const id = input('id', 'Site ID', site?.id || ''); id.required = true; id.disabled = !!site || !enabled;
    for (const [key, title, fallback] of fields) { const el = input(key, title, site?.configuration?.[key] ?? fallback); el.required = true; el.readOnly = key === 'build_command' || site?.runtime?.provisioned === true; }
    if (site) {
      for (const entry of site.credential_requests || []) {
        const row=node('p'); row.textContent='Vault key: '+entry.name+' · '+entry.origin;
        form.append(row);
      }
      form.append(node('p','Ask the agent to call request_credential for the keys above. Enter tokens only in the private credential panel. The agent receives saved status, never values.'));
    }
    const revoke = input('revoked', 'Revoke connection', '', 'checkbox'); revoke.checked = !!site?.revoked;
    const button = node('button', site ? 'Save remote site' : 'Add remote site');
    button.type = 'submit'; button.className = 'ac-btn ac-btn-sm'; button.disabled = !enabled;
    button.onclick = event => { if (!button.disabled) requestTouchVibration(event); };
    form.append(button);
    form.onsubmit = async event => {
      event.preventDefault(); if (!enabled || button.disabled || !form.reportValidity()) return;
      const configuration = Object.fromEntries(fields.map(([key]) => [key, controls[key].value]));
      if (!/^[A-Za-z0-9_-]{1,80}$/.test(id.value)) { status.textContent = 'Use letters, numbers, underscore or hyphen for the site ID.'; return; }
      button.disabled = true; for (const control of Object.values(controls)) control.disabled = true;
      status.textContent = 'Saving your remote site connection…';
      try {
        await request(scope + '/source-control/sites/' + encodeURIComponent(id.value), 'PUT', {
          expected_revision: site?.revision || 0, configuration, revoked: revoke.checked,
        });
        await rerender({ administratorNotice: 'Remote site connection saved. Ask the agent to create or connect the site.' });
      } catch (error) {
        status.textContent = error?.status === 409 ? 'Connection changed. Reload before saving again.' : 'Remote site could not be saved. Check fields and your authority.';
        button.disabled = !enabled; for (const control of Object.values(controls)) control.disabled = !enabled;
        id.disabled = !!site || !enabled;
      }
    };
    content.append(form);
  };
  for (const site of sites) add(site);
  if (sites.length < 32) add(null);
  content.append(node('p', 'One-time setup: authorize the Cloudflare GitHub App for the repository and configure a Cloudflare build token. GitHub tokens need repository Contents access; creating repositories needs additional account permission. Cloudflare tokens need the selected account’s Workers and Workers CI permissions. Public previews may remain reachable until Cloudflare deletion propagates; WebAgent verifies cleanup.'));
  return buildConfigRow({ name: 'Remote sites', control: content });
}
