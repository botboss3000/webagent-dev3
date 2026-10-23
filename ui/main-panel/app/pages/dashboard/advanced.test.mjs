import {test} from 'node:test';
import assert from 'node:assert/strict';
import {readDashboard,metricOptions} from './advanced.js';
test('dashboard contract binds window and authority, and preserves unavailable responses',async()=>{
 const original=globalThis.fetch,controller=new AbortController();let mode='good';
 globalThis.fetch=async(path,options)=>{
  assert.equal(path,'/api/v3/admin/system/dashboard?window=7d');
  assert.equal(options.signal,controller.signal);assert.equal(options.credentials,'same-origin');
  return mode==='denied'?new Response('{}',{status:403}):new Response(JSON.stringify(mode==='good'?{schema_version:1,window:'7d',history:{points:[]},database:{probes:[]},...Object.fromEntries(['scope','devices','git','loop','throughput','usage','runtime'].map(key=>[key,{}]))}:{}));
 };
 try{
  assert.ok((await readDashboard(controller.signal,'7d')).history);
  mode='denied';assert.equal((await readDashboard(controller.signal,'7d')).error,'Administrator access required');
  mode='invalid';assert.match((await readDashboard(controller.signal,'7d')).error,/Invalid/);
  assert.ok(metricOptions.some(([key])=>key==='db_ms'));
 }finally{globalThis.fetch=original;}
});
