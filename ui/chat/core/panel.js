import {createMainNotes} from '../elements/main-notes/index.js';
// @code-index {"version":1,"feature":"chat-panel","aliases":["chat panel","conversation panel","session transcript"],"purpose":"Locates the v3 chat panel lifecycle, transcript, and run controls Config: data/config/chat.json -> chat_display.default and chat_common.composer.keyboard_focus. Profile keys merge chat_common with chat_desktop/chat_mobile/chat_widget overrides. Touch feedback: ui/vibrate.json -> enabled, duration_ms; gestures stay client-owned.","span_lines":80,"configuration":{"status":"configured","references":[{"path":"data/config/chat.json","pointers":["/chat_display/default","/chat_common/composer/keyboard_focus"],"profile_overrides":true},{"path":"ui/vibrate.json","pointers":["/enabled","/duration_ms"]}],"opportunities":[]}}
// V2-style panel/client separation; markup and message protocol remain V3.
// @code-index {"version":1,"feature":"chat-screen-panel-routing","aliases":["conversation display lifecycle"],"purpose":"Panel feeds selected-session runs/live messages to display policy from data/config/chat.json -> chat_common.screen_display and surface overrides; history loading is excluded.","span_lines":3,"configuration":{"status":"configured","references":[{"path":"data/config/chat.json","pointers":["/chat_common/screen_display"],"profile_overrides":true}],"opportunities":[]}}
import {mountScreenAwake} from '../../shared/screen-awake.js';
import {mountChatControlFeedback} from '../controls/feedback.js';
import {mountSharedChat} from '../controls/shared-chat.js';
import { createChatClient } from './client.js';
import {mountFollowupQueue} from './followup-queue.js';
import {mountTranscriptAnchor} from './transcript-anchor.js';
import {mountComposerMedia} from '../controls/composer-media.js';
import {mountSessionControls} from '../controls/session-controls.js';
import {mountChatNotifications} from './chat-notifications.js';
import {mountUsageStats} from '../controls/usage-stats.js';
import {createMessageRow as createMessage,updateAdmission} from './message-view.js';
import {closeGutterOverlays,updateGutterTimes} from './bubble-gutters.js';
import {mountComposerProgress} from './composer-progress.js';
import {createChatElements} from '../elements/index.js';
import {closeChatElementFullView} from './full-view.js';
import {currentChatProfile} from '../controls/chat-config.js';
import { applyChatConfig } from '../controls/chat-config.js';
import { mountSessionDropdown } from '../controls/session-dropdown/controller.js';
import { createSessionList } from '../controls/session-dropdown/list.js';
import {createRecoveryDetails} from './recovery-details.js';
const mountedPanels = new WeakMap();
/**
 * Mounts the V3 chat partial, transport, transcript and controls under the supplied shell root.
 * Replaces an earlier panel for that root through its destroy lifecycle and returns the panel API.
 * Called by ui/app.js with the authenticated request, action guard, identity and selected-agent adapters.
 * Reports session/run/sent changes through onEvent and returns the mounted panel API.
 * Missing or incomplete partial markup and synchronous setup failures propagate to the shell.
 */
export async function mountChatPanel({root = document.getElementById('chat-panel'), createStream, resolveAsset, enableMainNotes=true, onEvent = () => {}, request, guard, note, clearNoticeFrom, issues, keyId, forgetKey, refresh, getState, getAgentId, getIdentity=()=>({})}) {
  mountedPanels.get(root)?.destroy();
  // Resolves partial element IDs only within this panel root.
  const $ = id => root.querySelector('#' + id);
  let session = '';
  // Chat elements share the panel session, public runs and merged profile; private groups retain admin gating.
  const activity=createChatElements({app:{/* Reads the current selected session. */get currentSessionId(){return session;}},/* Returns the current account's admin flag. */isAdmin:()=>!!getIdentity().is_admin,/* Uses profile configuration when set, otherwise the default. */chatUiFlag:(key,fallback)=>currentChatProfile()?.[key]??fallback});
  let activityRuns=[];
  const expandedApprovals=new Set();
  const expandedRecovery=new Set();
  const gutterRuns=new Map();
  /** Keeps recent run snapshots available to message gutters, trimming older entries to 100. */
  function rememberRuns(items){for(const run of items)gutterRuns.set(run.id,run);for(const [id] of [...gutterRuns.entries()].sort((a,b)=>(b[1].updated_at||0)-(a[1].updated_at||0)).slice(100))gutterRuns.delete(id);}
  /** Refreshes gutter run details from the session snapshot, ignoring stale-session replies. */
  async function refreshGutterRuns(id=session){const epoch=sessionGeneration;try{const data=await client.snapshot(id);if(id===session&&epoch===sessionGeneration){rememberRuns(data.runs||[]);issues?.resolve('message_statistics',id);}}catch(error){if(id===session&&epoch===sessionGeneration)issues?issues.fail('message_statistics',error,id):note('Message statistics unavailable: '+error.message);}}
  // Projects panel run states into the activity view's display-state vocabulary.
  const paintActivity=()=>{try{activity.render({sessionId:session,runs:activityRuns.map(r=>({...r,state:({completed:'completed',failed:'failed',stopped:'cancelled',waiting_approval:'running',waiting_gate:'finalizing'})[r.status]||r.status}))},$('chat-messages-inner'));issues?.resolve('activity_display',session);}catch(error){if(issues)issues.fail('activity_display',error,session);else note('Activity display unavailable: '+error.message);}};
  const client = createChatClient({request, note, issues, clearNoticeFrom, createStream});
  /** Creates a panel button whose action passes through the shell's guard. */
  function button(text,fn){const node=document.createElement('button');node.type='button';node.textContent=text;node.onclick=guard(fn);return node;}
/** Fetches, validates, and mounts the required V3 chat partial into the supplied root. */
async function mountChat(){
  const response=await fetch('/v3-ui/chat/chat.html?v=chat-partial-20261007-notifications',{credentials:'same-origin'});
  if(!response.ok)throw Error('Chat shell unavailable');
  const template=document.createElement('template');
  template.innerHTML=await response.text();
  for(const id of ['chat-header','chat-messages','chat-messages-inner','sessions','new','chat-notifications','runs','composer','chat-input','chat-send','chat-stop-btn']){
    if(!template.content.querySelector('#'+id))throw Error('Chat shell is incomplete: '+id);
  }
  root.replaceChildren(template.content);
}
await mountChat();
const mainNotes=enableMainNotes?createMainNotes({request}):null;if(mainNotes){mainNotes.element.dataset.headerExtension='main-notes';$('chat-header').append(mainNotes.element);}
// Display activity uses authoritative selected-session run states and live publications.
const screenAwake=mountScreenAwake(root,{getProfile:currentChatProfile});
const notifications=mountChatNotifications({root,guard,request,note});
const {messagingRoutes}=notifications;
const anchor=mountTranscriptAnchor({scroller:$('chat-messages'),inner:$('chat-messages-inner'),header:$('chat-header'),getProfile:currentChatProfile});
// Progress visibility is filtered using the current account's administrator identity.
const progress=mountComposerProgress({root,getProfile:currentChatProfile,isAdmin:()=>!!getIdentity().is_admin});
const stats=mountUsageStats({root,client,note,issues});
// Refreshes relative gutter times once per second while visible; destroy clears this timer.
const gutterClock=setInterval(()=>{if(document.visibilityState==='visible')updateGutterTimes(root);},1000);
// Session-control capability updates enable or disable the media adapter.
const controls=mountSessionControls({root,client,guard,note,issues,ensureSession,notifications,canWrite:()=>sharedAccess?.can_control!==false,/* Forwards advertised attachment availability and limits to media controls. */onState:data=>{media?.setAvailable(sharedAccess?.can_send!==false&&data?.composer?.attachments===true,data?.composer?.attachment_limits);}});
let submitting=false,sessionGeneration=0,creatingSession=null,media=null,historyBusy=false,historyLastSeq=0,historyRevision=0,historyDeferredRevision=0,historyReady=false,closeUndoDialog=null,canManageQueue=false;
const historyRequests=new Map();
const followups=mountFollowupQueue({root,host:$('chat-above-pill'),client,getSession:()=>session,canWrite:()=>canManageQueue&&sharedAccess?.can_control!==false,guard,note,keyId,forgetKey,onSnapshot:syncSnapshot});
/** Background history reopening is independent from explicit undo/fork action feedback. */
async function reopenHistory(id){
  const pending=open(id,'replace'),generation=sessionGeneration;
  try{await pending;if(id===session&&generation===sessionGeneration)issues?.resolve('history_refresh',id);}
  catch(error){if(id!==session||generation!==sessionGeneration)return;if(issues)issues.fail('history_refresh',error,id);else note(error.message);}
}
/** Rewinds rebuild the transcript; queue and poll snapshots may only merge the same history revision. */
function historyChanged(summary){
  if((summary?.history_revision||0)>historyRevision){if(historyBusy)historyDeferredRevision=Math.max(historyDeferredRevision,summary.history_revision);else void reopenHistory(session);}
}
/** Merge a matching-session poll snapshot only when it shares the displayed history revision. */
function syncSnapshot(data){
  if(data.session?.id&&data.session.id!==session)return false;
  if(data.access){sharedAccess=data.access;canManageQueue=data.access.can_manage_queue!==false;}
  if((data.session?.history_revision||0)!==historyRevision){historyChanged(data.session);return false;}
  historyLastSeq=Math.max(historyLastSeq,data.last_seq||0);
  for(const item of data.messages||[])message(item);followups.update(data);runs(data.runs||[],true);return true;
}
/** Confirms rewind and resolves existing composer text without silently discarding a draft. */
function undoChoice(hasDraft){return new Promise(resolve=>{const dialog=document.createElement('dialog');dialog.className='wa-confirm-panel';dialog.dataset.chatControlSurface='undo';const text=document.createElement('p');text.textContent='Undo this message and all later messages? This changes conversation history; external tool effects remain.';dialog.append(text);if(hasDraft){const hint=document.createElement('p');hint.textContent='The composer already has text. Choose how to return the undone message.';dialog.append(hint);}let settled=false;const finish=value=>{if(settled)return;settled=true;dialog.close();dialog.remove();closeUndoDialog=null;resolve(value);};for(const [label,value] of hasDraft?[['Clear & replace','replace'],['Keep current','keep'],['Append','append'],['Cancel',null]]:[['Undo','replace'],['Cancel',null]]){const node=document.createElement('button');node.type='button';node.className='ac-btn';node.textContent=label;node.onclick=()=>finish(value);dialog.append(node);}dialog.addEventListener('cancel',event=>{event.preventDefault();finish(null);});closeUndoDialog=()=>finish(null);document.body.append(dialog);dialog.showModal();dialog.querySelector('button:last-child').focus();});}
/** Uses the displayed transcript cursor, durable retry identity and panel generation fence. */
async function historyAction(kind,item){
  if(historyBusy||submitting)return;
  const sid=session,generation=sessionGeneration,last=historyLastSeq;
  const draft=$('chat-input').value,actionKey=sid+':'+kind+':'+item.id;historyBusy=true;
  try{
    const choice=kind==='undo'?await undoChoice(!!draft.trim()):null;
    if(kind==='undo'&&choice===null)return;
    if(sid!==session||generation!==sessionGeneration)return;
    let pending=historyRequests.get(actionKey);
    if(!pending){const receipt='history:'+sid+':'+kind+':'+item.id+':'+last;pending={receipt,body:{message_id:item.id,expected_last_seq:last,client_request_id:keyId(receipt)},result:null,composerApplied:false};historyRequests.set(actionKey,pending);}
    // Keep the retry on the same client operation and durable request body.
    const execute=()=>client[kind](sid,pending.body);
    if(!pending.result){try{pending.result=await execute();}catch(error){if(error.status)throw error;pending.result=await execute();}}
    const data=pending.result;
    if(sid!==session||generation!==sessionGeneration)return;
    if(kind==='undo'){
      const current=$('chat-input').value;
      if(!pending.composerApplied&&choice!=='keep'){const text=typeof data.text==='string'?data.text:'';
        $('chat-input').value=choice==='append'||current!==draft?[current,text].filter(Boolean).join('\n'):text;
        $('chat-input').dispatchEvent(new Event('input',{bubbles:true}));}
      pending.composerApplied=true;await open(sid,'replace');note('Messages undone');
    }else{await sessions();await open(data.session_id);note('Conversation forked');}
    historyRequests.delete(actionKey);forgetKey(pending.receipt);
  }catch(error){if(error.status>=400&&error.status<500&&!historyRequests.get(actionKey)?.result){const pending=historyRequests.get(actionKey);if(pending)forgetKey(pending.receipt);historyRequests.delete(actionKey);}note(error.message||'Conversation action failed');}
  finally{historyBusy=false;const pending=historyDeferredRevision;historyDeferredRevision=0;if(sid===session&&pending>historyRevision)void reopenHistory(sid);}
}
let sharedAccess=null;
const sharing=mountSharedChat({root,client,guard,note,issues,ensureSession,/* Refresh panel access and session projections after sharing changes. */onState:data=>{sharedAccess=data?.access||null;if(sharedAccess?.can_send===false)media?.setAvailable(false);runs(activityRuns);if(data&&session){const sid=session,epoch=sessionGeneration;client.snapshot(sid).then(snapshot=>{if(session!==sid||sessionGeneration!==epoch||!historyReady)return;issues?.resolve('shared_snapshot',sid);if((snapshot.session.history_revision||0)!==historyRevision){if(historyBusy)historyDeferredRevision=Math.max(historyDeferredRevision,snapshot.session.history_revision||0);else void reopenHistory(sid);return;}if(snapshot.access){sharedAccess=snapshot.access;canManageQueue=snapshot.access.can_manage_queue!==false;}for(const item of snapshot.messages||[])message(item);followups.update(snapshot);runs(snapshot.runs||[],true);}).catch(error=>{if(session!==sid||sessionGeneration!==epoch)return;if(issues)issues.fail('shared_snapshot',error,sid);else note(error.message);});}}});
// Media changes reapply composer layout and refresh run-dependent button states.
media=mountComposerMedia({root,client,ensureSession,note,guard,getState,refresh,/* Reapplies composer geometry and run-dependent actions after media changes. */changed:()=>{applyChatConfig();runs(activityRuns);}});
let sessionItems=[];
// Dropdown refresh reads sessions; its search reset delegates to the session-list controller.
const dropdown=mountSessionDropdown($('session-dropdown'),{/* Reloads session options. */refresh:()=>sessions(),select:open,guard,/* Clears search through the list controller. */resetSearch:()=>sessionList.reset()});
// Session-list refresh shares the session loader; recycle completion resets transcript and session-scoped state.
const sessionList=createSessionList($('session-dropdown'),{guard,screenSettings:()=>screenAwake.settings(),/* Shares the panel session loader. */refresh:()=>sessions(),select:open,client,/* Clears the selected transcript and its session-scoped controls after recycle. */recycled:()=>{screenAwake.select('');followups.reset();anchor.reset();progress.reset();activity.disconnect();resetReads();session='';issues?.select('');stats.disconnect();++sessionGeneration;controls.open('');sharing.disconnect();media.open('');client.disconnect();$('chat-messages-inner').replaceChildren();notifications.clearTools();$('runs').replaceChildren();const url=new URL(location.href);url.searchParams.delete('session_id');history.replaceState({},'',url);}});
// Exposes dropdown configuration through the session-list controller.
$('session-dropdown').configure=cfg=>sessionList.configure(cfg);
const chatPanel=root;
// Keeps transcript tail clearance aligned with measured header and composer heights.
const updateChatClearance=()=>{
  const header=$('chat-header').getBoundingClientRect().height;
  const footer=$('chat-input-area').getBoundingClientRect().height + (parseFloat(getComputedStyle($('chat-input-area')).bottom)||0);
  if(header)chatPanel.style.setProperty('--v3-chat-header-clearance',`${Math.ceil(header)+12}px`);
  if(footer)chatPanel.style.setProperty('--v3-chat-footer-clearance',`${Math.ceil(footer)+16}px`);
};
// Recomputes clearance when either bounding region changes size.
const chatClearanceObserver=new ResizeObserver(updateChatClearance);
chatClearanceObserver.observe($('chat-header'));
chatClearanceObserver.observe($('chat-input-area'));
let readGeneration=0, readThrough=0, readPending=0, readTimer=0;
const readAbort=new AbortController();
const visibleRows=new Set();
/** Debounces read acknowledgments to the highest visible sequence and fences stale session writes. */
function queueRead(){
  if(sharedAccess?.observer||document.visibilityState!=='visible'||!session||!root.getClientRects().length||root.dataset.embedVisible==='false'){clearTimeout(readTimer);return;}
  for(const row of visibleRows){if(row.isConnected)readPending=Math.max(readPending,Number(row.dataset.seq)||0);}
  clearTimeout(readTimer);
  if(readPending<=readThrough)return;
  const id=session, generation=readGeneration, through=readPending;
  readTimer=setTimeout(async()=>{try{if(!root.getClientRects().length||root.dataset.embedVisible==='false')return;const result=await client.read(id,through);if(generation!==readGeneration||id!==session)return;readThrough=Math.max(readThrough,result.session?.read_through_seq||through);issues?.resolve('read_ack',id);void sessions().catch(()=>{});}catch(error){if(generation===readGeneration&&id===session){if(issues)issues.fail('read_ack',error,id);else note('Read acknowledgment failed: '+error.message);clearTimeout(readTimer);readTimer=setTimeout(()=>{if(generation===readGeneration&&id===session)queueRead();},3000);}}},250);
}
// Tracks visible transcript rows and queues read-marker advancement as visibility changes.
const readObserver=new IntersectionObserver(entries=>{for(const entry of entries){if(entry.isIntersecting&&entry.intersectionRect.height>0)visibleRows.add(entry.target);else visibleRows.delete(entry.target);}queueRead();},{root:$('chat-messages'),threshold:0});
document.addEventListener('visibilitychange',queueRead,{signal:readAbort.signal});
document.addEventListener('v3:chat-visibility',queueRead,{signal:readAbort.signal});
// Invalidates pending acknowledgments and clears visibility/read cursors on session changes.
function resetReads(){++readGeneration;clearTimeout(readTimer);readThrough=0;readPending=0;visibleRows.clear();readObserver.disconnect();}
let reviewRefreshBusy=false;
// Rechecks admission rows once per second while a session is open; serializes snapshots and stops in destroy.
const reviewTimer=setInterval(async()=>{
  if(!session||reviewRefreshBusy||document.visibilityState==='hidden')return;
  const rows=[...$('chat-messages-inner').querySelectorAll('[data-admission-status]')];
  if(!followups.pending&&!rows.some(row=>['pending_review','reviewing'].includes(row.dataset.admissionStatus)))return;
  const sid=session,generation=sessionGeneration;reviewRefreshBusy=true;
  try{
    const data=await client.snapshot(sid);
    if(sid!==session||generation!==sessionGeneration)return;
    if(!syncSnapshot(data))return;
    issues?.resolve('manager_status',sid);
    if(rows.some(row=>['pending_review','reviewing'].includes(row.dataset.admissionStatus))&&!data.messages.some(item=>['pending_review','reviewing'].includes(item.admission?.status)))void refresh().then(()=>issues?.resolve('status_poll')).catch(error=>{if(issues)issues.fail('status_poll',error);else note(error.message);});
  }catch(error){if(sid===session&&generation===sessionGeneration)issues?issues.fail('manager_status',error,sid):note('Manager status unavailable: '+error.message);}
  finally{reviewRefreshBusy=false;}
},1000);
/** Inserts or updates a public message row, observes it for read tracking, and repaints activity. */
function message(item){historyLastSeq=Math.max(historyLastSeq,item.seq||0);const target=$('chat-messages-inner');const existing=[...target.children].find(row=>row.dataset.publicId===item.id);if(existing){updateAdmission(existing,item.admission);return;}const row=createMessage(item,{resolveAsset,/* Supplies the latest cached run for gutter actions. */getRun:()=>gutterRuns.get(item.run_id),client,/* Resolves the currently selected session for message actions. */getSession:()=>session,note,historyAction,/* Opens usage/context details for the selected run. */showContext:run=>stats.show(run)});if(!row)return false;const before=[...target.children].find(node=>Number(node.dataset.seq)>item.seq);target.insertBefore(row,before||null);readObserver.observe(row);paintActivity();return true;}
// Base presentation is display-only. Never turn model-supplied choice or
// approval descriptions into an action or an authorization control.
/** Renders bounded display-only components; these cards never submit model-described actions. */
function components(items){
  if(!Array.isArray(items))return;
  for(const row of items.slice(0,64)){
    const spec=row?.component;
    if(!spec||spec.authority!=='display_only'||!['status','todo_list','choice','approval','form'].includes(spec.type))continue;
    const card=document.createElement('article');card.className='v3-message assistant';
    const body=document.createElement('div');body.className='message-content';
    const heading=document.createElement('strong');heading.textContent=spec.title||'Session display';
    body.append(heading);
    const data=spec.data||{};
    const lines=spec.type==='status'?data.sections?.map(item=>[item.label,item.value].filter(Boolean).join(': ')):
      spec.type==='todo_list'?data.items?.map(item=>(item.done?'☑ ':'☐ ')+item.label):
      spec.type==='choice'?data.options?.map(item=>'Option: '+item.label):
      spec.type==='form'?data.fields?.map(item=>'Field: '+item.label):
      [data.message];
    for(const value of Array.isArray(lines)?lines.slice(0,100):[]){
      if(typeof value!=='string')continue;
      const line=document.createElement('p');line.textContent=value;body.append(line);
    }
    if(['choice','approval','form'].includes(spec.type)){
      const note=document.createElement('small');note.textContent='Display only — no action submitted';body.append(note);
    }
    card.append(body);$('chat-messages-inner').append(card);
  }
}

// Focus follows a session load only while the initiating focus choice is unchanged.
/** Watches focus during an async load; returns a predicate and listener remover for the caller's finally block. */
function watchFocus(){const source=document.activeElement;let moved=false;/* Records focus leaving the initiating control during the load. */const onFocus=()=>{if(document.activeElement!==source)moved=true;};document.addEventListener('focusin',onFocus);return {/* Allows focus restore only while the user has not moved focus. */unchanged:()=>!moved&&(document.activeElement===source||(!source.isConnected&&document.activeElement===document.body)),/* Removes the temporary focus listener after the operation. */stop:()=>document.removeEventListener('focusin',onFocus)};}
// Applies per-profile keyboard-focus policy for new versus existing sessions.
function shouldFocusComposer(kind){const value=currentChatProfile()?.composer?.keyboard_focus?.[kind];return kind==='new_session'?value!==false:value===true;}
/** Loads session options and synchronizes both the native select and dropdown list. */
let sessionsGeneration=0;
async function sessions(){const turn=++sessionsGeneration;let data;try{data=await client.sessions();if(turn===sessionsGeneration)issues?.resolve('sessions_list');}catch(error){if(issues&&turn===sessionsGeneration)issues.fail('sessions_list',error);throw error;}if(turn!==sessionsGeneration)return data.sessions;$('sessions').replaceChildren();for(const item of data.sessions){const option=document.createElement('option');option.value=item.id;option.textContent=(item.title||'Conversation')+' — '+(item.agent_id||'shared_default');$('sessions').append(option);}$('sessions').value=session;sessionItems=data.sessions;sessionList.update(sessionItems,session);return data.sessions;}
/**
 * Switches the selected session, resets session-scoped views, then renders its snapshot and live events.
 * Route mode controls push/replace history; composer focus is restored only if the user did not move it.
 * Resets the display lease for the selected session; only newly delivered messages restore brightness.
 * Client snapshot failures propagate; controls report independent issues, or note when no tracker is injected.
 */
async function open(id,routeMode='push',focusKind='existing_session',focusWatch=watchFocus()){try{mainNotes?.setAgent(null);messagingRoutes.disconnect();canManageQueue=false;sharedAccess={can_send:false,can_control:false};followups.reset();historyReady=false;anchor.reset();progress.open(id);notifications.clearTools();$('runs').replaceChildren();closeGutterOverlays();gutterRuns.clear();++sessionGeneration;session=id;issues?.select(id);screenAwake.select(id);stats.open(id);media.open(id);activity.disconnect();closeChatElementFullView();resetReads();session=id;const url=new URL(location.href);url.searchParams.set('session_id',id);if(url.href!==location.href)history[routeMode==='replace'?'replaceState':'pushState']({},'',url);sharing.open(id).catch(error=>{if(id===session&&!issues)note('Shared controls unavailable: '+error.message);});controls.open(id).catch(error=>{if(id===session&&!issues)note(error.message);});const focusGeneration=sessionGeneration;await client.open(id,data=>{if(data.access){sharedAccess=data.access;canManageQueue=data.access.can_manage_queue!==false;}messagingRoutes.open(id,canManageQueue);historyReady=true;historyLastSeq=data.last_seq||0;historyRevision=data.session.history_revision||0;readThrough=data.session.read_through_seq||0;const sessionAgentId=data.session.agent_id||'shared_default';if(id!==session)return;mainNotes?.setAgent(sessionAgentId);$('chat-input-row').title='Conversation agent: '+sessionAgentId;$('sessions').value=id;sessionList.update(sessionItems,id);onEvent('session',{session_id:id});document.dispatchEvent(new Event('v3:session-open'));$('chat-messages-inner').replaceChildren();for(const item of data.messages)message(item);components(data.components||[]);followups.update(data);runs(data.runs||[],true);anchor.focus();},(item,{recovered=false}={})=>{if(message(item)===false)return false;if(!recovered)screenAwake.attention('message');try{media.published(item);issues?.resolve('media_display',id);}catch(error){if(issues)issues.fail('media_display',error,id);else note('Media display unavailable: '+error.message);}if(item.role==='assistant')void refreshGutterRuns();void stats.refresh(true).catch(error=>note(error.message));try{if(recovered)anchor.changed();else anchor.published(item);issues?.resolve('transcript_position',id);}catch(error){if(issues)issues.fail('transcript_position',error,id);else note('Transcript positioning unavailable: '+error.message);}return true;},summary=>{historyChanged(summary);void sessions().catch(error=>{if(!issues)note(error.message);});},syncSnapshot);if(session===id&&focusGeneration===sessionGeneration&&focusWatch.unchanged()&&shouldFocusComposer(focusKind))$('chat-input').focus({preventScroll:true});}finally{focusWatch.stop();}}
// Requests run cancellation and refreshes shell state even when the request fails.
async function cancel(id){if(sharedAccess?.can_control===false)throw Error('This conversation is read only');progress.note('Stopping…');try{await client.cancel(id,{client_request_id:keyId('cancel:'+id)});}finally{await refresh();}}
// Adds/removes a transcript notice only for certain failed runs; uncertain outcomes remain in recovery UI.
function failedRunMessages(items){
  const target=$('chat-messages-inner');
  const failed=items.filter(run=>run.status==='failed'&&!run.uncertain);
  const ids=new Set(failed.map(run=>run.id));
  for(const row of target.querySelectorAll('[data-failed-run-id]'))if(!ids.has(row.dataset.failedRunId))row.remove();
  for(const run of failed){
    let row=[...target.querySelectorAll('[data-failed-run-id]')].find(node=>node.dataset.failedRunId===run.id);
    if(!row){
      row=document.createElement('article');row.className='v3-message system';row.dataset.failedRunId=run.id;
      const body=document.createElement('div');body.className='message-content';row.append(body);
      const turns=[...target.children].filter(node=>node.dataset.turnId===run.id);
      const after=turns.at(-1);
      target.insertBefore(row,after?.nextSibling||null);
    }
    row.firstChild.textContent='Run failed'+(run.error?' — '+run.error:'')+'. To continue, write a new message in the chat.';
  }
}
/** Fans run snapshots to transport, display lease, controls, progress and activity, then paints composer and run actions. Approval, recovery, resume, and cancel handlers call their matching client endpoints. */
function runs(items,authoritative=false){if(!authoritative&&sharing.state&&activityRuns.length&&!items.some(run=>run.session_id===session))items=activityRuns;onEvent('runs',{session_id:session,runs:items.filter(r=>r.session_id===session||!r.session_id).map(r=>({id:r.id,status:r.status,uncertain:!!r.uncertain,approval_id:r.pending?.id||null}))});screenAwake.runs(items.filter(run=>run.session_id===session||!run.session_id));client.updateRuns(items);anchor.updateRuns(items.filter(run=>run.session_id===session||!run.session_id));progress.update(items);void stats.refresh();controls.updateRuns(items.filter(run=>run.session_id===session||!run.session_id));activityRuns=items.filter(run=>run.session_id===session||!run.session_id);rememberRuns(activityRuns);if(session)paintActivity();$('runs').replaceChildren();notifications.clearTools();const relevant=items.filter(run=>run.session_id===session||!run.session_id);const pendingReview=!!$('chat-messages-inner').querySelector('[data-admission-status="pending_review"],[data-admission-status="reviewing"]');const active=relevant.find(run=>['queued','running','waiting_approval','waiting_gate','waiting_external'].includes(run.status));$('chat-input-row').dataset.runStatus=active?.status||relevant[0]?.status||'Ready';$('chat-stop-btn').hidden=!active||$('chat-stop-btn').dataset.configEnabled==='false';$('chat-stop-btn').dataset.run=active?.id||'';$('chat-stop-btn').style.display=$('chat-stop-btn').hidden?'none':'';$('chat-send').disabled=historyBusy||sharedAccess?.can_send===false||!!getState().shutoff||submitting||!!media?.uploading;
$('chat-input').readOnly=sharedAccess?.can_send===false;
$('chat-stop-btn').disabled=sharedAccess?.can_control===false;
failedRunMessages(relevant);
const continuing=!pendingReview&&!active&&!media?.count&&!$('chat-input').value.trim()&&$('chat-continue-btn').dataset.configEnabled!=='false';
$('chat-continue-btn').hidden=!continuing;$('chat-continue-btn').style.display=continuing?'':'none';
$('chat-continue-btn').disabled=sharedAccess?.can_send===false||sharing.state?.participation?.active===false||!!getState().shutoff||submitting;
// Context reports live in the composer panel and each bubble's More menu.
// Ordinary active runs use the progress control and composer Stop button.
// Approval and recovery cards retain their explicit decisions.
for(const run of relevant){if(run.status==='completed'||(!run.uncertain&&['queued','running','waiting_gate','waiting_external','failed'].includes(run.status)))continue;const card=document.createElement('section');card.className='run-card';const title=document.createElement('strong');title.textContent=run.status+(run.error?' — '+run.error:'');card.append(title);
if(run.pending && run.status==='waiting_approval'){
  const p=run.pending;
  if(!p.id||p.can_decide===false||sharedAccess?.observer){const waiting=document.createElement('p');waiting.textContent=p.waiting_message||'Waiting for the authorized user';card.replaceChildren(waiting);$('runs').append(card);continue;}
  notifications.showTool({approval:p,run,active,expanded:expandedApprovals,decide:async (decision,extra={})=>{await client.decide(p.id,{decision,...extra,client_request_id:keyId('approval:'+p.id+':'+decision+':'+JSON.stringify(extra))});await refresh();},cancel:()=>cancel(run.id)});
  continue;
}
if((run.uncertain || run.status==='recovery')&&sharedAccess?.can_control!==false){
  title.remove();
  card.append(createRecoveryDetails(run,{expanded:expandedRecovery,button,/* Submit the authorized recovery choice, then reload run state. */decide:async(decision,output)=>{
    await client.recover(run.id,{decision,output,client_request_id:keyId('recovery:'+run.id+':'+decision+':'+output)});await refresh();
  }}));
}
if(sharedAccess?.can_control!==false&&!run.uncertain && ['paused','stopped'].includes(run.status))card.append(button('Resume run',async()=>{await client.resume(run.id,{client_request_id:crypto.randomUUID()});await refresh();}));if(sharedAccess?.can_control!==false&&active?.id===run.id)card.append(button('Cancel run',()=>cancel(run.id)));$('runs').append(card);}}
$('sessions').onchange=guard(()=>open($('sessions').value));
/** Deduplicates session creation and rejects completion if the user changed sessions mid-request. */
async function ensureSession(){
  if(session)return session;
  if(creatingSession)return creatingSession;
  const epoch=sessionGeneration;
  creatingSession=(async()=>{
    const focusWatch=watchFocus();
    try {
      const requestKey='session:'+getAgentId();
      const item=await client.create({agent_id:getAgentId(),client_request_id:keyId(requestKey)});
      forgetKey(requestKey);
      if(epoch!==sessionGeneration)throw Error('Conversation changed; repeat the action in the selected conversation.');
      await sessions();
      if(epoch!==sessionGeneration)throw Error('Conversation changed; repeat the action.');
      await open(item.id,'push','new_session',focusWatch);return item.id;
    } finally {focusWatch.stop();}
  })().finally(()=>{creatingSession=null;});
  return creatingSession;
}
// Creates a fresh session from the New control and restores the initiating focus choice.
$('new').onclick=guard(async()=>{
  const focusWatch=watchFocus();
  try{
    const item=await client.create({agent_id:getAgentId(),client_request_id:crypto.randomUUID()});
    await sessions();
    await open(item.id,'push','new_session',focusWatch);
  }finally{focusWatch.stop();}
});
let elementContext='';
/** Submits text plus staged attachments/page context, preserving draft and session identity across awaits. */
async function submit(text,clearInput=false){
  if(!text&&!media.count)throw Error('Enter a message');
  if(historyBusy||submitting||getState().shutoff||$('chat-send').disabled)throw Error('Chat is busy or execution is disabled');
  const origin=session,epoch=sessionGeneration,draft=$('chat-input').value;
  progress.note('Sending…');submitting=true;$('chat-send').disabled=true;$('chat-continue-btn').disabled=true;
  try{
    const sid=await ensureSession();
    if((origin&&origin!==session)||(epoch!==sessionGeneration&&session!==sid))throw Error('Conversation changed; message was not sent.');
    const ids=media.snapshot(sid),content=text+(elementContext?'\n\n[Untrusted page context selected by user]\n'+elementContext:'');
    const requestKey='message:'+sid+':'+content+':'+ids.join(',');
    const sent=await client.send(sid,{text:content,attachment_ids:ids,client_message_id:keyId(requestKey)});
    if(sid===session){
      progress.note(sent.submission_id?'Message queued':sent.review_id?'Awaiting manager review…':'Message sent');
      const data=await client.snapshot(sid);
      if(sid===session)syncSnapshot(data);
    }
    forgetKey(requestKey);media.committed(sid,ids);
    if(clearInput&&sid===session&&$('chat-input').value===draft)$('chat-input').value='';
    applyChatConfig();await refresh();
    onEvent('sent',{session_id:sid});return {session_id:sid};
  }finally{submitting=false;await refresh();}
}
// Sends the current composer draft on form submission.
$('composer').onsubmit=guard(()=>submit($('chat-input').value.trim(),true));
// Continue is a new user turn, not an automatic retry of an exhausted run.
$('chat-continue-btn').onclick=guard(()=>submit('Continue'));
// Enter submits unless Shift is held or IME composition is active.
$('chat-input').onkeydown=event=>{if(event.key==='Enter'&&!event.shiftKey&&!event.isComposing){event.preventDefault();if(!$('chat-send').disabled)$('composer').requestSubmit();}};
// Stops the run currently identified on the stop control.
$('chat-stop-btn').onclick=guard(()=>cancel($('chat-stop-btn').dataset.run));

  /** Clears session visuals and disconnects every session-scoped transport/control. */
  function disconnect(){issues?.select('');messagingRoutes.disconnect();screenAwake.select('');followups.reset();session='';activityRuns=[];elementContext='';anchor.reset();progress.reset();notifications.clearTools();$('runs').replaceChildren();closeGutterOverlays();gutterRuns.clear();stats.disconnect();media.disconnect();controls.disconnect();sharing.disconnect();activity.disconnect();closeChatElementFullView();window.speechSynthesis?.cancel();resetReads();client.disconnect();}
  /** Disconnects and disposes panel-owned timers, observers, listeners, and mounted child controls. */
  function destroy(){
    mainNotes?.destroy();disposeFeedback();screenAwake.destroy();++sessionGeneration;closeUndoDialog?.();
    disconnect();followups.destroy();notifications.destroy();anchor.destroy();progress.destroy();clearInterval(gutterClock);clearInterval(reviewTimer);stats.destroy();media.destroy();controls.destroy();sharing.destroy();sessionList.destroy();dropdown.destroy();readAbort.abort();chatClearanceObserver.disconnect();
    $('sessions').onchange=null;$('new').onclick=null;$('composer').onsubmit=null;
    $('chat-input').onkeydown=null;$('chat-stop-btn').onclick=null;$('chat-continue-btn').onclick=null;
  }
  /** Creates and opens a new conversation, rejecting while a message or upload is pending. */
  async function newSession(){
    if(submitting||media.uploading)throw Error('Wait for the pending message or upload');
    const item=await client.create({agent_id:getAgentId(),client_request_id:crypto.randomUUID()});
    await sessions();await open(item.id);return {session_id:item.id};
  }
  /** Returns a run from the selected conversation or throws when it is absent. */
  function selectedRun(id){const run=activityRuns.find(r=>r.id===id);if(!run)throw Error('Run is not in the selected conversation');return run;}
  const panel = {open, sessions, runs, disconnect, destroy, newSession,
    /** Reads the panel's current session ID. */
    get sessionId(){return session;},
    /** Focuses and opens the conversation selector. */
    showSessions(){$('session-dropdown-trigger').focus({preventScroll:true});return dropdown.open();},
    /** Stores user-selected page context up to 2,000 characters; rejects larger or non-string values. */
    setElementContext(text){if(typeof text!=='string'||text.length>2000)throw Error('Invalid page context');elementContext=text;},
    /** Sets the composer draft and reapplies the configured layout; rejects non-text input. */
    setDraft(text){if(typeof text!=='string')throw Error('Draft must be text');$('chat-input').value=text;applyChatConfig();},
    /** Sets and submits a programmatic text draft; call with this bound to the returned panel. */
    async send(text){if(typeof text!=='string')throw Error('Message must be text');this.setDraft(text);return submit(text.trim(),true);},
    /** Starts a new Continue user turn. */
    continue:()=>submit('Continue'),
    /** Cancels the selected run by ID or the first active run; throws if none is active. */
    async stop(id){const run=id?selectedRun(id):activityRuns.find(r=>['queued','running','waiting_approval','waiting_gate','waiting_external'].includes(r.status));if(!run)throw Error('No active run');await cancel(run.id);},
    /** Decides an approval only when it is pending in this conversation, then refreshes shell state. */
    async approve(id,decision){const run=activityRuns.find(r=>r.pending?.id===id&&r.status==='waiting_approval');if(!run||run.pending.can_decide===false||sharedAccess?.observer)throw Error('Approval is not available to this user');await client.decide(id,{decision,client_request_id:keyId('approval:'+id+':'+decision)});await refresh();},
    /** Resolves a selected uncertain/recovery run and refreshes shell state. */
    async recover(id,decision,output){if(sharedAccess?.can_control===false)throw Error('This conversation is read only');const run=selectedRun(id);if(!run.uncertain&&run.status!=='recovery')throw Error('Run is not waiting for recovery');await client.recover(id,{decision,output,client_request_id:keyId('recovery:'+id+':'+decision+':'+output)});await refresh();}
  };
  // One capture owner also covers chat portals and the shell Chat/Float buttons.
  const disposeFeedback=mountChatControlFeedback(root,{portals:true,shell:true});
  mountedPanels.set(root, panel);
  return panel;
}
