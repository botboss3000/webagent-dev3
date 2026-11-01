// @code-index {"version":1,"feature":"chat-composer-media","aliases":["voice capture"],"purpose":"Locates V3 dictation microphone capture, framing, and transcript callbacks Config: ui/chat/chat.json -> chat_limits.attachments.{max_file_size_mb,max_per_message}; chat_common.{active_footer,idle_footer}.chat_pill.controls.{attach,mic,mic_send}. Profile keys merge chat_common with chat_desktop/chat_mobile/chat_widget overrides.","span_lines":60,"configuration":{"status":"configured","references":[{"path":"ui/chat/chat.json","pointers":["/chat_limits/attachments","/chat_common/active_footer/chat_pill/controls","/chat_common/idle_footer/chat_pill/controls"],"profile_overrides":true}],"opportunities":[]}}
// PCM16 dictation capture streamed to the local recognizer by authenticated V3 HTTP frames.
/** Purpose: Convert mono float samples to clamped PCM16 at the requested rate. Empty input, a falsey inputRate, or outputRate above inputRate returns an empty buffer; otherwise rates are expected to be positive and finite. */
export function downsampleToPcm16(input,inputRate,outputRate=16000){
 if(!input?.length||!inputRate||outputRate>inputRate)return new ArrayBuffer(0);
 const ratio=inputRate/outputRate,out=new Int16Array(Math.max(1,Math.floor(input.length/ratio)));
 for(let i=0;i<out.length;i++){const start=Math.floor(i*ratio),end=Math.min(input.length,Math.max(start+1,Math.floor((i+1)*ratio)));let sum=0;for(let j=start;j<end;j++)sum+=input[j];const sample=Math.max(-1,Math.min(1,sum/(end-start)));out[i]=sample<0?Math.round(sample*32768):Math.round(sample*32767);}return out.buffer;
}
/** Purpose: Base64-encode bytes in bounded chunks so large recordings do not exceed spread-argument limits. */
export function encodeBytes(bytes){let s='';for(let at=0;at<bytes.length;at+=8192)s+=String.fromCharCode(...bytes.subarray(at,at+8192));return btoa(s);}
/** Purpose: Own one dictation's microphone lifecycle and expose start, finish, cancel, and active state. Callbacks report progress, the whole transcript so far, the final transcript, and terminal errors; generation checks fence stale work after cancellation or session changes. composer-media.js supplies the send hook through client.voiceAudio. */
export function createAudioCapture({send,onState,onText,onComplete,onError}){
 let active=null,generation=0;
  // Purpose: Accept asynchronous work only while this capture still owns the active generation.
 const valid=c=>active===c&&c.generation===generation;
  // Purpose: Release capture timers, Web Audio nodes, context, and microphone tracks; context-close rejection is ignored during teardown.
 function stop(c){clearTimeout(c.timer);c.processor?.disconnect();c.source?.disconnect();c.sink?.disconnect();c.context?.close().catch(()=>{});c.stream?.getTracks().forEach(t=>t.stop());}
  // Purpose: Send a capture-scoped frame or terminal operation, retrying at most twice only for retryable failures while this generation remains active.
 async function request(c,body){for(let n=0;;n++){if(!valid(c))throw Error('Dictation cancelled');try{return await send(c.sid,{capture_id:c.id,...body});}catch(e){if(n>=2||!valid(c)||(e.status&&e.status<500))throw e;onState('reconnecting');await new Promise(r=>setTimeout(r,350*2**n));}}}
  // Purpose: Tear down a current capture, best-effort cancel its server capture, and notify the owner once.
 function fail(c,error){if(!valid(c))return;stop(c);active=null;++generation;if(c.id)send(c.sid,{op:'cancel',capture_id:c.id}).catch(()=>{});onState('idle');onError(error);}
  // Purpose: Serialize queued PCM frames, forward the server's transcript so far, and finish after the queue drains; request errors fail the capture.
 async function pump(c){
  if(c.pumping)return;c.pumping=true;
  try{
   while(valid(c)&&c.queue.length){const data=encodeBytes(c.queue.shift());const r=await request(c,{op:'chunk',seq:++c.seq,data});if(!valid(c))return;if(typeof r.text==='string')onText(r.text);}
   if(valid(c)&&c.finishing){onState('transcribing');const r=await request(c,{op:'finish'});if(!valid(c))return;active=null;++generation;onState('idle');await onComplete(r.text||'',c.sid);}
  }catch(e){fail(c,e);}finally{c.pumping=false;}
 }
  // Purpose: Buffer PCM into 0.2-second frames so text keeps pace with speech, and fail visibly when more than 64 frames accumulate.
 function append(c,bytes){const next=new Uint8Array(c.pending.length+bytes.length);next.set(c.pending);next.set(bytes,c.pending.length);c.pending=next;while(c.pending.length>=6400){c.queue.push(c.pending.slice(0,6400));c.pending=c.pending.slice(6400);}if(c.queue.length>64){fail(c,Error('Voice connection is too slow. Try again.'));return;}pump(c);}
  // Purpose: Finish once, flushing the final PCM tail before the queue is drained.
 async function finish(){const c=active;if(!c||c.finishing)return;c.finishing=true;stop(c);if(c.pending.length)c.queue.push(c.pending);c.pending=new Uint8Array();onState('finishing');await pump(c);}
  // Purpose: Acquire secure-context microphone access and start a server dictation streaming Web Audio PCM. Returns false when already active or invalidated. Secure-context or microphone-availability preflight errors reject before capture creation; failures during later setup are reported through onError and return false.
 async function start(sid){
  if(active)return false;
  const Context=window.AudioContext||window.webkitAudioContext;
  if(!window.isSecureContext||!navigator.mediaDevices?.getUserMedia||!Context)throw Error('Dictation needs microphone access on a secure connection.');
  const c={sid,generation:++generation,id:'',seq:0,queue:[],pending:new Uint8Array(),pumping:false,finishing:false};active=c;
  try{
   onState('requesting_microphone');c.stream=await navigator.mediaDevices.getUserMedia({audio:{channelCount:1,echoCancellation:true,noiseSuppression:true,autoGainControl:true}});
   if(!valid(c)){stop(c);return false;}
   onState('connecting');const ready=await send(sid,{op:'start'});c.id=ready.capture_id;
   if(!valid(c)){stop(c);send(sid,{op:'cancel',capture_id:c.id}).catch(()=>{});return false;}
      // Emits downsampled PCM only for the current capture and stops accepting samples after finish begins.
   c.context=new Context();await c.context.resume();c.source=c.context.createMediaStreamSource(c.stream);c.processor=c.context.createScriptProcessor(4096,1,1);c.sink=c.context.createGain();c.sink.gain.value=0;c.processor.onaudioprocess=e=>{if(valid(c)&&!c.finishing)append(c,new Uint8Array(downsampleToPcm16(e.inputBuffer.getChannelData(0),c.context.sampleRate)));};c.source.connect(c.processor);c.processor.connect(c.sink);c.sink.connect(c.context.destination);
      // The server accepts five minutes of audio per dictation.
   c.timer=setTimeout(()=>finish(),300000);onState('listening');return true;
  }catch(e){fail(c,e);return false;}
 }
  // Purpose: Invalidate the active generation immediately, stop local recording, and best-effort cancel the server capture.
 function cancel(){const c=active;if(!c)return;active=null;++generation;stop(c);if(c.id)send(c.sid,{op:'cancel',capture_id:c.id}).catch(()=>{});onState('idle');}
  // Public API: start/finish/cancel control the single capture; active reflects whether one is still owned.
 return {start,finish,cancel,get active(){return !!active;}};
}
