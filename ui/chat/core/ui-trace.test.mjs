import {test} from 'node:test';
import assert from 'node:assert/strict';
import {describe,frames,mountUiTrace} from './ui-trace.js';

test('elements and stack frames reduce to the structural alphabet',()=>{
  globalThis.document={};globalThis.window={};
  assert.equal(describe({nodeType:1,tagName:'TEXTAREA',id:'chat-input',className:'a b c d'}),'textarea#chat-input.a.b.c');
  assert.equal(frames('Error\n    at HTMLElement.blur (https://dev.webagent.live/v3-ui/chat/core/ui-trace.js:1:1)\n    at applyKb (https://dev.webagent.live/v3-ui/boot/viewport.js?v=2:45:52)\n    at applyAll (https://dev.webagent.live/v3-ui/boot/viewport.js:63:36)'),
    'applyKb (boot/viewport.js:45:52) > applyAll (boot/viewport.js:63:36)');
});
test('collection stays off unless the Diagnostics category enables it',async()=>{
  const calls=[];
  for(const reply of [{enabled:false},null,Promise.reject(Error('offline'))]){
    const dispose=await mountUiTrace({request:async path=>{calls.push(path);return reply;}});
    assert.equal(typeof dispose,'function');dispose();
  }
  assert.deepEqual(calls,Array(3).fill('/diagnostics/ui-trace/config'));
});
