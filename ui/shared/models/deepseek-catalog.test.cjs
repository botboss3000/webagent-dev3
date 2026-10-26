'use strict';
const {test} = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

async function adapter(request) {
  const source = fs.readFileSync(path.join(__dirname, 'v3-adapter.js'), 'utf8');
  const module = await import('data:text/javascript;base64,' + Buffer.from(source).toString('base64'));
  return module.createModelAdapter(request, 'shared_default').adapter;
}

test('DeepSeek preview lists the provider file models and modalities without a key', async () => {
  // Shape of GET /models/providers (the server's provider files; the Rust tests validate the real ones).
  const providers = [
    {provider:'deepseek', name:'DeepSeek', base_url:'https://api.deepseek.com', validated:true, models:[
      {id:'deepseek-flash', name:'DeepSeek-V4.1-Flash', input:['text','image'], context:1048576, max_output:393216, efforts:['none','low','high','max'], default_effort:'high'},
      {id:'deepseek-v4-pro', name:'DeepSeek-V4-Pro', input:['text'], context:1048576, max_output:393216, efforts:['none','low','high','max'], default_effort:'high'}]},
    {provider:'openrouter', name:'OpenRouter', base_url:'https://openrouter.ai/api/v1', validated:false, models:[]},
  ];
  const view = await adapter(async (url) => {
    if (url === '/models/providers') return {providers};
    throw Error('Preview must not call the provider');
  });
  const result = await view.fetchModels({provider:'deepseek',api_key:''});
  assert.equal(result.preview, true);
  assert.match(result.preview_message, /DeepSeek/);
  assert.deepEqual(result.models.map(model => model.id), ['deepseek-flash','deepseek-v4-pro']);
  assert.deepEqual(result.models.map(model => model.image_capable), [true,false]);
  for (const model of result.models) {
    assert.equal(model.text_capable, true);
    assert.equal(model.context, 1048576);
    assert.equal(model.max_output, 393216);
  }
  const presets = await view.loadPresets();
  assert.equal(presets.deepseek.validated, true);
  assert.equal(presets.openrouter.validated, false);
  assert.deepEqual(presets.deepseek.models[0].efforts, ['none','low','high','max']);
});

test('DeepSeek key uses live discovery and preserves provider additions and errors', async () => {
  const calls = [];
  const live = {models:[{id:'future-model'}]};
  const view = await adapter(async (...args) => { calls.push(args); return live; });
  assert.equal(await view.fetchModels({provider:'deepseek',api_key:'fixture-key'}), live);
  assert.deepEqual(calls, [['/models/discovery','POST',{provider:'deepseek',api_key:'fixture-key'}]]);
  const broken = await adapter(async () => { throw Error('Provider unavailable'); });
  await assert.rejects(broken.fetchModels({provider:'deepseek',api_key:'fixture-key'}), /Provider unavailable/);
});
