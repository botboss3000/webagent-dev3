// Remote sites belong to the signed-in user; credentials never come back from the API.
import { buildConfigRow } from './ability-row-controls/ability-row-controls.js';
import { createConfigList, createTextRow, createToggleRow, createConfigForm } from '../../../../shared/config-rows/config-rows.js';
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
    const section = node('div');
    section.append(node('strong', site ? site.id : 'New remote site'));
    const list = createConfigList(section, { label: site ? site.id : 'New remote site' });
    // [row, stays disabled]: the site ID is fixed once saved; the build command and a provisioned site are read-only.
    const rows = [];
    const form = createConfigForm(section, { saveLabel: site ? 'Save remote site' : 'Add remote site', onSave: async values => {
      if (!enabled) return;
      const configuration = Object.fromEntries(fields.map(([key]) => [key, values[key]]));
      const problem = !values.id || Object.values(configuration).some(value => !value) ? 'Fill in every field before saving.'
        : !/^[A-Za-z0-9_-]{1,80}$/.test(values.id) ? 'Use letters, numbers, underscore or hyphen for the site ID.' : '';
      if (problem) { status.textContent = problem; throw new Error(problem); }
      for (const [handle] of rows) handle.setDisabled(true);
      status.textContent = 'Saving your remote site connection…';
      try {
        await request(scope + '/source-control/sites/' + encodeURIComponent(values.id), 'PUT', {
          expected_revision: site?.revision || 0, configuration, revoked: values.revoked,
        });
        await rerender({ administratorNotice: 'Remote site connection saved. Ask the agent to create or connect the site.' });
      } catch (error) {
        status.textContent = error?.status === 409 ? 'Connection changed. Reload before saving again.' : 'Remote site could not be saved. Check fields and your authority.';
        for (const [handle, locked] of rows) handle.setDisabled(locked);
        throw new Error(status.textContent);
      }
    } });
    const text = (key, title, value, locked) => rows.push([createTextRow(list, { title, value, maxLength: 512,
      stacked: key === 'build_command', disabled: !enabled || locked, key, form }), !enabled || locked]);
    text('id', 'Site ID', site?.id || '', !!site);
    for (const [key, title, fallback] of fields) text(key, title, site?.configuration?.[key] ?? fallback,
      key === 'build_command' || site?.runtime?.provisioned === true);
    if (site) {
      for (const entry of site.credential_requests || []) list.append(node('p', 'Vault key: ' + entry.name + ' · ' + entry.origin));
      list.append(node('p', 'Ask the agent to call request_credential for the keys above. Enter tokens only in the private credential panel. The agent receives saved status, never values.'));
    }
    rows.push([createToggleRow(list, { title: 'Revoke connection', value: !!site?.revoked, disabled: !enabled,
      key: 'revoked', form }), !enabled]);
    form.attach();
    content.append(section);
  };
  for (const site of sites) add(site);
  if (sites.length < 32) add(null);
  content.append(node('p', 'One-time setup: authorize the Cloudflare GitHub App for the repository and configure a Cloudflare build token. GitHub tokens need repository Contents access; creating repositories needs additional account permission. Cloudflare tokens need the selected account’s Workers and Workers CI permissions. Public previews may remain reachable until Cloudflare deletion propagates; WebAgent verifies cleanup.'));
  return buildConfigRow({ name: 'Remote sites', control: content });
}
