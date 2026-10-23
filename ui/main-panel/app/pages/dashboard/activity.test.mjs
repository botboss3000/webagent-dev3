import {test} from 'node:test';
import assert from 'node:assert/strict';
import {readActivity} from './activity.js';
const fixtures={
 '/api/v3/models/agents':{agents:[{id:'a',name:'<img onerror=alert(1)>',status:'active'}]},
 '/api/v3/sessions/stats?limit=100':{sessions:[
  {agent_id:'a',message_count:2,usage:{input_tokens:20,output_tokens:5,reported_calls:2,unreported_calls:1,known_cost_usd:.12,unknown_cost_calls:2}},
  {agent_id:'b',message_count:3,usage:null}
 ],next_offset:100},
 '/api/v3/admin/access/accounts':{accounts:[{role:'administrator',status:'active'},{role:'guest',status:'active'}]},
 '/api/v3/auth/sessions':{sessions:[{current:true}]},
 '/api/v3/status':{model:'fixture',runs:[{id:'same',status:'running'},{id:'same',status:'running'},{id:'bad',status:'failed',error:'fixture failure',context_controls:{trigger_tokens:40,tail_tokens:10}}]},
 '/health/ready':{ready:true}
};
test('scoped accounting, bounded pages, partial cost, deduplication and failure isolation',async()=>{
 const original=globalThis.fetch,controller=new AbortController();let mode='live',calls=0;
 globalThis.fetch=async(path,options)=>{
  calls++;assert.equal(options.signal,controller.signal);assert.equal(options.credentials,'same-origin');assert.equal(options.method,undefined);
  if(mode==='denied')return new Response('{}',{status:403});
  if(mode==='broken'&&path==='/api/v3/models/agents')return new Response(JSON.stringify({agents:[null]}));
  const value=structuredClone(fixtures[path]);
  if(mode==='unknown-cost'&&value.sessions?.[0]?.usage){value.sessions[0].usage.unknown_cost_calls=3;value.sessions[0].usage.known_cost_usd=0;}
  return new Response(JSON.stringify(value));
 };
 try{
  let tiles=await readActivity(controller.signal);assert.equal(calls,6);
  assert.equal(tiles.get('agents_panel').rows[1][0],'<img onerror=alert(1)>');
  assert.equal(tiles.get('tokens').rows[0][1],20);assert.equal(tiles.get('tokens').state,'partial');
  assert.match(tiles.get('tokens').note,/first page only/);
  assert.equal(tiles.get('cost').rows[0][1],'$0.1200');
  assert.equal(tiles.get('active_runs').rows[0][1],1);
  assert.equal(tiles.get('user_mgmt').rows[0][1],2);
  assert.match(tiles.get('security').note,/Current account only/);
  mode='unknown-cost';tiles=await readActivity(controller.signal);assert.equal(tiles.get('cost').rows[0][1],'Unavailable');
  mode='broken';tiles=await readActivity(controller.signal);
  assert.equal(tiles.get('agents_panel').state,'unavailable');assert.equal(tiles.get('user_mgmt').state,'live');
  mode='denied';tiles=await readActivity(controller.signal);
  assert.equal(tiles.size,12);assert.ok([...tiles.values()].every(t=>t.state==='unavailable'&&t.rows.length===0));
  assert.ok([...tiles.values()].every(t=>t.note==='Administrator access required'));
 }finally{globalThis.fetch=original;}
});
