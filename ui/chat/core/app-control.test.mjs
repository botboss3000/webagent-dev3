import test from 'node:test';
import assert from 'node:assert/strict';
import { appViewRequest, appViewUrl, createAppViewDispatcher, credentialRequest } from './app-control.js';
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
test('credential requests are session state: label and HTTPS link pass, unsafe links and other callers do not',()=>{
 const request={actor_id:'alice',agent_id:'agent-a',destination:'credential',expires_at:200,session_id:'s',credential_name:'source_x',credential_origin:'https://api.github.com',request_id:'a'.repeat(64),expected_revision:0,credential_label:'GitHub token',credential_url:'https://github.com/settings/personal-access-tokens/new'};
 const detail=credentialRequest({revision:3,request},{user_id:'alice'},100000);
 assert.equal(detail.label,'GitHub token');assert.equal(detail.createUrl,'https://github.com/settings/personal-access-tokens/new');assert.equal(detail.requestId,'a'.repeat(64));
 assert.equal(credentialRequest({revision:3,request},{user_id:'alice'},100000).requestId,'a'.repeat(64),'repeat snapshots keep rendering it');
 assert.equal(credentialRequest({revision:3,request:{...request,credential_url:'javascript:alert(1)'}},{user_id:'alice'},100000).createUrl,'');
 assert.equal(credentialRequest({revision:3,request},{user_id:'bob'},100000),null);
 assert.equal(credentialRequest({revision:3,request:{...request,destination:'app:files'}},{user_id:'alice'},100000),null);
 assert.equal(credentialRequest(null,{user_id:'alice'}),null);
});
