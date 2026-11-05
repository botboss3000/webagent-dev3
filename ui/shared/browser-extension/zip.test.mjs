import assert from 'node:assert/strict';
import test from 'node:test';
import {crc32, zip} from './zip.js';

test('crc32 matches the standard check value', () => {
  assert.equal(crc32(new TextEncoder().encode('123456789')), 0xcbf43926);
});

test('zip lays out headers, data and the end record', () => {
  const bytes = new TextEncoder().encode('hello');
  const out = zip([{name: 'a.txt', bytes}]);
  const view = new DataView(out.buffer);
  assert.equal(view.getUint32(0, true), 0x04034b50);
  assert.equal(view.getUint32(18, true), 5);
  assert.equal(new TextDecoder().decode(out.slice(30, 35)), 'a.txt');
  assert.equal(new TextDecoder().decode(out.slice(35, 40)), 'hello');
  const end = out.length - 22;
  assert.equal(view.getUint32(end, true), 0x06054b50);
  assert.equal(view.getUint16(end + 10, true), 1);
  assert.equal(view.getUint32(end + 16, true), 40);
  assert.equal(view.getUint32(40, true), 0x02014b50);
});
