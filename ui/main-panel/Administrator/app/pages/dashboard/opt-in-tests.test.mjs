import assert from 'node:assert/strict';
import test from 'node:test';
import { readOptInTests, optInTestsModel } from './opt-in-tests.js';

const JOB='11111111-2222-3333-4444-555555555555';
const view=(state,failed=[])=>({schema_version:1,state,checked_at:1795500000,groups:[{group:'multi_loop',status:state==='failed'?'failed':'passed',job_id:JOB,tests:5,failed_tests:failed}]});
const answer=(body,status=200)=>async()=>({ok:status===200,status,json:async()=>body});

test('a failed run names its failed tests and points to the job', () => {
  const model=optInTestsModel(view('failed',['multi_loop_tests::killed_loop']));
  assert.equal(model.state,'error');
  assert.match(model.note,/multi_loop: multi_loop_tests::killed_loop/);
  assert.match(model.rows[1][1],/job 11111111/);
  assert.equal(optInTestsModel(view('passed')).state,'live');
});

test('unreadable or refused responses never show as passed', async () => {
  const original=globalThis.fetch;
  try{
    globalThis.fetch=answer({...view('passed'),groups:[{group:'x',status:'passed',job_id:null,tests:1,failed_tests:['rm -rf /']}]});
    assert.ok((await readOptInTests()).read_error);
    globalThis.fetch=answer({},404);
    assert.equal((await readOptInTests()).read_error,'Administrator access required');
    globalThis.fetch=answer(view('passed'));
    assert.equal((await readOptInTests()).state,'passed');
  }finally{globalThis.fetch=original;}
  assert.equal(optInTestsModel({read_error:'x'}).state,'unavailable');
});
