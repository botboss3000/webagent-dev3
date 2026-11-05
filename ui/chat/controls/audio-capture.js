// @code-index {"version":1,"feature":"chat-composer-media","aliases":["voice capture"],"purpose":"Locates V3 dictation microphone capture, framing, and transcript callbacks Config: ui/chat/chat.json -> chat_limits.attachments.{max_file_size_mb,max_per_message}; chat_common.{active_footer,idle_footer}.chat_pill.controls.{attach,mic,mic_send}. Profile keys merge chat_common with chat_desktop/chat_mobile/chat_widget overrides.","span_lines":60,"configuration":{"status":"configured","references":[{"path":"ui/chat/chat.json","pointers":["/chat_limits/attachments","/chat_common/active_footer/chat_pill/controls","/chat_common/idle_footer/chat_pill/controls"],"profile_overrides":true}],"opportunities":[]}}
// PCM16 dictation capture streamed to the local recognizer by authenticated V3 HTTP frames. The browser keeps the whole recording until the dictation ends, so a lost connection or an expired server capture is recovered by sending it again.
/** Purpose: Convert mono float samples to clamped PCM16 at the requested rate. Empty input, a falsey inputRate, or outputRate above inputRate returns an empty buffer; otherwise rates are expected to be positive and finite. */
export function downsampleToPcm16(input,inputRate,outputRate=16000){
 if(!input?.length||!inputRate||outputRate>inputRate)return new ArrayBuffer(0);
 const ratio=inputRate/outputRate,out=new Int16Array(Math.max(1,Math.floor(input.length/ratio)));
 for(let i=0;i<out.length;i++){const start=Math.floor(i*ratio),end=Math.min(input.length,Math.max(start+1,Math.floor((i+1)*ratio)));let sum=0;for(let j=start;j<end;j++)sum+=input[j];const sample=Math.max(-1,Math.min(1,sum/(end-start)));out[i]=sample<0?Math.round(sample*32768):Math.round(sample*32767);}return out.buffer;
}
/** Purpose: Base64-encode bytes in bounded chunks so large recordings do not exceed spread-argument limits. */
export function encodeBytes(bytes){let s='';for(let at=0;at<bytes.length;at+=8192)s+=String.fromCharCode(...bytes.subarray(at,at+8192));return btoa(s);}
/** Frames (0.1 s each) waiting to be sent before the connection counts as lagging, and the backlog it must drain to before lagging clears. */
export const LAG_ON_FRAMES=20,LAG_OFF_FRAMES=5;
/** Frames sent in one request once a backlog has built up: 64,000 bytes, inside the server's 64 KiB frame limit. */
export const BATCH_FRAMES=20;
/** Server captures restarted from the browser's recording before the dictation fails. */
export const MAX_RESTARTS=3;
/** A request with no answer after this long is retried (finish waits for the recognizer, up to 15 s server side); the retry keeps its sequence number, so the server replays rather than repeats it. */
const CHUNK_TIMEOUT_MS=10000,FINISH_TIMEOUT_MS=20000;
/** Requests keep being retried until none has succeeded for this long. */
const GIVE_UP_MS=120000;
/** The AudioWorklet half of the capture; a same-origin module because the page policy refuses blob: scripts. */
const WORKLET=new URL('./audio-capture-worklet.js',import.meta.url).href;
/** Purpose: Join recorded frames into one request body. */
function join(parts){const out=new Uint8Array(parts.reduce((n,p)=>n+p.length,0));let at=0;for(const p of parts){out.set(p,at);at+=p.length;}return out;}
/** Purpose: Own one dictation's microphone lifecycle and expose start, finish, cancel, and active state. Callbacks report progress, the whole transcript so far, the final transcript, terminal errors, and onLag(true|false) while the upload backlog or reconnect retries mean the connection is not keeping up; generation checks fence stale work after cancellation or session changes. composer-media.js supplies the send hook through client.voiceAudio. */
export function createAudioCapture({send,onState,onText,onComplete,onError,onLag=()=>{}}){
 let active=null,generation=0;
  // Purpose: Accept asynchronous work only while this capture still owns the active generation.
 const valid=c=>active===c&&c.generation===generation;
  // Purpose: Report lag transitions once, with hysteresis so a backlog hovering at the threshold does not flicker.
 function lag(c){if(!valid(c))return;const on=c.retrying||c.queue.length>(c.lagging?LAG_OFF_FRAMES:LAG_ON_FRAMES);if(on!==c.lagging){c.lagging=on;onLag(on);}}
  // Purpose: Clear any reported lag when a capture ends for any reason.
 function quiet(c){c.retrying=false;if(c.lagging){c.lagging=false;onLag(false);}}
  // Purpose: Release capture timers, Web Audio nodes, context, and microphone tracks; context-close rejection is ignored during teardown.
 function stop(c){clearTimeout(c.timer);if(c.node)c.node.port.onmessage=null;c.node?.disconnect();c.source?.disconnect();c.sink?.disconnect();c.context?.close().catch(()=>{});c.stream?.getTracks().forEach(t=>t.stop());}
  // Purpose: Send a request for the current server capture, retrying network failures, timeouts and server errors with capped backoff until none has succeeded for GIVE_UP_MS. Client errors are final; pump handles an expired capture. Retries mark the capture lagging until a request succeeds.
 async function request(c,body){
  for(let n=0;;n++){
   if(!valid(c))throw Error('Dictation cancelled');
   let timer;
   try{
    const r=await Promise.race([send(c.sid,c.id?{capture_id:c.id,...body}:body),new Promise((_,reject)=>{timer=setTimeout(()=>reject(Error('Dictation request timed out')),body.op==='finish'?FINISH_TIMEOUT_MS:CHUNK_TIMEOUT_MS);})]);
    c.okAt=Date.now();if(c.retrying&&valid(c)){c.retrying=false;onState(c.finishing?'finishing':'listening');lag(c);}return r;
   }catch(e){if(!valid(c)||(e.status&&e.status<500)||Date.now()-c.okAt>GIVE_UP_MS)throw e;c.retrying=true;lag(c);onState('reconnecting');await new Promise(r=>setTimeout(r,Math.min(5000,350*2**n)));}
   finally{clearTimeout(timer);}
  }
 }
  // Purpose: Tear down a current capture, best-effort cancel its server capture, and notify the owner once.
 function fail(c,error){if(!valid(c))return;quiet(c);stop(c);active=null;++generation;if(c.id)send(c.sid,{op:'cancel',capture_id:c.id}).catch(()=>{});onState('idle');onError(error);}
  // Purpose: Stop recording when the dictation reaches the server's length limit, counted from the first sample.
 function limit(c){clearTimeout(c.timer);if(c.node&&!c.stopping)c.timer=setTimeout(()=>finish(),Math.max(0,c.limitMs-(Date.now()-c.started)));}
  // Purpose: Open a server capture, taking its length limit, and cancel it best-effort if this capture ended meanwhile.
 async function open(c){c.id='';c.seq=0;const ready=await request(c,{op:'start'});if(!valid(c)){send(c.sid,{op:'cancel',capture_id:ready.capture_id}).catch(()=>{});return;}c.id=ready.capture_id;if(ready.max_seconds>0){c.limitMs=ready.max_seconds*1000;limit(c);}}
  // Purpose: Replace an expired server capture (idle too long, its recognizer stopped, or the server restarted) with a new one fed the whole recording again; its partial text stays hidden until that replay is sent.
 function restart(c,e){if(e.status!==409||c.restarts>=MAX_RESTARTS)return false;c.restarts++;c.id='';c.queue=c.recorded.slice();c.replay=c.queue.length;lag(c);return true;}
  // Purpose: Open the server capture, send queued frames (several per request when a backlog has built up), forward the server's transcript so far, and finish after the queue drains. Other request errors fail the capture.
 async function pump(c){
  if(c.pumping)return;c.pumping=true;
  try{
   while(valid(c)&&(!c.id||c.queue.length||c.finishing)){
    if(!c.id){await open(c);continue;}
    if(c.queue.length){
     const frames=c.queue.splice(0,BATCH_FRAMES);let r;
     try{r=await request(c,{op:'chunk',seq:c.seq+1,data:encodeBytes(join(frames))});}catch(e){if(valid(c)&&restart(c,e))continue;throw e;}
     if(!valid(c))return;c.seq++;c.replay=Math.max(0,c.replay-frames.length);lag(c);if(typeof r.text==='string'&&!c.replay)onText(r.text);continue;
    }
    onState('transcribing');let r;
    try{r=await request(c,{op:'finish'});}catch(e){if(valid(c)&&restart(c,e))continue;throw e;}
    if(!valid(c))return;quiet(c);active=null;++generation;onState('idle');await onComplete(r.text||'',c.sid);return;
   }
  }catch(e){fail(c,e);}finally{c.pumping=false;}
 }
  // Purpose: Buffer PCM into 0.1-second frames (3200 bytes of 16 kHz PCM16) so text keeps pace with speech, keeping every frame for a replay. A slow connection only signals lag; the recording is bounded by the server's dictation limit (five minutes, about 9.6 MB, by default) and the backlog drains when the connection recovers.
 function append(c,bytes){const next=new Uint8Array(c.pending.length+bytes.length);next.set(c.pending);next.set(bytes,c.pending.length);c.pending=next;while(c.pending.length>=3200){const frame=c.pending.slice(0,3200);c.queue.push(frame);c.recorded.push(frame);c.pending=c.pending.slice(3200);}lag(c);pump(c);}
  // Purpose: Finish once recording has started: collect the worklet's last partial block, then queue the PCM tail and drain the queue.
 async function finish(){
  const c=active;if(!c||c.stopping||!c.node)return;c.stopping=true;onState('finishing');
  await new Promise(r=>{c.flushed=r;c.node.port.postMessage('flush');setTimeout(r,300);});
  if(!valid(c))return;stop(c);c.finishing=true;if(c.pending.length){c.queue.push(c.pending);c.recorded.push(c.pending);}c.pending=new Uint8Array();await pump(c);
 }
  // Purpose: Acquire secure-context microphone access and record Web Audio PCM from the first sample while the server capture opens alongside. Returns false when already active or invalidated. Secure-context or browser-support preflight errors reject before capture creation; failures during later setup are reported through onError and return false.
 async function start(sid){
  if(active)return false;
  const Context=window.AudioContext||window.webkitAudioContext;
  if(!window.isSecureContext||!navigator.mediaDevices?.getUserMedia||!Context||!window.AudioWorkletNode)throw Error('Dictation needs a current browser with microphone access on a secure connection.');
  const c={sid,generation:++generation,started:Date.now(),limitMs:300000,id:'',seq:0,queue:[],recorded:[],pending:new Uint8Array(),replay:0,restarts:0,okAt:Date.now(),pumping:false,stopping:false,finishing:false,lagging:false,retrying:false};active=c;
  try{
      // Ready before the microphone prompt resolves, so recording begins with the first sample.
   c.context=new Context();const resumed=c.context.resume();
   onState('requesting_microphone');pump(c);
   await c.context.audioWorklet.addModule(WORKLET);if(!valid(c)){stop(c);return false;}
   c.stream=await navigator.mediaDevices.getUserMedia({audio:{channelCount:1,echoCancellation:true,noiseSuppression:true,autoGainControl:true}});
   if(!valid(c)){stop(c);return false;}
      // Emits downsampled PCM only for the current capture and stops accepting samples after finish has collected the last block.
   c.source=c.context.createMediaStreamSource(c.stream);c.node=new window.AudioWorkletNode(c.context,'dictation-capture');c.sink=c.context.createGain();c.sink.gain.value=0;
   c.node.port.onmessage=e=>{if(e.data==='flushed')c.flushed?.();else if(valid(c)&&!c.finishing)append(c,new Uint8Array(downsampleToPcm16(e.data,c.context.sampleRate)));};
   c.source.connect(c.node);c.node.connect(c.sink);c.sink.connect(c.context.destination);await resumed;
      // Five minutes until the server's start reply gives its own limit.
   limit(c);if(!c.retrying)onState('listening');return true;
  }catch(e){fail(c,e);return false;}
 }
  // Purpose: Invalidate the active generation immediately, stop local recording, and best-effort cancel the server capture.
 function cancel(){const c=active;if(!c)return;quiet(c);active=null;++generation;stop(c);if(c.id)send(c.sid,{op:'cancel',capture_id:c.id}).catch(()=>{});onState('idle');}
  // Public API: start/finish/cancel control the single capture; active reflects whether one is still owned.
 return {start,finish,cancel,get active(){return !!active;}};
}
