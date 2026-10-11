// Durable delivery state is display-only until a revision-checked user action commits.
export function mountFollowupQueue({root,host,client,getSession,guard,note,keyId,forgetKey,onSnapshot}) {
  let generation=0,revision=-1,entries=[],held=false,bannerKey='';
  const busy=new Set();
  const banner=document.createElement('section');banner.className='followup-queue-banner';banner.hidden=true;host.append(banner);
  const terminal=new Set(['delivered','cancelled','rejected']);
  const label=entry=>({pending:entry.choice==='interrupt'?'Interrupting…':entry.choice==='steer'?'Steering — applies next turn':'Queued — sends after this run',reviewing:'Reviewing…',held:'Held — waiting to resume',interrupting:'Interrupting…',delivered:'Delivered',cancelled:'Cancelled',rejected:'Not sent — rejected'})[entry.status]||entry.status;
  function makeButton(text,action){const b=document.createElement('button');b.type='button';b.textContent=text;b.onclick=guard(action);return b;}
  function paint(){
    for(const row of root.querySelectorAll('[data-public-id]')) {
      const entry=entries.find(item=>item.message_id===row.dataset.publicId);
      let box=row.querySelector('.followup-delivery');
      if(!entry){box?.remove();continue;}
      if(!box){box=document.createElement('div');box.className='followup-delivery';row.append(box);}
      const paintKey=JSON.stringify(entry);
      if(box.queuePaintKey===paintKey){for(const button of box.querySelectorAll('button'))button.disabled=busy.has(entry.id);continue;}
      const focused=box.contains(document.activeElement)?document.activeElement:null;
      box.queuePaintKey=paintKey;box.replaceChildren();row.dataset.deliveryStatus=entry.status;
      const status=document.createElement('p');status.className='followup-delivery-status';status.setAttribute('role','status');status.setAttribute('aria-live','polite');status.textContent=label(entry);box.append(status);
      if(entry.reason){const reason=document.createElement('p');reason.textContent=entry.reason;box.append(reason);}
      if(terminal.has(entry.status)||entry.status==='interrupting'||entry.choice==='interrupt'){if(focused){status.tabIndex=-1;status.focus({preventScroll:true});}continue;}
      const actions=document.createElement('div');actions.className='followup-delivery-actions';actions.setAttribute('aria-label','Message delivery');
      for(const [choice,text] of [['interrupt','Interrupt'],['steer','Steer'],['queue','Remain queued'],['cancel','Cancel message']]){
        const button=makeButton(text,()=>change(entry,choice));button.dataset.deliveryChoice=choice;button.disabled=busy.has(entry.id);button.setAttribute('aria-pressed',String(entry.choice===choice));actions.append(button);
      }
      box.append(actions);
      if(focused){const replacement=[...actions.children].find(button=>button.dataset.deliveryChoice===focused.dataset.deliveryChoice&&!button.disabled);if(replacement)replacement.focus({preventScroll:true});else{status.tabIndex=-1;status.focus({preventScroll:true});}}
    }
  }
  async function refresh(sid,epoch){const data=await client.snapshot(sid);if(epoch!==generation||sid!==getSession())return;onSnapshot(data);}
  async function change(entry,choice){
    if(busy.has(entry.id))return;
    const sid=getSession(),epoch=generation,key='delivery:'+sid+':'+entry.id+':'+entry.revision+':'+choice;
    busy.add(entry.id);paint();
    try{await client.delivery(sid,entry.id,{expected_revision:entry.revision,client_request_id:keyId(key),choice});forgetKey(key);await refresh(sid,epoch);}
    catch(error){if(epoch===generation&&sid===getSession()){note('Message delivery update failed: '+error.message);await refresh(sid,epoch);}throw error;}
    finally{if(epoch===generation){busy.delete(entry.id);paint();}}
  }
  function update(data){
    const state=data.queue_state||{},next=Number(state.revision)||0;
    if(next<revision)return;revision=next;entries=Array.isArray(data.queue)?data.queue:[];held=!!state.held;
    const nextBannerKey=JSON.stringify([held,state.reason,next]);
    if(nextBannerKey===bannerKey){paint();return;}
    const bannerFocused=banner.contains(document.activeElement);bannerKey=nextBannerKey;
    banner.replaceChildren();banner.hidden=!held;
    if(held){const text=document.createElement('p');text.setAttribute('role','status');text.textContent='Queue held'+(state.reason?' — '+state.reason:'');banner.append(text);const b=makeButton('Resume queue',async()=>{
      const sid=getSession(),epoch=generation,key='resume-queue:'+sid+':'+revision;b.disabled=true;
      try{await client.resumeQueue(sid,{client_request_id:keyId(key),expected_revision:revision});forgetKey(key);await refresh(sid,epoch);}
      catch(error){if(epoch===generation&&sid===getSession())await refresh(sid,epoch);throw error;}
      finally{if(epoch===generation)b.disabled=false;}
    });banner.append(b);if(bannerFocused)b.focus({preventScroll:true});}
    paint();
  }
  function reset(){++generation;revision=-1;entries=[];held=false;busy.clear();bannerKey='';banner.replaceChildren();banner.hidden=true;paint();}
  return {update,reset,paint,get pending(){return entries.some(e=>!terminal.has(e.status));},destroy(){reset();banner.remove();}};
}
