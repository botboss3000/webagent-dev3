import test from 'node:test';
import assert from 'node:assert/strict';
import { readDraft, saveDraft, adoptNewDraft } from './composer-draft.js';
const memory=()=>{const map=new Map();return {getItem:k=>map.has(k)?map.get(k):null,setItem:(k,v)=>map.set(k,v),removeItem:k=>map.delete(k),map};};
test('drafts are kept per session and blank drafts are removed',()=>{
 const s=memory();saveDraft('a','hello',s);saveDraft('','new text',s);
 assert.equal(readDraft('a',s),'hello');assert.equal(readDraft('b',s),'');assert.equal(readDraft('',s),'new text');
 saveDraft('a','  ',s);assert.equal(s.map.has('v3.chat.draft.a'),false);
});
test('a created session adopts the new-session draft only when it has none',()=>{
 const s=memory();saveDraft('','first',s);adoptNewDraft('x',s);
 assert.equal(readDraft('x',s),'first');assert.equal(readDraft('',s),'');
 saveDraft('','second',s);adoptNewDraft('x',s);assert.equal(readDraft('x',s),'first');assert.equal(readDraft('',s),'second');
});
test('blocked storage reads empty and ignores writes',()=>{
 const s={getItem(){throw Error('blocked');},setItem(){throw Error('blocked');},removeItem(){throw Error('blocked');}};
 assert.equal(readDraft('a',s),'');saveDraft('a','x',s);adoptNewDraft('a',s);
});
