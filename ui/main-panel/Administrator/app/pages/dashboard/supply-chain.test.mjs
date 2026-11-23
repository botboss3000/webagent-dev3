import {test} from 'node:test';
import assert from 'node:assert/strict';
import {readSupplyChain, supplyChainModel, prepareBrief, prepareUpdate, fixable} from './supply-chain.js';

const apt={package:'apt',installed:'2.6.1',fixed:'2.6.2',id:'CVE-2026-109518'};
const view=(extra={})=>({schema_version:1,state:'ok',available:true,scanned_at:1795441914,checked_at:1795441950,
 release:'9850cba2-4b43-4a2d-bd06-6a4c3b8d6e0a',stale_days:8,
 targets:[{name:'api',by_severity:{HIGH:1115,CRITICAL:7},fixable_by_severity:{HIGH:1},fixable_high_critical:[apt]},
  {name:'loop',by_severity:{HIGH:39},fixable_by_severity:{HIGH:1},fixable_high_critical:[apt]},
  {name:'cargo',by_severity:{},fixable_by_severity:{},fixable_high_critical:[]}],
 ui_libraries:{missing:false,recorded:{marked:'12.0.0'},mismatch:[]},...extra});

test('reader refuses invalid views and reports refusals without counts',async()=>{
 const original=globalThis.fetch;
 try{
  globalThis.fetch=async()=>new Response(JSON.stringify(view()));
  assert.equal((await readSupplyChain()).state,'ok');
  globalThis.fetch=async()=>new Response(JSON.stringify({...view(),state:'great'}));
  assert.match((await readSupplyChain()).read_error,/Invalid/);
  globalThis.fetch=async()=>new Response(JSON.stringify(view({targets:[{name:'api',by_severity:{},fixable_by_severity:{},fixable_high_critical:[{package:'x'}]}]})));
  assert.match((await readSupplyChain()).read_error,/Invalid/);
  globalThis.fetch=async()=>new Response('{}',{status:404});
  assert.equal((await readSupplyChain()).read_error,'Administrator access required');
  assert.equal(supplyChainModel({read_error:'x'}).state,'unavailable');
 }finally{globalThis.fetch=original;}
});

test('model shows states, counts, UI library check and deduplicated findings',()=>{
 const ok=supplyChainModel(view());
 assert.equal(ok.state,'live');
 assert.equal(ok.findings.length,1);
 assert.deepEqual(fixable(view())[0].targets,['api','loop']);
 assert.ok(ok.rows.some(([label,value])=>label==='api findings'&&value==='7 critical · 1115 high'));
 assert.ok(ok.rows.some(([label,value])=>label==='UI library versions'&&value.startsWith('Match')));
 assert.equal(supplyChainModel(view({state:'stale'})).badge,'Scan is older than 8 days or the scanner stopped');
 const failed=supplyChainModel(view({state:'failed',error:'docker inspect failed'}));
 assert.equal(failed.state,'error');assert.match(failed.note,/docker inspect failed/);
 assert.equal(supplyChainModel(view({ui_libraries:{missing:false,recorded:{},mismatch:[{library:'marked',header:'4.0.0',recorded:'12.0.0'}]}})).state,'partial');
 assert.equal(supplyChainModel(view({targets:[]})).findings.length,0);
});

test('prepare update only opens a chat with a brief; it changes nothing itself',async()=>{
 const calls=[];let opened='';
 const fetchFn=async(path,options={})=>{
  calls.push([path,options.method||'GET',options.headers?.['x-csrf-token'],options.body&&JSON.parse(options.body)]);
  if(path==='/api/v3/auth/me')return new Response(JSON.stringify({csrf_token:'csrf-fixture',is_admin:true}));
  if(path==='/api/v3/sessions')return new Response(JSON.stringify({id:'s1'}));
  return new Response('{}');
 };
 await prepareUpdate(view(),{fetchFn,assign:url=>{opened=url;}});
 assert.deepEqual(calls.map(c=>c[0]),['/api/v3/auth/me','/api/v3/sessions','/api/v3/sessions/s1/messages']);
 assert.equal(calls[1][2],'csrf-fixture');assert.equal(calls[1][3].agent_id,'shared_default');
 assert.match(calls[2][3].text,/apt 2\.6\.1 → 2\.6\.2 \(CVE-2026-109518; in api, loop\)/);
 assert.match(prepareBrief(view()),/ordinary broker task/);
 assert.equal(opened,'/app?session_id=s1');
});
