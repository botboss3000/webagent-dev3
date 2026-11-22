import {test} from 'node:test';
import assert from 'node:assert/strict';
import {changeParts} from './session-controls.js';

const text=parts=>parts.map(([label,name],index)=>(index?' · ':'')+label+name).join('');
test('the change row names the mode and model against where the turn started',()=>{
  const start={mode:'Ask',model:'Haiku'};
  assert.equal(text(changeParts(start,{mode:'Plan',model:'Fable'})),'Mode changed to Plan · Model changed to Fable');
  assert.equal(text(changeParts(start,{mode:'Auto',model:'Haiku'})),'Mode changed to Auto · Model stays Haiku');
  assert.equal(text(changeParts(start,{mode:'Ask',model:'Opus'})),'Model changed to Opus');
  assert.deepEqual(changeParts(start,{mode:'Ask',model:'Haiku'}),[]);
});
