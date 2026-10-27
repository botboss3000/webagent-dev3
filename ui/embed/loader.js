// @code-index {"version":1,"feature":"chat-embed-host","aliases":["embedded chat host","iframe chat widget","embed bridge"],"purpose":"Creates the floating embed host and launcher from the descriptor profile, then exposes the authenticated iframe bridge and bounded parent command API. Origin/channel validation, authorization and command checks remain fixed security invariants. Touch feedback: ui/vibrate.json -> enabled, duration_ms; gestures stay client-owned.","span_lines":80,"configuration":{"status":"configured","references":[{"path":"ui/chat/chat.json","pointers":["/chat_common","/chat_widget"],"profile_overrides":false},{"path":"ui/vibrate.json","pointers":["/enabled","/duration_ms"]}],"opportunities":[],"rationale":"These pointers control presentation only; origin binding, channel validation and authorization checks are fixed security invariants."}}
(() => {
 'use strict';
 const script=document.currentScript;
 if(!script)return;
 // Purpose: Create one shadow-root host, iframe, floating shell, launcher, and public API after the document body exists.
 function mount(){
 if(window.WebAgentWidget)return;
 const agent=script.dataset.agent;
 if(!agent){console.error('WebAgent widget requires data-agent.');return}
 const origin=new URL(script.src,location.href).origin;
 const namespace='webagent.widget',version=1;
 const host=document.createElement('div');host.setAttribute('data-webagent-widget','');document.body.append(host);
 const shadow=host.attachShadow({mode:'open'});
 const style=document.createElement('link');style.rel='stylesheet';style.href=origin+'/v3-ui/chat/widget/window.css';shadow.append(style);
 const iframe=document.createElement('iframe');iframe.hidden=true;iframe.title='WebAgent chat';iframe.referrerPolicy='origin';iframe.src=origin+'/embed/'+encodeURIComponent(agent)+'?origin='+encodeURIComponent(location.origin);
 // Allow the first-party authentication popup without sharing its credential with the embedding page.
 iframe.setAttribute('allow','clipboard-write; microphone; autoplay');
 let channel=null,shell=null,launcher=null,failed=null,destroyed=false,vibrationConfig=null;
 const requests=new Map(),listeners=new Map();
 let resolveReady,rejectReady;
 const ready=new Promise((resolve,reject)=>{resolveReady=resolve;rejectReady=reject});
 ready.catch(()=>{});
 const timer=setTimeout(()=>{failed=new Error('Chat did not become ready. Reload to try again.');rejectReady(failed)},30000);
 // Purpose: Deliver listener notifications without letting one listener exception interrupt the others.
 function notify(event,data){for(const listener of listeners.get(event)||[]){try{listener(data)}catch(error){console.error(error)}}}
 // Purpose: Accept only messages from this iframe/origin/namespace/version/channel and settle matching commands or trusted UI events.
 function onMessage(event){
  if(destroyed||event.source!==iframe.contentWindow||event.origin!==origin)return;
  const message=event.data;
  if(!message||message.namespace!==namespace||message.version!==version)return;
  if(message.type==='ready'){
   vibrationConfig=null;
   if(typeof message.channel!=='string'||message.channel.length<16)return;
   if(channel&&channel!==message.channel){for(const request of requests.values()){clearTimeout(request.timer);request.reject(new Error('Chat reloaded. Retry the action.'))}requests.clear()}
   channel=message.channel;clearTimeout(timer);resolveReady();command('visibility',{visible:shell?.isOpen()||false}).catch(()=>{});notify('ready',{});return;
  }
  if(!channel||message.channel!==channel)return;
  if(message.type==='result'){
   const request=requests.get(message.id);if(!request)return;requests.delete(message.id);clearTimeout(request.timer);
   if(message.ok)request.resolve(message.result);else request.reject(new Error(typeof message.error==='string'?message.error:'Chat action failed.'));
  }else if(message.type==='event'&&typeof message.event==='string'){if(message.event==='vibration-config'){const value=message.data?.config;vibrationConfig=typeof value?.enabled==='boolean'&&Number.isSafeInteger(value.duration_ms)&&value.duration_ms>=0?{enabled:value.enabled,duration_ms:value.duration_ms}:null;}else if(message.event==='theme'&&['light','dark'].includes(message.data?.theme))host.dataset.theme=message.data.theme;else if(message.event==='minimize')shell?.minimize();else if(message.event==='close')shell?.close();notify(message.event,message.data);}
 }
 window.addEventListener('message',onMessage);
 // Purpose: Send a correlated command after readiness; requests time out after 30 seconds and advise reconciling uncertain effects before retry.
 async function command(name,args={}){
  if(destroyed)throw new Error('Widget has been destroyed.');
  if(failed)throw failed;
  await ready;
  if(destroyed)throw new Error('Widget destroyed.');
  const id=crypto.randomUUID();
  return new Promise((resolve,reject)=>{
   const timer=setTimeout(()=>{requests.delete(id);reject(new Error('Chat action timed out. Check its result before retrying.'))},30000);
   requests.set(id,{resolve,reject,timer});
   iframe.contentWindow.postMessage({namespace,version,type:'command',channel,id,command:name,args},origin);
  });
 }
 // API methods expose the supported chat commands and UI events; destroy rejects pending work and removes host-owned UI/listeners.
 const api={
  async open(){await initialized;shell.open();launcher.hide();await command('visibility',{visible:true});notify('open',{})},
  async minimize(){await initialized;shell.minimize()},
  async close(){await initialized;shell.close()},
  setDraft:text=>command('setDraft',{text}),
  send:text=>command('send',{text}),
  selectSession:sessionId=>command('selectSession',{sessionId}),
  newSession:()=>command('newSession'),
  stop:()=>command('stop'),
  continue:()=>command('continue'),
  approve:(approvalId,decision)=>command('approve',{approvalId,decision}),
  recover:(runId,decision,output)=>command('recover',{runId,decision,output}),
  on(event,listener){if(typeof listener!=='function')throw new TypeError('Listener must be a function.');if(!listeners.has(event))listeners.set(event,new Set());listeners.get(event).add(listener);return ()=>listeners.get(event)?.delete(listener)},
  destroy(){destroyed=true;rejectReady(new Error('Widget destroyed.'));clearTimeout(timer);window.removeEventListener('message',onMessage);for(const request of requests.values()){clearTimeout(request.timer);request.reject(new Error('Widget destroyed.'))}requests.clear();shell?.destroy();launcher?.destroy();host.remove()}
 };
 // Purpose: Load the public embed descriptor, derive the widget profile, and create the iframe shell and launcher.
 const initialized=(async()=>{
  const response=await fetch(origin+'/api/v3/embed/'+encodeURIComponent(agent)+'/descriptor?origin='+encodeURIComponent(location.origin),{credentials:'omit'});
  if(!response.ok)throw new Error('Chat embedding is unavailable for this website.');
  const descriptor=await response.json();
  const {mergeProfile,createFloatingChat,createChatLauncher,createTouchVibration}=await import(origin+'/v3-ui/chat/widget/window.js');
  if(destroyed)throw new Error('Widget destroyed.');
  // Host chrome uses local user activation and the iframe-loaded central UI configuration.
  const feedback=createTouchVibration(()=>vibrationConfig);
  const profile=mergeProfile(descriptor.chat_ui?.chat_common||{},descriptor.chat_ui?.chat_widget||{});
  shell=createFloatingChat({
   panel:iframe,profile,feedback,container:shadow,embedded:true,storageKey:'webagent.widget.'+origin+'.'+agent+'.geometry',
   onMinimize(){launcher.show('minimized');command('visibility',{visible:false}).catch(()=>{});notify('minimize',{})},
   onClose(){launcher.show('closed');command('visibility',{visible:false}).catch(()=>{});notify('close',{})}
  });
  iframe.hidden=false;
  launcher=createChatLauncher({profile,feedback,storageKey:'webagent.widget.'+origin+'.'+agent+'.draft',onOpen:()=>api.open(),onPickup:text=>command('setElementContext',{text}),onAction:async type=>{if(type==='close_all'){await api.close();return}await api.open();if(type==='sessions')await command('showSessions');else if(type==='new_session')await api.newSession();else if(type!=='open_all')throw new Error('Unknown launcher action');},onSend:async text=>{await api.open();return command('send',{text})}});
  shadow.append(launcher.element);
  return descriptor;
 })();
 initialized.catch(error=>{if(destroyed)return;failed=error;rejectReady(error);clearTimeout(timer);const status=document.createElement('div');status.className='wa-chat-launcher';status.setAttribute('role','status');status.textContent=error.message;shadow.append(status);notify('error',{message:error.message})});
 // One loader owns one widget; keep the host API deliberately free of authentication material.
 window.WebAgentWidget=api;
 }
 if(document.body)mount();else document.addEventListener('DOMContentLoaded',mount,{once:true});
})();
