import test from 'node:test';
import assert from 'node:assert/strict';
import { createAudioCapture, LAG_ON_FRAMES, LAG_OFF_FRAMES, BATCH_FRAMES } from './audio-capture.js';

// Minimal Web Audio and microphone stand-ins: a 16 kHz context makes 1600 worklet samples exactly one 3200-byte frame.
function fakeBrowser(){
 let node;
 class Context{
  constructor(){this.sampleRate=16000;this.destination={};this.audioWorklet={addModule:async()=>{}};}
  async resume(){}
  close(){return Promise.resolve();}
  createMediaStreamSource(){return {connect(){},disconnect(){}};}
  createGain(){return {gain:{},connect(){},disconnect(){}};}
 }
 class WorkletNode{constructor(){node=this;this.port={postMessage:m=>{if(m==='flush')setTimeout(()=>this.port.onmessage?.({data:'flushed'}));}};}connect(){}disconnect(){}}
 globalThis.window={AudioContext:Context,AudioWorkletNode:WorkletNode,isSecureContext:true};
 Object.defineProperty(globalThis,'navigator',{value:{mediaDevices:{getUserMedia:async()=>({getTracks:()=>[]})}},configurable:true});
 return {feed(frames){node.port.onmessage({data:new Float32Array(1600*frames)});}};
}
const tick=()=>new Promise(r=>setImmediate(r));

function harness(chunk,start=async()=>({capture_id:'cap'})){
 const events={lag:[],states:[],errors:[],text:[],done:[],starts:0},held=[];
 const send=async(_sid,body)=>{
  if(body.op==='start'){events.starts++;return start();}
  if(body.op==='cancel')return {cancelled:true};
  if(body.op==='finish')return {text:'done'};
  return chunk(body,held);
 };
 const capture=createAudioCapture({send,onState:s=>events.states.push(s),onText:t=>events.text.push(t),onComplete:t=>{events.done.push(t);},onError:e=>events.errors.push(e),onLag:on=>events.lag.push(on)});
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
  assert.ok(sent>1&&sent<LAG_ON_FRAMES,'a backlog drains in batches of up to BATCH_FRAMES frames');
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

const frames=body=>Buffer.from(body.data,'base64').length/3200;

test('audio recorded before the server capture opens is sent first, and a backlog goes in batches',async()=>{
 const browser=fakeBrowser();let open;const sent=[];
 const {capture,events}=harness(async body=>{sent.push(body);return {text:'t'+body.seq};},()=>new Promise(r=>{open=()=>r({capture_id:'cap'});}));
 try{
  assert.equal(await capture.start('s1'),true);
  browser.feed(BATCH_FRAMES+3);await tick();
  assert.deepEqual(sent,[],'nothing is sent before the capture opens');
  open();for(let i=0;i<5;i++)await tick();
  assert.deepEqual(sent.map(b=>[b.seq,frames(b),b.capture_id]),[[1,BATCH_FRAMES,'cap'],[2,3,'cap']]);
  assert.deepEqual(events.text,['t1','t2']);
 }finally{capture.cancel();}
});

test('an expired server capture is restarted with the whole recording, hiding its catch-up text',async()=>{
 const browser=fakeBrowser();let expire=true,ids=0;const sent=[];
 const {capture,events}=harness(async body=>{
  sent.push(body);
  if(body.capture_id==='cap1'&&body.seq===2&&expire){expire=false;throw Object.assign(Error('Dictation expired; start it again'),{status:409});}
  return {text:body.capture_id+':'+body.seq};
 },async()=>({capture_id:'cap'+(++ids)}));
 try{
  await capture.start('s1');
  browser.feed(1);await tick();
  browser.feed(1);for(let i=0;i<6;i++)await tick();
  assert.equal(events.starts,2);
  assert.deepEqual(sent.filter(b=>b.capture_id==='cap2').map(b=>[b.seq,frames(b)]),[[1,2]],'the new capture gets every frame recorded so far');
  assert.deepEqual(events.text,['cap1:1','cap2:1']);
  assert.deepEqual(events.errors,[]);
  await capture.finish();
  assert.deepEqual(events.done,['done']);
 }finally{capture.cancel();}
});

test('requests keep retrying past a short outage instead of ending the dictation',async()=>{
 const browser=fakeBrowser();let failures=3;
 const {capture,events}=harness(async()=>{if(failures-->0)throw Error('offline');return {text:'back'};});
 try{
  await capture.start('s1');
  browser.feed(1);
  for(let waited=0;waited<4000&&!events.text.length;waited+=50)await new Promise(r=>setTimeout(r,50));
  assert.deepEqual(events.text,['back']);
  assert.deepEqual(events.errors,[]);
  assert.equal(events.states.at(-1),'listening');
 }finally{capture.cancel();}
});
