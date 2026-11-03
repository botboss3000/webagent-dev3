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
