// @code-index {"version":1,"feature":"chat-composer-media","aliases":["chat footer","composer media","voice capture"],"purpose":"Locates composer attachments and dictation behavior Config: ui/chat/chat.json -> chat_limits.attachments.{max_file_size_mb,max_per_message}; chat_common.{active_footer,idle_footer}.chat_pill.controls.{attach,mic,mic_send}. Profile keys merge chat_common with chat_desktop/chat_mobile/chat_widget overrides.","span_lines":80,"configuration":{"status":"configured","references":[{"path":"ui/chat/chat.json","pointers":["/chat_limits/attachments","/chat_common/active_footer/chat_pill/controls","/chat_common/idle_footer/chat_pill/controls"],"profile_overrides":true}],"opportunities":[]}}
import {createAudioCapture,encodeBytes} from './audio-capture.js';
// V2 attachment staging and composer dictation; the server recognizes speech locally.
/** Purpose: Prefer the browser-reported media type, falling back to the supported filename map or a generic binary type. */
export function inferMedia(file){
 if(file.type)return file.type.split(';')[0].toLowerCase();
 return ({txt:'text/plain',md:'text/markdown',csv:'text/csv',tsv:'text/tab-separated-values',json:'application/json',jsonl:'application/x-ndjson',yaml:'application/yaml',yml:'application/yaml',toml:'application/toml',xml:'application/xml',html:'text/html',pdf:'application/pdf',png:'image/png',jpg:'image/jpeg',jpeg:'image/jpeg',webp:'image/webp',gif:'image/gif',svg:'image/svg+xml',mp3:'audio/mpeg',wav:'audio/wav',ogg:'audio/ogg',m4a:'audio/mp4',mp4:'video/mp4',webm:'video/webm'})[file.name.split('.').pop().toLowerCase()]||'application/octet-stream';
}
/** Purpose: Restrict attachments to supported image, audio, video, text, and explicitly listed document types. */
export function allowedFile(file){const type=inferMedia(file);return ['image/','audio/','video/','text/'].some(p=>type.startsWith(p))||['application/pdf','application/json','application/ld+json','application/x-ndjson','application/x-jsonlines','application/x-yaml','application/yaml','application/toml','application/xml'].includes(type);}
/** Purpose: Remove markup, links, and credential-shaped text before speech, keeping line breaks (the server turns them into sentence pauses); truncate the body at maxChars and append a spoken note that the remainder is available on screen when truncated. */
export function textForSpeech(value,maxChars=1800){
 const text=String(value||'').replace(/\x60\x60\x60[\s\S]*?\x60\x60\x60/g,' Code omitted. ').replace(/\x60([^\x60]+)\x60/g,'$1').replace(/!\[[^\]]*\]\([^)]*\)/g,'').replace(/\[([^\]]+)\]\([^)]*\)/g,'$1').replace(/https?:\/\/\S+/g,' link omitted ').replace(/<[^>]+>/g,' ').replace(/\b(?:bearer\s+)?(?:sk-[a-z0-9_-]{12,}|api[_ -]?key\s*[:=]\s*\S+|token\s*[:=]\s*[a-z0-9._-]{12,})/gi,'[redacted]').replace(/[*_~#>|]/g,'').replace(/[^\S\n]+/g,' ').replace(/ ?\n\s*/g,'\n').trim();
 return text.length<=maxChars?text:text.slice(0,maxChars).replace(/\s+\S*$/,'')+'. The rest is available on screen.';
}
/** Purpose: Mount per-session attachment staging and dictation. Files are staged as selected; snapshot refuses submission while any upload is pending, failed, or incomplete. Reset and destroy invalidate session-bound async work and release local media resources. panel.js mounts this module, opens/disconnects/destroys it, and brackets sends with snapshot/committed while reading count/uploading. */
/** Whether uploads need a description: false only when the selected model reads images itself. */
export function describesUploads(controls){return !controls?.models?.find(m=>m.entry_id===(controls.model?.entry_id??null))?.image_in;}
export function mountComposerMedia({root,client,ensureSession,note,changed,refresh,guard,getState}){
 // An image-capable selected model receives images directly, so no description is requested.
 let describeImages=true;
  // Purpose: Resolve control IDs within the supplied chat root.
 const $=id=>root.querySelector('#'+id),pill=$('chat-input-row'),preview=$('chat-preview-bar'),mic=$('chat-voice-btn'),input=$('chat-input');
 const abort=new AbortController(),staged=new Map();
 let attachmentLimits={max_file_size_mb:null,max_per_message:null};
 let available=false,session='',generation=0,disposed=false;
  // Composer text around the dictation insertion point, captured when dictation starts.
 let around=null,typingTimer=null;
  // Stop any in-progress letter-by-letter typing animation.
 function stopTyping(){if(typingTimer){clearTimeout(typingTimer);typingTimer=null;}}
  // Type the transcript so far between the text that was before and after the cursor, keeping the caret after it.
  // New transcript text is revealed one character at a time for a typed-on-keyboard feel; corrections apply instantly.
 function dictate(text){
  if(!around)return;const target=String(text||'');const lead=around.before&&target&&!/\s$/.test(around.before)?' ':'',trail=around.after&&target&&!/^\s/.test(around.after)?' ':'';
  around.lead=lead;around.trail=trail;around.targetTranscript=target;
  const prev=around.lastTarget||'';
  // Non-incremental change: apply the whole transcript immediately.
  if(!target.startsWith(prev)||target.length<prev.length){around.shown=target.length;around.lastTarget=target;const full=around.before+lead+target+(target?trail:'')+around.after;input.value=full;const at=(around.before+lead+target).length;input.setSelectionRange(at,at);input.scrollTop=input.scrollHeight;input.dispatchEvent(new Event('input',{bubbles:true}));return;}
  around.lastTarget=target;
  if(!typingTimer)typeNext();
 }
  // Reveal one more character of the dictated transcript, then schedule the next.
 function typeNext(){
  if(!around){typingTimer=null;return;}
  const target=around.targetTranscript||'',lead=around.lead||'',trail=around.shown>=target.length?(around.trail||''):'';
  if(around.shown<target.length){around.shown++;const shownText=target.slice(0,around.shown);const finalTrail=around.shown>=target.length?(around.trail||''):'';
   input.value=around.before+lead+shownText+finalTrail+around.after;const at=(around.before+lead+shownText).length;input.setSelectionRange(at,at);input.scrollTop=input.scrollHeight;input.dispatchEvent(new Event('input',{bubbles:true}));
   typingTimer=setTimeout(typeNext,18);}else{typingTimer=null;}
 }
  // Rebuild attachment previews from the active session and synchronize composer availability, count, and voice-button state.
 function paint(){
  const files=staged.get(session)||[];preview.replaceChildren();
  for(const item of files){
   const card=document.createElement('div');card.className='chat-preview-item';
   if(item.url){const image=document.createElement('img');image.src=item.url;image.alt=item.file.name;card.append(image);}
   const label=document.createElement('span');label.textContent=item.file.name+(item.error?' — upload failed':item.pending?' — uploading…':'');label.title=item.issue||item.error||item.file.name;card.append(label);
   if(item.error){const retry=document.createElement('button');retry.type='button';retry.textContent='Retry';retry.onclick=guard(()=>upload(item));card.append(retry);}
   const remove=document.createElement('button');remove.type='button';remove.className='chat-preview-remove';remove.textContent='×';remove.setAttribute('aria-label','Remove '+item.file.name);remove.onclick=()=>removeFile(item);card.append(remove);preview.append(card);
  }
  preview.style.display=files.length?'':'none';pill.dataset.attachmentCount=String(files.length);pill.classList.toggle('has-attachments',!!files.length);pill.dataset.composerAvailable=String(available);$('chat-attach-btn').disabled=!available;mic.disabled=!available||!!getState().shutoff;changed();
 }
  // Remove a staged item, revoke its preview URL, and remove an already-uploaded server copy best-effort.
 function removeFile(item){item.removed=true;if(item.url)URL.revokeObjectURL(item.url);staged.set(item.sid,(staged.get(item.sid)||[]).filter(x=>x!==item));if(item.uploaded)client.removeUpload(item.sid,item.id).catch(e=>note(e.message));paint();}
  // Upload one staged file and optionally request its preview description; preserve retryable error state and report failures only in the owning session.
 async function upload(item){
  if(item.pending||item.removed)return;item.pending=true;item.error='';paint();
  try{const data=encodeBytes(new Uint8Array(await item.file.arrayBuffer()));const info=await client.upload(item.sid,{client_upload_id:item.id,name:item.file.name,media_type:inferMedia(item.file),data});item.uploaded=true;if(item.removed){await client.removeUpload(item.sid,item.id);return;}if(info.preview&&describeImages){try{await client.describeUpload(item.sid,item.id);}catch(e){item.issue=e.message;if(!disposed&&item.sid===session&&!item.removed)note(item.file.name+': '+e.message);}}}
  catch(e){item.error=e.message;if(!disposed&&item.sid===session&&!item.removed)note(item.file.name+': '+e.message);}finally{item.pending=false;if(!disposed)paint();}
 }
  // Validate and stage selected, pasted, or dropped files for the current ensured session, applying server-provided count and size limits.
 async function addFiles(files){
  if(!available)return;
  const candidates=Array.from(files||[]);if(!candidates.length)return;
  const origin=session,epoch=generation,sid=await ensureSession();if(disposed||(origin&&origin!==session)||(epoch!==generation&&session!==sid))return;
  const items=staged.get(sid)||[];
  for(const file of candidates){if(attachmentLimits.max_per_message!=null&&items.length>=attachmentLimits.max_per_message){note('At most '+attachmentLimits.max_per_message+' attachments per message.');break;}if(!allowedFile(file)||!file.size||(attachmentLimits.max_file_size_mb!=null&&file.size>attachmentLimits.max_file_size_mb*1024*1024)){note(file.name+': unsupported type or empty file'+(attachmentLimits.max_file_size_mb!=null?', or larger than '+attachmentLimits.max_file_size_mb+' MB.':'.'));continue;}
   const item={sid,id:crypto.randomUUID(),file,pending:false,uploaded:false,removed:false,error:'',url:inferMedia(file).startsWith('image/')&&inferMedia(file)!=='image/svg+xml'?URL.createObjectURL(file):''};items.push(item);staged.set(sid,items);paint();await upload(item);
  }
 }
  // Button activation opens the hidden picker; change clears its value first so the same file can be selected again.
 const attach=$('chat-attach-btn'),chooser=$('chat-file-input');attach.type='button';mic.type='button';attach.onclick=()=>chooser.click();
 chooser.onchange=guard(async()=>{const files=Array.from(chooser.files||[]);chooser.value='';await addFiles(files);});
  // Paste/drop listeners share addFiles; the AbortController below owns their lifetime.
 pill.addEventListener('paste',e=>{const files=e.clipboardData?.files;if(files?.length){e.preventDefault();guard(()=>addFiles(files))();}},{signal:abort.signal});
 pill.addEventListener('dragover',e=>{if(e.dataTransfer?.types.includes('Files')){e.preventDefault();pill.classList.add('dragover');}},{signal:abort.signal});
 pill.addEventListener('dragleave',()=>pill.classList.remove('dragover'),{signal:abort.signal});
 pill.addEventListener('drop',e=>{if(e.dataTransfer?.files.length){e.preventDefault();pill.classList.remove('dragover');guard(()=>addFiles(e.dataTransfer.files))();}},{signal:abort.signal});
  // Reflect capture state in the microphone (a stop square while listening), its accessibility label, and the composer, which is read-only while text is typed into it.
 // A slow dictation connection flashes the microphone orange (.lagging) instead of showing a toast; it clears when the upload backlog drains.
 let voiceState='idle',voiceLagging=false;
 function state(value){
  voiceState=value;if(value==='idle')voiceLagging=false;
  const recording=['listening','reconnecting'].includes(value),busy=value!=='idle'&&!recording;mic.classList.toggle('recording',recording);mic.classList.toggle('transcribing',busy);mic.classList.toggle('lagging',voiceLagging);mic.setAttribute('aria-pressed',String(value!=='idle'));mic.title=(recording?'Stop dictation':busy?{requesting_microphone:'Waiting for microphone permission…',reconnecting:'Reconnecting…'}[value]||'Finishing dictation…':'Dictate')+(voiceLagging?' (slow connection, catching up)':'');mic.setAttribute('aria-label',mic.title);pill.classList.toggle('dictating',value!=='idle');
  if(value==='idle'){stopTyping();input.readOnly=false;}changed?.();
 }
  // Stop typing into the composer and hand it back to the user with the caret after the dictated text.
 function release(){stopTyping();around=null;input.readOnly=false;if(!disposed&&document.activeElement!==input)input.focus();}
  // Bridge capture frames to client.voiceAudio; each transcript update and the final text are typed at the cursor, and errors leave what was typed.
 const capture=createAudioCapture({send:(id,b)=>client.voiceAudio(id,b),onState:state,onLag:on=>{voiceLagging=!!on;state(voiceState);},onText:dictate,onComplete:text=>{dictate(text);const finishRelease=()=>{if(typingTimer){setTimeout(finishRelease,30);return;}if(around&&!String(text||'').trim())note('No speech detected.');release();};finishRelease();},onError:e=>{release();note(e.name==='NotAllowedError'?'Microphone permission is blocked. Allow it in the browser site controls and try again.':e.message);}});
  // Toggle: stop the active dictation, or start one at the cursor once availability and session-generation checks pass.
 mic.onclick=guard(async()=>{if(capture.active){await capture.finish();return;}if(!available)return;const epoch=generation,origin=session;const sid=await ensureSession();if(disposed||(origin&&origin!==session)||(epoch!==generation&&session!==sid))return;around={before:input.value.slice(0,input.selectionStart),after:input.value.slice(input.selectionEnd),targetTranscript:'',shown:0,lastTarget:'',lead:'',trail:''};input.readOnly=true;try{if(!await capture.start(sid))release();}catch(e){release();throw e;}});
 input.addEventListener('click',()=>{if(!around)return;const start=input.selectionStart,end=input.selectionEnd;stopTyping();capture.cancel();around=null;input.readOnly=false;input.setSelectionRange(start,end);},{signal:abort.signal});
  // Cancel any dictation before displaying a new session.
 function open(id){stopTyping();capture.cancel();around=null;input.readOnly=false;++generation;session=id;paint();}
  // Return staged upload IDs for submission, refusing while any file is pending, failed, or not uploaded.
 function snapshot(sid=session){const files=staged.get(sid)||[];if(files.some(i=>i.pending||i.error||!i.uploaded))throw Error('Finish or remove failed uploads before sending.');return files.map(i=>i.id);}
  // Retire only the submitted IDs and revoke their preview URLs after the message is accepted.
 function committed(sid,ids){const files=staged.get(sid)||[];for(const i of files.filter(i=>ids.includes(i.id))){i.removed=true;if(i.url)URL.revokeObjectURL(i.url);}staged.set(sid,files.filter(i=>!ids.includes(i.id)));paint();}
  // Tear down dictation, listeners, and object URLs; request removal of uploaded attachments best-effort without waiting for server cleanup.
 function destroy(){disposed=true;stopTyping();capture.cancel();input.readOnly=false;abort.abort();attach.onclick=null;chooser.onchange=null;mic.onclick=null;for(const files of staged.values())for(const i of files){i.removed=true;if(i.url)URL.revokeObjectURL(i.url);if(i.uploaded)client.removeUpload(i.sid,i.id).catch(()=>{});}}
  // Public API: open resets the session view; snapshot/committed bracket submission; setAvailable applies runtime limits; setDescribeImages(false) skips upload descriptions while the selected model reads images itself; disconnect and destroy release the microphone; count/uploading expose current-session composer state.
 return {open,snapshot,committed,destroy,setDescribeImages(value){describeImages=value!==false;},setAvailable(value,limits){attachmentLimits={max_file_size_mb:null,max_per_message:null,...limits};if(available!==value){available=value;paint();}},disconnect:()=>{capture.cancel();input.readOnly=false;},get dictating(){return!!around;},get count(){return(staged.get(session)||[]).length;},get uploading(){return(staged.get(session)||[]).some(i=>i.pending||i.error);}};
}
