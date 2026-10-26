'use strict';
const {test} = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

test('model levels win over the provider scale; unlisted models inherit it', async () => {
  const source = fs.readFileSync(path.join(__dirname, 'effort-slider.js'), 'utf8');
  const {modelEfforts, effortLabel} = await import('data:text/javascript;base64,' + Buffer.from(source).toString('base64'));
  const preset = {efforts:['low','medium'], default_effort:'medium', models:[{id:'a', efforts:['none','high'], default_effort:'high'}, {id:'b', efforts:[]}]};
  assert.deepEqual(modelEfforts(preset, 'a'), {levels:['none','high'], providerDefault:'high'});
  assert.deepEqual(modelEfforts(preset, 'b'), {levels:[], providerDefault:''});
  assert.deepEqual(modelEfforts(preset, 'new'), {levels:['low','medium'], providerDefault:'medium'});
  assert.deepEqual(modelEfforts(undefined, 'x'), {levels:[], providerDefault:''});
  assert.equal(effortLabel('xhigh'), 'Extra High Effort');
});
