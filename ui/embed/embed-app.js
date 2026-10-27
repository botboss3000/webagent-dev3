// @code-index {"version":1,"feature":"chat-embed-runtime","aliases":["embedded chat runtime","iframe chat session","embedded chat commands"],"purpose":"Locates the embedded chat runtime, credential lifecycle, session wiring, and serialized parent command bridge. Presentation profile is supplied by the public descriptor; authorization policy remains server-authoritative in the encrypted agent embed record. Touch feedback: ui/vibrate.json -> enabled, duration_ms; gestures stay client-owned.","span_lines":80,"configuration":{"status":"configured","references":[{"path":"ui/chat/chat.json","pointers":["/chat_common","/chat_widget"],"profile_overrides":false},{"path":"ui/vibrate.json","pointers":["/enabled","/duration_ms"]}],"opportunities":[]}}
import {vibrationConfigReady,getVibrationConfig} from '../shared/vibrate.js';
import {mountChatPanel} from '../chat/core/panel.js';
import {initChatConfig,setChatSurfaceActions} from '../chat/controls/chat-config.js';
import {watchAuthorization} from './auth-handoff.js';
import {openSignInPanel} from '../chat/core/credential-panel.js';
import {createFetchStream} from '../chat/core/fetch-stream.js';
const params=new URL(location.href).searchParams,hostOrigin=params.get('origin'),agentId=decodeURIComponent(location.pathname.split('/').pop());
const root=document.getElementById('chat-panel');root.dataset.embedVisible='false';window.__CHAT_PORTAL__=true;
const namespace='webagent.widget',version=1,channel=crypto.randomUUID();
// Purpose: Send versioned events only to the origin bound by the embed URL.

const emit=(event,data={})=>parent.postMessage({namespace,version,type:'event',channel,event,data},hostOrigin);
// Purpose: Show a bounded human-facing status message in the embed document.

const note=text=>{document.getElementById('notice').textContent=text||'';};
// Purpose: Turn guarded UI actions into status-reporting handlers while preventing form submission.

const guard=fn=>async event=>{event?.preventDefault();try{await fn(event)}catch(error){note(error.message)}};
let credential=null,identity={},chat,state={shutoff:false},poll,authBusy=false,expires=0,popupChallenge='',popup=null,authWatch=null;
const assetUrls=new Map();
// Purpose: Fetch and cache attachment object URLs with the current embed bearer; reconnect or 401 clears access, and destroy revokes cached URLs.

async function resolveAsset(url){if(assetUrls.has(url))return assetUrls.get(url);if(!credential)throw Error('Reconnect to view attachments');const response=await fetch(url,{credentials:'omit',headers:{Authorization:'Bearer '+credential}});if(!response.ok)throw Error('Attachment access denied');const objectUrl=URL.createObjectURL(await response.blob());assetUrls.set(url,objectUrl);return objectUrl;}
// Purpose: Revoke every cached attachment object URL before replacing credentials or tearing down.

function clearAssets(){for(const url of assetUrls.values())URL.revokeObjectURL(url);assetUrls.clear();}
// Purpose: Reuse one request ID for each logical chat operation key so client retries remain idempotent.

const ids=new Map(),keyId=key=>{if(!ids.has(key))ids.set(key,crypto.randomUUID());return ids.get(key)};
// Purpose: Call only public embed endpoints without cookies or bearer credentials; surface server error text on failure.

async function publicRequest(path,body){const response=await fetch('/api/v3'+path,{method:body===undefined?'GET':'POST',credentials:'omit',headers:{'Content-Type':'application/json'},...(body===undefined?{}:{body:JSON.stringify(body)})});const value=await response.json().catch(()=>({}));if(!response.ok)throw Error(value.error||value.message||'Embedded chat unavailable');return value;}
// Purpose: Send bearer-authenticated embed API requests only while the current credential is unexpired; a 401 clears credentials, assets, polling, and chat state.

async function request(path,method='GET',body){if(!credential||Date.now()/1000>=expires)throw Error('Sign in or continue as guest');
 const response=await fetch('/api/v3'+path,{method,credentials:'omit',headers:{'Content-Type':'application/json',Authorization:'Bearer '+credential},...(body===undefined?{}:{body:JSON.stringify(body)})});const value=await response.json().catch(()=>({}));
 if(!response.ok){if(response.status===401){credential=null;clearAssets();clearInterval(poll);chat?.disconnect();document.getElementById('embed-identity').textContent='Reconnect to chat';emit('authentication',{connected:false});}const error=Error(value.error||value.message||'Request failed ('+response.status+')');error.status=response.status;throw error}return value;}
// Purpose: Refresh caller status and project active runs into the chat panel while authenticated.

async function refresh(){if(!credential)return;state=await request('/user/status');chat?.runs(state.runs||[]);}
// Purpose: Replace the current credential, select the requested or first accessible session, refresh state, and start visible-session polling.

async function connected(value){clearAssets();credential=value.token;expires=value.expires_at;identity={...value.user,is_admin:false};document.getElementById('embed-identity').textContent=identity.is_guest?'Guest':identity.user;document.getElementById('embed-guest').hidden=true;
 const items=await chat.sessions(),wanted=params.get('session_id'),selected=items.find(s=>s.id===wanted)||items[0];if(selected)await chat.open(selected.id,'replace');
 await refresh();clearInterval(poll);poll=setInterval(()=>refresh().catch(error=>note(error.message)),3000);note('');emit('authentication',{connected:true,is_guest:!!identity.is_guest});}
// Purpose: Acquire public guest access only once per pending connection and clear the busy flag on completion.

async function guest(){if(authBusy)throw Error('Connection is pending');authBusy=true;try{await connected(await publicRequest('/embed/'+encodeURIComponent(agentId)+'/guest',{origin:hostOrigin}))}finally{authBusy=false}}
// Purpose: Reuse a live credential or obtain guest access only when both runtime and agent descriptor allow it.

async function ensureAuthenticated(){if(credential&&Date.now()/1000<expires)return;if(!(access.guest_access&&descriptor.config?.guest_access))throw Error('Sign in to send your message');await guest();}
const descriptor=await publicRequest('/embed/'+encodeURIComponent(agentId)+'/descriptor?origin='+encodeURIComponent(hostOrigin));
document.getElementById('embed-agent-name').textContent=descriptor.agent.name||'WebAgent';
setChatSurfaceActions({title:descriptor.agent.name||'WebAgent',minimize:()=>emit('minimize'),close:()=>emit('close')});
chat=await mountChatPanel({root,resolveAsset,enableMainNotes:false,request,guard,note,keyId,forgetKey:key=>ids.delete(key),refresh,getState:()=>state,getAgentId:()=>agentId,getIdentity:()=>identity,onLogin:()=>login(),onEvent:emit,createStream:url=>createFetchStream(url,{headers:()=>({Authorization:'Bearer '+credential})})});
await initChatConfig({config:descriptor.chat_ui,root});
// Apply the normal send rules before sign-in; the shell's Send starts disabled and only status refresh re-enables it, so /login could not be clicked.
chat.runs([]);
const access=await publicRequest('/auth/access-mode').catch(()=>({guest_access:false}));document.getElementById('embed-guest').hidden=!(access.guest_access&&descriptor.config?.guest_access);
let theme;try{theme=localStorage.getItem('v3-theme')}catch{}document.body.classList.toggle('light-mode',theme==='light');
document.getElementById('embed-guest').onclick=guard(guest);
// @code-index {"version":1,"feature":"chat-embed-auth","aliases":["embedded chat authorization","PKCE popup handoff","embed sign in"],"purpose":"Locates popup consent, PKCE challenge creation, one-use redemption, and popup-origin/source validation for embedded chat authorization.","span_lines":40,"configuration":{"status":"not_applicable","references":[],"opportunities":[],"rationale":"Origin binding, channel checks, one-use authorization and command validation are security invariants; they must not become UI bypass settings."}}
const signIn=guard(async()=>{
 if(authBusy)throw Error('Connection is pending');
 popup=window.open('about:blank','webagent-widget-signin','popup,width=580,height=720');if(!popup)throw Error('Allow the sign-in popup to connect');
 authBusy=true;try{const bytes=crypto.getRandomValues(new Uint8Array(32)),verifier=[...bytes].map(x=>x.toString(16).padStart(2,'0')).join('');
 const hash=await crypto.subtle.digest('SHA-256',new TextEncoder().encode(verifier)),proof=btoa(String.fromCharCode(...new Uint8Array(hash))).replaceAll('+','-').replaceAll('/','_').replace(/=+$/,'');
 const challenge=await publicRequest('/embed/'+encodeURIComponent(agentId)+'/challenge',{origin:hostOrigin,code_challenge:proof});popupChallenge=challenge.challenge_id;
 popup.location='/app?embed_auth='+encodeURIComponent(popupChallenge);note('Complete sign-in in the popup.');
 const abort=new AbortController();
 // Purpose: Abort popup listeners and clear pending challenge/busy state after success or terminal failure.

 const finish=()=>{abort.abort();authBusy=false;popupChallenge='';};
 authWatch=watchAuthorization({redeem:()=>publicRequest('/embed/redeem',{challenge_id:challenge.challenge_id,code_verifier:verifier}),connected:async value=>{finish();await connected(value)},ended:error=>{finish();note('Sign-in ended. Click Sign in to try again. '+(error.message||''))},isClosed:()=>popup.closed,isExpired:()=>Date.now()/1000>=challenge.expires_at});
 // Purpose: Accept only the same-origin authorization signal from the expected popup and challenge.

 const complete=event=>{if(event.origin!==location.origin||event.source!==popup||event.data?.namespace!=='webagent.widget.auth'||event.data.challenge_id!==challenge.challenge_id||event.data.authorized!==true)return;void authWatch.complete();};
 window.addEventListener('message',complete,{signal:abort.signal});
 }catch(error){authBusy=false;popup.close();throw error}
});
document.getElementById('embed-connect').onclick=signIn;
// Purpose: Show the sign-in panel for "/login"; its Sign in button starts the same first-party popup flow.
function login(){const connected=credential&&Date.now()/1000<expires;openSignInPanel({agent:descriptor.agent.name||'WebAgent',message:'Sign in on '+location.host+' in a secure popup to use your account access on '+hostOrigin+'. Your password is never entered on this website.',identity:connected?(identity.is_guest?'Guest':identity.user):'',signIn,guest:!connected&&access.guest_access&&descriptor.config?.guest_access?guard(guest):null});}
// Parent commands are serialized; the bounded receipt journal replays identical IDs and rejects changed input.
const receipts=new Map();let queue=Promise.resolve();
// Purpose: Dispatch the exact supported parent command set, validate command-specific fields, and require current authentication before session actions.

async function action(command,args){
 if(command==='visibility'){if(typeof args.visible!=='boolean')throw Error('Visibility must be boolean');root.dataset.embedVisible=String(args.visible);document.dispatchEvent(new Event('v3:chat-visibility'));return {visible:args.visible}}
 if(command==='setDraft'){chat.setDraft(args.text);return {updated:true}}
 if(command==='send'&&typeof args.text==='string'&&args.text.trim().toLowerCase()==='/login'){login();return {login:true}}
 if(command==='send'){chat.setDraft(args.text);try{await ensureAuthenticated();return await chat.send(args.text)}catch(error){note(error.message);throw error}}
 if(command==='setElementContext'){chat.setElementContext(args.text);return {updated:true};}
 await ensureAuthenticated();
 if(command==='selectSession'){if(typeof args.sessionId!=='string')throw Error('Session ID required');await request('/sessions/'+encodeURIComponent(args.sessionId));await chat.open(args.sessionId);return {session_id:chat.sessionId}}
 if(command==='showSessions'){await chat.showSessions();return {shown:true};}
 if(command==='newSession')return chat.newSession();
 if(command==='stop'){await chat.stop(args.runId);return {stopped:true}}
 if(command==='continue')return chat.continue();
 if(command==='approve'){if(!['approve','deny'].includes(args.decision))throw Error('Approval decision required');await chat.approve(args.approvalId,args.decision);return {decided:true}}
 if(command==='recover'){if(typeof args.runId!=='string'||typeof args.decision!=='string')throw Error('Recovery decision required');await chat.recover(args.runId,args.decision,args.output);return {decided:true}}
 throw Error('Unknown widget command');
}
window.addEventListener('message',event=>{
 const m=event.data;if(event.source!==parent||event.origin!==hostOrigin||m?.namespace!==namespace||m.version!==version||m.type!=='command'||m.channel!==channel)return;
 if(typeof m.id!=='string'||m.id.length>128||typeof m.command!=='string'||!m.args||typeof m.args!=='object')return;
 let input;try{input=JSON.stringify([m.command,m.args]);if(input.length>2*1024*1024)return}catch{return}
 // Purpose: Return one correlated result to the bound parent origin.
 const reply=result=>parent.postMessage({namespace,version,type:'result',channel,id:m.id,...result},hostOrigin);
 if(receipts.has(m.id)){const previous=receipts.get(m.id);if(previous.input!==input){reply({ok:false,error:'Request ID reused with different input'});return}previous.promise.then(reply);return;}
 if(receipts.size>=128){for(const [id,r] of receipts){if(r.done){receipts.delete(id);break}}if(receipts.size>=128){reply({ok:false,error:'Too many pending commands'});return}}
 const entry={input,done:false};
 entry.promise=queue.then(()=>action(m.command,m.args)).then(result=>({ok:true,result}),error=>({ok:false,error:error.message})).then(value=>{entry.done=true;return value});receipts.set(m.id,entry);queue=entry.promise.then(()=>{});entry.promise.then(reply);
});
window.addEventListener('pagehide',()=>{clearInterval(poll);authWatch?.cancel();chat.destroy();clearAssets();credential=null;popup?.close();});
parent.postMessage({namespace,version,type:'ready',channel},hostOrigin);
// Only sanitized UI configuration crosses the bridge; each host tap requests its own local pulse.
void vibrationConfigReady.then(()=>emit('vibration-config',{config:getVibrationConfig()}));
emit('theme',{theme:document.body.classList.contains('light-mode')?'light':'dark'});
