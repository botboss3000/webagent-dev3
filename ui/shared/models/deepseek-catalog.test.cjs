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
  assert.deepEqual(result.models.map(model => model.vision_capable), [true,false]);
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

test('A catalog model the server already offers is adopted when added, not saved as WebAgent Catalog', async () => {
  // Published local models are in the agent's catalog view before any adoption.
  const kev = {id:'platform:local_kev',owner_id:'platform_catalog',connection_id:'platform:local_kev',upstream_model:'kev-0.8b',
    display_name:'Kev 0.8B (sidekick)',capabilities:['decision'],allowed_agents:['shared_default'],revision:1};
  const catalog = {revision:4,entries:{'platform:local_kev':kev},
    connections:{'platform:local_kev':{id:'platform:local_kev',owner_id:'platform_catalog',transport:'local',credential_ref:'platform_key:local_kev',allowed_agents:['shared_default'],revision:1,revoked:false}},
    platform_sources:{'platform:local_kev':{}},agent_models:{shared_default:{agent_id:'shared_default',owner_id:'admin',entry_ids:[],roles:{},revision:2}}};
  const view = () => ({catalog:structuredClone(catalog),can_manage:true,caller_id:'admin',agent_owner_id:'admin',adoptions:{}});
  const calls = [];
  const models = await adapter(async (url, method, body) => {
    calls.push([url, method || 'GET']);
    if (url === '/models/providers') return {providers:[{provider:'local',name:'Local models',base_url:'relay://local-models',validated:true}]};
    if (url === '/models/me/catalog') return {available_platform_models:[{id:'platform:local_kev',origin:'platform',display_name:'Kev 0.8B (sidekick)'}],entries:[]};
    if (url.endsWith('/adoptions')) {
      assert.deepEqual([body.source, body.alias], [{origin:'platform',entry_id:'platform:local_kev'}, 'Kev']);
      return {entry_id:'platform:local_kev'};
    }
    if (url.startsWith('/models/catalog') && method === 'PUT') {
      assert.deepEqual(body.agent_models.shared_default.entry_ids, ['platform:local_kev']);
      return view();
    }
    if (url.startsWith('/models/catalog')) return view();
    throw Error('Unexpected request ' + url);
  });
  await models.loadConfig();
  await models.saveRoster({providers:[{provider:'webagent_catalog',entry_id:'platform:local_kev',model:'platform:local_kev',display_name:'Kev',decision_capable:true}]});
  assert.ok(calls.some(([url]) => url.endsWith('/adoptions')));
  assert.ok(calls.some(([url, method]) => url.startsWith('/models/catalog') && method === 'PUT'));
});
