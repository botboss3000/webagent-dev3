import {test} from 'node:test';
import assert from 'node:assert/strict';
import {changeParts,modelRows} from './session-controls.js';

const text=parts=>parts.map(([label,name],index)=>(index?' · ':'')+label+name).join('');
test('the change row names the mode and model against where the turn started',()=>{
  const start={mode:'Ask',model:'Haiku'};
  assert.equal(text(changeParts(start,{mode:'Plan',model:'Fable'})),'Mode changed to Plan · Model changed to Fable');
  assert.equal(text(changeParts(start,{mode:'Auto',model:'Haiku'})),'Mode changed to Auto · Model stays Haiku');
  assert.equal(text(changeParts(start,{mode:'Ask',model:'Opus'})),'Model changed to Opus');
  assert.deepEqual(changeParts(start,{mode:'Ask',model:'Haiku'}),[]);
});

const fallback={entry_id:null,label:'Mode default · opencode deepseek',display:'opencode deepseek',model:'deepseek-v4'};
const claude={entry_id:'a',label:'Claude',display:'Claude',model:'claude'};
const deepseek={entry_id:'b',label:'opencode deepseek',display:'opencode deepseek',model:'deepseek-v4'};

test('the picker hides the mode-default row when a roster model is the same model',()=>{
  const {rows,selectedId}=modelRows([fallback,claude,deepseek],{entry_id:null});
  assert.deepEqual(rows.map(row=>row.entry_id),['a','b']);
  assert.equal(selectedId,'b');
});

test('the picker keeps the mode-default row when no roster model matches it',()=>{
  const {rows,selectedId}=modelRows([fallback,claude],{entry_id:null});
  assert.deepEqual(rows.map(row=>row.entry_id),[null,'a']);
  assert.equal(selectedId,null);
});

test('an explicitly pinned model stays the selected row',()=>{
  const {selectedId}=modelRows([fallback,claude,deepseek],{entry_id:'a'});
  assert.equal(selectedId,'a');
});
