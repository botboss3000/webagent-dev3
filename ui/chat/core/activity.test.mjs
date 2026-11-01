import test from 'node:test';
import assert from 'node:assert/strict';
import { activityGroups } from './activity.js';

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
