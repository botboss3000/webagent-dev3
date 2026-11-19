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

test('closer attempts are system rows: a status names its flag, a failure reads as failed',()=>{
  const event=(id,payload)=>({kind:'closing_status',event_id:id,run_id:'r1',payload});
  const done=activityRows([event('e1',{attempt:1,of:3,model:'sys',flag:'needs_input',notification:'Pick an option'})]);
  assert.deepEqual(done.map(r=>[r.category,r.closer,r.failed,r.label,r.text,r.model]),
    [['closers',true,false,'Closing status · needs_input','needs_input · Pick an option','sys']]);
  const failed=activityRows([event('e2',{attempt:3,of:3,model:'sys',error:'flag is missing',fallback:true})]);
  assert.equal(failed[0].failed,true);
  assert.equal(failed[0].label,'Closing status');
  assert.equal(failed[0].text,'Attempt 3 of 3 failed: flag is missing\n\nFallback status recorded.');
});

test('each model call is one row, counted apart, with its cache share and estimate',()=>{
  const usage={kind:'model_usage',event_id:'u1',payload:{input_tokens:65219,cached_input_tokens:64896,cache_write_tokens:0,
    matched_bytes:241030,sent_bytes:242157,first_change:'history',first_change_entry:8}};
  const rows=activityRows([usage]);
  assert.equal(rows[0].label,'Model call · 65.2k in · 99.5% cached');
  assert.match(rows[0].text,/Estimate: 99.5% of the request unchanged since the previous call; first change: history entry 8/);
  const unreported=activityRows([{kind:'model_usage',event_id:'u2',payload:{sent_bytes:10,matched_bytes:0,first_change:'no_previous_call'}}]);
  assert.equal(unreported[0].label,'Model call · usage not reported');
  const groups=activityGroups({events:[{...usage,run_id:'r1',seq:1}],segments:[segment('a','r1')]},[{id:'r1',state:'completed'}]);
  assert.equal(groups[0].rows.length,1);
  assert.equal(groups[0].counts.models,1);
  assert.deepEqual(groups[0].cache,{input:65219,cached:64896});
});
