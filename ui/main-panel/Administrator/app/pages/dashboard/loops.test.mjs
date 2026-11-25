import assert from 'node:assert/strict';
import test from 'node:test';
import { readLoops, loopsModel } from './loops.js';

const member=(index,state='serving',age=1,runs=0,accounts=3)=>({index,state,heartbeat_age_seconds:age,started_at:1,runs_in_progress:runs,accounts});
const view=(members)=>({schema_version:1,loops:members.length,accounts_total:6,runs_in_progress:members.reduce((a,m)=>a+m.runs_in_progress,0),members});
const answer=(body,status=200)=>async()=>({ok:status===200,status,json:async()=>body});

test('every loop is listed with its heartbeat, runs and accounts', () => {
  const model=loopsModel(view([member(0,'serving',1,2,4),member(1,'serving',2,0,2)]));
  assert.equal(model.state,'live');
  assert.equal(model.badge,'2 loops');
  assert.equal(model.rows[0][1],'serving · heartbeat 1 s ago · 2 runs · 4 accounts');
});

test('a stale or missing loop turns the card red and is counted', () => {
  const model=loopsModel(view([member(0),member(1,'missing',null)]));
  assert.equal(model.state,'error');
  assert.match(model.badge,/1 not serving/);
  assert.match(model.rows[1][1],/no heartbeat/);
});

test('no loops, refused or invalid responses never read as healthy', async () => {
  assert.equal(loopsModel({schema_version:1,loops:0,members:[],accounts_total:0,runs_in_progress:0}).state,'error');
  const original=globalThis.fetch;
  try{
    globalThis.fetch=answer({},404);
    assert.equal((await readLoops()).read_error,'Administrator access required');
    globalThis.fetch=answer({...view([member(0)]),members:[{...member(0),state:'weird'}]});
    assert.ok((await readLoops()).read_error);
    globalThis.fetch=answer(view([member(0)]));
    assert.equal((await readLoops()).loops,1);
  }finally{globalThis.fetch=original;}
  assert.equal(loopsModel({read_error:'x'}).state,'unavailable');
});
