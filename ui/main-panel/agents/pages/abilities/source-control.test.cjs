const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
class Element {
  constructor(tag) { this.tag = tag; this.children = []; this.textContent = ''; this.value = ''; this.disabled = false; this.checked = false; }
  append(...children) { this.children.push(...children); }
  setAttribute(name, value) { this[name] = value; }
  reportValidity() { return true; }
}
function all(root) { return [root, ...root.children.flatMap(all)]; }
// Stand-ins for the shared config rows: each row is an input element; forms record onSave.
function rowsModule(forms) {
  const field = (list, { title, value, disabled, key, form }, type) => {
    const control = new Element('input'); control.type = type; control.disabled = !!disabled;
    if (type === 'checkbox') control.checked = !!value; else control.value = value;
    const row = new Element('row'); row.textContent = title; row.append(control); list.append(row);
    const getValue = () => type === 'checkbox' ? control.checked : control.value;
    form.set(key, getValue());
    return { row, control, getValue, setDisabled: off => { control.disabled = !!off; } };
  };
  return {
    createConfigList: host => { const list = new Element('list'); host.append(list); return list; },
    createTextRow: (list, options) => field(list, options, 'text'),
    createToggleRow: (list, options) => field(list, options, 'checkbox'),
    createConfigForm: (host, { onSave }) => {
      const values = {}; const save = new Element('button'); save.disabled = true;
      const form = { set: (key, value) => { values[key] = value; }, attach: () => host.append(save), submit: () => onSave({ ...values }) };
      forms.push(form); return form;
    },
  };
}
async function load() {
  const forms = [];
  const context = vm.createContext({ document: { createElement: tag => new Element(tag) }, Object });
  const source = new vm.SourceTextModule(fs.readFileSync(__dirname + '/source-control.js', 'utf8'), { context });
  await source.link(async specifier => {
    const exports = specifier.endsWith('config-rows.js') ? rowsModule(forms) : { buildConfigRow: ({ name, control }) => {
      const row = new Element('row'); row.textContent = name; row.append(control); return row;
    } };
    return new vm.SyntheticModule(Object.keys(exports), function () {
      for (const [name, value] of Object.entries(exports)) this.setExport(name, value);
    }, { context });
  });
  await source.evaluate(); const build = source.namespace.buildSourceControlControls;
  return options => { forms.length = 0; return { panel: build(options), forms }; };
}
const connection = { id: 'site', revision: 2, configuration: { github_owner: 'owner', github_repo: 'site', production_branch: 'main', cloudflare_account_id: 'a'.repeat(32), worker_name: 'site', build_token_uuid: '11111111-1111-4111-8111-111111111111', root_directory: '/', build_command: 'npm install --ignore-scripts' }, github_configured: true, cloudflare_configured: true };
test('connection settings never collect or transmit credential values', async () => {
  const build = await load(); const writes = []; let rerenders = 0;
  const { panel, forms } = build({ data: { sites: [connection] }, scope: '/agents/a', enabled: true, status: {}, rerender: async () => { rerenders++; }, request: async (...args) => { writes.push(args); } });
  assert.deepEqual(all(panel).filter(el => el.type === 'password'), []);
  await forms[0].submit();
  assert.equal(writes.length, 1); assert.equal(rerenders, 1);
  assert.equal(writes[0][0], '/agents/a/source-control/sites/site');
  assert.equal(writes[0][2].expected_revision, 2);
  assert.equal('github_token' in writes[0][2], false); assert.equal('cloudflare_token' in writes[0][2], false);
});
test('vault requests display names and origins without credential entry fields', async () => {
  const build=await load();
  const site={...connection,credential_requests:[{name:'source_example_github',origin:'https://api.github.com'}]};
  const {panel}=build({data:{sites:[site]},scope:'/agents/a',enabled:true,status:{},rerender:async()=>{},request:async()=>{}});
  assert.ok(all(panel).some(el=>el.textContent.includes('source_example_github')));
  assert.equal(all(panel).filter(el=>el.type==='password').length,0);
});
test('capability previews cannot save user credentials', async () => {
  const build = await load(); let writes = 0;
  const { panel, forms } = build({ data: { sites: [connection] }, scope: '/agents/a', enabled: false, status: {}, rerender: async () => {}, request: async () => { writes++; } });
  for (const control of all(panel).filter(el => ['input', 'button'].includes(el.tag))) assert.equal(control.disabled, true);
  await forms[0].submit(); assert.equal(writes, 0);
});
