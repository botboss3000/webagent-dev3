import {createChatNotification} from './notification-panel.js';
// @code-index {"version":1,"feature":"chat-followup-queue","aliases":["queued message delivery","message followup queue","delivery queue controls"],"purpose":"Projects durable queued-message state beside transcript rows and commits revision-fenced user choices. No chat JSON key is consumed; consider queue status presentation or placement preferences while keeping access and revision checks fixed.","span_lines":80,"configuration":{"status":"opportunity","references":[],"opportunities":["Consider configurable queue status labels or placement preferences; access and revision checks remain enforced."]}}
// Durable delivery state is display-only until a revision-checked user action commits.
/**
 * Mount the session queued-message delivery controls beside its transcript.
 * Inputs are panel-owned DOM, transport, session and access adapters; writes use the
 * displayed queue revision plus a durable request key, then refresh from a snapshot.
 * The generation fence prevents late reads/writes from repainting a newly selected session.
 * Returns update/reset/paint, pending state, and destroy for the owning panel lifecycle.
 */
export function mountFollowupQueue({root,host,client,getSession,guard,note,keyId,forgetKey,onSnapshot,canWrite=()=>true}) {
  let generation=0,revision=-1,entries=[],held=false,bannerKey='';
  const busy=new Set();
  const banner=createChatNotification({title:'Queue held',guard});banner.className+=' followup-queue-banner';banner.hidden=true;host.append(banner);
  const terminal=new Set(['delivered','cancelled','rejected']);
  // Translate durable queue states into short status text for transcript controls.
  const label=entry=>({pending:entry.choice==='interrupt'?'Interrupting…':entry.choice==='steer'?'Steering — applies next turn':'Queued — sends after this run',reviewing:'Reviewing…',held:'Held — waiting to resume',interrupting:'Interrupting…',delivered:'Delivered',cancelled:'Cancelled',rejected:'Not sent — rejected'})[entry.status]||entry.status;
  // Create a text-only action button and pass its callback through the panel guard.
  function makeButton(text,action){const b=document.createElement('button');b.type='button';b.textContent=text;b.onclick=guard(action);return b;}
  // Reconcile visible delivery rows from the current queue while retaining focused controls.
  function paint(){
    for(const row of root.querySelectorAll('[data-public-id]')) {
      // Nested bubble IDs serve anchoring; only the outer transcript row owns delivery controls.
      if(row.parentElement?.closest('[data-public-id]'))continue;
      const entry=entries.find(item=>item.message_id===row.dataset.publicId);
      let box=row.querySelector('.followup-delivery');
      if(!entry){box?.remove();continue;}
      if(!box){box=document.createElement('div');box.className='followup-delivery';row.append(box);}
      const paintKey=JSON.stringify(entry);
      if(box.queuePaintKey===paintKey){for(const button of box.querySelectorAll('button'))button.disabled=!canWrite()||busy.has(entry.id);continue;}
      const focused=box.contains(document.activeElement)?document.activeElement:null;
      box.queuePaintKey=paintKey;box.replaceChildren();row.dataset.deliveryStatus=entry.status;
      const status=document.createElement('p');status.className='followup-delivery-status';status.setAttribute('role','status');status.setAttribute('aria-live','polite');status.textContent=label(entry);box.append(status);
      if(entry.reason){const reason=document.createElement('p');reason.textContent=entry.reason;box.append(reason);}
      if(terminal.has(entry.status)||entry.status==='interrupting'||entry.choice==='interrupt'){if(focused){status.tabIndex=-1;status.focus({preventScroll:true});}continue;}
      const actions=document.createElement('div');actions.className='followup-delivery-actions';actions.setAttribute('aria-label','Message delivery');
      for(const [choice,text] of [['interrupt','Interrupt'],['steer','Steer'],['queue','Remain queued'],['cancel','Cancel message']]){
        const button=makeButton(text,()=>change(entry,choice));button.dataset.deliveryChoice=choice;button.disabled=!canWrite()||busy.has(entry.id);button.setAttribute('aria-pressed',String(entry.choice===choice));actions.append(button);
      }
      box.append(actions);
      if(focused){const replacement=[...actions.children].find(button=>button.dataset.deliveryChoice===focused.dataset.deliveryChoice&&!button.disabled);if(replacement)replacement.focus({preventScroll:true});else{status.tabIndex=-1;status.focus({preventScroll:true});}}
    }
  }
  // Refresh the authoritative snapshot only if this session still owns the request.
  async function refresh(sid,epoch){const data=await client.snapshot(sid);if(epoch!==generation||sid!==getSession())return;onSnapshot(data);}
  // Commit one revision-checked delivery choice and reconcile errors from a fresh snapshot.
  async function change(entry,choice){
    if(!canWrite())throw Error("Message delivery controls are not available to this user");
    if(busy.has(entry.id))return;
    const sid=getSession(),epoch=generation,key='delivery:'+sid+':'+entry.id+':'+entry.revision+':'+choice;
    busy.add(entry.id);paint();
    try{await client.delivery(sid,entry.id,{expected_revision:entry.revision,client_request_id:keyId(key),choice});forgetKey(key);await refresh(sid,epoch);}
    catch(error){if(epoch===generation&&sid===getSession()){note('Message delivery update failed: '+error.message);await refresh(sid,epoch);}throw error;}
    finally{if(epoch===generation){busy.delete(entry.id);paint();}}
  }
  // Accept monotonic snapshots and repaint queue state plus the held-queue banner.
  function update(data){
    const state=data.queue_state||{},next=Number(state.revision)||0;
    if(next<revision)return;revision=next;entries=Array.isArray(data.queue)?data.queue:[];held=!!state.held;
    const nextBannerKey=JSON.stringify([held,state.reason,next,canWrite()]);
    if(nextBannerKey===bannerKey){paint();return;}
    const bannerFocused=banner.contains(document.activeElement);bannerKey=nextBannerKey;
    banner.replaceChildren();banner.hidden=!held;
    if(held){
      const view=createChatNotification({title:'Queue held',message:state.reason||undefined,guard,actions:[{label:'Resume queue',kind:'approve',disabled:!canWrite(),run:async()=>{
        if(!canWrite())throw Error('Queue resume is not available to this user');
        const sid=getSession(),epoch=generation,key='resume-queue:'+sid+':'+revision;b.disabled=true;
        try{await client.resumeQueue(sid,{client_request_id:keyId(key),expected_revision:revision});forgetKey(key);await refresh(sid,epoch);}
        catch(error){if(epoch===generation&&sid===getSession())await refresh(sid,epoch);throw error;}
        finally{if(epoch===generation)b.disabled=!canWrite();}
      }}]});
      const b=view.querySelector('button');banner.replaceChildren(...view.childNodes);if(bannerFocused)b.focus({preventScroll:true});
    }
    paint();
  }
  // Invalidate outstanding work and clear this session queue presentation.
  function reset(){++generation;revision=-1;entries=[];held=false;busy.clear();bannerKey='';banner.replaceChildren();banner.hidden=true;paint();}
  // Dispose the mount after clearing state so no old queue UI remains in the panel.
  return {update,reset,paint,/* True while any queued entry is not in a terminal state. */get pending(){return entries.some(e=>!terminal.has(e.status));},/* Reset state and remove this mount from the panel host. */destroy(){reset();banner.remove();}};
}
