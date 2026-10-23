import test from 'node:test';
import assert from 'node:assert/strict';
import { appViewRequest, appViewUrl, createAppViewDispatcher } from './app-control.js';
test('only fresh caller-bound metadata can request registered app navigation',()=>{
 const request={actor_id:'alice',agent_id:'agent-a',destination:'agents:abilities',expires_at:200};
 const calls=[];const dispatch=x=>calls.push(x);
 assert.equal(appViewRequest({request},{user_id:'alice'},dispatch,100000),true);
 assert.equal(appViewRequest({request},{user_id:'bob'},dispatch,100000),false);
 assert.equal(appViewRequest({request},{user_id:'alice'},dispatch,300000),false);
 assert.equal(appViewRequest({request:{...request,destination:'https://example.invalid'}},{user_id:'alice'},dispatch,100000),false);
 assert.equal(calls.length,1);
});

test('handoff navigation accepts only a private conversation identifier',()=>{
 const id='12345678-1234-1234-1234-123456789abc';
 assert.equal(appViewUrl('handoff','agent','https://example.invalid/app?tab=agents',id),'https://example.invalid/app?session_id='+id);
 assert.equal(appViewUrl('handoff','agent','https://example.invalid/app','https://elsewhere.invalid'),null);
});

test('snapshot polling cannot reopen forms or reapply old view requests',()=>{
 const apply=createAppViewDispatcher(),calls=[];
 const request={actor_id:'alice',session_id:'source',agent_id:'agent',destination:'registration',expires_at:200};
 const send=()=>calls.push('opened'),identity={user_id:'alice'};
 assert.equal(apply({revision:2,request},identity,send,100000),true);
 assert.equal(apply({revision:2,request},identity,send,100000),false);
 assert.equal(apply({revision:1,request},identity,send,100000),false);
 assert.equal(apply({revision:3,request},{user_id:'bob'},send,100000),false);
 assert.equal(apply({revision:3,request},identity,send,100000),true);
 assert.equal(calls.length,2);
});
