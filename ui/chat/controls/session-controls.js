// Locates session-scoped execution-mode and next-turn model controls Control visibility/layout: config/ui/chat/chat.json ->…
// V2 cycle/picker presentation with session-scoped, revision-fenced Rust writes.
import {createEffortSlider} from '../../shared/models/effort-slider.js';
// Compact role markers on picker rows (V2 footer picker).
const ROLE_ICONS={standard:['T','Text'],vision:['👁','Vision'],image:['🖼','Image generation'],
  system:['⚙','System'],voice:['🎤','Voice'],decision:['D','Decision']};
/** Model picker rows and the row that shows the saved selection, hiding the mode-default row when a roster model is the same model. */
export function modelRows(models=[],selected=null) {
  const fallback=models.find(item=>item.entry_id==null)||null;
  const duplicate=fallback&&models.find(item=>item.entry_id!=null&&item.model===fallback.model&&item.display===fallback.display);
  const rows=duplicate?models.filter(item=>item!==fallback):models.slice();
  const selectedId=selected?.entry_id??duplicate?.entry_id??null;
  return {rows,selectedId};
}
/** Wording of the transcript row for mode and model changes since the last message, as [text, name] pairs; empty once both are back where they started. */
export function changeParts(before,now) {
  const parts=[];
  if(now.mode!==before.mode)parts.push(['Mode changed to ',now.mode]);
  if(now.model&&now.model!==before.model)parts.push(['Model changed to ',now.model]);
  else if(now.model&&parts.length)parts.push(['Model stays ',now.model]);
  return parts;
}
/** Purpose: Mount execution-mode and model controls for the current conversation. The optional onState callback receives refreshed control data so panel.js can gate composer features. The panel drives open/updateRuns/disconnect/destroy; this module reads client.controls and writes client.mode/client.model. */
export function mountSessionControls({root,client,guard,note,issues,ensureSession,notifications,onState=()=>{},canWrite=()=>true}) {
    // Resolve controls within this chat root rather than the whole document.
  const $=id=>root.querySelector('#'+id);
  const mode=$('chat-mode-btn'), model=$('chat-model-changer');
  const abort=new AbortController();
  let session='',generation=0,state=null,busy=false,inflight=null,active=null;
  const picker=document.createElement('div');
  picker.className='chat-skill-picker chat-model-list-panel v3-chat-model-list';
  picker.hidden=true;picker.setAttribute('role','dialog');picker.setAttribute('aria-label','Conversation model');
  document.body.append(picker);
  // Persistent effort control below the roster, for the selected model in this conversation.
  const effort=createEffortSlider({defaultName:'Saved default',onCommit:level=>setEffort(level)});
  model.setAttribute('aria-haspopup','dialog');model.setAttribute('aria-expanded','false');
  // One transcript row reports the mode and model changes made since the last message; each further change rewrites it.
  const transcript=$('chat-messages-inner');
  let notice=null,baseline=null;
  const current=()=>({mode:state?.execution_modes.find(item=>item.id===state.mode.execution_mode)?.label||'',
    model:state?.models.find(item=>item.entry_id===state.model.entry_id)?.display||''});
  function report(before){
    if(!transcript||!state)return;
    // A message after the row closes it, so the next change starts a new row.
    let later=notice?.nextElementSibling;
    while(later&&!later.dataset?.publicId)later=later.nextElementSibling;
    if(!notice?.isConnected||later){notice=null;baseline=before;}
    const parts=changeParts(baseline,current());
    if(!parts.length){notice?.remove();notice=null;return;}
    if(!notice){notice=document.createElement('div');notice.className='chat-change-notice';notice.setAttribute('role','status');transcript.append(notice);}
    notice.replaceChildren(...parts.flatMap(([text,name],index)=>{
      const value=document.createElement('b');value.textContent=name;
      return [document.createTextNode((index?' · ':'')+text),value];
    }));
    notice.scrollIntoView?.({block:'nearest'});
  }
    // Hide the picker and clear the model button expanded state.
  function close(){picker.hidden=true;model.setAttribute('aria-expanded','false');}
    // Render the current execution mode, selected model, and active run model without treating display state as a write.
  function paint(){
    const modes=state?.execution_modes||[];
    const definition=modes.find(item=>item.id===state?.mode.execution_mode);
    mode.textContent=definition?.label||'Ask';
    mode.title=definition?definition.description:'Choose execution mode';
    mode.disabled=busy||!state||!canWrite();model.disabled=busy||!state||!canWrite();
    const selected=state?.models.find(item=>item.entry_id===state.model.entry_id);
    const running=active?.model?.model;
    model.querySelector('.cmc-name').textContent=selected?.label||running||'Choose model';
    model.title=running?'Running: '+running+'. Next loop turn: '+(selected?.label||'Unavailable'):
      'Next loop turn: '+(selected?.label||'Unavailable')+' — click for models; arrows cycle';
    model.classList.toggle('no-model',!!state&&!selected);
  }
    // Share one in-flight controls request and ignore stale session generations; a missing session clears state, while other errors propagate.
  async function load(){
    if(!session)return;
    if(inflight)return inflight;
    const sid=session,epoch=generation;
    const pending=client.controls(sid).then(data=>{
      if(sid!==session||epoch!==generation)return;
      state=data;onState(data);paint();issues?.resolve('session_controls',sid);
    }).catch(error=>{if(sid!==session||epoch!==generation)return;if(issues)issues.fail('session_controls',error,sid);if(error.status===404){if(sid===session&&epoch===generation){state=null;onState(null);paint();}return;}throw error;}).finally(()=>{if(inflight===pending)inflight=null;});
    inflight=pending;
    return pending;
  }
    // Ensure a session and a current control snapshot before any mode/model action.
  async function ready(){
    if(!canWrite())throw Error('This conversation is read only');
    if(!session)await ensureSession();
    await load();
    if(!state)throw Error('Conversation controls are not ready');
  }
    // Advance through server-permitted modes; entering Auto mode can require confirmation, and the revision-fenced write is followed by a refresh.
  async function cycleMode(){
    if(busy)return;
    await ready();
    const sid=session,epoch=generation,snapshot=state.mode,before=current();
    const modes=state.execution_modes;
    if(!modes.length)return;
    const index=modes.findIndex(item=>item.id===snapshot.execution_mode);
    const next=modes[(index+1)%modes.length];
    const confirmationRequired=next.id==='auto'&&state.confirm_auto_mode!==false;
    if(confirmationRequired&&!await notifications.confirmMode(next.label))return;
    if(sid!==session||epoch!==generation)return;
    state={...state,mode:{...snapshot,execution_mode:next.id}};
    busy=true;paint();
    let changed=false;
    try{
      await client.mode(sid,{mode:next.id,expected_revision:snapshot.revision,confirm_write:confirmationRequired});
      changed=true;
    }finally{if(sid===session&&epoch===generation){busy=false;await load();paint();if(changed)report(before);}}
  }
    // Persist the selected connected model with the snapshot revision and a unique request ID; it applies at the next model-call boundary; an already-dispatched call retains its pinned model.
  async function choose(entry){
    if(busy)return;
    // The hidden mode-default row is the model the mode already follows; choosing it keeps that default instead of pinning the model.
    if(state.model.entry_id==null&&entry.entry_id===modelRows(state.models,state.model).selectedId){close();return;}
    const sid=session,epoch=generation,revision=state.model.revision,before=current();
    busy=true;paint();
    let changed=false;
    try{
      await client.model(sid,{entry_id:entry.entry_id,expected_revision:revision,client_request_id:crypto.randomUUID()});
      changed=true;
      if(sid===session&&epoch===generation)close();
    }finally{if(sid===session&&epoch===generation){busy=false;await load();paint();if(changed)report(before);}}
  }
    // Save this conversation's effort for the selected model (null = that model's saved default); the revision fence applies.
  async function setEffort(level){
    const sid=session,epoch=generation,entry=state.model.entry_id;
    await client.model(sid,{entry_id:entry,expected_revision:state.model.revision,client_request_id:crypto.randomUUID(),effort:level});
    if(sid===session&&epoch===generation){await load();paint();}
  }
    // Populate and position the permitted-model picker, disabling disconnected entries and preserving composer focus on pointer activation.
  function show(){
    picker.replaceChildren();
    const list=document.createElement('div');list.className='csp-list';picker.append(list);
    const {rows,selectedId}=modelRows(state.models,state.model);
    for(const entry of rows){
      const active=entry.entry_id===selectedId;
      const button=document.createElement('button');button.type='button';button.className='cml-item'+(active?' cml-active csp-active':'');
      const name=document.createElement('span');name.className='cml-name';
      name.textContent=entry.label+(entry.connected?'':' · Connection required');button.append(name);
      const icons=document.createElement('span');icons.className='cml-role-icons';
      for(const role of entry.roles||[]){
        const [symbol,title]=ROLE_ICONS[role]||[];if(!symbol)continue;
        const icon=document.createElement('span');icon.className='cml-role-icon';icon.textContent=symbol;icon.title=title;icon.setAttribute('aria-label',title);icons.append(icon);
      }
      if(icons.childElementCount)button.append(icons);
      button.title=entry.model;button.disabled=!entry.connected;
      button.setAttribute('aria-pressed',String(active));
      button.onclick=guard(()=>choose(entry));list.append(button);
    }
    const selected=state.models.find(item=>item.entry_id===state.model.entry_id);
    effort.set({levels:selected?.efforts||[],value:state.model.effort||null,defaultEffort:selected?.default_effort||'',disabled:busy||!canWrite()});
    picker.append(effort.element);
    if(!rows.length){const text=document.createElement('p');text.className='csp-empty';text.textContent='No permitted chat models. Add one on this agent’s Models page.';list.append(text);}
    const rect=model.getBoundingClientRect();
    picker.style.bottom=Math.max(8,innerHeight-rect.top+6)+'px';
    picker.style.right=Math.max(8,innerWidth-rect.right)+'px';
    picker.hidden=false;model.setAttribute('aria-expanded','true');
    // Pointer activation keeps typing focus; keyboard activation enters the list.
    if(document.activeElement!==$('chat-input'))list.querySelector('button:not(:disabled)')?.focus();
  }
    // Cycle only through connected models and delegate the revisioned write to choose.
  async function cycleModel(delta){
    if(busy)return;
    await ready();
    const entries=state.models.filter(item=>item.connected);
    if(!entries.length)return;
    const index=entries.findIndex(item=>item.entry_id===state.model.entry_id);
    await choose(entries[(index+delta+entries.length)%entries.length]);
  }
    // Guarded mode/model listeners route click and arrow-key actions through the same permission and revision checks; document listeners close the picker and are removed by abort.
  mode.addEventListener('click',guard(cycleMode),{signal:abort.signal});
  model.addEventListener('click',guard(async event=>{
    const direction=event.target.closest('.cmc-chev-left')?-1:event.target.closest('.cmc-chev-right')?1:0;
    if(direction){await cycleModel(direction);return;}
    if(!picker.hidden){close();return;}
    await ready();show();
  }),{signal:abort.signal});
  model.addEventListener('keydown',event=>{
    if(['ArrowLeft','ArrowRight'].includes(event.key)){event.preventDefault();guard(()=>cycleModel(event.key==='ArrowLeft'?-1:1))();}
  },{signal:abort.signal});
  document.addEventListener('click',event=>{if(!picker.contains(event.target)&&!model.contains(event.target))close();},{signal:abort.signal});
  document.addEventListener('keydown',event=>{if(event.key==='Escape'&&!picker.hidden){close();model.focus();}},{signal:abort.signal});
  window.addEventListener('resize',close,{signal:abort.signal});
  return {
        // Fence older requests, reset conversation state, clear pending mode confirmation, and load controls for the newly opened session.
    async open(id){generation++;session=id;state=null;inflight=null;active=null;busy=false;close();notifications.cancelMode();paint();await load();},
        // Track the current actionable run for display and refresh controls when the session is idle.
    updateRuns(items){active=items.find(run=>!run.parked&&['queued','running','waiting_approval','waiting_gate'].includes(run.status));paint();if(session&&!busy)void load().catch(error=>{if(!issues)note(error.message);});},
        // Invalidate pending loads and clear session, run, busy, picker, and confirmation state.
    disconnect(){generation++;session='';state=null;inflight=null;active=null;busy=false;close();notifications.cancelMode();paint();},
        // Disconnect session state, remove abort-scoped listeners, and remove the portal picker.
    destroy(){this.disconnect();abort.abort();picker.remove();}
  };
}
