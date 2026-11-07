import test from 'node:test';
import assert from 'node:assert/strict';
import { activityGroups, activityRows } from './activity.js';

const segment=(id,run_id,closed=true)=>({id,run_id,boundary_item_id:'q-'+id,first_seq:null,last_seq:null,closed});

test('a stopped run keeps its Stopped state until the conversation resumes',()=>{
  const groups=activityGroups({events:[],segments:[segment('a','r1')]},[{id:'r1',state:'cancelled'}]);
  assert.equal(groups[0].state,'cancelled');
});

test('a stopped run is shown as finished once a later run resumes the conversation',()=>{
  const groups=activityGroups({events:[],segments:[segment('a','r1'),segment('b','r2',false)]},
    [{id:'r1',state:'cancelled'},{id:'r2',state:'running'}]);
  assert.deepEqual(groups.map(g=>g.state),['finished','running']);
});

test('failures stay visible after a later run',()=>{
  const groups=activityGroups({events:[],segments:[segment('a','r1'),segment('b','r2',false)]},
    [{id:'r1',state:'failed'},{id:'r2',state:'running'}]);
  assert.deepEqual(groups.map(g=>g.state),['failed','running']);
});

test('a tool result reporting an error status is labelled failed with its message',()=>{
  const call={kind:'tool_call',event_id:'e1',payload:{tool:'dev_browser_url',tool_call_id:'c1'}};
  const result=(output)=>({kind:'tool_result',event_id:'e2',payload:{tool_call_id:'c1',output}});
  const failed=activityRows([call,result(JSON.stringify({status:'error',output:'path rejected'}))]);
  assert.equal(failed[0].label,'dev_browser_url · failed: path rejected');
  const done=activityRows([call,result(JSON.stringify({status:'success',output:{url:'x'}}))]);
  assert.equal(done[0].label,'dev_browser_url · done');
});

test('a held duplicate is labelled held and a typed rejection failed',()=>{
  const call={kind:'tool_call',event_id:'e1',payload:{tool:'push',tool_call_id:'c1'}};
  const result=(output)=>({kind:'tool_result',event_id:'e2',payload:{tool_call_id:'c1',output}});
  assert.equal(activityRows([call,result(JSON.stringify({status:'held',output:'x'}))])[0].label,'push · held: possible duplicate');
  assert.equal(activityRows([call,result(JSON.stringify({status:'rejected',output:'bad path'}))])[0].label,'push · failed: bad path');
});

test('a compaction is one row: the call labels it and the result fills it, counted once',()=>{
  const call={kind:'compaction_call',event_id:'e1',run_id:'r1',seq:1,payload:{model:'sys-model',input_bytes:2048,reason:'automatic'}};
  const done={kind:'compaction_result',event_id:'e2',run_id:'r1',seq:2,payload:{summary:'x'.repeat(512)}};
  assert.deepEqual(activityRows([call]).map(r=>[r.label,r.text,r.events.length]),
    [['Compaction · automatic, context limit · System model sys-model · 2.0 KB input','',1]]);
  const rows=activityRows([call,done]);
  assert.deepEqual(rows.map(r=>[r.category,r.compaction,r.label,r.events.length]),
    [['compactions',true,'Compaction · automatic, context limit · System model sys-model · 2.0 KB → 0.5 KB summary',2]]);
  const requested={...call,payload:{...call.payload,reason:'requested'}};
  assert.match(activityRows([requested])[0].label,/^Compaction · requested by agent · /);
  const failed=activityRows([call,{...done,payload:{error:'timeout'}}]);
  assert.equal(failed[0].text,'Compaction failed: timeout');
  assert.match(failed[0].label,/2\.0 KB input$/);
  // A result without its call (older journals) still shows as its own row.
  assert.equal(activityRows([done])[0].text,'x'.repeat(512));
  const groups=activityGroups({events:[call,done],segments:[{...segment('a','r1'),first_seq:1,last_seq:2}]},[{id:'r1',state:'running'}]);
  assert.equal(groups[0].counts.compactions,1);
});
