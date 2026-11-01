import test from 'node:test';
import assert from 'node:assert/strict';
import { createAudioCapture, LAG_ON_FRAMES, LAG_OFF_FRAMES } from './audio-capture.js';

// Minimal Web Audio and microphone stand-ins: a 16 kHz context makes 1600 input samples exactly one 3200-byte frame.
function fakeBrowser(){
 let processor;
 class Context{
  constructor(){this.sampleRate=16000;this.destination={};}
  async resume(){}
  close(){return Promise.resolve();}
  createMediaStreamSource(){return {connect(){},disconnect(){}};}
  createScriptProcessor(){processor={connect(){},disconnect(){}};return processor;}
  createGain(){return {gain:{},connect(){},disconnect(){}};}
 }
 globalThis.window={AudioContext:Context,isSecureContext:true};
 Object.defineProperty(globalThis,'navigator',{value:{mediaDevices:{getUserMedia:async()=>({getTracks:()=>[]})}},configurable:true});
 return {feed(frames){processor.onaudioprocess({inputBuffer:{getChannelData:()=>new Float32Array(1600*frames)}});}};
}
const tick=()=>new Promise(r=>setImmediate(r));

function harness(chunk){
 const events={lag:[],states:[],errors:[]},held=[];
 const send=async(_sid,body)=>{
  if(body.op==='start')return {capture_id:'cap'};
  if(body.op==='cancel')return {cancelled:true};
  if(body.op==='finish')return {text:'done'};
  return chunk(body,held);
 };
 const capture=createAudioCapture({send,onState:s=>events.states.push(s),onText(){},onComplete(){},onError:e=>events.errors.push(e),onLag:on=>events.lag.push(on)});
 return {capture,events,held};
}

// Every test cancels in finally so a failed assertion never leaves the five-minute capture timer running.
test('a send backlog flashes lag instead of failing, and clears once it drains',async()=>{
 const browser=fakeBrowser();
 const {capture,events,held}=harness((_body,held)=>new Promise(resolve=>held.push(()=>resolve({text:''}))));
 try{
  assert.equal(await capture.start('s1'),true);
  browser.feed(1);await tick();
  assert.equal(held.length,1);
  browser.feed(LAG_ON_FRAMES);await tick();
  assert.deepEqual(events.lag,[],'a backlog at the threshold is not yet lag');
  browser.feed(1);await tick();
  assert.deepEqual(events.lag,[true]);
  // A huge backlog (well past the old 128-frame toast limit) keeps capturing without an error.
  browser.feed(200);await tick();
  assert.deepEqual(events.errors,[]);
  assert.equal(capture.active,true);
  let sent=0;
  while(events.lag.length<2&&held.length&&sent<1000){held.shift()();sent++;await tick();}
  assert.deepEqual(events.lag,[true,false]);
  assert.ok(sent>LAG_ON_FRAMES,'lag clears only after the backlog drains to LAG_OFF_FRAMES');
  assert.deepEqual(events.errors,[]);
 }finally{capture.cancel();}
 assert.deepEqual(events.lag,[true,false]);
});

test('cancel clears an active lag signal',async()=>{
 const browser=fakeBrowser();
 const {capture,events}=harness(()=>new Promise(()=>{}));
 try{
  await capture.start('s1');
  browser.feed(LAG_ON_FRAMES+2);await tick();
  assert.deepEqual(events.lag,[true]);
 }finally{capture.cancel();}
 assert.deepEqual(events.lag,[true,false]);
 assert.equal(events.states.at(-1),'idle');
 assert.deepEqual(events.errors,[]);
});

test('a retried request flashes lag and restores listening when it succeeds',async()=>{
 const browser=fakeBrowser();
 let failures=1;
 const {capture,events}=harness(async()=>{if(failures-->0)throw Object.assign(Error('busy'),{status:503});return {text:'hi'};});
 try{
  await capture.start('s1');
  browser.feed(1);await tick();
  assert.deepEqual(events.lag,[true]);
  assert.equal(events.states.at(-1),'reconnecting');
  for(let waited=0;waited<3000&&events.lag.length<2;waited+=50)await new Promise(r=>setTimeout(r,50));
  assert.deepEqual(events.lag,[true,false]);
  assert.equal(events.states.at(-1),'listening');
  assert.deepEqual(events.errors,[]);
  assert.ok(LAG_OFF_FRAMES<LAG_ON_FRAMES);
 }finally{capture.cancel();}
});
