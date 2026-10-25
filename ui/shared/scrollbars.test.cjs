const {test} = require('node:test');
const assert = require('node:assert/strict'), fs = require('node:fs'), vm = require('node:vm');
// Without a document the installer exits, leaving only the pure geometry.
const context = vm.createContext({});
vm.runInContext(fs.readFileSync(__dirname + '/scrollbars.js', 'utf8'), context);
const thumb = (...a) => JSON.parse(JSON.stringify(context.scrollThumb(...a) ?? null));

test('no thumb until content overflows the view', () => {
  assert.equal(thumb(300, 300, 0), null);
  assert.equal(thumb(300, 301, 0), null);
  assert.deepEqual(thumb(300, 600, 0), {size: 150, offset: 0});
});
test('thumb travels from start to end with scroll position', () => {
  assert.deepEqual(thumb(300, 600, 150), {size: 150, offset: 75});
  assert.deepEqual(thumb(300, 600, 300), {size: 150, offset: 150});
  assert.deepEqual(thumb(300, 600, 900), {size: 150, offset: 150}, 'overscroll clamps');
  assert.deepEqual(thumb(300, 600, -40), {size: 150, offset: 0}, 'rubber-band clamps');
});
test('long content keeps a grabbable minimum length', () => {
  assert.deepEqual(thumb(300, 100000, 0), {size: 24, offset: 0});
  assert.deepEqual(thumb(300, 100000, 99700), {size: 24, offset: 276});
  assert.deepEqual(thumb(10, 100, 0), {size: 10, offset: 0}, 'never longer than the view');
});
