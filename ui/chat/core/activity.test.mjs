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

test('a scheduled retry is its own counted row, labelled by attempt, backup or automatic resume',()=>{
  const event=(id,payload)=>({kind:'retry_scheduled',event_id:id,run_id:'r1',seq:Number(id.slice(1)),payload});
  const rows=activityRows([
    event('e1',{attempt:1,of:2,wait_seconds:2,error:'Bridge provider overloaded: busy',backup:false,auto_resume:false}),
    event('e2',{attempt:0,of:2,wait_seconds:180,error:'still busy',backup:false,auto_resume:true}),
    event('e3',{attempt:0,of:2,wait_seconds:2,error:'x',backup:true,auto_resume:false})]);
  assert.deepEqual(rows.map(r=>[r.category,r.retry,r.autoResume,r.label]),[
    ['retries',true,false,'Retrying connection in 2 s · attempt 1 of 2'],
    ['retries',true,true,'Retries used up · resuming automatically in 3 min'],
    ['retries',true,false,'Switching to the backup model in 2 s']]);
  assert.equal(rows[0].text,'Bridge provider overloaded: busy');
});

test('messages shown in chat and the publish control stay out of activity rows',()=>{
  const rows=activityRows([
    {kind:'agent_step_end',event_id:'m1',payload:{asst_id:'a1',content:'private note',in_chat:false}},
    {kind:'agent_step_end',event_id:'m2',payload:{asst_id:'a2',content:'Plan: do X',in_chat:true}},
    {kind:'tool_call',event_id:'c1',payload:{tool:'publish_main_message',tool_call_id:'p1'}},
    {kind:'tool_result',event_id:'c2',payload:{tool_call_id:'p1',output:'{}'}},
  ]);
  assert.deepEqual(rows.map(row=>row.text),['private note']);
});

test('a preload shows its preloaded and suggested names in one line, scores when expanded',()=>{
  const rows=activityRows([{kind:'preload',event_id:'p1',payload:{
    preloaded:[{kind:'tool',name:'file_read',score:0.86},{kind:'skill',name:'codebase_admin:selectable:ui_proposals',score:0.9}],
    suggested:[{kind:'tool',name:'browser_read',score:0.7}],skipped:null}}]);
  assert.equal(rows[0].category,'preloads');
  assert.equal(rows[0].label,'Preloaded: file_read, skill ui_proposals · Suggested: browser_read');
  assert.match(rows[0].text,/file_read — 0\.86/);
  assert.match(rows[0].text,/skill ui_proposals — 0\.90/);
});

test('a skipped preload names its reason and is not counted in the group summary',()=>{
  const rows=activityRows([{kind:'preload',event_id:'p2',payload:{skipped:'matcher_unavailable'}}]);
  assert.equal(rows[0].label,'Preload skipped: matcher unavailable');
  const groups=activityGroups({events:[{kind:'preload',event_id:'p2',seq:1,run_id:'r1',payload:{skipped:'timeout'}}],
    segments:[{...segment('a','r1'),first_seq:1,last_seq:1}]},[{id:'r1',state:'completed'}]);
  assert.equal(groups[0].rows.length,1);
  assert.equal(Object.values(groups[0].counts).reduce((a,b)=>a+b,0),0);
});
