const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const source = fs.readFileSync(path.join(__dirname, '../ui/shared/ability-row-controls.js'), 'utf8');
function control({ capable = true, maxConcurrency = capable ? 4 : 1, canEdit = true, save = async () => {}, execution } = {}) {
  const handlers = {};
  const tri = {
    classList: { add() {} }, dataset: {}, _paint() {}, setAttribute() {},
    addEventListener(name, fn) { handlers[name] = fn; },
    getBoundingClientRect() { return { right: 200, bottom: 20 }; },
  };
  let factory;
  const flashes = [];
  const context = {
    _noop() {}, _buildPermissionTri: () => tri, _markSaving() {},
    _flashSaveCheck: (_, ok, message) => flashes.push({ ok, message }),
    _TOOL_STATE_OFF_SVG: '', _TOOL_STATE_ASK_SVG: '', _TOOL_STATE_AUTO_SVG: '',
    _EYE_OFF_SVG: '', _EYE_ON_SVG: '',
    document: { getElementById: () => null },
    closeFloatingMenu() {}, openFloatingMenu: items => { factory = items; },
  };
  const start = source.indexOf('function _buildToolStateControl(');
  const end = source.indexOf('const _EYE_ON_SVG', start);
  vm.runInNewContext(source.slice(start, end) + '\nglobalThis.build = _buildToolStateControl;', context);
  context.build({
    name: 'file_read', permMode: 'auto', mode: 'visible', background_capable: capable,
    max_supported_concurrency: maxConcurrency,
    execution: execution || { mode: 'foreground', max_concurrency: capable ? 4 : 1, on_run_end: 'stop' },
  }, { canEdit, onExecutionChange: save });
  handlers.click({ stopPropagation() {} });
  return { items: () => factory(), flashes, opened: () => !!factory };
}

test('execution and end-of-run radios share the permission/visibility menu with bounded number input', () => {
  const ui = control();
  const items = ui.items();
  for (const heading of ['Permission', 'Visibility', 'Execution', 'When run ends']) {
    assert(items.some(item => item.info && item.label === heading));
  }
  assert.equal(items.filter(item => item.radio && item.checked).length, 2);
  assert.equal(items.filter(item => item.radio).length, 6);
  const field = items.find(item => item.field);
  assert.equal(field.fieldType, 'number');
  assert.deepEqual([field.min, field.max, field.step, field.value], [1, 4, 1, 4]);
});

test('saves complete settings and reverts failed selections', async () => {
  const saves = [];
  const ui = control({ save: async (name, settings) => { saves.push({ name, ...settings }); } });
  await ui.items().find(item => item.label === 'Background').action();
  await ui.items().find(item => item.label === 'Wake (read-only)').action();
  await ui.items().find(item => item.field).onSave('2');
  assert.deepEqual(saves[2], { name: 'file_read', mode: 'background', max_concurrency: 2, on_run_end: 'wake' });
  assert(ui.items().find(item => item.label === 'Background').checked);
  const failed = control({ save: async () => { throw Error('revision changed'); } });
  await failed.items().find(item => item.label === 'Background').action();
  assert(failed.items().find(item => item.label === 'Foreground').checked);
  assert.deepEqual(failed.flashes.at(-1), { ok: false, message: 'revision changed' });
});

test('unsupported background choices are disabled with reasons, while foreground remains editable', () => {
  const ui = control({ capable: false });
  for (const label of ['Background', 'Model chooses', 'Silent (notify only)', 'Wake (read-only)']) {
    const item = ui.items().find(item => item.label === label);
    assert.equal(item.disabled, true);
    assert.match(item.title, /not supported/);
  }
  assert.equal(ui.items().find(item => item.label === 'Foreground').disabled, false);
  assert.equal(ui.items().find(item => item.field).disabled, true);
  assert.equal(control({ canEdit: false }).opened(), false);
});

test('concurrency rejects blank, fractional and out-of-bounds values without saving', async () => {
  let writes = 0;
  const ui = control({ save: async () => { writes++; } });
  for (const value of ['', '0', '5', '1.5', 'no', 'Infinity']) {
    await ui.items().find(item => item.field).onSave(value);
  }
  assert.equal(writes, 0);
  await ui.items().find(item => item.field).onSave('1');
  assert.equal(writes, 1);
});

test('execution writes cannot overlap and pending controls are disabled', async () => {
  let finish;
  let writes = 0;
  const ui = control({ save: () => { writes++; return new Promise(resolve => { finish = resolve; }); } });
  const pending = ui.items().find(item => item.label === 'Background').action();
  assert(ui.items().filter(item => item.radio || item.field).every(item => item.disabled));
  await ui.items().find(item => item.label === 'Wake (read-only)').action();
  assert.equal(writes, 1);
  finish();
  await pending;
  assert.equal(ui.items().find(item => item.label === 'Wake (read-only)').disabled, false);
});

test('abilities adapter uses current revision, encoded route, full settings and conflict feedback', async () => {
  const page = fs.readFileSync(path.join(__dirname, '../ui/main-panel/agents/pages/abilities/page.js'), 'utf8');
  const start = page.indexOf('onExecutionChange: async');
  const end = page.indexOf('        onPermissionChange:', start);
  const requests = [];
  const notices = [];
  const status = { textContent: '' };
  const context = {
    request: async (...args) => { requests.push(args); },
    scope: '/agents/agent-1', ability: { id: 'codebase_admin' }, tool: { name: 'file/read' },
    data: { revision: 7 }, rerender: async notice => notices.push(notice),
    status, expandedAbility: null,
  };
  const callback = vm.runInNewContext('({' + page.slice(start, end) + '}).onExecutionChange', context);
  const settings = { mode: 'model', max_concurrency: 2, on_run_end: 'silent' };
  await callback('file/read', settings);
  assert.equal(requests[0][0], '/agents/agent-1/abilities/codebase_admin/tools/file%2Fread/execution');
  assert.equal(requests[0][1], 'PUT');
  assert.equal(requests[0][2].expected_revision, 7);
  assert.equal(requests[0][2].settings, settings);
  assert.equal(notices.length, 1);
  context.request = async () => { throw Object.assign(Error('stale revision'), { status: 409 }); };
  await assert.rejects(callback('file/read', settings), /stale revision/);
  assert.match(status.textContent, /Reload before saving/);
  assert.equal(notices.length, 1);
});

test('foreground-only parallel tools use the server concurrency ceiling independently of background support', async () => {
  const writes = [];
  const ui = control({ capable: false, maxConcurrency: 4, save: async (name, settings) => writes.push({ name, ...settings }) });
  const field = ui.items().find(item => item.field);
  assert.equal(field.disabled, false);
  assert.equal(field.max, 4);
  assert.equal(field.value, 1);
  await field.onSave('4');
  assert.deepEqual(writes[0], { name: 'file_read', mode: 'foreground', max_concurrency: 4, on_run_end: 'stop' });
  assert.equal(ui.items().find(item => item.label === 'Background').disabled, true);
  assert.equal(ui.items().find(item => item.label === 'Model chooses').disabled, true);
  await ui.items().find(item => item.field).onSave('5');
  assert.equal(writes.length, 1);
  const serial = control({ maxConcurrency: 1 });
  assert.equal(serial.items().find(item => item.field).disabled, true);
  await serial.items().find(item => item.field).onSave('2');
  assert.equal(serial.flashes.at(-1).ok, false);
});

test('end-of-run labels describe user notification, next-turn results and read-only wake', () => {
  const items = control().items();
  assert(items.some(item => item.radio && item.label === 'Silent (notify only)'));
  assert(items.some(item => item.radio && item.label === 'Wake (read-only)'));
  assert(items.some(item => item.info && /completion note without an agent loop/.test(item.label)
    && /next user turn/.test(item.label) && /read-only continuation/.test(item.label)));
});
